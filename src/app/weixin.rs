// SPDX-License-Identifier: MIT
use super::*;
use crate::private_state::account_directory;
use platform_weixin::{
    Client as WeixinClient, Config as WeixinConfig, LoginAttempt, LoginProfile, LoginStatus,
    MediaKind, Message, Session,
};
use std::collections::BTreeSet;

fn client(profile: LoginProfile) -> Result<WeixinClient> {
    let mut config = WeixinConfig::default();
    config.profile = profile;
    config.max_media_bytes = Budget::default().max_resource_bytes;
    WeixinClient::new(config).map_err(|e| e.to_string())
}

fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("private account record exceeds 8 MiB".into());
    }
    let temporary = path.with_extension("next");
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|e| e.to_string())?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&temporary, path).map_err(|e| e.to_string())?;
    File::open(path.parent().ok_or("account parent missing")?)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}
fn load_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&read_bounded(path, 8 * 1024 * 1024)?)
        .map_err(|_| "invalid private account record".into())
}
fn bind_identity(dir: &Path, session: &Session) -> Result<()> {
    let binding = dir.join("identity.json");
    let identity = session.stable_identity();
    if binding.exists() {
        if load_json(&binding)?.get("identity").and_then(Value::as_str) != Some(identity.as_str()) {
            return Err(
                "This alias belongs to another Weixin account; use a new --account alias".into(),
            );
        }
    } else {
        atomic_json(&binding, &json!({"schema":1,"identity":identity}))?;
    }
    Ok(())
}
fn inbox(dir: &Path) -> Result<Vec<Value>> {
    let path = dir.join("inbox.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = load_json(&path)?;
    let list = data
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("invalid inbox record")?;
    if list.len() > 512 {
        return Err("inbox item limit".into());
    }
    Ok(list.clone())
}

fn trim_inbox(cached: &mut Vec<Value>) -> Result<usize> {
    let mut keep_from = cached.len();
    let mut bytes = 256_usize;
    let mut kept = 0;
    for value in cached.iter().rev() {
        let size = serde_json::to_vec(value).map_err(|e| e.to_string())?.len() + 1;
        if kept >= 512 || bytes.saturating_add(size) > 4 * 1024 * 1024 {
            break;
        }
        bytes += size;
        kept += 1;
        keep_from -= 1;
    }
    if keep_from > 0 {
        cached.drain(..keep_from);
    }
    Ok(keep_from)
}

impl Runtime {
    fn weixin_directory(&self) -> Result<PathBuf> {
        if self.account == "public" {
            return Err("Weixin personal accounts require an explicit --account alias".into());
        }
        account_directory(&self.state, "weixin", &self.account)
    }
    fn load_weixin_session(&self) -> Result<(PathBuf, Session)> {
        let dir = self.weixin_directory()?;
        let path = dir.join("session.json");
        if !path.exists() {
            return Err("No Weixin session for this alias; use the CLI weixin login first".into());
        }
        let session = Session::load(&path).map_err(|e| e.to_string())?;
        bind_identity(&dir, &session)?;
        Ok((dir, session))
    }

    /// Human-only control plane; deliberately absent from Runtime::call and AI tools.
    pub fn weixin_login(&mut self) -> Result<Value> {
        let dir = self.weixin_directory()?;
        let profile = if dir.join("session.json").exists() {
            Session::load(&dir.join("session.json"))
                .map_err(|e| e.to_string())?
                .profile()
        } else {
            LoginProfile::Web
        };
        self.weixin_login_profile(match profile {
            LoginProfile::Web => "web",
            LoginProfile::DesktopCompatibility => "desktop",
        })
    }
    pub fn weixin_login_profile(&mut self, profile: &str) -> Result<Value> {
        let profile = match profile {
            "web" => LoginProfile::Web,
            "desktop" => LoginProfile::DesktopCompatibility,
            _ => return Err("login profile must be web or desktop".into()),
        };
        let dir = self.weixin_directory()?;
        let session_path = dir.join("session.json");
        if session_path.exists() {
            let mut saved = Session::load(&session_path).map_err(|e| e.to_string())?;
            bind_identity(&dir, &saved)?;
            let api = client(saved.profile())?;
            match api.resume(&mut saved) {
                Ok(info) => {
                    saved.save(&session_path).map_err(|e| e.to_string())?;
                    return Ok(json!({"status":"online","reused_session":true,"user":info.user}));
                }
                Err(platform_weixin::Error::AuthExpired { .. }) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let api = client(profile)?;
        let mut attempt = api.begin_login().map_err(|e| e.to_string())?;
        let temporary = dir.join("qrcode-next");
        if temporary.exists() {
            fs::remove_file(&temporary).map_err(|e| e.to_string())?;
        }
        let receipt = api
            .download_qr(&mut attempt, &temporary)
            .map_err(|e| e.to_string())?;
        let extension = match receipt.mime.as_str() {
            "image/jpeg" => "jpg",
            "image/png" => "png",
            _ => "gif",
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let qr_path = dir.join(format!("qrcode-{nonce}.{extension}"));
        attempt
            .save(&dir.join("login-attempt.json"))
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, &qr_path).map_err(|e| e.to_string())?;
        File::open(&dir)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        atomic_json(
            &dir.join("last-login.json"),
            &json!({"status":"waiting","route":"webwx","profile":profile}),
        )?;
        Ok(
            json!({"status":"waiting","qr_path":qr_path,"mime":receipt.mime,"expires_in_seconds":receipt.expires_in_seconds,"personal_account":true,"profile":profile}),
        )
    }
    pub fn weixin_poll_login(&mut self) -> Result<Value> {
        let dir = self.weixin_directory()?;
        let path = dir.join("login-attempt.json");
        let mut attempt = LoginAttempt::load(&path).map_err(|e| e.to_string())?;
        let profile = attempt.profile();
        let api = client(profile)?;
        let status = api.poll_login(&mut attempt).map_err(|e| e.to_string())?;
        attempt.save(&path).map_err(|e| e.to_string())?;
        match status {
            LoginStatus::Confirmed => {
                let session = match api.finish_login(&mut attempt) {
                    Ok(session) => session,
                    Err(error) => {
                        let public =
                            if matches!(error, platform_weixin::Error::WebLoginPageUnavailable) {
                                "web_login_page_unavailable"
                            } else {
                                "failed"
                            };
                        atomic_json(
                            &dir.join("last-login.json"),
                            &json!({"status":public,"route":"webwx","profile":profile}),
                        )?;
                        return Err(error.to_string());
                    }
                };
                bind_identity(&dir, &session)?;
                session
                    .save(&dir.join("session.json"))
                    .map_err(|e| e.to_string())?;
                let info = session.info();
                fs::remove_file(path).map_err(|e| e.to_string())?;
                atomic_json(
                    &dir.join("last-login.json"),
                    &json!({"status":"online","route":"webwx","profile":profile}),
                )?;
                Ok(json!({"status":"online","user":info.user,"personal_account":true}))
            }
            other => {
                atomic_json(
                    &dir.join("last-login.json"),
                    &json!({"status":other,"route":"webwx","profile":profile}),
                )?;
                Ok(json!({"status":other,"personal_account":true}))
            }
        }
    }

    pub(super) fn call_weixin(&mut self, method: &str, params: &Value) -> Result<Value> {
        if method == "weixin.status" {
            if self.account == "public" {
                return Ok(
                    json!({"status":"unconfigured","detail":"Choose a dedicated account alias and use CLI login"}),
                );
            }
            let dir = self.weixin_directory()?;
            let last = dir.join("last-login.json");
            let last_status = if last.exists() {
                let value = load_json(&last)?;
                match value.get("status").and_then(Value::as_str) {
                    Some(
                        status @ ("waiting"
                        | "scanned"
                        | "expired"
                        | "online"
                        | "failed"
                        | "web_login_page_unavailable"),
                    ) => Some(status.to_owned()),
                    _ => None,
                }
            } else {
                None
            };
            return Ok(if dir.join("session.json").exists() {
                let session =
                    Session::load(&dir.join("session.json")).map_err(|e| e.to_string())?;
                bind_identity(&dir, &session)?;
                json!({"status":"saved_session","user":session.info().user,"online_checked":false,"last_login_status":last_status})
            } else {
                json!({"status":last_status.unwrap_or_else(||"signed_out".into()),"session_available":false,"login_attempt_present":dir.join("login-attempt.json").exists()})
            });
        }
        if !matches!(
            method,
            "weixin.resume"
                | "weixin.contacts"
                | "weixin.receive"
                | "weixin.inbox"
                | "weixin.media"
        ) {
            return Err("unknown Weixin tool; login is available only through human CLI".into());
        }
        let (dir, mut session) = self.load_weixin_session()?;
        let api = client(session.profile())?;
        let scope = self.scope(Platform::Weixin);
        let session_path = dir.join("session.json");
        match method {
            "weixin.resume" => {
                let info = api.resume(&mut session).map_err(|e| e.to_string())?;
                session.save(&session_path).map_err(|e| e.to_string())?;
                Ok(json!({"status":"online","user":info.user}))
            }
            "weixin.contacts" => {
                let page = api
                    .contacts(&mut session, number(params, "sequence", 0)?)
                    .map_err(|e| e.to_string())?;
                session.save(&session_path).map_err(|e| e.to_string())?;
                Ok(json!({"contacts":page.contacts,"next_sequence":page.next_sequence}))
            }
            "weixin.inbox" => {
                let list = inbox(&dir)?;
                let offset =
                    usize::try_from(number(params, "offset", 0)?).map_err(|_| "invalid offset")?;
                let limit =
                    usize::try_from(number(params, "limit", 20)?).map_err(|_| "invalid limit")?;
                if limit == 0 || limit > 100 {
                    return Err("inbox limit must be 1..100".into());
                }
                let messages: Vec<_> = list.iter().skip(offset).take(limit).collect();
                Ok(
                    json!({"messages":messages,"cached_count":list.len(),"offset":offset,"next_offset":offset+messages.len()}),
                )
            }
            "weixin.receive" => {
                let status = api.sync_check(&mut session).map_err(|e| e.to_string())?;
                if !status.has_updates {
                    return Ok(json!({"messages":[],"has_updates":false,"cursor_advanced":false}));
                }
                let batch = api.sync(&mut session).map_err(|e| e.to_string())?;
                let mut cached = inbox(&dir)?;
                let mut seen: BTreeSet<String> = cached
                    .iter()
                    .filter_map(|v| v["id"].as_str().map(str::to_owned))
                    .collect();
                let mut assets = self.locators.batch();
                let mut resources = self.store.batch();
                let mut fresh = Vec::new();
                for message in &batch.messages {
                    if !seen.insert(message.id.clone()) {
                        continue;
                    }
                    let mut blocks = Vec::new();
                    if message.msg_type == 1 {
                        let projection = runtime_content::normalize_html(&message.content)?;
                        blocks.push(ContentBlock::Html {
                            html: projection.html,
                            truncated: projection.truncated,
                        });
                        let source = resources.import(
                            &scope,
                            &format!("webwx:{}:source", message.id),
                            "application/json",
                            &mut serde_json::to_vec(&message.source)
                                .map_err(|e| e.to_string())?
                                .as_slice(),
                        )?;
                        blocks.push(ContentBlock::Source {
                            resource: source,
                            label: "message text source".into(),
                        });
                    } else {
                        blocks.push(ContentBlock::Text {
                            text: format!(
                                "[Weixin message type {}: platform payload retained privately]",
                                message.msg_type
                            ),
                        });
                    }
                    let payload = if message.msg_type != 1 {
                        Some(assets.put(&scope, "weixin-payload", &message.source)?)
                    } else {
                        None
                    };
                    let asset = if message.media.is_some() {
                        let mut locator = message.clone();
                        locator.content.clear();
                        locator.source = Value::Null;
                        Some(assets.put(
                            &scope,
                            "weixin-media",
                            &json!({"identity":session.stable_identity(),"message":locator}),
                        )?)
                    } else {
                        None
                    };
                    let document = Document::new(
                        scope.clone(),
                        format!("webwx:{}", message.id),
                        "Weixin message".into(),
                        blocks,
                    );
                    fresh.push(json!({"id":message.id,"from":message.from,"to":message.to,"conversation":message.conversation,"group_sender":message.group_sender,"created_at":message.created_at,"msg_type":message.msg_type,"media_kind":message.media,"media_asset":asset,"private_payload_asset":payload,"voice_length_ms":message.voice_length_ms,"video_length_seconds":message.video_length_seconds,"document":document}));
                }
                // The entire newly received batch is durable before cursor persistence.
                let batch_content = json!({"messages":fresh,"changed_contacts":batch.changed_contacts,"continue_sync":batch.continue_sync});
                let batch_ref = resources.import(
                    &scope,
                    "weixin-received-batch",
                    "application/json",
                    &mut serde_json::to_vec(&batch_content)
                        .map_err(|e| e.to_string())?
                        .as_slice(),
                )?;
                cached.extend(fresh.iter().cloned());
                let evicted = trim_inbox(&mut cached)?;
                // Published inbox refs must survive all later errors, including a
                // directory fsync error after rename. Keep their backing objects.
                resources.commit();
                assets.commit();
                atomic_json(
                    &dir.join("inbox.json"),
                    &json!({"schema":1,"messages":cached}),
                )?;
                session.save(&session_path).map_err(|e| e.to_string())?;
                let preview: Vec<_> = fresh.iter().take(20).cloned().collect();
                Ok(
                    json!({"messages":preview,"received_count":fresh.len(),"batch_resource":batch_ref,"continue_sync":batch.continue_sync,"cursor_advanced":true,"older_cache_evicted":evicted}),
                )
            }
            "weixin.media" => {
                let asset = required_str(params, "asset")?;
                let value: Value = self.locators.get(&scope, "weixin-media", asset)?;
                if value.get("identity").and_then(Value::as_str)
                    != Some(session.stable_identity().as_str())
                {
                    return Err("media locator belongs to another Weixin identity".into());
                }
                let message: Message = serde_json::from_value(value["message"].clone())
                    .map_err(|_| "invalid private message locator")?;
                let work = WorkDir::new(&self.state)?;
                let path = work.path.join("media");
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .map_err(|e| e.to_string())?;
                let receipt = api
                    .download_media(
                        &mut session,
                        &message,
                        &mut file,
                        Budget::default().max_resource_bytes,
                    )
                    .map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                drop(file);
                let group = match receipt.kind {
                    MediaKind::Image => "image",
                    MediaKind::Voice => "audio",
                    MediaKind::Video => "video",
                };
                let mime = super::download_mime(&path, receipt.mime.as_deref(), group)?;
                let resource = self.import_output(&scope, "weixin-media", &path, &mime)?;
                Ok(
                    json!({"asset":asset,"resource":resource,"kind":receipt.kind,"model_observed":false}),
                )
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inbox_limit_preserves_latest_messages_within_byte_budget() {
        let mut rows: Vec<_> = (0..300)
            .map(|id| json!({"id":id,"text":"x".repeat(20000)}))
            .collect();
        let removed = trim_inbox(&mut rows).unwrap();
        assert!(removed > 0);
        assert_eq!(rows.last().unwrap()["id"], 299);
        assert_eq!(rows[0]["id"], removed);
        assert!(rows.len() <= 512);
        assert!(serde_json::to_vec(&rows).unwrap().len() < 4 * 1024 * 1024);
    }
}
