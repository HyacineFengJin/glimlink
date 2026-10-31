// SPDX-License-Identifier: MIT
//! Client-only private X web-session input. Nothing contacts X from this page.
use super::*;
use crate::Secret;
use serde::Deserialize;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cookies {
    auth_token: String,
    ct0: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    twid: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookiePaste {
    cookie: String,
}

fn cookie_header(text: &str) -> Result<Cookies> {
    if text.len() > 32768 || text.bytes().any(|b| b.is_ascii_control()) {
        return Err("private Cookie header is invalid".into());
    }
    let text = text.trim();
    let text = if text
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("cookie:"))
    {
        &text[7..]
    } else {
        text
    };
    let mut auth_token = None;
    let mut ct0 = None;
    for (index, field) in text.split(';').enumerate() {
        if index >= 256 {
            return Err("private Cookie header has too many fields".into());
        }
        let Some((name, value)) = field.trim().split_once('=') else {
            continue;
        };
        let slot = match name.trim() {
            "auth_token" => &mut auth_token,
            "ct0" => &mut ct0,
            _ => continue,
        };
        if slot.is_some() {
            return Err("duplicate private Cookie field".into());
        }
        *slot = Some(value.trim().to_owned());
    }
    Ok(Cookies {
        auth_token: auth_token.ok_or("private Cookie header needs auth_token and ct0")?,
        ct0: ct0.ok_or("private Cookie header needs auth_token and ct0")?,
        twid: None,
    })
}

/// Validate and canonicalize only the fields needed for X web-session import.
/// Errors deliberately contain neither the supplied JSON nor cookie values.
pub fn cookie_secret(bytes: Vec<u8>) -> Result<Secret> {
    if bytes.is_empty() || bytes.len() > MAX_BODY {
        return Err("private web-session JSON exceeds the input limit".into());
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "private web-session input must be UTF-8")?
        .trim();
    let mut cookies: Cookies = if text.starts_with('{') {
        match serde_json::from_str(text) {
            Ok(cookies) => cookies,
            Err(_) => {
                let paste: CookiePaste = serde_json::from_str(text).map_err(
                    |_| "private web-session input needs Cookie header or auth_token and ct0",
                )?;
                cookie_header(&paste.cookie)?
            }
        }
    } else {
        cookie_header(text)?
    };
    fn valid(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 4096
            && value
                .bytes()
                .all(|byte| matches!(byte, 0x21..=0x7e) && !b"\";,\\".contains(&byte))
    }
    if !valid(&cookies.auth_token) || !valid(&cookies.ct0) || cookies.ct0.len() > 512 {
        return Err("private web-session cookie value is missing or invalid".into());
    }
    if cookies.twid.as_ref().is_some_and(String::is_empty) {
        cookies.twid = None;
    }
    if cookies.twid.as_ref().is_some_and(|value| !valid(value)) {
        return Err("private web-session cookie value is invalid".into());
    }
    // twid is neither needed for authentication nor trusted for identity.
    cookies.twid = None;
    Secret::new(
        serde_json::to_vec(&cookies).map_err(|_| "cannot encode private web-session input")?,
    )
}

