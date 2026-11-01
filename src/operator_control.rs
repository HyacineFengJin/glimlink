// SPDX-License-Identifier: MIT
//! Private JSONL operator endpoint. Spawned locally or through SSH stdio;
//! intentionally absent from the model-facing MCP tool catalog.
use crate::app::Runtime;
use runtime_control::{Action, Frame, MAX_SECRET, Platform, Request, read_json, write_json};
use runtime_core::{Result, resources::private_dir};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// A short-lived private file adapts stdin-only credentials to the existing
/// native management APIs. It is never named or supplied by the remote client.
pub(crate) struct PrivateInput {
    pub(crate) path: PathBuf,
}
impl PrivateInput {
    pub(crate) fn create(state: &Path, bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET {
            return Err("private input size rejected".into());
        }
        // A subsequent Runtime open clears work after an interrupted process.
        let dir = state.join("work/operator-inputs");
        private_dir(&dir)?;
        let mut random = [0u8; 24];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|_| "cannot generate private input name")?;
        let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| "cannot create private operator input")?;
        let input = Self { path };
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "cannot write private operator input")?;
        Ok(input)
    }
}
impl Drop for PrivateInput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn execute_legacy(
    runtime: &mut Runtime,
    request: &Request,
    emit: &mut impl FnMut(&str, Value) -> Result<()>,
) -> Result<Value> {
    let input = request
        .secret
        .as_ref()
        .map(|secret| PrivateInput::create(runtime.state_dir(), secret.bytes()))
        .transpose()?;
    let path = || {
        input
            .as_ref()
            .map(|i| i.path.as_path())
            .ok_or_else(|| "private input missing".to_owned())
    };
    let mut result = match request.action {
        Action::Status => match request.platform {
            Platform::Runtime => {
                json!({"accounts":runtime.call("runtime.accounts",&json!({}))?,"modules":runtime.call("runtime.modules",&json!({}))?})
            }
            platform => {
                let mut value = runtime.call(&format!("{}.status", platform.name()), &json!({}))?;
                if value["status"] == "saved_session"
                    && matches!(platform, Platform::Bilibili | Platform::Tieba | Platform::X)
                {
                    if let Ok(details) =
                        runtime.call(&format!("{}.me", platform.name()), &json!({}))
                    {
                        if details["user"].is_object() {
                            value["user"] = details["user"].clone();
                            value["status"] = json!("online");
                            value["online_checked"] = json!(true);
                        }
                    }
                }
                value
            }
        },
        Action::ImportLogin => match request.platform {
            Platform::Bilibili => runtime.bilibili_import_login(path()?)?,
            Platform::Tieba => runtime.tieba_import_login(path()?)?,
            Platform::Qq => runtime.qq_import_login(path()?)?,
            Platform::Github => runtime.github_login(path()?)?,
            Platform::X => runtime.x_import_login(path()?)?,
            _ => return Err("unsupported login import".into()),
        },
        Action::PasswordLogin | Action::PasswordResume => {
            let resume = request.action == Action::PasswordResume;
            runtime.operator_check_password_input(
                resume,
                &request.params,
                request
                    .secret
                    .as_ref()
                    .ok_or("private input missing")?
                    .bytes(),
            )?;
            let mut result = if resume {
                runtime.qq_password_resume(path()?)?
            } else {
                runtime.qq_password_login(path()?)?
            };
            if result["status"] == "verification_required" {
                result["challenge"] = serde_json::to_value(runtime.operator_challenge()?)
                    .map_err(|_| "cannot encode operator challenge")?;
            }
            result
        }
        Action::Challenge => {
            json!({"status":"verification_required","challenge":runtime.operator_challenge()?})
        }
        Action::Qr => match request.platform {
            Platform::Bilibili => runtime.operator_bilibili_qr()?,
            Platform::Tieba => runtime.operator_tieba_qr()?,
            Platform::Qq => runtime.operator_qq_qr(
                request.params.get("app_profile").and_then(Value::as_str),
                emit,
            )?,
            _ => return Err("unsupported QR platform".into()),
        },
        Action::QrPoll => match request.platform {
            Platform::Bilibili => runtime.bilibili_poll_login()?,
            Platform::Tieba => runtime.tieba_poll_login()?,
            _ => return Err("unsupported QR polling platform".into()),
        },
        Action::ReuseLogin => match request.platform {
            Platform::Bilibili => runtime.call("bilibili.me", &json!({}))?,
            Platform::Tieba => runtime.call("tieba.me", &json!({}))?,
            Platform::Qq => runtime.qq_login()?,
            Platform::X => runtime.x_operator_reuse()?,
            _ => return Err("unsupported saved login platform".into()),
        },
        _ => return Err("unsupported compatibility control action".into()),
    };
    if let Some(object) = result.as_object_mut() {
        // Legacy CLI instructions refer to files on the core's machine. The
        // client uses the typed challenge and transports its proof instead.
        for field in ["management_file", "operator_instruction", "qr_path"] {
            object.remove(field);
        }
    }
    Ok(result)
}

