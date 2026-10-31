// SPDX-License-Identifier: MIT
//! Optional human UI for the operator client. Only the client binds an ephemeral
//! loopback socket; official JavaScript runs in the human's external browser.
//! This module has no Runtime, QQ protocol, media or model dependencies.
mod cookies;
use crate::{CaptchaProof, Challenge, MAX_QR, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
pub use cookies::{cookie_secret, cookies_with_cancel_check};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const MAX_BODY: usize = 64 * 1024;
fn unix_seconds() -> Result<u32> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "invalid operator clock".to_owned())?
        .as_secs()
        .try_into()
        .map_err(|_| "operator clock exceeds protocol range".into())
}
fn nonce() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| "cannot generate operator page nonce")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
struct ListenerConfig {
    bind: &'static str,
    forwarded: bool,
}
fn listener_config(
    test: Option<&str>,
    bind: Option<&str>,
    origin: Option<&str>,
) -> Result<ListenerConfig> {
    match (test,bind,origin) {
        (None,None,None)=>Ok(ListenerConfig{bind:"127.0.0.1:0",forwarded:false}),
        (Some("1"),Some("0.0.0.0:18799"),Some("http://127.0.0.1:18799"))=>Ok(ListenerConfig{bind:"0.0.0.0:18799",forwarded:true}),
        _=>Err("client UI address override is only available in the fixed isolated operator-ui test mode".into()),
    }
}
fn listener() -> Result<(TcpListener, String, String, bool)> {
    let test = std::env::var("AGENTCTL_TEST_UI").ok();
    let bind = std::env::var("AGENTCTL_UI_LISTEN_ADDR").ok();
    let origin = std::env::var("AGENTCTL_UI_ORIGIN").ok();
    let config = listener_config(test.as_deref(), bind.as_deref(), origin.as_deref())?;
    let socket = TcpListener::bind(config.bind).map_err(|_| "cannot bind client loopback page")?;
    socket
        .set_nonblocking(true)
        .map_err(|_| "cannot configure client loopback page")?;
    let host = if config.forwarded {
        "127.0.0.1:18799".to_string()
    } else {
        socket
            .local_addr()
            .map_err(|_| "cannot inspect client page address")?
            .to_string()
    };
    let origin = format!("http://{host}");
    Ok((socket, host, origin, config.forwarded))
}

fn options(url: &str) -> Result<BTreeMap<String, String>> {
    if url.len() > 16384
        || !url.starts_with("https://ti.qq.com/safe/tools/captcha/sms-verify-login?")
        || url.contains('#')
        || url.chars().any(char::is_control)
    {
        return Err("CAPTCHA URL is not the supported official QQ login template".into());
    }
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for field in url.split_once('?').unwrap().1.split('&') {
        let (key, value) = field.split_once('=').ok_or("invalid CAPTCHA query")?;
        if ["aid", "login_appid", "sid", "uin"].contains(&key) {
            if value.is_empty()
                || value.len() > 8192
                || found.insert(key.into(), value.into()).is_some()
            {
                return Err("missing, duplicate or oversized CAPTCHA option".into());
            }
        }
    }
    for key in ["aid", "login_appid", "sid", "uin"] {
        if !found.contains_key(key) {
            return Err("official CAPTCHA options incomplete".into());
        }
    }
    if ["aid", "login_appid", "uin"]
        .iter()
        .any(|key| !found[*key].bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("invalid QQ CAPTCHA numeric option".into());
    }
    // The official template passes raw query values, including sid, directly
    // to TencentCaptcha; do not decode or invent a new challenge here.
    Ok(found)
}
fn javascript(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)
        .map_err(|_| "cannot encode CAPTCHA page")?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}