fn page(path: &str, nonce: &str) -> Result<String> {
    let result_path = javascript(&format!("{path}/result"))?;
    let cancel_path = javascript(&format!("{path}/cancel"))?;
    Ok(format!(
        r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>X 私密会话导入</title>
<style>body{{font:17px system-ui,sans-serif;max-width:42rem;margin:6vh auto;padding:1.5rem;color:#183047;background:#f7f9fc}}p{{line-height:1.7}}label{{display:block;margin:1rem 0}}input,select{{display:block;box-sizing:border-box;width:100%;padding:.8rem;font:inherit}}button{{padding:.8rem 1.2rem;margin-right:.7rem;font:inherit}}#status{{white-space:pre-wrap}}[hidden]{{display:none!important}}</style>
<h1>X 私密会话导入</h1><p>可以一次粘贴整段 Cookie，无需逐项查找。在已登录的 x.com 开发者工具 Network（网络）里，选该站请求，复制 Request Headers（请求头）的 Cookie 值。也可切换为填写两项。不需要 twid，不要把 Cookie 发到聊天中。</p>
<p>此临时页面只在操控端收集输入，不访问 X，不读取浏览器目录，也不写入浏览器存储。提交后仅保留 auth_token 和 ct0，交给 Runtime 核验身份。</p>
<form id="session" autocomplete="off"><label>导入方式<select id="mode"><option value="paste">一次粘贴 Cookie / 最小 JSON</option><option value="fields">填写 auth_token、ct0</option></select></label>
<label id="paste-field">Cookie 或最小 JSON<input id="cookie" type="password" autocomplete="off" spellcheck="false" maxlength="32768" required></label>
<div id="fields" hidden><label>auth_token<input id="auth_token" type="password" autocomplete="off" spellcheck="false" maxlength="4096" disabled></label><label>ct0<input id="ct0" type="password" autocomplete="off" spellcheck="false" maxlength="512" disabled></label></div>
<button id="submit" type="submit">私密导入</button><button id="cancel" type="button">取消登录</button></form><p id="status" role="status"></p>
<script nonce="{nonce}">let completed=false;const form=document.getElementById('session'),status=document.getElementById('status'),submit=document.getElementById('submit'),mode=document.getElementById('mode');
function setMode(){{const paste=mode.value==='paste';document.getElementById('paste-field').hidden=!paste;document.getElementById('fields').hidden=paste;const cookie=document.getElementById('cookie');cookie.required=paste;cookie.disabled=!paste;for(const id of ['auth_token','ct0']){{const input=document.getElementById(id);input.required=!paste;input.disabled=paste;input.value='';}}cookie.value='';}}
mode.onchange=setMode;function clear(){{form.reset();setMode();}}function cancel(){{if(completed)return;completed=true;clear();navigator.sendBeacon({cancel_path},new Blob(['{{}}'],{{type:'application/json'}}));submit.disabled=true;status.textContent='登录已取消。';}}document.getElementById('cancel').onclick=cancel;window.addEventListener('pagehide',cancel);
form.onsubmit=async event=>{{event.preventDefault();if(completed)return;let body;if(mode.value==='paste'){{const raw=document.getElementById('cookie').value.trim();body=raw.startsWith('{{')?raw:JSON.stringify({{cookie:raw}});}}else{{body=JSON.stringify({{auth_token:document.getElementById('auth_token').value.trim(),ct0:document.getElementById('ct0').value.trim()}});}}clear();submit.disabled=true;try{{const response=await fetch({result_path},{{method:'POST',headers:{{'Content-Type':'application/json'}},body}});body='';if(!response.ok)throw new Error('rejected');completed=true;submit.textContent='已交回登录程序';status.textContent='私密输入已交回，可以关闭此页面。登录结果请查看操控客户端。';}}catch(error){{body='';submit.disabled=false;status.textContent='输入未被接受或页面已过期，请返回操控客户端重试。';}}}};</script></html>"#
    ))
}

/// Wait for private cookie input using the same client-local nonce/Host/Origin
/// boundaries as the QR/CAPTCHA portal. Closing a parent window cancels promptly.
pub fn cookies_with_cancel_check(
    deadline: Instant,
    cancelled: &AtomicBool,
    on_ready: impl FnOnce(&str) -> Result<()>,
    mut window_status: impl FnMut() -> UiStatus,
) -> Result<UiOutcome<Secret>> {
    if Instant::now() >= deadline {
        return Ok(UiOutcome::TimedOut);
    }
    let (listener, host, origin, forwarded) = listener()?;
    let nonce = nonce()?;
    let path = format!("/{nonce}");
    let page = page(&path, &nonce)?;
    let policy = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'"
    );
    on_ready(&format!("{origin}{path}"))?;
    let mut requests = 0;
    while Instant::now() < deadline && requests < 128 {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(UiOutcome::Cancelled);
        }
        match window_status() {
            UiStatus::Cancelled => return Ok(UiOutcome::Cancelled),
            UiStatus::TimedOut => return Ok(UiOutcome::TimedOut),
            UiStatus::Failed => return Err("client private-input window failed".into()),
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
                        let _ = respond_with_policy(
                            &mut stream,
                            400,
                            "text/plain",
                            b"Invalid request",
                            deadline,
                            &policy,
                        );
                        continue;
                    }
                };
                if request.method == "GET" && request.path == path {
                    respond_with_policy(
                        &mut stream,
                        200,
                        "text/html; charset=utf-8",
                        page.as_bytes(),
                        deadline,
                        &policy,
                    )?;
                } else if request.method == "POST" && valid_cancel(&request, &origin) {
                    if request.path == format!("{path}/cancel") {
                        respond_with_policy(
                            &mut stream,
                            200,
                            "application/json",
                            b"{\"cancelled\":true}",
                            deadline,
                            &policy,
                        )?;
                        return Ok(UiOutcome::Cancelled);
                    }
                    if request.path == format!("{path}/result") {
                        match cookie_secret(request.body) {
                            Ok(secret) => {
                                respond_with_policy(
                                    &mut stream,
                                    200,
                                    "application/json",
                                    b"{\"accepted\":true}",
                                    deadline,
                                    &policy,
                                )?;
                                return Ok(UiOutcome::Completed(secret));
                            }
                            Err(_) => {
                                respond_with_policy(
                                    &mut stream,
                                    400,
                                    "application/json",
                                    b"{\"accepted\":false}",
                                    deadline,
                                    &policy,
                                )?;
                                continue;
                            }
                        }
                    }
                    respond_with_policy(
                        &mut stream,
                        404,
                        "text/plain",
                        b"Not found",
                        deadline,
                        &policy,
                    )?;
                } else {
                    respond_with_policy(
                        &mut stream,
                        404,
                        "text/plain",
                        b"Not found",
                        deadline,
                        &policy,
                    )?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(40))
            }
            Err(_) => return Err("client private-input listener failed".into()),
        }
    }
    Ok(UiOutcome::TimedOut)
}

