// SPDX-License-Identifier: MIT
//! Human-control adapters. Only authentication artifacts, never model tools.
//! Original orchestration; QQ wire protocol remains behind platform-qq's API.
use super::*;
use crate::private_state::account_directory;
use base64::{Engine, engine::general_purpose::STANDARD};
use runtime_control::{Challenge, MAX_QR};
use std::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

fn private_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = open_regular(path, limit)?;
    if file
        .metadata()
        .map_err(|_| "cannot inspect private operator state")?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("operator state must be private0600".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read private operator state")?;
    if bytes.len() as u64 > limit {
        return Err("operator state exceeds limit".into());
    }
    Ok(bytes)
}
fn challenge(continuation: &platform_qq::password::PasswordContinuation) -> Result<Challenge> {
    use platform_qq::password::PasswordChallengeKind;
    let expires_at = continuation.expires_at();
    let now = platform_qq::unix_seconds()?;
    if expires_at <= now || expires_at > now.saturating_add(600) {
        return Err("password challenge expired or invalid".into());
    }
    let (kind, challenge_id) = match continuation.kind {
        PasswordChallengeKind::Captcha => ("captcha", continuation.captcha_fingerprint()?),
        PasswordChallengeKind::NewDevice => ("new_device", format!("new-device-{expires_at}")),
        PasswordChallengeKind::Unusual => {
            ("unusual_device", format!("unusual-device-{expires_at}"))
        }
    };
    Ok(Challenge {
        kind: kind.into(),
        challenge_id,
        expires_at,
        verify_url: continuation.verify_url.clone().unwrap_or_default(),
    })
}