fn public_failure(id: u64, error: &str) -> Frame {
    if let Some(code) = error.strip_prefix("x-request:") {
        let (code, message) = match code {
            "needs_auth" => (
                "needs_auth",
                "X did not accept this web session. Keep the existing saved state and import a valid session through the private operator input.",
            ),
            "verification_required" => (
                "verification_required",
                "X requires browser verification. Complete it in your existing browser; no automatic alternate login was started.",
            ),
            "rate_limited" => (
                "rate_limited",
                "X temporarily limited this account. No retry, identity change or paid API fallback was started.",
            ),
            "permission_denied" => (
                "permission_denied",
                "This X session cannot access the selected content.",
            ),
            "request_denied" => (
                "request_denied",
                "X denied an HTTP request without a confirmed verification challenge. Existing account state was retained; no automatic login or retry was started.",
            ),
            "protocol_changed" => (
                "protocol_changed",
                "The Runtime could not interpret X web request metadata or response data. Existing account state was retained.",
            ),
            "network_error" => (
                "network_error",
                "The X web request could not be completed. Existing account state was retained.",
            ),
            _ => (
                "operation_failed",
                "The X web request failed. Existing account state was retained.",
            ),
        };
        return Frame::failure(id, code, message);
    }
    let (code, message) = if error.contains("same_connection_unavailable") {
        (
            "same_connection_unavailable",
            "The original login connection is no longer available. Start one agentctl login flow and complete verification in that same session.",
        )
    } else if error.contains("login_id") {
        (
            "login_session_mismatch",
            "This login ID does not belong to the selected platform, account and control connection.",
        )
    } else if error.contains("another login") || error.contains("another subscription login") {
        (
            "login_busy",
            "A login is already active. Complete or cancel that exact login before starting another.",
        )
    } else if error == "state is already in use" {
        (
            "state_busy",
            "Another Runtime owns this state. Close that process before using spawn-based operator management.",
        )
    } else if error == "engine credential home is busy" {
        (
            "engine_busy",
            "The selected subscription home is in use by a model or login process. Complete that process before starting another login.",
        )
    } else if error.contains("another account")
        || error.contains("identity mismatch")
        || error.contains("account binding differs")
    {
        (
            "account_mismatch",
            "The login belongs to a different account identity. Use its existing account alias or create a new explicit alias.",
        )
    } else if error.contains("expired")
        || error.contains("different challenge")
        || error.contains("different challenge session ID")
    {
        (
            "challenge_changed",
            "The pending verification expired or changed. Fetch the current challenge or explicitly start login again.",
        )
    } else if error.contains("signer") {
        (
            "signer_required",
            "QQ's configured native signer is unavailable. Configure it on the Runtime core before login.",
        )
    } else if error.contains("private password input")
        || error.contains("exactly one nonempty password")
        || error.contains("password continuation must")
        || error.contains("imported session")
    {
        (
            "invalid_private_input",
            "The private login input is invalid or incomplete for this action. Check the documented platform input format.",
        )
    } else {
        (
            "operation_failed",
            "Operator action failed; private authentication state was retained. Check account status or restart the explicit login flow.",
        )
    };
    Frame::failure(id, code, message)
}