fn html(opts: &BTreeMap<String, String>, path: &str, nonce: &str) -> Result<String> {
    let config = javascript(
        &json!({"aid":opts["aid"],"options":{"type":"full","showHeader":false,"login_appid":opts["login_appid"],"uin":opts["uin"],"sid":opts["sid"],"enableAged":true},"result_path":format!("{path}/result")}),
    )?;
    Ok(format!(
        r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>QQ 登录验证</title><style>body{{font:17px system-ui,sans-serif;max-width:40rem;margin:12vh auto;padding:1.5rem;color:#183047;background:#f7f9fc}}button{{padding:.8rem 1.3rem;border:0;border-radius:.7rem;color:white;background:#2175d9;font:inherit}}p{{line-height:1.7}}#status{{white-space:pre-wrap}}</style><h1>QQ 登录验证</h1><p>请手动完成 QQ 提供的验证。验证成功后自动回到登录流程。</p><button id="begin" disabled>正在加载验证组件…</button><button id="cancel">取消登录</button><p id="status" role="status"></p><script nonce="{nonce}">let completed=false;const config={config};const begin=document.getElementById('begin'),status=document.getElementById('status');function cancelLogin(){{if(completed)return;completed=true;navigator.sendBeacon(config.result_path.replace('/result','/cancel'),new Blob(['{{}}'],{{type:'application/json'}}));status.textContent='登录已取消。';begin.disabled=true;}}document.getElementById('cancel').onclick=cancelLogin;window.addEventListener('pagehide',cancelLogin);function sdkReady(){{begin.disabled=false;begin.textContent='开始验证';}}function sdkFailed(){{begin.disabled=true;status.textContent='QQ 验证组件加载失败，请检查本机浏览器网络后刷新。';}}begin.onclick=()=>{{begin.disabled=true;status.textContent='请在 QQ 验证窗口中手动操作。';try{{const widget=new window.TencentCaptcha(config.aid,async result=>{{if(result.ret!==0||result.errorCode||typeof result.ticket!=='string'||result.ticket.startsWith('terror_')||!result.ticket||typeof result.randstr!=='string'||!result.randstr){{status.textContent='验证尚未完成。';begin.disabled=false;return;}}try{{const response=await fetch(config.result_path,{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{ret:result.ret,ticket:result.ticket,randstr:result.randstr,widget_app_id:config.aid}})}});if(!response.ok)throw new Error('callback rejected');completed=true;status.textContent='验证已交回登录程序，可以关闭此页面。';begin.textContent='验证已完成';}}catch(error){{status.textContent='验证交回失败，可能已超时。请返回登录程序。';}}}},config.options);widget.show();}}catch(error){{sdkFailed();}}}};const loader=document.createElement('script');loader.src='https://captcha.gtimg.com/TCaptcha.js';loader.onload=sdkReady;loader.onerror=sdkFailed;document.head.appendChild(loader);</script></html>"#
    ))
}
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
fn bounded_read(stream: &mut TcpStream, buffer: &mut [u8], deadline: Instant) -> Result<usize> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or("CAPTCHA page deadline reached")?;
    stream
        .set_read_timeout(Some(remaining.min(Duration::from_secs(3))))
        .map_err(|_| "cannot set CAPTCHA request timeout")?;
    stream
        .read(buffer)
        .map_err(|_| "CAPTCHA request read failed".to_owned())
}
fn read_request(stream: &mut TcpStream, deadline: Instant, host: &str) -> Result<Request> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|_| "cannot set CAPTCHA request timeout")?;
    let mut bytes = Vec::new();
    let boundary = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() >= 8192 {
            return Err("CAPTCHA request headers too large".into());
        }
        let mut chunk = [0u8; 1024];
        let n = bounded_read(stream, &mut chunk, deadline)?;
        if n == 0 {
            return Err("incomplete CAPTCHA request".into());
        }
        bytes.extend_from_slice(&chunk[..n]);
    };
    if boundary > 8192 {
        return Err("CAPTCHA request headers too large".into());
    }
    let head =
        std::str::from_utf8(&bytes[..boundary]).map_err(|_| "invalid CAPTCHA HTTP header")?;
    let mut lines = head.split("\r\n");
    let mut first = lines
        .next()
        .ok_or("CAPTCHA request line missing")?
        .split_whitespace();
    let method = first.next().ok_or("CAPTCHA method missing")?.to_owned();
    let path = first.next().ok_or("CAPTCHA path missing")?.to_owned();
    if first.next() != Some("HTTP/1.1") || first.next().is_some() {
        return Err("CAPTCHA HTTP version unsupported".into());
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("invalid CAPTCHA header")?;
        if headers
            .insert(key.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err("duplicate CAPTCHA header".into());
        }
    }
    if headers.get("host").map(String::as_str) != Some(host)
        || headers.contains_key("transfer-encoding")
    {
        return Err("CAPTCHA request host or framing rejected".into());
    }
    let length = match headers.get("content-length") {
        None => 0,
        Some(n) => n
            .parse::<usize>()
            .map_err(|_| "invalid CAPTCHA body length")?,
    };
    if length > MAX_BODY {
        return Err("CAPTCHA callback exceeds size limit".into());
    }
    let mut body = bytes[boundary..].to_vec();
    if body.len() > length {
        return Err("extra CAPTCHA HTTP bytes".into());
    }
    while body.len() < length {
        let mut chunk = [0u8; 4096];
        let room = (length - body.len()).min(chunk.len());
        let n = bounded_read(stream, &mut chunk[..room], deadline)?;
        if n == 0 {
            return Err("incomplete CAPTCHA callback".into());
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}
fn respond(
    stream: &mut TcpStream,
    code: u16,
    mime: &str,
    body: &[u8],
    nonce: &str,
    deadline: Instant,
) -> Result<()> {
    respond_with_policy(
        stream,
        code,
        mime,
        body,
        deadline,
        &format!(
            "default-src 'self' https: data: blob:; script-src https: 'nonce-{nonce}'; style-src 'self' https: 'unsafe-inline'; connect-src 'self' https:; frame-ancestors 'none'"
        ),
    )
}
fn respond_with_policy(
    stream: &mut TcpStream,
    code: u16,
    mime: &str,
    body: &[u8],
    deadline: Instant,
    policy: &str,
) -> Result<()> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or("CAPTCHA page deadline reached")?;
    stream
        .set_write_timeout(Some(remaining.min(Duration::from_secs(3))))
        .map_err(|_| "cannot set CAPTCHA response timeout")?;
    let reason = if code == 200 { "OK" } else { "Rejected" };
    let headers = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: {policy}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|_| stream.write_all(body))
        .and_then(|_| stream.flush())
        .map_err(|_| "cannot write CAPTCHA response".into())
}
fn callback(
    request: &Request,
    aid: &str,
    session_id: &str,
    origin: &str,
    expires_at: u32,
) -> Result<CaptchaProof> {
    if request.headers.get("origin").map(String::as_str) != Some(origin)
        || !request
            .headers
            .get("content-type")
            .is_some_and(|s| s.split(';').next() == Some("application/json"))
    {
        return Err("CAPTCHA callback origin or content type rejected".into());
    }
    let value: Value =
        serde_json::from_slice(&request.body).map_err(|_| "invalid CAPTCHA callback JSON")?;
    if value["ret"] != 0
        || value.get("errorCode").is_some_and(|v| v != 0)
        || value["widget_app_id"].as_str() != Some(aid)
    {
        return Err("CAPTCHA callback did not report a real completed challenge".into());
    }
    let ticket = value["ticket"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 16384 && !s.starts_with("terror_"))
        .ok_or("CAPTCHA ticket missing or SDK load-error fallback")?
        .to_owned();
    let randstr = value["randstr"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 16384)
        .ok_or("CAPTCHA randstr missing")?
        .to_owned();
    if unix_seconds()? >= expires_at {
        return Err("CAPTCHA continuation expired".into());
    }
    Ok(CaptchaProof {
        ticket,
        randstr,
        widget_app_id: aid.into(),
        session_id: session_id.into(),
    })
}