impl Runtime {
    /// Probe only an explicitly selected saved account. Only a typed provider
    /// authentication rejection permits the control plane to start a new flow.
    pub(crate) fn operator_reuse_login(&mut self, platform: &str) -> Result<Value> {
        let Some(path) = super::auth::saved_session(&self.state, platform, &self.account)? else {
            return Ok(json!({"status":"needs_auth"}));
        };
        let dir = path.parent().ok_or("account directory missing")?;
        match platform {
            "bilibili" => match self.bili.current_user() {
                Ok(user) => {
                    super::auth::bind_identity(dir, &user.mid.to_string())?;
                    Ok(json!({"status":"online","reused_session":true,"user":user}))
                }
                Err(platform_bilibili::Error::LoginRequired) => Ok(json!({"status":"needs_auth"})),
                Err(error) => Err(error.to_string()),
            },
            "tieba" => {
                let mut session =
                    platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
                let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
                match api.me(&mut session) {
                    Ok(user) => {
                        if user.stable_identity() != session.stable_identity() {
                            return Err("Tieba session identity mismatch".into());
                        }
                        super::auth::bind_identity(dir, &session.stable_identity())?;
                        session.save(&path).map_err(|e| e.to_string())?;
                        Ok(json!({"status":"online","reused_session":true,"user":user}))
                    }
                    Err(platform_tieba::AuthError::LoginRequired) => {
                        Ok(json!({"status":"needs_auth"}))
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            "qq" => {
                // A previous process cannot resume a live verification socket.
                if dir.join("login-required.json").exists()
                    || dir.join("login-challenge.json").exists()
                {
                    return Ok(json!({"status":"needs_auth"}));
                }
                match self.qq_login() {
                    Ok(mut value) => {
                        value["reused_session"] = json!(true);
                        Ok(value)
                    }
                    Err(error) if dir.join("login-required.json").exists() => {
                        let _ = error;
                        Ok(json!({"status":"needs_auth"}))
                    }
                    Err(error) => Err(error),
                }
            }
            "github" => self.github_operator_reuse(),
            "x" => self.x_operator_reuse(),
            "pixiv" => self.pixiv_operator_reuse(),
            _ => Err("unsupported saved login platform".into()),
        }
    }

    pub(crate) fn operator_cancel_qr(&self, platform: &str) -> Result<()> {
        if !matches!(platform, "bilibili" | "tieba") {
            return Err("unsupported QR cancellation platform".into());
        }
        let dir = account_directory(&self.state, platform, &self.account)?;
        for entry in fs::read_dir(&dir)
            .map_err(|_| "cannot inspect login artifacts")?
            .take(128)
        {
            let entry = entry.map_err(|_| "cannot inspect login artifact")?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let ephemeral = name == "login-attempt.json"
                || name == "login-qr.png"
                || name == "qrcode.png"
                || name.starts_with("login-qr-")
                    && [".png", ".jpg", ".image"]
                        .iter()
                        .any(|suffix| name.ends_with(suffix));
            if ephemeral {
                let kind = entry
                    .file_type()
                    .map_err(|_| "cannot inspect login artifact")?;
                if !kind.is_file() {
                    return Err("login artifact is not a regular file".into());
                }
                fs::remove_file(entry.path()).map_err(|_| "cannot clear login artifact")?;
            }
        }
        Ok(())
    }

    pub(crate) fn operator_challenge(&self) -> Result<Challenge> {
        if self.account == "public" {
            return Err("challenge requires an explicit account alias".into());
        }
        let dir = account_directory(&self.state, "qq", &self.account)?;
        let pending: platform_qq::password::PasswordContinuation =
            serde_json::from_slice(&private_bytes(&dir.join("password-login.json"), 65536)?)
                .map_err(|_| "invalid private password continuation")?;
        challenge(&pending)
    }
    pub(crate) fn operator_check_password_input(
        &self,
        resume: bool,
        params: &Value,
        bytes: &[u8],
    ) -> Result<()> {
        let input: Value =
            serde_json::from_slice(bytes).map_err(|_| "invalid private password input")?;
        if !input.is_object() {
            return Err("private password input must be an object".into());
        }
        if !resume {
            if input.get("resume").is_some_and(|v| v != false)
                || input.get("captcha").is_some_and(|v| !v.is_null())
            {
                return Err("password continuation must use password_resume".into());
            }
            return Ok(());
        }
        let pending = self.operator_challenge()?;
        if params.get("challenge_id").and_then(Value::as_str) != Some(&pending.challenge_id) {
            return Err("password continuation belongs to a different challenge".into());
        }
        if pending.kind == "captcha" {
            let proof = input
                .get("captcha")
                .and_then(Value::as_object)
                .ok_or("captcha proof must be supplied by the operator client")?;
            let expected_sid = pending
                .verify_url
                .split_once('?')
                .and_then(|(_, q)| q.split('&').find_map(|field| field.strip_prefix("sid=")))
                .ok_or("captcha session ID missing in pending challenge")?;
            if proof
                .get("session_id")
                .or_else(|| proof.get("aid"))
                .and_then(Value::as_str)
                != Some(expected_sid)
            {
                return Err("captcha proof has a different challenge session ID".into());
            }
        }
        Ok(())
    }

    /// The Bilibili service returns a URL; QR encoding is entirely client-side.
    pub(crate) fn operator_bilibili_qr(&mut self) -> Result<Value> {
        self.operator_bilibili_qr_mode(true)
    }
    pub(crate) fn operator_bilibili_qr_mode(&mut self, reuse_saved: bool) -> Result<Value> {
        if self.account == "public" {
            return Err("QR login requires an explicit account alias".into());
        }
        let dir = account_directory(&self.state, "bilibili", &self.account)?;
        if reuse_saved && dir.join("session.json").exists() {
            match self.bili.current_user() {
                Ok(user) => {
                    super::auth::bind_identity(&dir, &user.mid.to_string())?;
                    return Ok(json!({"status":"online","reused_session":true,"user":user}));
                }
                Err(platform_bilibili::Error::LoginRequired) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let file = dir.join("login-attempt.json");
        let saved = if file.exists() {
            Some(platform_bilibili::LoginAttempt::load(&file).map_err(|e| e.to_string())?)
        } else {
            None
        };
        if saved
            .as_ref()
            .is_some_and(|a| a.status() == platform_bilibili::LoginStatus::Confirmed)
        {
            return self.bilibili_poll_login();
        }
        let reusable = saved.as_ref().is_some_and(|a| {
            a.expires_in_seconds() > 0
                && matches!(
                    a.status(),
                    platform_bilibili::LoginStatus::Waiting
                        | platform_bilibili::LoginStatus::Scanned
                )
        });
        let attempt = if reusable {
            saved.unwrap()
        } else {
            platform_bilibili::AuthClient::new(Config::default())
                .and_then(|api| api.begin_login())
                .map_err(|e| e.to_string())?
        };
        attempt.save(&file).map_err(|e| e.to_string())?;
        Ok(
            json!({"status":"waiting","qr_url":attempt.qr_url(),"expires_in_seconds":attempt.expires_in_seconds(),"reused_attempt":reusable}),
        )
    }
    pub(crate) fn operator_tieba_qr(&mut self) -> Result<Value> {
        let mut result = self.tieba_login_mode(false)?;
        if let Some(path) = result
            .get("qr_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
        {
            let bytes = private_bytes(&path, MAX_QR as u64)?;
            result
                .as_object_mut()
                .ok_or("invalid QR result")?
                .remove("qr_path");
            result["image_base64"] = json!(STANDARD.encode(bytes));
        }
        Ok(result)
    }

    /// QQ's SDK delivers a service-provided PNG. The host only forwards the
    /// bounded artifact through notifications, with no stdout logging or HTTP.
    pub(crate) fn operator_qq_qr(
        &mut self,
        profile: Option<&str>,
        emit: &mut impl FnMut(&str, Value) -> Result<()>,
    ) -> Result<Value> {
        self.operator_qq_qr_with_stop(profile, &AtomicBool::new(false), emit)
    }
    pub(crate) fn operator_qq_qr_with_stop(
        &mut self,
        profile: Option<&str>,
        stop: &AtomicBool,
        emit: &mut impl FnMut(&str, Value) -> Result<()>,
    ) -> Result<Value> {
        if self.account == "public" {
            return Err("QR login requires an explicit account alias".into());
        }
        self.qq_password_cancel()?;
        let dir = account_directory(&self.state, "qq", &self.account)?;
        let session_path = dir.join("session.json");
        let mut session = if session_path.exists() {
            platform_qq::Session::load(&session_path)?
        } else {
            platform_qq::Session::empty()
        };
        let binding_path = dir.join("identity.json");
        if binding_path.exists() {
            let binding: Value = serde_json::from_slice(&private_bytes(&binding_path, 1024)?)
                .map_err(|_| "invalid account binding")?;
            let bound: u32 = binding["identity"]
                .as_str()
                .ok_or("account binding missing")?
                .parse()
                .map_err(|_| "invalid account binding")?;
            if bound == 0 || session.uin != 0 && session.uin != bound {
                return Err("saved account binding differs".into());
            }
            session.uin = bound;
        }
        if let Some(profile) = profile {
            session.app_profile = profile.parse()?;
        }
        let device_path = dir.join("device.json");
        let device = if session_path.exists() || binding_path.exists() {
            platform_qq::Device::load(&device_path)?
        } else {
            platform_qq::Device::load_or_create(&device_path)?
        };
        let config_path = self.state.join("qq-config.json");
        let signer_module = if config_path.exists() {
            let value: Value = serde_json::from_slice(&private_bytes(&config_path, 4096)?)
                .map_err(|_| "invalid QQ signer configuration")?;
            value["signer_module"].as_str().map(PathBuf::from)
        } else {
            None
        };
        let mut client = platform_qq::Client::connect(
            session,
            device,
            platform_qq::ClientConfig {
                session_file: session_path,
                device_file: device_path,
                signer_module,
            },
        )?;
        let output = WorkDir::new(&self.state)?;
        let sink = RefCell::new(emit);
        let failure = RefCell::new(None);
        let notify = |kind: &str, data: Result<Value>| {
            let delivered = data.and_then(|data| (sink.borrow_mut())(kind, data));
            if let Err(error) = delivered {
                *failure.borrow_mut() = Some(error);
                stop.store(true, Ordering::Relaxed);
            }
        };
        let outcome = platform_qq::login_qr(
            &mut client,
            &output.path,
            stop,
            |path| {
                notify(
                    "qr_ready",
                    private_bytes(path, MAX_QR as u64).map(
                        |bytes| json!({"image_base64":STANDARD.encode(bytes),"mime":"image/png"}),
                    ),
                )
            },
            |state| {
                let status = match state {
                    platform_qq::QrState::Confirmed { .. } => "confirmed",
                    platform_qq::QrState::WaitingForScan => "waiting_for_scan",
                    platform_qq::QrState::WaitingForConfirm => "waiting_for_confirm",
                    platform_qq::QrState::Expired => "expired",
                    platform_qq::QrState::Canceled => "canceled",
                    platform_qq::QrState::Unknown(_) => "unknown",
                };
                notify("qr_status", Ok(json!({"status":status})));
            },
        );
        if let Some(error) = failure.into_inner() {
            return Err(error);
        }
        match outcome? {
            platform_qq::LoginOutcome::Online { source } => {
                super::auth::bind_identity(&dir, &client.session().uin.to_string())?;
                for name in [
                    "login-challenge.json",
                    "login-required.json",
                    "password-login.json",
                ] {
                    let path = dir.join(name);
                    if path.exists() {
                        fs::remove_file(path).map_err(|_| "cannot clear completed login state")?;
                    }
                }
                Ok(
                    json!({"status":"online","scope":self.scope(Platform::Qq),"identity":client.session().uin.to_string(),"auth_source":format!("{source:?}"),"session_saved":true,"messages_sent":0}),
                )
            }
            _ => Err("QR login did not complete".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn challenge_projection_and_resume_bind_to_current_sid_without_secret_state() {
        let state = std::env::temp_dir().join(format!("operator-challenge-{}", std::process::id()));
        let rt = Runtime::open(&state, "synthetic", None).unwrap();
        let dir = account_directory(&state, "qq", "synthetic").unwrap();
        let expiry = platform_qq::unix_seconds().unwrap() + 120;
        let mut pending = json!({
            "version":1,"uin":42,"uid":"synthetic-private-uid","guid":[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15],
            "profile":"linux31939038","expires_at":expiry,"exchange_key":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],
            "key_sig":[2,2,2],"exchange_expiration":expiry,"cookies":"synthetic-private-cookie",
            "kind":"captcha","verify_url":"https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=123&login_appid=456&sid=synthetic%2Bsid&uin=42","new_device":null
        });
        let file = dir.join("password-login.json");
        save_private_new(&file, &pending).unwrap();
        let challenge = rt.operator_challenge().unwrap();
        let public = serde_json::to_string(&challenge).unwrap();
        for private in [
            "exchange_key",
            "key_sig",
            "cookies",
            "guid",
            "synthetic-private-uid",
            "synthetic-private-cookie",
        ] {
            assert!(!public.contains(private));
        }
        let params = json!({"challenge_id":challenge.challenge_id});
        assert!(rt.operator_check_password_input(true,&params,br#"{"uin":42,"captcha":{"ticket":"synthetic-ticket","randstr":"synthetic-rand","session_id":"synthetic%2Bsid"}}"#).is_ok());
        assert!(rt.operator_check_password_input(true,&params,br#"{"uin":42,"captcha":{"ticket":"synthetic-ticket","randstr":"synthetic-rand","session_id":"123"}}"#).is_err());
        assert!(
            rt.operator_check_password_input(
                true,
                &json!({"challenge_id":"old"}),
                br#"{"uin":42,"captcha":{"session_id":"synthetic%2Bsid"}}"#
            )
            .is_err()
        );
        pending["expires_at"] = json!(expiry - 121);
        fs::remove_file(&file).unwrap();
        save_private_new(&file, &pending).unwrap();
        assert!(rt.operator_challenge().is_err());
        drop(rt);
        fs::remove_dir_all(state).unwrap();
    }
    #[test]
    fn protocol_cannot_resume_via_fresh_password_route() {
        let path = std::env::temp_dir().join(format!("operator-input-{}", std::process::id()));
        let rt = Runtime::open(&path, "synthetic", None).unwrap();
        assert!(
            rt.operator_check_password_input(
                false,
                &json!({}),
                b"{\"uin\":42,\"password\":\"synthetic\"}"
            )
            .is_ok()
        );
        assert!(
            rt.operator_check_password_input(false, &json!({}), b"{\"uin\":42,\"resume\":true}")
                .is_err()
        );
        assert!(
            rt.operator_check_password_input(false, &json!({}), b"{\"uin\":42,\"captcha\":{}}")
                .is_err()
        );
        assert!(
            rt.operator_check_password_input(
                true,
                &json!({"challenge_id":"stale"}),
                b"{\"uin\":42}"
            )
            .is_err()
        );
        drop(rt);
        fs::remove_dir_all(path).unwrap();
    }
}