/// Serve until EOF. Neither raw request JSON nor authentication failures are
/// logged. Underlying server errors can contain private paths or capabilities;
/// return a stable public failure rather than serializing those strings.
pub fn serve(state: &Path, mut reader: impl BufRead, mut writer: impl Write) -> Result<()> {
    let mut controller = crate::operator_login::LoginController::new(state)?;
    loop {
        let request = match read_json::<Request>(&mut reader) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(_) => {
                write_json(
                    &mut writer,
                    &Frame::failure(
                        0,
                        "invalid_frame",
                        "Invalid or oversized operator JSONL frame",
                    ),
                )?;
                return Err("operator input framing rejected".into());
            }
        };
        if let Err(error) = request.validate() {
            write_json(
                &mut writer,
                &Frame::failure(request.id, "invalid_request", &error),
            )?;
            continue;
        }
        let result = controller.execute(&request, &mut |event, data| {
            write_json(&mut writer, &Frame::event(request.id, event, data))
        });
        let response = match result {
            Ok(value) => {
                let _ = crate::operator_identities::remember(
                    state,
                    request.platform.name(),
                    &request.account,
                    &value,
                );
                Frame::success(request.id, value)
            }
            Err(error) => public_failure(request.id, &error),
        };
        write_json(&mut writer, &response)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_control::{Secret, VERSION};
    use std::{io::Cursor, os::unix::fs::PermissionsExt};
    #[test]
    fn private_input_is_mode600_and_removed_on_all_returns() {
        let state = std::env::temp_dir().join(format!("operator-private-{}", std::process::id()));
        private_dir(&state).unwrap();
        let path;
        {
            let input = PrivateInput::create(&state, b"synthetic-secret").unwrap();
            path = input.path.clone();
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
            assert_eq!(fs::read(&path).unwrap(), b"synthetic-secret");
        }
        assert!(!path.exists());
        fs::remove_dir_all(state).unwrap();
    }
    #[test]
    fn jsonl_status_and_invalid_import_have_no_paths_or_credentials() {
        let state = std::env::temp_dir().join(format!("operator-server-{}", std::process::id()));
        let mut status = Request::new(1, Platform::Bilibili, "synthetic".into(), Action::Status);
        let mut bytes = Vec::new();
        write_json(&mut bytes, &status).unwrap();
        status.id = 2;
        status.action = Action::ImportLogin;
        status.secret =
            Some(Secret::new(b"synthetic-credential-must-not-appear".to_vec()).unwrap());
        write_json(&mut bytes, &status).unwrap();
        status.id = 3;
        status.params = json!({"path":"/remote/forbidden"});
        write_json(&mut bytes, &status).unwrap();
        let mut output = Vec::new();
        serve(&state, Cursor::new(bytes), &mut output).unwrap();
        let text = String::from_utf8(output.clone()).unwrap();
        assert!(!text.contains("synthetic-credential"));
        assert!(!text.contains("/remote/forbidden"));
        assert!(!text.contains(state.to_str().unwrap()));
        let mut reader = Cursor::new(output);
        assert!(matches!(
            read_json::<Frame>(&mut reader).unwrap(),
            Some(Frame::Response {
                version: VERSION,
                id: 1,
                ok: true,
                ..
            })
        ));
        assert!(matches!(
            read_json::<Frame>(&mut reader).unwrap(),
            Some(Frame::Response {
                id: 2,
                ok: false,
                ..
            })
        ));
        assert!(matches!(
            read_json::<Frame>(&mut reader).unwrap(),
            Some(Frame::Response {
                id: 3,
                ok: false,
                ..
            })
        ));
        assert_eq!(
            fs::read_dir(state.join("work/operator-inputs"))
                .map(|entries| entries.count())
                .unwrap_or(0),
            0
        );
        fs::remove_dir_all(state).unwrap();
    }
}
