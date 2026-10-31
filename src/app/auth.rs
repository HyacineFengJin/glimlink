// SPDX-License-Identifier: MIT
//! Human-only login control and private account identity bindings.
use super::*;
use crate::private_state::{account_directory, account_path};

pub(super) fn saved_session(
    state: &Path,
    platform: &str,
    account: &str,
) -> Result<Option<PathBuf>> {
    if account == "public" {
        return Ok(None);
    }
    let path = account_path(state, platform, account)?.join("session.json");
    Ok(path.exists().then_some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn login_identity_cannot_be_rebound_to_a_different_account() {
        let root = std::env::temp_dir().join(format!("auth-binding-{}", std::process::id()));
        private_dir(&root).unwrap();
        let directory = account_directory(&root, "bilibili", "../../账号").unwrap();
        bind_identity(&directory, "42").unwrap();
        bind_identity(&directory, "42").unwrap();
        assert!(bind_identity(&directory, "99").is_err());
        let binding: Value =
            serde_json::from_slice(&read_bounded(&directory.join("identity.json"), 1024).unwrap())
                .unwrap();
        assert_eq!(binding["identity"], "42");
        assert_eq!(
            fs::metadata(directory.join("identity.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn import_commit_rejects_actor_changes_and_preserves_previous_session_on_failure() {
        let root = std::env::temp_dir().join(format!("auth-import-commit-{}", std::process::id()));
        private_dir(&root).unwrap();
        bind_identity(&root, "42").unwrap();
        save_private_new(
            &root.join("session.json"),
            &json!({"old":"synthetic-saved-state"}),
        )
        .unwrap();
        let before = fs::read(root.join("session.json")).unwrap();
        let mut called = false;
        assert!(
            commit_import(&root, "99", |_| {
                called = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!called);
        assert!(
            commit_import(&root, "42", |_| Err(
                "synthetic persistence rejection".into()
            ))
            .is_err()
        );
        assert_eq!(fs::read(root.join("session.json")).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn private_import_rejects_public_alias_permissions_symlinks_and_bad_input_before_network() {
        let root = std::env::temp_dir().join(format!("auth-import-file-{}", std::process::id()));
        private_dir(&root).unwrap();
        let file = root.join("input");
        fs::write(&file, "SESSDATA=synthetic-incomplete").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(private_login_input(&link).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_login_input(&file).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let state = root.join("state");
        let mut rt = Runtime::open(&state, "public", None).unwrap();
        assert!(
            rt.bilibili_import_login(&file)
                .unwrap_err()
                .contains("explicit")
        );
        drop(rt);
        let mut rt = Runtime::open(&state, "owner", None).unwrap();
        assert!(rt.bilibili_import_login(&file).is_err());
        fs::write(&file, "STOKEN=synthetic-without-bduss").unwrap();
        assert!(rt.tieba_import_login(&file).is_err());
        for platform in ["bilibili", "tieba"] {
            let dir = account_directory(&state, platform, "owner").unwrap();
            assert!(!dir.join("session.json").exists());
            assert!(!dir.join("identity.json").exists());
        }
        drop(rt);
        fs::remove_dir_all(root).unwrap();
    }
}

pub(super) fn bind_identity(dir: &Path, identity: &str) -> Result<()> {
    let path = dir.join("identity.json");
    if path.exists() {
        let value: Value = serde_json::from_slice(&read_bounded(&path, 1024)?)
            .map_err(|_| "invalid account identity binding")?;
        if value["identity"].as_str() != Some(identity) {
            return Err("This alias is bound to another account; use a new --account alias".into());
        }
    } else {
        save_private_new(&path, &json!({"schema":1,"identity":identity}))?;
        File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn private_login_input(path: &Path) -> Result<String> {
    const MAX_IMPORT: u64 = 256 * 1024;
    let file = open_regular(path, MAX_IMPORT)?;
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect login import file")?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("login import file must be private (0600)".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read login import file")?;
    if bytes.len() as u64 > MAX_IMPORT {
        return Err("login import file exceeds limit".into());
    }
    String::from_utf8(bytes).map_err(|_| "login import file must be UTF-8".into())
}
fn commit_import(
    dir: &Path,
    identity: &str,
    persist: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    // Live identity validation has already completed. An alias cannot change
    // its actor, and SDK save uses an atomic private replacement.
    bind_identity(dir, identity)?;
    persist(&dir.join("session.json"))
}
fn qr_png(dir: &Path, value: &str) -> Result<PathBuf> {
    if value.len() > 4096 {
        return Err("login QR payload exceeds limit".into());
    }
    let code = qrcode::QrCode::new(value.as_bytes()).map_err(|_| "invalid login QR payload")?;
    let width = code.width();
    let scale = 6;
    let edge = (width + 8) * scale;
    let colors = code.into_colors();
    let image = image::GrayImage::from_fn(edge as u32, edge as u32, |x, y| {
        let (x, y) = (x as usize / scale, y as usize / scale);
        let dark = x >= 4
            && y >= 4
            && x < width + 4
            && y < width + 4
            && colors[(y - 4) * width + x - 4] == qrcode::Color::Dark;
        image::Luma([if dark { 0 } else { 255 }])
    });
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = dir.join(format!("login-qr-{nonce}.png"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| e.to_string())?;
    image
        .write_to(&mut file, image::ImageFormat::Png)
        .map_err(|_| "failed to encode login QR")?;
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(path)
}

impl Runtime {
    fn login_directory(&self, platform: &str) -> Result<PathBuf> {
        if self.account == "public" {
            return Err("Personal login needs an explicit --account alias".into());
        }
        account_directory(&self.state, platform, &self.account)
    }

    /// Human management only. Accept a Runtime session JSON or a Cookie header
    /// in an existing private file. No QR, browser discovery or model credential tool.
    pub fn bilibili_import_login(&mut self, path: &Path) -> Result<Value> {
        let dir = self.login_directory("bilibili")?;
        let input = private_login_input(path)?;
        let (session, user, source) = if input.trim_start().starts_with('{') {
            let session = platform_bilibili::Session::load(path).map_err(|e| e.to_string())?;
            let mut config = Config::default();
            config.cookie = Some(session.cookie_header());
            let user = Client::new(config)
                .map_err(|e| e.to_string())?
                .current_user()
                .map_err(|e| e.to_string())?;
            if user.mid.to_string() != session.stable_identity() {
                return Err("Bilibili imported session identity mismatch".into());
            }
            (session, user, "runtime_session")
        } else {
            let (session, user) = platform_bilibili::AuthClient::new(Config::default())
                .map_err(|e| e.to_string())?
                .import_cookie_header(&input)
                .map_err(|e| e.to_string())?;
            (session, user, "cookie_header")
        };
        let mut config = Config::default();
        config.max_download_bytes = Budget::default().max_resource_bytes;
        config.cookie = Some(session.cookie_header());
        let client = Client::new(config).map_err(|e| e.to_string())?;
        commit_import(&dir, &session.stable_identity(), |path| {
            session.save(path).map_err(|e| e.to_string())
        })?;
        self.bili = client;
        Ok(
            json!({"status":"online","scope":self.scope(Platform::Bilibili),"user":user,"source":source,"session_saved":true,"qr_started":false,"model_credentials_exposed":false}),
        )
    }
    /// Human management only. The private input is a Runtime session JSON,
    /// Cookie header, or bare formal BDUSS; the existing identity handshake is
    /// completed before any saved credential is replaced.
    pub fn tieba_import_login(&mut self, path: &Path) -> Result<Value> {
        let dir = self.login_directory("tieba")?;
        let input = private_login_input(path)?;
        let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
        let (session, source) = if input.trim_start().starts_with('{') {
            let mut session = platform_tieba::Session::load(path).map_err(|e| e.to_string())?;
            api.me(&mut session).map_err(|e| e.to_string())?;
            (session, "runtime_session")
        } else {
            (
                api.import_cookie_header(&input)
                    .map_err(|e| e.to_string())?,
                "cookie_or_bduss",
            )
        };
        let user = session.info().clone();
        commit_import(&dir, &session.stable_identity(), |path| {
            session.save(path).map_err(|e| e.to_string())
        })?;
        let mut config = platform_tieba::Config::default();
        config.bduss = Some(session.bduss().to_owned());
        self.tieba = super::tieba::TiebaState::new(config);
        Ok(
            json!({"status":"online","scope":self.scope(Platform::Tieba),"user":user,"source":source,"session_saved":true,"qr_started":false,"model_credentials_exposed":false}),
        )
    }

    /// Human CLI only: no login action is registered with the model tool router.
    pub fn bilibili_login(&mut self) -> Result<Value> {
        let dir = self.login_directory("bilibili")?;
        if dir.join("session.json").exists() {
            match self.bili.current_user() {
                Ok(user) => {
                    bind_identity(&dir, &user.mid.to_string())?;
                    return Ok(json!({"status":"online","reused_session":true,"user":user}));
                }
                Err(platform_bilibili::Error::LoginRequired) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let path = dir.join("login-attempt.json");
        let mut reused = false;
        let attempt = if path.exists() {
            let saved = platform_bilibili::LoginAttempt::load(&path).map_err(|e| e.to_string())?;
            if saved.status() == platform_bilibili::LoginStatus::Confirmed {
                return self.bilibili_poll_login();
            }
            if saved.expires_in_seconds() > 0
                && matches!(
                    saved.status(),
                    platform_bilibili::LoginStatus::Waiting
                        | platform_bilibili::LoginStatus::Scanned
                )
            {
                reused = true;
                saved
            } else {
                platform_bilibili::AuthClient::new(Config::default())
                    .map_err(|e| e.to_string())?
                    .begin_login()
                    .map_err(|e| e.to_string())?
            }
        } else {
            platform_bilibili::AuthClient::new(Config::default())
                .map_err(|e| e.to_string())?
                .begin_login()
                .map_err(|e| e.to_string())?
        };
        attempt.save(&path).map_err(|e| e.to_string())?;
        let qr = qr_png(&dir, attempt.qr_url())?;
        Ok(
            json!({"status":"waiting","qr_path":qr,"expires_in_seconds":attempt.expires_in_seconds(),"reused_attempt":reused}),
        )
    }

    pub fn bilibili_poll_login(&mut self) -> Result<Value> {
        let dir = self.login_directory("bilibili")?;
        let path = dir.join("login-attempt.json");
        let mut attempt =
            platform_bilibili::LoginAttempt::load(&path).map_err(|e| e.to_string())?;
        let api =
            platform_bilibili::AuthClient::new(Config::default()).map_err(|e| e.to_string())?;
        let polled = api.poll_login(&mut attempt);
        attempt.save(&path).map_err(|e| e.to_string())?;
        let status = polled.map_err(|e| e.to_string())?;
        if status != platform_bilibili::LoginStatus::Confirmed {
            return Ok(json!({"status":status}));
        }
        let session = attempt.take_session().map_err(|e| e.to_string())?;
        // Verify the service's current identity before committing credentials.
        let mut config = Config::default();
        config.cookie = Some(session.cookie_header());
        config.max_download_bytes = Budget::default().max_resource_bytes;
        let client = Client::new(config).map_err(|e| e.to_string())?;
        let user = client.current_user().map_err(|e| e.to_string())?;
        if user.mid.to_string() != session.stable_identity() {
            return Err("Bilibili session identity mismatch".into());
        }
        bind_identity(&dir, &session.stable_identity())?;
        session
            .save(&dir.join("session.json"))
            .map_err(|e| e.to_string())?;
        fs::remove_file(path).map_err(|e| e.to_string())?;
        File::open(&dir)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        self.bili = client;
        Ok(json!({"status":"online","user":user,"session_saved":true}))
    }

    pub(super) fn call_bilibili_account(&mut self, method: &str) -> Result<Value> {
        if method == "bilibili.status" {
            let path = saved_session(&self.state, "bilibili", &self.account)?;
            return Ok(if let Some(path) = path {
                let session = platform_bilibili::Session::load(&path).map_err(|e| e.to_string())?;
                json!({"status":"saved_session","identity":session.stable_identity(),"online_checked":false})
            } else {
                json!({"status":"signed_out","online_checked":false})
            });
        }
        let dir = self.login_directory("bilibili")?;
        let user = self.bili.current_user().map_err(|e| e.to_string())?;
        bind_identity(&dir, &user.mid.to_string())?;
        match method {
            "bilibili.me" => Ok(json!({"user":user,"online_checked":true})),
            "bilibili.favorites" => {
                let folders = self
                    .bili
                    .favorite_folders(user.mid)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"owner":user,"folders":folders}))
            }
            _ => Err("unknown Bilibili account read".into()),
        }
    }

    pub fn tieba_login(&mut self) -> Result<Value> {
        self.tieba_login_mode(true)
    }
    pub(super) fn tieba_login_mode(&mut self, reuse_saved: bool) -> Result<Value> {
        let dir = self.login_directory("tieba")?;
        let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
        let session_path = dir.join("session.json");
        if reuse_saved && session_path.exists() {
            let mut session =
                platform_tieba::Session::load(&session_path).map_err(|e| e.to_string())?;
            let user = api.me(&mut session).map_err(|e| e.to_string())?;
            if user.stable_identity() != session.stable_identity() {
                return Err("Tieba session identity mismatch".into());
            }
            bind_identity(&dir, &user.stable_identity())?;
            session.save(&session_path).map_err(|e| e.to_string())?;
            return Ok(json!({"status":"online","reused_session":true,"user":user}));
        }
        let path = dir.join("login-attempt.json");
        let mut reused = false;
        let mut attempt = if path.exists() {
            let saved = platform_tieba::LoginAttempt::load(&path).map_err(|e| e.to_string())?;
            if saved.exchange_attempted()
                || saved.status() == platform_tieba::LoginStatus::Confirmed
            {
                return self.tieba_poll_login();
            }
            if saved.expires_in_seconds() > 0
                && matches!(
                    saved.status(),
                    platform_tieba::LoginStatus::Waiting | platform_tieba::LoginStatus::Scanned
                )
            {
                reused = true;
                saved
            } else {
                api.begin_login().map_err(|e| e.to_string())?
            }
        } else {
            api.begin_login().map_err(|e| e.to_string())?
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let temporary = dir.join(format!("login-qr-{nonce}.image"));
        let receipt = api
            .download_qr(&mut attempt, &temporary)
            .map_err(|e| e.to_string())?;
        attempt.save(&path).map_err(|e| e.to_string())?;
        let extension = if receipt.mime == "image/jpeg" {
            "jpg"
        } else {
            "png"
        };
        let qr = temporary.with_extension(extension);
        fs::rename(&temporary, &qr).map_err(|e| e.to_string())?;
        Ok(
            json!({"status":"waiting","qr_path":qr,"mime":receipt.mime,"expires_in_seconds":attempt.expires_in_seconds(),"reused_attempt":reused}),
        )
    }

    pub fn tieba_poll_login(&mut self) -> Result<Value> {
        let dir = self.login_directory("tieba")?;
        let path = dir.join("login-attempt.json");
        let mut attempt = platform_tieba::LoginAttempt::load(&path).map_err(|e| e.to_string())?;
        let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
        let session = if attempt.exchange_attempted() {
            api.resume_exchanged_login(&attempt)
                .map_err(|e| e.to_string())?
        } else {
            let polled = api.poll_login(&mut attempt);
            attempt.save(&path).map_err(|e| e.to_string())?;
            let status = polled.map_err(|e| e.to_string())?;
            if status != platform_tieba::LoginStatus::Confirmed {
                return Ok(json!({"status":status}));
            }
            let exchanged = api.finish_login_persisted(&mut attempt, &path);
            attempt.save(&path).map_err(|e| e.to_string())?;
            exchanged.map_err(|e| e.to_string())?
        };
        let user = session.info().clone();
        if user.stable_identity() != session.stable_identity() {
            return Err("Tieba session identity mismatch".into());
        }
        bind_identity(&dir, &session.stable_identity())?;
        session
            .save(&dir.join("session.json"))
            .map_err(|e| e.to_string())?;
        fs::remove_file(path).map_err(|e| e.to_string())?;
        File::open(&dir)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        let mut config = platform_tieba::Config::default();
        config.bduss = Some(session.bduss().to_owned());
        self.tieba = super::tieba::TiebaState::new(config);
        Ok(json!({"status":"online","user":user,"session_saved":true}))
    }

    pub(super) fn call_tieba_account(&mut self, method: &str) -> Result<Value> {
        let path = saved_session(&self.state, "tieba", &self.account)?;
        if method == "tieba.status" {
            return Ok(if let Some(path) = path {
                let session = platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
                json!({"status":"saved_session","identity":session.stable_identity(),"online_checked":false})
            } else {
                json!({"status":"signed_out","online_checked":false})
            });
        }
        let path = path.ok_or("No saved Tieba login for this account alias")?;
        let mut session = platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
        let user = platform_tieba::AuthClient::new()
            .map_err(|e| e.to_string())?
            .me(&mut session)
            .map_err(|e| e.to_string())?;
        if user.stable_identity() != session.stable_identity() {
            return Err("Tieba session identity mismatch".into());
        }
        bind_identity(
            path.parent().ok_or("account directory missing")?,
            &user.stable_identity(),
        )?;
        session.save(&path).map_err(|e| e.to_string())?;
        Ok(json!({"user":user,"online_checked":true}))
    }
}
