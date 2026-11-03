// SPDX-License-Identifier: MIT
//! Operator-only session handoff from an installed browser. No browser engine
//! or browser profile reaches the Runtime, and no credential enters UI events.
use runtime_control::{Platform, Result, Secret};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::DirBuilderExt, net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_FRAME: usize = 256 * 1024;
const POLL: Duration = Duration::from_millis(750);

pub fn supported(platform: Platform) -> bool {
    matches!(
        platform,
        Platform::Bilibili | Platform::Tieba | Platform::X | Platform::Pixiv
    )
}
fn root(platform: Platform) -> Result<&'static str> {
    match platform {
        Platform::Bilibili => Ok("https://www.bilibili.com/"),
        Platform::Tieba => Ok("https://tieba.baidu.com/"),
        Platform::X => Ok("https://x.com/"),
        Platform::Pixiv => Ok("https://www.pixiv.net/"),
        _ => Err("browser_platform_unsupported".into()),
    }
}
fn login_url(platform: Platform) -> Result<&'static str> {
    match platform {
        Platform::X => Ok("https://x.com/i/flow/login"),
        _ => root(platform),
    }
}
fn landing(platform: Platform, url: &str) -> bool {
    let Ok(root) = root(platform) else {
        return false;
    };
    let Some(path) = url.strip_prefix(root) else {
        return false;
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    match platform {
        Platform::X => matches!(path, "" | "home" | "home/"),
        Platform::Bilibili => !path.starts_with("account") && !path.starts_with("login"),
        Platform::Tieba => !path.starts_with("login"),
        Platform::Pixiv => !path.starts_with("login"),
        _ => false,
    }
}

/// CDP is asked only for cookies applicable to this platform's HTTPS root.
/// Filter again locally, and retain just the adapter's documented fields.
fn session(platform: Platform, cookies: &Value) -> Result<Option<Secret>> {
    let names: &[&str] = match platform {
        Platform::X => &["auth_token", "ct0"],
        Platform::Bilibili => &[
            "SESSDATA",
            "bili_jct",
            "DedeUserID",
            "DedeUserID__ckMd5",
            "sid",
        ],
        Platform::Tieba => &["BDUSS", "BDUSS_BFESS", "STOKEN"],
        Platform::Pixiv => &["PHPSESSID"],
        _ => return Err("browser_platform_unsupported".into()),
    };
    let mut values = BTreeMap::new();
    let cookies = cookies
        .as_array()
        .filter(|v| v.len() <= 512)
        .ok_or("browser_cookie_protocol")?;
    for c in cookies {
        let (Some(name), Some(value), Some(domain)) = (
            c["name"].as_str(),
            c["value"].as_str(),
            c["domain"].as_str(),
        ) else {
            continue;
        };
        if !names.contains(&name) || c["path"] != "/" {
            continue;
        }
        let domain = domain.strip_prefix('.').unwrap_or(domain);
        let allowed = match platform {
            Platform::X => domain == "x.com" && c["secure"] == true,
            Platform::Bilibili => matches!(domain, "bilibili.com" | "www.bilibili.com"),
            Platform::Tieba => matches!(domain, "baidu.com" | "tieba.baidu.com"),
            Platform::Pixiv => matches!(domain, "pixiv.net" | "www.pixiv.net"),
            _ => false,
        };
        if !allowed {
            continue;
        }
        if value.is_empty()
            || value.len() > 8192
            || !value
                .bytes()
                .all(|b| matches!(b, 0x21..=0x7e) && !b";\r\n\"\\".contains(&b))
        {
            return Err("browser_cookie_invalid".into());
        }
        if values.insert(name, value).is_some() {
            return Err("browser_cookie_ambiguous".into());
        }
    }
    let required: &[&str] = match platform {
        Platform::X => &["auth_token", "ct0"],
        Platform::Bilibili => &["SESSDATA", "bili_jct"],
        Platform::Tieba => &["BDUSS"],
        Platform::Pixiv => &["PHPSESSID"],
        _ => unreachable!(),
    };
    if required.iter().any(|k| !values.contains_key(k)) {
        return Ok(None);
    }
    if platform == Platform::X {
        return runtime_control::human_ui::cookie_secret(
            serde_json::to_vec(&values).map_err(|_| "browser_cookie_protocol")?,
        )
        .map(Some);
    }
    Secret::new(
        values
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
            .into_bytes(),
    )
    .map(Some)
}

struct Profile(PathBuf);
impl Profile {
    fn create() -> Result<Self> {
        let mut random = [0u8; 24];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut random))
            .map_err(|_| "browser_profile_unavailable")?;
        let nonce: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("glimlink-login-{nonce}"));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "browser_profile_unavailable")?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Cdp {
    pipe: UnixStream,
    buffer: Vec<u8>,
    id: u64,
}
impl Cdp {
    fn new(pipe: UnixStream) -> Result<Self> {
        pipe.set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|_| "browser_pipe_unavailable")?;
        pipe.set_write_timeout(Some(Duration::from_millis(300)))
            .map_err(|_| "browser_pipe_unavailable")?;
        Ok(Self {
            pipe,
            buffer: Vec::new(),
            id: 0,
        })
    }
    fn frame(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<Value> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("operator_cancelled".into());
            }
            if Instant::now() >= deadline {
                return Err("operator_timeout".into());
            }
            if let Some(end) = self.buffer.iter().position(|b| *b == 0) {
                let result = serde_json::from_slice(&self.buffer[..end])
                    .map_err(|_| "browser_pipe_protocol".to_owned());
                self.buffer.drain(..=end);
                return result;
            }
            if self.buffer.len() >= MAX_FRAME {
                return Err("browser_pipe_limit".into());
            }
            let mut bytes = [0u8; 4096];
            match self.pipe.read(&mut bytes) {
                Ok(0) => return Err("browser_closed".into()),
                Ok(n) if self.buffer.len() + n <= MAX_FRAME => {
                    self.buffer.extend_from_slice(&bytes[..n])
                }
                Ok(_) => return Err("browser_pipe_limit".into()),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err("browser_pipe_unavailable".into()),
            }
        }
    }
    fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        self.id += 1;
        let mut request = json!({"id":self.id,"method":method,"params":params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&request).map_err(|_| "browser_pipe_protocol")?;
        bytes.push(0);
        self.pipe
            .write_all(&bytes)
            .map_err(|_| "browser_pipe_unavailable")?;
        let deadline = deadline.min(Instant::now() + Duration::from_secs(5));
        for _ in 0..512 {
            let response = self.frame(deadline, cancel)?;
            if response["id"].as_u64() != Some(self.id) {
                continue;
            }
            if response.get("error").is_some() {
                return Err("browser_pipe_protocol".into());
            }
            return response
                .get("result")
                .cloned()
                .ok_or("browser_pipe_protocol".into());
        }
        Err("browser_pipe_limit".into())
    }
}