/// Runs one human challenge and returns its proof in memory. The server's
/// private continuation never enters this client or the browser page.
pub fn captcha(challenge: &Challenge, on_ready: impl FnOnce(&str)) -> Result<CaptchaProof> {
    let signal = AtomicBool::new(false);
    match captcha_with_cancel(challenge, &signal, on_ready)? {
        UiOutcome::Completed(proof) => Ok(proof),
        UiOutcome::Cancelled => Err("human challenge cancelled".into()),
        UiOutcome::TimedOut => Err("human challenge timed out".into()),
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum UiStatus {
    Active,
    Cancelled,
    TimedOut,
    Failed,
}
pub enum UiOutcome<T> {
    Completed(T),
    Cancelled,
    TimedOut,
}
fn valid_cancel(request: &Request, origin: &str) -> bool {
    request.headers.get("origin").map(String::as_str) == Some(origin)
        && request
            .headers
            .get("content-type")
            .is_some_and(|value| value.split(';').next() == Some("application/json"))
}
pub fn captcha_with_cancel(
    challenge: &Challenge,
    cancelled: &AtomicBool,
    on_ready: impl FnOnce(&str),
) -> Result<UiOutcome<CaptchaProof>> {
    captcha_with_cancel_check(
        challenge,
        cancelled,
        |url| {
            on_ready(url);
            Ok(())
        },
        || UiStatus::Active,
    )
}

/// The client may own an optional window. Its closure/error is checked while
/// waiting for human input, without adding a window or GUI dependency here.
pub fn captcha_with_cancel_check(
    challenge: &Challenge,
    cancelled: &AtomicBool,
    on_ready: impl FnOnce(&str) -> Result<()>,
    mut window_status: impl FnMut() -> UiStatus,
) -> Result<UiOutcome<CaptchaProof>> {
    if challenge.kind != "captcha"
        || challenge.challenge_id.is_empty()
        || challenge.challenge_id.len() > 128
    {
        return Err("not a supported human CAPTCHA challenge".into());
    }
    let opts = options(&challenge.verify_url)?;
    let remaining = challenge.expires_at.saturating_sub(unix_seconds()?);
    if remaining == 0 {
        return Ok(UiOutcome::TimedOut);
    }
    let (listener, host, origin, forwarded) = listener()?;
    let nonce = nonce()?;
    let path = format!("/{nonce}");
    let page = html(&opts, &path, &nonce)?;
    let deadline = Instant::now() + Duration::from_secs(u64::from(remaining.min(240)));
    on_ready(&format!("{origin}{path}"))?;
    let mut requests = 0;
    while Instant::now() < deadline && requests < 128 {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(UiOutcome::Cancelled);
        }
        match window_status() {
            UiStatus::Cancelled => return Ok(UiOutcome::Cancelled),
            UiStatus::TimedOut => return Ok(UiOutcome::TimedOut),
            UiStatus::Failed => return Err("client verification window failed".into()),
            UiStatus::Active => {}
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                requests += 1;
                if !peer.ip().is_loopback() && !forwarded {
                    continue;
                }
                let request = match read_request(&mut stream, deadline, &host) {
                    Ok(request) => request,
                    Err(_) => {
                        let _ = respond(
                            &mut stream,
                            400,
                            "text/plain",
                            b"Invalid request",
                            &nonce,
                            deadline,
                        );
                        continue;
                    }
                };
                if request.method == "GET" && request.path == path {
                    respond(
                        &mut stream,
                        200,
                        "text/html; charset=utf-8",
                        page.as_bytes(),
                        &nonce,
                        deadline,
                    )?;
                } else if request.method == "POST"
                    && request.path == format!("{path}/cancel")
                    && valid_cancel(&request, &origin)
                {
                    respond(
                        &mut stream,
                        200,
                        "application/json",
                        b"{\"cancelled\":true}",
                        &nonce,
                        deadline,
                    )?;
                    return Ok(UiOutcome::Cancelled);
                } else if request.method == "POST" && request.path == format!("{path}/result") {
                    match callback(
                        &request,
                        &opts["aid"],
                        &opts["sid"],
                        &origin,
                        challenge.expires_at,
                    ) {
                        Ok(proof) => {
                            respond(
                                &mut stream,
                                200,
                                "application/json",
                                b"{\"accepted\":true}",
                                &nonce,
                                deadline,
                            )?;
                            return Ok(UiOutcome::Completed(proof));
                        }
                        Err(_) => respond(
                            &mut stream,
                            400,
                            "application/json",
                            b"{\"accepted\":false}",
                            &nonce,
                            deadline,
                        )?,
                    }
                } else {
                    respond(
                        &mut stream,
                        404,
                        "text/plain",
                        b"Not found",
                        &nonce,
                        deadline,
                    )?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(40))
            }
            Err(_) => return Err("client challenge listener failed".into()),
        }
    }
    Ok(UiOutcome::TimedOut)
}