#[cfg(test)]
mod tests {
    use super::*;
    const INPUT: &[u8] =
        br#"{"auth_token":"synthetic-auth-token","ct0":"synthetic-csrf","twid":"u%3D42"}"#;

    fn request(
        url: &str,
        method: &str,
        suffix: &str,
        host_override: Option<&str>,
        origin_override: Option<&str>,
        body: &[u8],
    ) -> String {
        let (host, path) = url
            .strip_prefix("http://")
            .unwrap()
            .split_once('/')
            .unwrap();
        let mut stream = TcpStream::connect(host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stream, "{method} /{path}{suffix} HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", host_override.unwrap_or(host), origin_override.unwrap_or(&format!("http://{host}")), body.len()).unwrap();
        stream.write_all(body).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn private_cookie_json_only_accepts_the_explicit_cookie_fields() {
        let secret = cookie_secret(INPUT.to_vec()).unwrap();
        assert!(!format!("{secret:?}").contains("synthetic-auth-token"));
        let value: Value = serde_json::from_slice(secret.bytes()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 2);
        assert!(value.get("twid").is_none());
        for invalid in [
            br#"{"auth_token":"synthetic-secret","ct0":""}"#.as_slice(),
            br#"{"auth_token":"synthetic-secret","ct0":"csrf;injected=yes"}"#.as_slice(),
            br#"{"auth_token":"synthetic-secret","ct0":"csrf\nvalue"}"#.as_slice(),
            br#"{"auth_token":"synthetic-secret","auth_token":"duplicate","ct0":"csrf"}"#
                .as_slice(),
            br#"{"auth_token":"synthetic-secret","ct0":"csrf","password":"unneeded"}"#.as_slice(),
        ] {
            let error = cookie_secret(invalid.to_vec()).err().unwrap();
            assert!(!error.contains("synthetic-secret"));
        }
        assert!(cookie_secret(vec![b'a'; MAX_BODY + 1]).is_err());
    }

    #[test]
    fn cookie_header_paste_discards_unneeded_fields_and_rejects_ambiguity() {
        for input in [
            "Cookie: unrelated=not-forwarded; auth_token=synthetic-auth; ct0=synthetic-csrf; twid=u%3D42",
            "auth_token=synthetic-auth; ct0=synthetic-csrf",
            r#"{"cookie":"other=unused; auth_token=synthetic-auth; ct0=synthetic-csrf"}"#,
        ] {
            let secret = cookie_secret(input.as_bytes().to_vec()).unwrap();
            let value: Value = serde_json::from_slice(secret.bytes()).unwrap();
            assert_eq!(
                value,
                serde_json::json!({"auth_token":"synthetic-auth","ct0":"synthetic-csrf"})
            );
            assert!(!format!("{secret:?}").contains("synthetic-auth"));
        }
        for input in [
            "auth_token=synthetic-auth; auth_token=duplicate; ct0=synthetic-csrf",
            "auth_token=synthetic-auth; ct0=a; ct0=b",
            "auth_token=synthetic-auth\r\nCookie: ct0=synthetic-csrf",
            "auth_token=synthetic-auth",
            r#"{"cookie":"auth_token=synthetic-auth; ct0=synthetic-csrf","password":"unneeded"}"#,
            r#"{"cookie":"auth_token=synthetic-auth; ct0=synthetic-csrf","cookie":"duplicate"}"#,
        ] {
            let error = cookie_secret(input.as_bytes().to_vec()).err().unwrap();
            assert!(!error.contains("synthetic-auth"));
        }
        let oversized = format!("{}auth_token=a; ct0=b", "other=x;".repeat(256));
        assert!(cookie_secret(oversized.into_bytes()).is_err());
    }

    #[test]
    fn private_cookie_page_enforces_origin_host_csp_and_never_echoes_input() {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            cookies_with_cancel_check(
                Instant::now() + Duration::from_secs(4),
                &AtomicBool::new(false),
                |url| {
                    tx.send(url.to_owned()).unwrap();
                    Ok(())
                },
                || UiStatus::Active,
            )
            .unwrap()
        });
        let url = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            request(&url, "GET", "", Some("wrong.invalid"), None, b"").starts_with("HTTP/1.1 400")
        );
        let response = request(&url, "GET", "", None, None, b"");
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("default-src 'none'"));
        assert!(response.contains("form-action 'none'"));
        assert!(response.contains("Cache-Control: no-store"));
        assert_eq!(response.matches("type=\"password\"").count(), 3);
        assert!(!response.contains("https://"));
        assert!(!response.contains("localStorage"));
        assert!(
            request(
                &url,
                "POST",
                "/result",
                None,
                Some("https://untrusted.invalid"),
                INPUT
            )
            .starts_with("HTTP/1.1 404")
        );
        assert!(request(&url, "POST", "/result", None, None, b"{}").starts_with("HTTP/1.1 400"));
        let accepted = request(&url, "POST", "/result", None, None, INPUT);
        assert!(accepted.starts_with("HTTP/1.1 200"));
        assert!(!accepted.contains("synthetic-auth-token"));
        let secret = match worker.join().unwrap() {
            UiOutcome::Completed(secret) => secret,
            _ => panic!("expected private input"),
        };
        assert_eq!(
            serde_json::from_slice::<Value>(secret.bytes()).unwrap()["auth_token"],
            "synthetic-auth-token"
        );
        let host = url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        assert!(TcpStream::connect(host).is_err());
    }

    #[test]
    fn private_cookie_input_obeys_cancel_close_and_deadline() {
        assert!(matches!(
            cookies_with_cancel_check(
                Instant::now(),
                &AtomicBool::new(false),
                |_| panic!("expired page opened"),
                || UiStatus::Active
            )
            .unwrap(),
            UiOutcome::TimedOut
        ));
        for cancelled in [true, false] {
            let result = cookies_with_cancel_check(
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false),
                |_| Ok(()),
                || {
                    if cancelled {
                        UiStatus::Cancelled
                    } else {
                        UiStatus::TimedOut
                    }
                },
            )
            .unwrap();
            assert!(matches!(
                (cancelled, result),
                (true, UiOutcome::Cancelled) | (false, UiOutcome::TimedOut)
            ));
        }
        let result = cookies_with_cancel_check(
            Instant::now() + Duration::from_secs(2),
            &AtomicBool::new(true),
            |_| Ok(()),
            || UiStatus::Active,
        )
        .unwrap();
        assert!(matches!(result, UiOutcome::Cancelled));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            cookies_with_cancel_check(
                Instant::now() + Duration::from_secs(3),
                &AtomicBool::new(false),
                |url| {
                    tx.send(url.to_owned()).unwrap();
                    Ok(())
                },
                || UiStatus::Active,
            )
            .unwrap()
        });
        let url = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request(&url, "POST", "/cancel", None, None, b"{}").starts_with("HTTP/1.1 200"));
        assert!(matches!(worker.join().unwrap(), UiOutcome::Cancelled));
    }
}