struct Browser {
    child: Child,
    cdp: Cdp,
    _profile: Profile,
}
impl Browser {
    fn spawn(mut command: Command, profile: Profile) -> Result<Self> {
        let (parent, child) = UnixStream::pair().map_err(|_| "browser_pipe_unavailable")?;
        let fd = child.as_raw_fd();
        let cdp = Cdp::new(parent)?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        // Only async-signal-safe descriptor operations run between fork/exec.
        // Duplicate above the reserved pipe descriptors before replacing 3/4.
        unsafe {
            command.pre_exec(move || {
                let source = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10);
                if source < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let a = libc::dup2(source, 3);
                let b = libc::dup2(source, 4);
                libc::close(source);
                if a < 0 || b < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child_process = command.spawn().map_err(|_| "browser_not_found")?;
        drop(child);
        Ok(Self {
            child: child_process,
            cdp,
            _profile: profile,
        })
    }
    fn installed(platform: Platform) -> Result<Self> {
        let executable = [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "microsoft-edge-stable",
            "brave-browser",
        ]
        .into_iter()
        .find_map(|name| {
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        })
        .ok_or("browser_not_found")?;
        let profile = Profile::create()?;
        let mut command = Command::new(executable);
        command
            .arg(format!("--user-data-dir={}", profile.0.display()))
            .args([
                "--remote-debugging-pipe",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-mode",
                "--new-window",
            ])
            .arg(login_url(platform)?);
        Self::spawn(command, profile)
    }
}
impl Drop for Browser {
    fn drop(&mut self) {
        // Closing only our dedicated browser never touches the user's windows.
        let _ = self
            .cdp
            .pipe
            .write_all(b"{\"id\":999999,\"method\":\"Browser.close\"}\0");
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Launch a real, locally installed browser with a private non-default profile
/// and an inherited pipe, not a TCP debugging port. The closure receives only
/// fixed progress codes; the returned secret belongs to this one human flow.
pub fn capture(
    platform: Platform,
    deadline: Instant,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&'static str),
) -> Result<Option<Secret>> {
    if !supported(platform) {
        return Err("browser_platform_unsupported".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    if Instant::now() >= deadline {
        return Err("operator_timeout".into());
    }
    progress("opening");
    let mut browser = Browser::installed(platform)?;
    browser
        .cdp
        .call("Browser.getVersion", json!({}), None, deadline, cancel)
        .map_err(|error| {
            if error == "operator_cancelled" {
                error
            } else {
                "browser_startup_failed".into()
            }
        })?;
    progress("waiting");
    let result = collect(&mut browser, platform, deadline, cancel);
    match result {
        Ok(secret) => {
            progress("verifying");
            Ok(Some(secret))
        }
        Err(error) if error == "operator_cancelled" || error == "browser_closed" => Ok(None),
        Err(error) => Err(error),
    }
}
fn collect(
    browser: &mut Browser,
    platform: Platform,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Secret> {
    let mut attached: Option<(String, String)> = None;
    let mut candidate: Option<Secret> = None;
    loop {
        let targets = browser
            .cdp
            .call("Target.getTargets", json!({}), None, deadline, cancel)?;
        let target = targets["targetInfos"]
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or("browser_pipe_protocol")?
            .iter()
            .find(|t| {
                t["type"] == "page" && t["url"].as_str().is_some_and(|u| landing(platform, u))
            });
        if let Some(id) = target
            .and_then(|t| t["targetId"].as_str())
            .filter(|s| !s.is_empty() && s.len() <= 128)
        {
            if attached.as_ref().is_none_or(|(target, _)| target != id) {
                if let Some((_, session)) = attached.take() {
                    let _ = browser.cdp.call(
                        "Target.detachFromTarget",
                        json!({"sessionId":session}),
                        None,
                        deadline,
                        cancel,
                    );
                }
                let result = browser.cdp.call(
                    "Target.attachToTarget",
                    json!({"targetId":id,"flatten":true}),
                    None,
                    deadline,
                    cancel,
                )?;
                let session = result["sessionId"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 128)
                    .ok_or("browser_pipe_protocol")?;
                attached = Some((id.into(), session.into()));
                candidate = None;
            }
            let result = browser.cdp.call(
                "Network.getCookies",
                json!({"urls":[root(platform)?]}),
                attached.as_ref().map(|(_, s)| s.as_str()),
                deadline,
                cancel,
            )?;
            if let Some(secret) = session(platform, &result["cookies"])? {
                if candidate
                    .as_ref()
                    .is_some_and(|previous| previous.bytes() == secret.bytes())
                {
                    return Ok(secret);
                }
                candidate = Some(secret);
            } else {
                candidate = None;
            }
        } else {
            candidate = None;
        }
        let next = (Instant::now() + POLL).min(deadline);
        while Instant::now() < next && !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "real browser acceptance; run only in a dedicated network-disabled browser test container"]
    fn real_browser_pipe_fixture() {
        let executable=std::env::var_os("GLIMLINK_FIXTURE_BROWSER").expect("explicit test browser executable");
        let profile=Profile::create().unwrap();let path=profile.0.clone();
        let mut command=Command::new(executable);
        command.arg(format!("--user-data-dir={}",profile.0.display()))
            .args(["--headless","--no-sandbox","--disable-dev-shm-usage","--remote-debugging-pipe","--no-first-run","--no-default-browser-check","about:blank"]);
        let mut browser=Browser::spawn(command,profile).unwrap();
        let end=Instant::now()+Duration::from_secs(15);let cancel=AtomicBool::new(false);
        assert!(browser.cdp.call("Browser.getVersion",json!({}),None,end,&cancel).unwrap()["product"].is_string());
        let target=browser.cdp.call("Target.createTarget",json!({"url":"about:blank"}),None,end,&cancel).unwrap();
        let attached=browser.cdp.call("Target.attachToTarget",json!({"targetId":target["targetId"],"flatten":true}),None,end,&cancel).unwrap();
        let sid=attached["sessionId"].as_str().unwrap();
        browser.cdp.call("Network.setCookies",json!({"cookies":[
            {"name":"auth_token","value":"synthetic-private-x","domain":".x.com","path":"/","secure":true,"httpOnly":true},
            {"name":"ct0","value":"synthetic-csrf","domain":".x.com","path":"/","secure":true},
            {"name":"SID","value":"synthetic-private-google","domain":".google.com","path":"/","secure":true,"httpOnly":true}
        ]}),Some(sid),end,&cancel).unwrap();
        let result=browser.cdp.call("Network.getCookies",json!({"urls":["https://x.com/"]}),Some(sid),end,&cancel).unwrap();
        assert_eq!(result["cookies"].as_array().unwrap().len(),2);
        let secret=session(Platform::X,&result["cookies"]).unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(secret.bytes()).unwrap(),json!({"auth_token":"synthetic-private-x","ct0":"synthetic-csrf"}));
        let pid=browser.child.id();drop(browser);
        assert!(!path.exists());assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        println!("{}",json!({"real_browser_pipe":true,"http_only_cookies":true,"scoped_cookie_query":true,"browser_and_profile_cleanup":true,"network":"none","real_account":false,"memory_peak_bytes":fs::read_to_string("/sys/fs/cgroup/memory.peak").ok().and_then(|s|s.trim().parse::<u64>().ok())}));
    }
    #[test]
    fn browser_handoff_keeps_only_exact_platform_cookies() {
        let cookie = |name: &str, value: &str, domain: &str| json!({"name":name,"value":value,"domain":domain,"path":"/","secure":true,"httpOnly":true});
        let cookies = json!([
            cookie("SESSDATA", "private-bili", ".bilibili.com"),
            cookie("bili_jct", "csrf", ".bilibili.com"),
            cookie("SID", "private-google", ".google.com"),
            cookie("auth_token", "private-x", ".x.com")
        ]);
        let secret = session(Platform::Bilibili, &cookies).unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(secret.bytes()).unwrap(),
            "SESSDATA=private-bili; bili_jct=csrf"
        );
        assert!(session(Platform::X, &cookies).unwrap().is_none());
        assert!(session(Platform::Qq, &cookies).is_err());
        let duplicate = json!([
            cookie("BDUSS", "one", ".baidu.com"),
            cookie("BDUSS", "two", "tieba.baidu.com")
        ]);
        assert_eq!(
            session(Platform::Tieba, &duplicate).err().unwrap(),
            "browser_cookie_ambiguous"
        );
        let foreign = json!([cookie("BDUSS", "one", "baidu.com.evil")]);
        assert!(session(Platform::Tieba, &foreign).unwrap().is_none());
    }
    #[test]
    fn login_and_sso_pages_cannot_finish_handoff() {
        assert!(landing(Platform::X, "https://x.com/home"));
        for url in [
            "https://x.com/i/flow/login",
            "https://x.com/account/access",
            "https://x.com.evil/home",
            "https://accounts.google.com/",
            "http://x.com/home",
            "https://x.com@evil/home",
        ] {
            assert!(!landing(Platform::X, url));
        }
        assert!(!landing(Platform::Tieba, "https://passport.baidu.com/"));
        assert!(landing(Platform::Tieba, "https://tieba.baidu.com/"));
    }
    #[test]
    fn pipe_reassembles_events_and_cancels_without_private_error_output() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut read = [0u8; 4096];
            let _ = b.read(&mut read).unwrap();
            b.write_all(b"{\"method\":\"event\"}\0{\"id\":1,").unwrap();
            b.write_all(b"\"result\":{\"ok\":true}}\0").unwrap();
        });
        let mut cdp = Cdp::new(a).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            cdp.call(
                "Target.getTargets",
                json!({}),
                None,
                Instant::now() + Duration::from_secs(2),
                &cancel
            )
            .unwrap()["ok"],
            true
        );
        worker.join().unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            cdp.frame(Instant::now() + Duration::from_secs(1), &cancel)
                .err()
                .unwrap(),
            "operator_cancelled"
        );
    }
    #[test]
    fn inherited_pipe_process_completes_without_an_open_port_or_ui_credentials() {
        let profile = Profile::create().unwrap();
        let path = profile.0.clone();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "browser_login::tests::pipe_child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("GLIMLINK_PIPE_FIXTURE", "complete");
        let mut browser = Browser::spawn(command, profile).unwrap();
        let secret = collect(
            &mut browser,
            Platform::X,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(secret.bytes()).unwrap(),
            json!({"auth_token":"synthetic-private-x","ct0":"synthetic-csrf"})
        );
        let pid = browser.child.id();
        drop(browser);
        assert!(!path.exists());
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
    #[test]
    fn cancelling_waiting_browser_reaps_process_and_removes_private_profile() {
        let profile = Profile::create().unwrap();
        let path = profile.0.clone();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "browser_login::tests::pipe_child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("GLIMLINK_PIPE_FIXTURE", "waiting");
        let mut browser = Browser::spawn(command, profile).unwrap();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let other = cancel.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            other.store(true, Ordering::Relaxed);
        });
        assert_eq!(
            collect(
                &mut browser,
                Platform::X,
                Instant::now() + Duration::from_secs(5),
                &cancel
            )
            .err()
            .unwrap(),
            "operator_cancelled"
        );
        let pid = browser.child.id();
        drop(browser);
        worker.join().unwrap();
        assert!(!path.exists());
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
    #[test]
    #[ignore = "child-process protocol fixture; invoked only by browser lifecycle tests"]
    fn pipe_child_fixture() {
        use std::os::fd::FromRawFd;
        let mode = std::env::var("GLIMLINK_PIPE_FIXTURE").unwrap();
        let mut input = unsafe { File::from_raw_fd(3) };
        let mut output = unsafe { File::from_raw_fd(4) };
        loop {
            let mut bytes = Vec::new();
            loop {
                let mut b = [0];
                if input.read(&mut b).unwrap() == 0 {
                    return;
                }
                if b[0] == 0 {
                    break;
                }
                bytes.push(b[0]);
            }
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "Target.getTargets" => {
                    json!({"targetInfos":[{"type":"page","targetId":"synthetic","url":"https://x.com/home"}]})
                }
                "Target.attachToTarget" => json!({"sessionId":"synthetic-session"}),
                "Network.getCookies" => {
                    assert_eq!(request["params"]["urls"], json!(["https://x.com/"]));
                    assert_eq!(request["sessionId"], "synthetic-session");
                    if mode == "waiting" {
                        json!({"cookies":[]})
                    } else {
                        json!({"cookies":[{"name":"auth_token","value":"synthetic-private-x","domain":".x.com","path":"/","secure":true,"httpOnly":true},{"name":"ct0","value":"synthetic-csrf","domain":".x.com","path":"/","secure":true}]})
                    }
                }
                "Browser.close" => return,
                _ => panic!("unexpected fixture command"),
            };
            let mut response =
                serde_json::to_vec(&json!({"id":request["id"],"result":result})).unwrap();
            response.push(0);
            output.write_all(&response).unwrap();
        }
    }
}