/// A short-lived local page displays a service-provided PNG/JPEG, or renders a
/// QR URL on the client. Dropping the guard immediately closes the listener.
pub struct QrPortal {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
impl QrPortal {
    pub fn status(&self) -> UiStatus {
        if self.cancelled.load(Ordering::Relaxed) {
            UiStatus::Cancelled
        } else if Instant::now() >= self.deadline {
            UiStatus::TimedOut
        } else {
            UiStatus::Active
        }
    }
}
impl Drop for QrPortal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub fn qr(data: &Value, on_ready: impl FnOnce(&str)) -> Result<QrPortal> {
    let image = if let Some(url) = data.get("qr_url").and_then(Value::as_str) {
        if url.len() > 16384 || !url.starts_with("https://") || url.chars().any(char::is_control) {
            return Err("invalid operator QR URL".into());
        }
        let code =
            qrcode::QrCode::new(url.as_bytes()).map_err(|_| "cannot encode operator QR URL")?;
        let svg = code
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(320, 320)
            .build();
        format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg))
    } else {
        let mime = data
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("image/png");
        if !["image/png", "image/jpeg"].contains(&mime) {
            return Err("unsupported operator QR image type".into());
        }
        let encoded = data
            .get("image_base64")
            .and_then(Value::as_str)
            .ok_or("operator QR image missing")?;
        if encoded.len() > MAX_QR.div_ceil(3) * 4 {
            return Err("operator QR image too large".into());
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "invalid operator QR image encoding")?;
        if bytes.is_empty() || bytes.len() > MAX_QR {
            return Err("invalid operator QR image size".into());
        }
        if mime == "image/png" && !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            || mime == "image/jpeg" && !bytes.starts_with(b"\xff\xd8\xff")
        {
            return Err("operator QR image signature mismatch".into());
        }
        format!("data:{mime};base64,{}", STANDARD.encode(bytes))
    };
    let (listener, host, origin, forwarded) = listener()?;
    let nonce = nonce()?;
    let path = format!("/{nonce}");
    let cancel = javascript(&format!("{path}/cancel"))?;
    let page = format!(
        "<!doctype html><html lang=zh-CN><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Agent Runtime 登录</title><style>body{{font:18px system-ui;text-align:center;margin:8vh auto}}img{{max-width:90vw;width:320px;image-rendering:pixelated}}</style><h1>扫码登录</h1><p>请使用相应平台的手机应用扫描，并在手机上确认。</p><img alt='登录二维码' src='{image}'><p>登录结果会返回操控客户端。</p><button id=cancel>取消登录</button><script nonce='{nonce}'>let done=false;function cancel(){{if(done)return;done=true;navigator.sendBeacon({cancel},new Blob(['{{}}'],{{type:'application/json'}}));document.getElementById('cancel').textContent='登录已取消';}}document.getElementById('cancel').onclick=cancel;window.addEventListener('pagehide',cancel);</script></html>"
    );
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    let now = u64::from(unix_seconds()?);
    let remaining = data
        .get("expires_at")
        .and_then(Value::as_u64)
        .map(|expiry| expiry.saturating_sub(now))
        .or_else(|| data.get("expires_in_seconds").and_then(Value::as_u64))
        .unwrap_or(300)
        .min(900);
    if remaining == 0 {
        return Err("QR code expired".into());
    }
    let deadline = Instant::now() + Duration::from_secs(remaining);
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_signal = cancelled.clone();
    let url = format!("{origin}/{nonce}");
    let worker = thread::spawn(move || {
        let mut requests = 0;
        while !signal.load(Ordering::Relaxed) && Instant::now() < deadline && requests < 128 {
            match listener.accept() {
                Ok((mut stream, peer)) => {
                    requests += 1;
                    if !peer.ip().is_loopback() && !forwarded {
                        continue;
                    }
                    match read_request(&mut stream, deadline, &host) {
                        Ok(request) if request.method == "GET" && request.path == path => {
                            let _ = respond(
                                &mut stream,
                                200,
                                "text/html; charset=utf-8",
                                page.as_bytes(),
                                &nonce,
                                deadline,
                            );
                        }
                        Ok(request)
                            if request.method == "POST"
                                && request.path == format!("{path}/cancel")
                                && valid_cancel(&request, &origin) =>
                        {
                            cancelled_signal.store(true, Ordering::Relaxed);
                            let _ = respond(
                                &mut stream,
                                200,
                                "application/json",
                                b"{\"cancelled\":true}",
                                &nonce,
                                deadline,
                            );
                            break;
                        }
                        _ => {
                            let _ = respond(
                                &mut stream,
                                404,
                                "text/plain",
                                b"Not found",
                                &nonce,
                                deadline,
                            );
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(40))
                }
                Err(_) => break,
            }
        }
    });
    on_ready(&url);
    Ok(QrPortal {
        stop,
        worker: Some(worker),
        cancelled,
        deadline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_client_isolation_test_accepts_exact_forwarded_bind_and_origin() {
        let production = listener_config(None, None, None).unwrap();
        assert_eq!(production.bind, "127.0.0.1:0");
        assert!(!production.forwarded);
        let test = listener_config(
            Some("1"),
            Some("0.0.0.0:18799"),
            Some("http://127.0.0.1:18799"),
        )
        .unwrap();
        assert!(test.forwarded);
        for (flag, bind, origin) in [
            (None, Some("0.0.0.0:18799"), Some("http://127.0.0.1:18799")),
            (
                Some("1"),
                Some("0.0.0.0:18800"),
                Some("http://127.0.0.1:18800"),
            ),
            (
                Some("1"),
                Some("0.0.0.0:18799"),
                Some("http://other.invalid:18799"),
            ),
            (Some("1"), None, None),
            (Some("0"), None, None),
        ] {
            assert!(listener_config(flag, bind, origin).is_err());
        }
    }
    #[test]
    fn official_options_and_client_callback_boundaries() {
        let opts = options("https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=123&login_appid=456&sid=synthetic%2Bsid&uin=789").unwrap();
        assert_eq!(opts["sid"], "synthetic%2Bsid");
        assert!(options("https://evil.invalid/?aid=123").is_err());
        let page = html(&opts, "/nonce", "nonce").unwrap();
        assert!(page.contains("https://captcha.gtimg.com/TCaptcha.js"));
        assert!(!page.contains("password"));
        let mut request = Request { method: "POST".into(), path: "/nonce/result".into(), headers: BTreeMap::from([("origin".into(), "http://127.0.0.1:1234".into()), ("content-type".into(), "application/json".into())]), body: serde_json::to_vec(&json!({"ret":0,"widget_app_id":"123","ticket":"synthetic-ticket","randstr":"synthetic-rand"})).unwrap() };
        let expiry = unix_seconds().unwrap() + 120;
        assert!(
            callback(
                &request,
                "123",
                "raw%2Bsid",
                "http://127.0.0.1:1234",
                expiry
            )
            .is_ok()
        );
        assert!(
            callback(
                &request,
                "124",
                "raw%2Bsid",
                "http://127.0.0.1:1234",
                expiry
            )
            .is_err()
        );
        assert!(
            callback(
                &request,
                "123",
                "raw%2Bsid",
                "http://127.0.0.1:5678",
                expiry
            )
            .is_err()
        );
        request.body = serde_json::to_vec(
            &json!({"ret":0,"widget_app_id":"123","ticket":"terror_load","randstr":"synthetic"}),
        )
        .unwrap();
        assert!(
            callback(
                &request,
                "123",
                "raw%2Bsid",
                "http://127.0.0.1:1234",
                expiry
            )
            .is_err()
        );
    }
    #[test]
    fn qr_page_is_ephemeral_loopback_and_displays_client_rendered_code() {
        let mut address = String::new();
        let guard = qr(
            &json!({"qr_url":"https://example.invalid/synthetic-qr"}),
            |url| address = url.into(),
        )
        .unwrap();
        let authority_path = address.strip_prefix("http://").unwrap();
        let (host, path) = authority_path.split_once('/').unwrap();
        assert!(host.starts_with("127.0.0.1:"));
        assert_ne!(host, "127.0.0.1:18799");
        let mut stream = TcpStream::connect(host).unwrap();
        write!(stream, "GET /{path} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("data:image/svg+xml;base64,"));
        assert!(response.contains("pagehide"));
        drop(guard);
        assert!(TcpStream::connect(host).is_err());
    }
    fn cancel_page(url: &str, origin_override: Option<&str>) -> String {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let origin = format!("http://{host}");
        let mut stream = TcpStream::connect(host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stream,"POST /{path}/cancel HTTP/1.1\r\nHost: {host}\r\nOrigin: {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",origin_override.unwrap_or(&origin)).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
    #[test]
    fn local_close_cancel_is_scoped_to_page_origin_and_reaches_driver() {
        let mut url = String::new();
        let portal = qr(
            &json!({"qr_url":"https://example.invalid/synthetic"}),
            |ready| url = ready.into(),
        )
        .unwrap();
        assert!(cancel_page(&url, Some("https://unrelated.invalid")).starts_with("HTTP/1.1 404"));
        assert_eq!(portal.status(), UiStatus::Active);
        assert!(cancel_page(&url, None).starts_with("HTTP/1.1 200"));
        assert_eq!(portal.status(), UiStatus::Cancelled);
        drop(portal);
        let challenge=Challenge{kind:"captcha".into(),challenge_id:"synthetic".into(),expires_at:unix_seconds().unwrap()+120,verify_url:"https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=123&login_appid=456&sid=raw%2Bsid&uin=42".into()};
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            captcha_with_cancel(&challenge, &AtomicBool::new(false), |url| {
                tx.send(url.to_string()).unwrap()
            })
            .unwrap()
        });
        let url = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(cancel_page(&url, None).starts_with("HTTP/1.1 200"));
        assert!(matches!(worker.join().unwrap(), UiOutcome::Cancelled));
        let expired=Challenge{kind:"captcha".into(),challenge_id:"expired".into(),expires_at:unix_seconds().unwrap().saturating_sub(1),verify_url:"https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=123&login_appid=456&sid=raw%2Bsid&uin=42".into()};
        assert!(matches!(
            captcha_with_cancel(&expired, &AtomicBool::new(false), |_| panic!(
                "expired challenge opened a page"
            ))
            .unwrap(),
            UiOutcome::TimedOut
        ));
    }
}
