// SPDX-License-Identifier: MIT
//! GitHub device authorization for Glimlink's registered public application.
//! Private device codes and tokens never implement Debug/Serialize.
use crate::{Error, Result};
use serde_json::Value;
use std::{io::Read, time::Duration};

// Public OAuth client identifier, not a secret. Registered by the project.
pub const DEFAULT_CLIENT_ID: &str = "Ov23lidAYSpAeHJPlD84";
pub const DEFAULT_SCOPES: &str = "read:user public_repo notifications";
pub const VERIFICATION_URL: &str = "https://github.com/login/device";
const MAX_BODY: u64 = 32 * 1024;

pub struct DeviceFlow {
    code: String,
    pub user_code: String,
    pub expires_in: u64,
    pub interval: u64,
}
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub refresh_expires_in: Option<u64>,
    pub scope: String,
}
pub enum Poll {
    Pending,
    Authorized(Tokens),
    Denied,
    Expired,
}
/// Retry only idempotent authorization polling / identity reads while a human
/// login is alive. Ordinary GitHub writes never use this policy.
pub fn retry_after(error: &Error) -> Option<u64> {
    match error {
        Error::Network | Error::HttpStatus(500..=599) => Some(10),
        Error::RateLimited {
            retry_after_seconds,
            ..
        } => Some(retry_after_seconds.unwrap_or(60).clamp(5, 3600)),
        _ => None,
    }
}
pub struct Client {
    client_id: String,
    agent: ureq::Agent,
    base: String,
}
pub fn valid_client_id(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
impl Client {
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    pub fn new(client_id: &str) -> Result<Self> {
        if !valid_client_id(client_id) {
            return Err(Error::InvalidConfig);
        }
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_per_read(Some(Duration::from_secs(10)))
            .max_response_header_size(16 * 1024)
            .max_idle_connections(0)
            .user_agent("glimlink/0.1.0")
            .build()
            .into();
        Ok(Self {
            client_id: client_id.into(),
            agent,
            base: "https://github.com".into(),
        })
    }
    fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<Value> {
        let mut response = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Accept", "application/json")
            .send_form(form.iter().copied())
            .map_err(|_| Error::Network)?;
        let status = response.status().as_u16();
        if status == 429 {
            let number = |key: &str| {
                response
                    .headers()
                    .get(key)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
            };
            return Err(Error::RateLimited {
                retry_after_seconds: number("retry-after"),
                reset_at: number("x-ratelimit-reset"),
            });
        }
        if (300..400).contains(&status) {
            return Err(Error::RedirectDenied);
        }
        if status != 200 && status != 400 {
            return Err(Error::HttpStatus(status));
        }
        if response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|n| n > MAX_BODY)
        {
            return Err(Error::Limit);
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_BODY + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Network)?;
        if bytes.len() as u64 > MAX_BODY {
            return Err(Error::Limit);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| Error::Schema("OAuth response"))?;
        if status == 400 && !value["error"].is_string() {
            return Err(Error::HttpStatus(400));
        }
        Ok(value)
    }
    pub fn start(&self) -> Result<DeviceFlow> {
        let value = self.post(
            "/login/device/code",
            &[("client_id", &self.client_id), ("scope", DEFAULT_SCOPES)],
        )?;
        if value["verification_uri"].as_str() != Some(VERIFICATION_URL) {
            return Err(Error::Schema("OAuth verification origin"));
        }
        let code = secret(&value, "device_code")?;
        let user_code = value["user_code"]
            .as_str()
            .filter(|s| {
                (4..=32).contains(&s.len())
                    && s.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
            })
            .ok_or(Error::Schema("OAuth user code"))?
            .into();
        let expires_in =
            seconds(&value, "expires_in", 900)?.ok_or(Error::Schema("OAuth device expiry"))?;
        let interval = seconds(&value, "interval", 60)?.unwrap_or(5).max(5);
        Ok(DeviceFlow {
            code,
            user_code,
            expires_in,
            interval,
        })
    }
    pub fn poll(&self, flow: &mut DeviceFlow) -> Result<Poll> {
        let value = self.post(
            "/login/oauth/access_token",
            &[
                ("client_id", &self.client_id),
                ("device_code", &flow.code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )?;
        match value["error"].as_str() {
            Some("authorization_pending") => Ok(Poll::Pending),
            Some("slow_down") => {
                flow.interval = seconds(&value, "interval", 60)?
                    .unwrap_or(flow.interval.saturating_add(5))
                    .max(flow.interval.saturating_add(5))
                    .min(60);
                Ok(Poll::Pending)
            }
            Some("access_denied") => Ok(Poll::Denied),
            Some("expired_token" | "token_expired") => Ok(Poll::Expired),
            Some(_) => Err(Error::Schema("OAuth authorization rejected")),
            None => tokens(&value).map(Poll::Authorized),
        }
    }
    pub fn refresh(&self, refresh_token: &str) -> Result<Tokens> {
        if !valid_secret(refresh_token) {
            return Err(Error::InvalidConfig);
        }
        let value = self.post(
            "/login/oauth/access_token",
            &[
                ("client_id", &self.client_id),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ],
        )?;
        if let Some(error) = value["error"].as_str() {
            return Err(match error {
                "bad_refresh_token"
                | "expired_refresh_token"
                | "invalid_grant"
                | "expired_token" => Error::LoginRequired,
                _ => Error::Schema("OAuth refresh rejected"),
            });
        }
        tokens(&value)
    }
}
fn valid_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.bytes().all(|b| b.is_ascii_graphic())
}
fn secret(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .filter(|v| valid_secret(v))
        .map(str::to_owned)
        .ok_or(Error::Schema("OAuth credential"))
}
fn seconds(value: &Value, key: &str, max: u64) -> Result<Option<u64>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0 && *n <= max)
            .map(Some)
            .ok_or(Error::Schema("OAuth expiry or interval")),
    }
}
fn tokens(value: &Value) -> Result<Tokens> {
    if !value["token_type"]
        .as_str()
        .is_some_and(|v| v.eq_ignore_ascii_case("bearer"))
    {
        return Err(Error::Schema("OAuth token type"));
    }
    let refresh_token = if value.get("refresh_token").is_some() {
        Some(secret(value, "refresh_token")?)
    } else {
        None
    };
    let expires_in = seconds(value, "expires_in", 31_536_000)?;
    let refresh_expires_in = seconds(value, "refresh_token_expires_in", 31_536_000)?;
    if expires_in.is_some() && refresh_token.is_none() {
        return Err(Error::Schema("OAuth expiring token without refresh"));
    }
    let scope = value["scope"].as_str().unwrap_or("");
    if scope.len() > 2048 || scope.chars().any(char::is_control) {
        return Err(Error::Schema("OAuth scope"));
    }
    Ok(Tokens {
        access_token: secret(value, "access_token")?,
        refresh_token,
        expires_in,
        refresh_expires_in,
        scope: scope.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    #[test]
    fn device_poll_slowdown_refresh_and_private_forms_use_only_fixed_routes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let responses = [
                json!({"device_code":"synthetic-device","user_code":"ABCD-EFGH","verification_uri":VERIFICATION_URL,"expires_in":900,"interval":5}),
                json!({"error":"authorization_pending"}),
                json!({"error":"slow_down","interval":10}),
                json!({"access_token":"synthetic-access","token_type":"bearer","scope":"read:user","expires_in":28800,"refresh_token":"synthetic-refresh","refresh_token_expires_in":15_552_000}),
                json!({"access_token":"synthetic-rotated","token_type":"bearer","scope":"read:user","expires_in":28800,"refresh_token":"synthetic-next-refresh","refresh_token_expires_in":15_552_000}),
                json!({"error":"bad_refresh_token"}),
            ];
            for (i, value) in responses.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..at]).to_ascii_lowercase();
                        let len = headers
                            .lines()
                            .find_map(|s| {
                                s.strip_prefix("content-length:")
                                    .and_then(|s| s.trim().parse::<usize>().ok())
                            })
                            .unwrap();
                        if bytes.len() >= at + 4 + len {
                            break;
                        }
                    }
                    assert!(bytes.len() < 16384);
                }
                let request = String::from_utf8(bytes).unwrap();
                assert!(request.starts_with(if i == 0 {
                    "POST /login/device/code "
                } else {
                    "POST /login/oauth/access_token "
                }));
                assert!(request.contains(DEFAULT_CLIENT_ID));
                assert!(!request.contains("client_secret"));
                if i > 0 && i < 4 {
                    assert!(request.contains("device_code=synthetic-device"));
                }
                if i == 4 {
                    assert!(request.contains("refresh_token=synthetic-refresh"));
                }
                let body = value.to_string();
                write!(stream,"HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",if i==5{"400 Bad Request"}else{"200 OK"},body.len(),body).unwrap();
            }
        });
        let mut client = Client::new(DEFAULT_CLIENT_ID).unwrap();
        client.base = format!("http://{address}");
        client.agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .max_idle_connections(0)
            .build()
            .into();
        let mut flow = client.start().unwrap();
        assert_eq!(flow.user_code, "ABCD-EFGH");
        assert!(matches!(client.poll(&mut flow).unwrap(), Poll::Pending));
        assert!(matches!(client.poll(&mut flow).unwrap(), Poll::Pending));
        assert_eq!(flow.interval, 10);
        let Poll::Authorized(tokens) = client.poll(&mut flow).unwrap() else {
            panic!("expected authorization")
        };
        assert_eq!(tokens.expires_in, Some(28800));
        let rotated = client
            .refresh(tokens.refresh_token.as_deref().unwrap())
            .unwrap();
        assert_eq!(rotated.access_token, "synthetic-rotated");
        assert_eq!(
            client.refresh("synthetic-refresh").err(),
            Some(Error::LoginRequired)
        );
        worker.join().unwrap();
    }
    #[test]
    fn malformed_tokens_origins_and_expiry_are_rejected() {
        assert_eq!(retry_after(&Error::Network), Some(10));
        assert_eq!(retry_after(&Error::HttpStatus(503)), Some(10));
        assert_eq!(
            retry_after(&Error::RateLimited {
                retry_after_seconds: Some(120),
                reset_at: None
            }),
            Some(120)
        );
        assert_eq!(retry_after(&Error::LoginRequired), None);
        assert_eq!(retry_after(&Error::PermissionDenied), None);
        assert!(!valid_client_id("https://evil.invalid"));
        assert!(!valid_client_id("client\nheader"));
        for v in [
            json!({"access_token":"secret","token_type":"basic"}),
            json!({"access_token":"secret","token_type":"bearer","expires_in":28800}),
            json!({"access_token":"secret\nheader","token_type":"bearer"}),
            json!({"access_token":"secret","token_type":"bearer","scope":"a\nb"}),
            json!({"access_token":"secret","token_type":"bearer","expires_in":u64::MAX,"refresh_token":"secret"}),
        ] {
            assert!(tokens(&v).is_err());
        }
        assert!(tokens(&json!({"access_token":"legacy","token_type":"bearer"})).is_ok());
    }
}
