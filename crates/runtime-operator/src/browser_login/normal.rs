// SPDX-License-Identifier: MIT
//! Human login in the ordinary browser, followed by an explicit extension handoff.
//! Only an ephemeral loopback endpoint exists. No browser debugging, password
//! access, background extension process or Runtime-side web server is involved.
use super::{Platform, Result, Secret, session};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
const ORIGIN: &str = "chrome-extension://ogmckfbpliahokocgfcebhicpjokpdag";
const MAX_BODY: usize = 32768;
const FILES: &[(&str, &[u8])] = &[
    ("manifest.json", include_bytes!("extension/manifest.json")),
    ("connect.html", include_bytes!("extension/connect.html")),
    ("connect.js", include_bytes!("extension/connect.js")),
    ("style.css", include_bytes!("extension/style.css")),
    ("probe.svg", include_bytes!("extension/probe.svg")),
    ("icon-16.png", include_bytes!("extension/icon-16.png")),
    ("icon-32.png", include_bytes!("extension/icon-32.png")),
    ("icon-128.png", include_bytes!("extension/icon-128.png")),
];
pub struct Context {
    pub account: String,
    pub destination: String,
}
impl Context {
    pub fn from_options(options: &crate::Options) -> Self {
        Self {
            account: options.account.clone(),
            destination: options
                .ssh
                .clone()
                .unwrap_or_else(|| "Local Runtime / 本机 Runtime".into()),
        }
    }
}
fn install() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("browser_extension_directory_unavailable")?;
    if !base.is_absolute() {
        return Err("browser_extension_directory_unavailable".into());
    }
    let path = base.join("glimlink/browser-extension");
    fs::create_dir_all(&path).map_err(|_| "browser_extension_directory_unavailable")?;
    let meta =
        fs::symlink_metadata(&path).map_err(|_| "browser_extension_directory_unavailable")?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != unsafe { libc::geteuid() } {
        return Err("browser_extension_directory_unavailable".into());
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .map_err(|_| "browser_extension_directory_unavailable")?;
    for (name, source) in FILES {
        let mut random = [0u8; 12];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut random))
            .map_err(|_| "browser_extension_directory_unavailable")?;
        let suffix = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let staging = path.join(format!(".{name}.{suffix}.new"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&staging)
            .map_err(|_| "browser_extension_directory_unavailable")?;
        if !file
            .metadata()
            .map_err(|_| "browser_extension_directory_unavailable")?
            .is_file()
        {
            return Err("browser_extension_directory_unavailable".into());
        }
        let result = file
            .write_all(source)
            .and_then(|_| file.sync_all())
            .and_then(|_| fs::rename(&staging, path.join(name)));
        if result.is_err() {
            let _ = fs::remove_file(&staging);
            return Err("browser_extension_directory_unavailable".into());
        }
    }
    Ok(path)
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn page(platform: Platform, context: &Context, port: u16, nonce: &str, directory: &Path) -> String {
    let target = escape(&context.destination);
    let account = escape(&context.account);
    let directory = escape(&directory.to_string_lossy());
    format!(
        r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Glimlink · 连接账号</title><style>{style}a{{display:inline-block;border-radius:30px;background:#663bdc;color:white;padding:14px 24px;text-decoration:none}}details{{margin-top:24px}}code{{overflow-wrap:anywhere}}li{{margin:12px 0}}</style><main><p class="eyebrow">GLIMLINK · SESSION BRIDGE</p><h1>连接 {site} / Connect {site}</h1><p>{target} · {account}</p><p>使用这个浏览器已登录的账号。<br>Use the account already signed in in this browser.</p><a href="{origin}/connect.html#{port}/{nonce}" rel="noreferrer">打开扩展连接 / Connect with extension</a><details open><summary>首次使用 / First use</summary><ol><li>在 <code>chrome://extensions</code> 开启开发者模式，加载下面的扩展目录。<br>Enable Developer mode, then Load unpacked from:</li><li><code>{directory}</code></li><li>然后点击上面的连接按钮。仅请求这个站点的临时访问权限。<br>Then connect above. Only temporary access to this site is requested.</li></ol></details><p>账号仍须由 Runtime 独立核验。<br>Runtime independently verifies the account before saving.</p></main></html>"#,
        style = include_str!("extension/style.css"),
        site = platform.name(),
        origin = ORIGIN
    )
}
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
fn read(stream: &mut TcpStream, deadline: Instant) -> Result<Request> {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|_| "browser_bridge_request")?;
    let end = deadline.min(Instant::now() + Duration::from_secs(1));
    let mut bytes = Vec::new();
    let mut buf = [0; 1024];
    let header_end = loop {
        if Instant::now() >= end || bytes.len() > 16384 {
            return Err("browser_bridge_request".into());
        }
        if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream
            .read(&mut buf)
            .map_err(|_| "browser_bridge_request")?;
        if n == 0 {
            return Err("browser_bridge_request".into());
        }
        bytes.extend_from_slice(&buf[..n]);
    };
    let text = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "browser_bridge_request")?;
    let mut lines = text.split("\r\n");
    let first = lines
        .next()
        .ok_or("browser_bridge_request")?
        .split(' ')
        .collect::<Vec<_>>();
    if first.len() != 3 || !matches!(first[2], "HTTP/1.1" | "HTTP/1.0") {
        return Err("browser_bridge_request".into());
    }
    let method = first[0].to_string();
    let path = first[1].to_string();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|v| !v.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("browser_bridge_request")?;
        if headers
            .insert(key.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err("browser_bridge_request".into());
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err("browser_bridge_request".into());
    }
    let size = headers
        .get("content-length")
        .map(|v| v.parse::<usize>().map_err(|_| "browser_bridge_request"))
        .transpose()?
        .unwrap_or(0);
    if size > MAX_BODY || bytes.len() > header_end + size {
        return Err("browser_bridge_request".into());
    }
    while bytes.len() < header_end + size {
        if Instant::now() >= end {
            return Err("browser_bridge_request".into());
        }
        let n = stream
            .read(&mut buf)
            .map_err(|_| "browser_bridge_request")?;
        if n == 0 {
            return Err("browser_bridge_request".into());
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() > header_end + size {
            return Err("browser_bridge_request".into());
        }
    }
    Ok(Request {
        method,
        path,
        headers,
        body: bytes[header_end..].to_vec(),
    })
}
fn response(stream: &mut TcpStream, status: u16, mime: &str, body: &str) -> Result<()> {
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .map_err(|_| "browser_bridge_response")?;
    let headers = format!(
        "HTTP/1.1 {status} Response\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'\r\nAccess-Control-Allow-Origin: {ORIGIN}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nVary: Origin\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .map_err(|_| "browser_bridge_response".into())
}
struct Flow {
    listener: TcpListener,
    host: String,
    path: String,
    platform: Platform,
    context: Context,
    page: String,
    deadline: Instant,
}
impl Flow {
    fn new(
        platform: Platform,
        context: Context,
        directory: &Path,
        deadline: Instant,
    ) -> Result<Self> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|_| "browser_bridge_unavailable")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "browser_bridge_unavailable")?;
        let port = listener
            .local_addr()
            .map_err(|_| "browser_bridge_unavailable")?
            .port();
        let mut random = [0u8; 24];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut random))
            .map_err(|_| "browser_bridge_unavailable")?;
        let nonce = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = format!("/{nonce}");
        let page = page(platform, &context, port, &nonce, directory);
        Ok(Self {
            listener,
            host: format!("127.0.0.1:{port}"),
            path,
            platform,
            context,
            page,
            deadline,
        })
    }
    fn url(&self) -> String {
        format!("http://{}{}", self.host, self.path)
    }
    fn serve(&self, stream: &mut TcpStream) -> Result<Option<Secret>> {
        let request = read(stream, self.deadline)?;
        if Instant::now() >= self.deadline {
            response(stream, 410, "application/json", "{\"error\":\"expired\"}")?;
            return Ok(None);
        }
        if request.headers.get("host") != Some(&self.host)
            || request.headers.get("origin").is_some_and(|v| v != ORIGIN)
        {
            response(stream, 403, "application/json", "{\"error\":\"forbidden\"}")?;
            return Ok(None);
        }
        if request.method == "OPTIONS"
            && request.headers.get("origin").is_some_and(|v| v == ORIGIN)
            && matches!(
                request.path.strip_prefix(&self.path),
                Some("/request" | "/complete")
            )
        {
            response(stream, 204, "application/json", "")?;
            return Ok(None);
        }
        if request.method == "GET" && request.path == self.path && request.body.is_empty() {
            response(stream, 200, "text/html; charset=utf-8", &self.page)?;
            return Ok(None);
        }
        if request.method == "GET"
            && request.path == format!("{}/request", self.path)
            && request.body.is_empty()
        {
            let data = json!({"version":1,"platform":self.platform.name(),"account":self.context.account,"destination":self.context.destination,"expires_in_seconds":self.deadline.saturating_duration_since(Instant::now()).as_secs()});
            response(stream, 200, "application/json", &data.to_string())?;
            return Ok(None);
        }
        if request.method == "POST"
            && request.path == format!("{}/complete", self.path)
            && request
                .headers
                .get("content-type")
                .is_some_and(|v| v.split(';').next() == Some("application/json"))
        {
            let value: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let valid = value.as_object().is_some_and(|v| v.len() == 4)
                && value["version"] == 1
                && value["platform"] == self.platform.name()
                && value["ready"] == true
                && value["cookies"].as_array().is_some_and(|v| v.len() <= 16);
            if valid {
                if let Ok(Some(secret)) = session(self.platform, &value["cookies"]) {
                    response(stream, 200, "application/json", "{\"submitted\":true}")?;
                    return Ok(Some(secret));
                }
            }
            response(
                stream,
                422,
                "application/json",
                "{\"error\":\"session_not_ready\"}",
            )?;
            return Ok(None);
        }
        response(stream, 404, "application/json", "{\"error\":\"not_found\"}")?;
        Ok(None)
    }
    fn wait(&self, cancel: &AtomicBool) -> Result<Option<Secret>> {
        while Instant::now() < self.deadline {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            match self.listener.accept() {
                Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                    if let Ok(Some(secret)) = self.serve(&mut stream) {
                        if cancel.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        if Instant::now() >= self.deadline {
                            return Err("operator_timeout".into());
                        }
                        return Ok(Some(secret));
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(_) => return Err("browser_bridge_unavailable".into()),
            }
        }
        Err("operator_timeout".into())
    }
}
pub(super) fn capture(
    platform: Platform,
    context: Context,
    deadline: Instant,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&'static str),
) -> Result<Option<Secret>> {
    let directory = install()?;
    let flow = Flow::new(platform, context, &directory, deadline)?;
    progress("normal_browser");
    let mut child = Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(flow.url())
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|_| "browser_not_found")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    flow.wait(cancel)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn flow() -> Flow {
        Flow::new(
            Platform::Pixiv,
            Context {
                account: "alice".into(),
                destination: "test-vps".into(),
            },
            Path::new("/synthetic-extension"),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
    }
    fn exchange(
        flow: &Flow,
        method: &str,
        path: &str,
        host: &str,
        origin: Option<&str>,
        body: &str,
    ) -> (String, Option<Secret>) {
        let mut client = TcpStream::connect(&flow.host).unwrap();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(origin) = origin {
            request.push_str(&format!("Origin: {origin}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        client.write_all(request.as_bytes()).unwrap();
        let (mut stream, _) = flow.listener.accept().unwrap();
        let secret = flow.serve(&mut stream).unwrap();
        drop(stream);
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        (response, secret)
    }
    fn payload() -> Value {
        json!({"version":1,"platform":"pixiv","ready":true,"cookies":[{"name":"PHPSESSID","value":"synthetic-session-not-a-real-account","domain":".pixiv.net","path":"/","secure":true}]})
    }
    #[test]
    fn ordinary_handoff_is_nonce_host_origin_platform_and_readiness_bound() {
        let flow = flow();
        let complete = format!("{}/complete", flow.path);
        for (path, host, origin) in [
            (complete.as_str(), "evil.invalid", Some(ORIGIN)),
            (
                complete.as_str(),
                flow.host.as_str(),
                Some("https://evil.invalid"),
            ),
            ("/wrong/complete", flow.host.as_str(), Some(ORIGIN)),
        ] {
            let (response, secret) =
                exchange(&flow, "POST", path, host, origin, &payload().to_string());
            assert!(secret.is_none());
            assert!(!response.starts_with("HTTP/1.1 200"));
            assert!(!response.contains("synthetic-session"));
        }
        for invalid in [
            json!({"platform":"discord","version":1,"ready":true,"cookies":payload()["cookies"]}),
            json!({"platform":"pixiv","version":1,"ready":false,"cookies":payload()["cookies"]}),
            json!({"platform":"pixiv","version":1,"ready":true,"cookies":[]}),
        ] {
            let (response, secret) = exchange(
                &flow,
                "POST",
                &complete,
                &flow.host,
                Some(ORIGIN),
                &invalid.to_string(),
            );
            assert!(secret.is_none());
            assert!(response.starts_with("HTTP/1.1 422"));
        }
        let (metadata, secret) = exchange(
            &flow,
            "GET",
            &format!("{}/request", flow.path),
            &flow.host,
            Some(ORIGIN),
            "",
        );
        assert!(secret.is_none());
        assert!(metadata.contains("test-vps"));
        assert!(!metadata.contains("synthetic-session"));
        let (ack, secret) = exchange(
            &flow,
            "POST",
            &complete,
            &flow.host,
            Some(ORIGIN),
            &payload().to_string(),
        );
        assert!(ack.contains("\"submitted\":true"));
        assert!(!ack.contains("synthetic-session"));
        assert_eq!(
            secret.unwrap().bytes(),
            b"PHPSESSID=synthetic-session-not-a-real-account"
        );
    }
    #[test]
    fn ordinary_handoff_does_not_wait_after_cancel_or_expiry_and_escapes_labels() {
        let f = flow();
        assert!(f.wait(&AtomicBool::new(true)).unwrap().is_none());
        let f = Flow::new(
            Platform::Pixiv,
            Context {
                account: "<script>".into(),
                destination: "<script>".into(),
            },
            Path::new("/synthetic"),
            Instant::now(),
        )
        .unwrap();
        assert!(!f.page.contains("<script>"));
        assert!(f.page.contains("&lt;script&gt;"));
        assert_eq!(
            f.wait(&AtomicBool::new(false)).err().unwrap(),
            "operator_timeout"
        );
        let manifest: Value =
            serde_json::from_str(include_str!("extension/manifest.json")).unwrap();
        assert!(manifest.get("background").is_none());
        assert_eq!(manifest["host_permissions"], json!(["http://127.0.0.1/*"]));
        assert_eq!(
            manifest["optional_host_permissions"],
            json!(["https://www.pixiv.net/*", "https://pixiv.net/*"])
        );
    }
    #[test]
    #[ignore = "real extension/loopback acceptance; dedicated network-disabled browser container only"]
    fn real_extension_pairing_fixture() {
        use super::super::{Browser, Profile};
        use std::sync::Arc;
        let directory = install().unwrap();
        let profile = Profile::create().unwrap();
        let f = flow();
        let url = f.url();
        let port = f.host.rsplit(':').next().unwrap();
        let extension_url = format!(
            "{ORIGIN}/connect.html#{port}/{}",
            f.path.trim_start_matches('/')
        );
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || f.wait(&worker_stop));
        let executable = std::env::var_os("GLIMLINK_FIXTURE_BROWSER").unwrap();
        let mut command = Command::new(executable);
        command
            .arg(format!("--user-data-dir={}", profile.0.display()))
            .arg(format!("--load-extension={}", directory.display()))
            .arg(format!(
                "--disable-extensions-except={}",
                directory.display()
            ))
            .args([
                "--headless",
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--remote-debugging-pipe",
                "--no-first-run",
                "about:blank",
            ]);
        let mut browser = Browser::spawn(command, profile).unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        let cancel = AtomicBool::new(false);
        let page = browser
            .cdp
            .call(
                "Target.createTarget",
                json!({"url":url}),
                None,
                end,
                &cancel,
            )
            .unwrap();
        let target = page["targetId"].as_str().unwrap();
        let attached = browser
            .cdp
            .call(
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
                None,
                end,
                &cancel,
            )
            .unwrap();
        let sid = attached["sessionId"].as_str().unwrap();
        browser
            .cdp
            .call(
                "Page.navigate",
                json!({"url":extension_url}),
                Some(sid),
                end,
                &cancel,
            )
            .unwrap();
        let mut ready = false;
        while Instant::now() < end {
            if let Ok(v)=browser.cdp.call("Runtime.evaluate",json!({"expression":"({id:chrome.runtime?.id,ready:document.getElementById('connect')?.disabled===false,target:document.getElementById('target')?.textContent})","returnByValue":true}),Some(sid),end,&cancel){
                if v.pointer("/result/value/ready")==Some(&Value::Bool(true)){assert_eq!(v.pointer("/result/value/id").and_then(Value::as_str),Some("ogmckfbpliahokocgfcebhicpjokpdag"));assert!(v.pointer("/result/value/target").and_then(Value::as_str).unwrap().contains("test-vps"));ready=true;break;}
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        stop.store(true, Ordering::Relaxed);
        assert!(worker.join().unwrap().unwrap().is_none());
        drop(browser);
        assert!(ready, "real extension did not pair with Rust loopback");
        println!(
            "{{\"real_extension_loaded\":true,\"loopback_pairing\":true,\"real_account\":false,\"site_permission_granted\":false}}"
        );
    }
}
