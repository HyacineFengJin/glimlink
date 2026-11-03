// SPDX-License-Identifier: MIT
//! Authorized tool writes: target resolution, durable one-shot submit and readback.
use super::*;
use crate::{
    outbox::{Operation, Outbox, Status},
    private_state::account_directory,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct CommentRequest {
    #[serde(default)]
    root: u64,
    #[serde(default)]
    parent: u64,
    image_resource: Option<String>,
}
impl CommentRequest {
    pub(super) fn from_params(name: &str, params: &Value) -> Result<Self> {
        let root = number(params, "root", 0)?;
        let parent = number(params, "parent", root)?;
        let image_resource = match params.get("image_resource") {
            None => None,
            Some(Value::String(id))
                if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                Some(id.clone())
            }
            _ => return Err("image_resource must be an existing opaque resource ID".into()),
        };
        if root > i64::MAX as u64
            || parent > i64::MAX as u64
            || (root == 0) != (parent == 0)
            || (name != "bilibili" && (root != 0 || image_resource.is_some()))
        {
            return Err("invalid comment reply or image parameters".into());
        }
        Ok(Self {
            root,
            parent,
            image_resource,
        })
    }
    fn frozen(&self) -> Result<Option<Value>> {
        if self == &Self::default() {
            Ok(None)
        } else {
            Ok(Some(value(self)?))
        }
    }
    pub(super) fn from_record(record: &Operation) -> Result<Self> {
        record
            .request
            .as_ref()
            .map(target)
            .transpose()
            .map(|v| v.unwrap_or_default())
    }
}

struct AccountGuard<'a> {
    runtime: &'a mut Runtime,
    account: Option<String>,
    bili: Option<Client>,
    tieba: Option<super::tieba::TiebaState>,
}
impl Drop for AccountGuard<'_> {
    fn drop(&mut self) {
        self.runtime.account = self.account.take().expect("saved account context");
        self.runtime.bili = self.bili.take().expect("saved Bilibili context");
        self.runtime.tieba = self.tieba.take().expect("saved Tieba context");
    }
}

fn aliases(state: &Path, name: &str) -> Result<Vec<String>> {
    platform(name)?;
    let directory = state.join("accounts").join(name);
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut aliases = Vec::new();
    for (count, entry) in fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if count >= 128 {
            return Err("saved account directory limit".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir()
            || !entry.path().join("session.json").is_file()
        {
            continue;
        }
        let encoded = entry.file_name();
        let Some(encoded) = encoded.to_str() else {
            continue;
        };
        let Ok(bytes) = URL_SAFE_NO_PAD.decode(encoded) else {
            continue;
        };
        let Ok(alias) = String::from_utf8(bytes) else {
            continue;
        };
        let scope = Scope {
            platform: platform(name)?,
            account: alias.clone(),
        };
        scope.validate()?;
        if alias != "public" {
            aliases.push(alias);
        }
    }
    aliases.sort();
    Ok(aliases)
}

pub(super) fn compact(record: Value) -> Value {
    let receipt=record.get("receipt").filter(|v|!v.is_null()).map(|r|json!({"rpid":r.get("rpid"),"post_id":r.get("post_id"),"oid":r.get("oid"),"thread_id":r.get("thread_id"),"need_captcha":r.get("need_captcha")}));
    let mut target = record["target"].clone();
    if let Some(title) = target["title"].as_str() {
        target["title"] = json!(title.chars().take(1024).collect::<String>());
    }
    json!({"operation_id":record["id"],"scope":record["scope"],"status":record["status"],"target":target,"message":record["message"],"request":record.get("request"),"receipt":receipt,"verification":record["verification"],"error":record["error"]})
}

fn platform(name: &str) -> Result<Platform> {
    match name {
        "bilibili" => Ok(Platform::Bilibili),
        "tieba" => Ok(Platform::Tieba),
        "qq" => Ok(Platform::Qq),
        "github" => Ok(Platform::Github),
        "x" => Ok(Platform::X),
        "pixiv" => Ok(Platform::Pixiv),
        "reddit" => Ok(Platform::Reddit),
        "discord" => Ok(Platform::Discord),
        _ => Err("unsupported platform account namespace".into()),
    }
}
fn value<T: serde::Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}
fn same_bili_target(actual: &platform_bilibili::CommentTarget, saved: &Value) -> Result<bool> {
    let mut expected: platform_bilibili::CommentTarget = target(saved)?;
    expected.actor_mid = expected.actor_mid();
    let mut actual = actual.clone();
    actual.actor_mid = actual.actor_mid();
    Ok(actual == expected)
}

fn target<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| "invalid prepared comment target".into())
}

impl Runtime {
    pub(super) fn call_comment_emote_packages(&mut self, params: &Value) -> Result<Value> {
        let (offset, limit) = emote_page_params(params)?;
        let package_id = match params.get("package_id") {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("package_id must be positive")?,
            ),
        };
        self.with_comment_account("bilibili",params,|runtime| {
            let packages=runtime.bili.comment_emote_packages().map_err(|e|e.to_string())?;
            let mut result=Vec::new();
            let mut next_offset=None;
            for package in packages {
                if package_id.is_some_and(|id|id!=package.id) {continue;}
                let mut package=value(&package)?;
                let count=package["emote"].as_array().map(Vec::len).unwrap_or(0);
                package["emote_count"]=json!(count);
                if package_id.is_none() {package.as_object_mut().ok_or("package object expected")?.remove("emote");}
                else {
                    let items=package["emote"].as_array_mut().ok_or("package items expected")?;
                    let (page,next)=emote_json_page(std::mem::take(items),offset,limit)?;
                    *items=page;
                    next_offset=next;
                }
                result.push(package);
            }
            if package_id.is_some() && result.is_empty() {return Err("package absent from this account panel".into());}
            let total=if package_id.is_some(){result[0]["emote_count"].as_u64().unwrap_or(0)}else{result.len() as u64};
            if package_id.is_none(){let (page,next)=emote_json_page(result,offset,limit)?;result=page;next_offset=next;}
            Ok(json!({"scope":runtime.scope(Platform::Bilibili),"packages":result,"total":total,"offset":offset,"next_offset":next_offset,
                "note":"added is the service panel flag. Availability and unlocked flags are preserved separately; panel membership is not purchase history."}))
        })
    }

    pub(super) fn call_comment_emotes(&mut self, params: &Value) -> Result<Value> {
        let (offset, limit) = emote_page_params(params)?;
        let exact = match params.get("text") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 256)
                    .ok_or("invalid emote text filter")?,
            ),
        };
        self.with_comment_account("bilibili", params, |runtime| {
            let all = runtime.bili.comment_emotes().map_err(|e| e.to_string())?;
            let items = all
                .iter()
                .filter(|e| exact.is_none_or(|text| e.text == text))
                .map(value)
                .collect::<Result<Vec<_>>>()?;
            let total = items.len();
            let (items, next_offset) = emote_json_page(items, offset, limit)?;
            Ok(json!({"items":items,"total":total,"offset":offset,"next_offset":next_offset}))
        })
    }

    pub(super) fn comment_image_bytes(&self, id: &str) -> Result<Vec<u8>> {
        let (_, resource, path) = self.resolve_any(id)?;
        if !matches!(resource.mime.as_str(), "image/png" | "image/jpeg")
            || resource.bytes > 2 * 1024 * 1024
        {
            return Err(
                "comment images must be cached PNG/JPEG at most 2 MiB in this account".into(),
            );
        }
        read_bounded(&path, 2 * 1024 * 1024)
    }

    fn prepare_bili_comment(
        &self,
        video: &VideoId,
        message: &str,
        request: &CommentRequest,
        pictures: Vec<platform_bilibili::UploadedCommentImage>,
    ) -> Result<platform_bilibili::PreparedComment> {
        let result = if request.root == 0 {
            self.bili
                .prepare_video_comment_with_pictures(video, message, pictures)
        } else {
            self.bili
                .prepare_video_reply(video, request.root, request.parent, message, pictures)
        };
        result.map_err(|e| e.to_string())
    }

    fn upload_comment_image(
        &mut self,
        record: &Operation,
        resource: &str,
    ) -> Result<platform_bilibili::UploadedCommentImage> {
        let expected: platform_bilibili::CommentTarget = target(&record.target)?;
        let id = format!("{}-image", record.id);
        let outbox = Outbox::open(&self.state)?;
        let upload_target = json!({"kind":"bilibili-comment-image","comment_operation":record.id,
            "image_resource":resource,"aid":expected.aid,"bvid":expected.bvid});
        let mut upload = if outbox.exists(&id)? {
            let existing = outbox.load(&id, &record.scope)?;
            if existing.target != upload_target || existing.actor_id != record.actor_id {
                return Err("image upload operation belongs to another request".into());
            }
            existing
        } else {
            let new = Operation::new(
                &id,
                record.scope.clone(),
                record.actor_id.clone(),
                upload_target,
                "comment-image-upload",
            )?;
            outbox.insert(&new)?;
            new
        };
        if !matches!(upload.status, Status::Sent | Status::Verified) {
            if upload.status != Status::Prepared {
                return Err(format!(
                    "Image upload already attempted ({:?}); no automatic reupload",
                    upload.status
                ));
            }
            let bytes = self.comment_image_bytes(resource)?;
            let video: VideoId = expected
                .bvid
                .parse()
                .map_err(|e: platform_bilibili::Error| e.to_string())?;
            let prepared = self
                .bili
                .prepare_video_image_upload(&video, &bytes)
                .map_err(|e| e.to_string())?;
            if !same_bili_target(prepared.target(), &record.target)? {
                return Err("image upload account or video target changed".into());
            }
            outbox.begin_attempt(&mut upload)?;
            match self.bili.submit_prepared_image_upload(prepared) {
                Ok(receipt) => {
                    upload.receipt = Some(value(&receipt)?);
                    upload.status = Status::Sent;
                }
                Err(error) => {
                    upload.status = bili_error_status(&error);
                    upload.error = Some(error.to_string());
                }
            }
            outbox.finish(&upload)?;
        }
        if upload.status != Status::Sent && upload.status != Status::Verified {
            return Err(format!(
                "Image upload {:?}: {}; comment was not submitted",
                upload.status,
                upload.error.as_deref().unwrap_or("unconfirmed result")
            ));
        }
        let receipt: platform_bilibili::ImageUploadReceipt = target(
            upload
                .receipt
                .as_ref()
                .ok_or("missing image upload receipt")?,
        )?;
        if receipt.actor_mid.to_string() != record.actor_id {
            return Err("image upload receipt author mismatch".into());
        }
        receipt.into_picture().map_err(|e| e.to_string())
    }

    pub(super) fn saved_accounts(&self) -> Result<Value> {
        Ok(
            json!({"bilibili":aliases(&self.state,"bilibili")?,"tieba":aliases(&self.state,"tieba")?,"qq":aliases(&self.state,"qq")?,"github":aliases(&self.state,"github")?,"x":aliases(&self.state,"x")?,"pixiv":aliases(&self.state,"pixiv")?,"reddit":aliases(&self.state,"reddit")?,"discord":aliases(&self.state,"discord")?}),
        )
    }

    pub(super) fn call_latest_comment(&mut self, name: &str, params: &Value) -> Result<Value> {
        let message = required_str(params, "message")?;
        if message.is_empty() || message.chars().count() > 1000 || message.contains('\0') {
            return Err("comment must contain 1..1000 characters".into());
        }
        let id = required_str(params, "operation_id")?;
        let request = CommentRequest::from_params(name, params)?;
        if request.image_resource.is_some() && id.len() > 90 {
            return Err("image comment operation_id must be at most 90 characters".into());
        }
        self.with_comment_account(name, params, |runtime| {
            let prepared = runtime.prepare_comment_request(name, id, message, &request)?;
            let sent = if prepared["status"] == "prepared" {
                runtime.submit_comment(name, id)?
            } else {
                prepared
            };
            let result = if sent["status"] == "sent"
                || sent["status"] == "attempting"
                || (name == "tieba" && sent["status"] == "unknown")
            {
                runtime.verify_comment(name, id)?
            } else {
                sent
            };
            Ok(compact(result))
        })
    }

    pub(super) fn with_comment_account(
        &mut self,
        name: &str,
        params: &Value,
        action: impl FnOnce(&mut Runtime) -> Result<Value>,
    ) -> Result<Value> {
        self.with_platform_account(name, params, true, action)
    }

    pub(super) fn with_platform_account(
        &mut self,
        name: &str,
        params: &Value,
        require_login: bool,
        action: impl FnOnce(&mut Runtime) -> Result<Value>,
    ) -> Result<Value> {
        let requested = match params.get("account") {
            None => None,
            Some(Value::String(v)) => Some(v.clone()),
            _ => return Err("account must be an alias string".into()),
        };
        let alias = if let Some(alias) = requested {
            alias
        } else if self.account != "public"
            && super::auth::saved_session(&self.state, name, &self.account)?.is_some()
        {
            self.account.clone()
        } else {
            let accounts = aliases(&self.state, name)?;
            if accounts.is_empty() && !require_login {
                "public".into()
            } else if accounts.len() != 1 {
                return Err(
                    "Choose an existing account alias; none or multiple are configured".into(),
                );
            } else {
                accounts[0].clone()
            }
        };
        let scope = Scope {
            platform: platform(name)?,
            account: alias.clone(),
        };
        scope.validate()?;
        if require_login && alias == "public" {
            return Err("this operation needs a saved personal account".into());
        }
        if alias != "public" && super::auth::saved_session(&self.state, name, &alias)?.is_none() {
            return Err("selected account has no saved session for this platform".into());
        }
        self.with_scope_context(&scope, action)
    }

    pub(super) fn with_scope_context<T>(
        &mut self,
        scope: &Scope,
        action: impl FnOnce(&mut Runtime) -> Result<T>,
    ) -> Result<T> {
        scope.validate()?;
        if !matches!(
            scope.platform,
            Platform::Local
                | Platform::Bilibili
                | Platform::Tieba
                | Platform::Qq
                | Platform::Github
                | Platform::X
                | Platform::Pixiv
                | Platform::Reddit
                | Platform::Discord
        ) {
            return Err("provider is unavailable or paused".into());
        }
        let alias = scope.account.clone();
        let mut bili_config = Config::default();
        bili_config.max_download_bytes = Budget::default().max_resource_bytes;
        let mut tieba_config = platform_tieba::Config::default();
        if scope.platform == Platform::Bilibili
            && let Some(path) = super::auth::saved_session(&self.state, "bilibili", &alias)?
        {
            let session = platform_bilibili::Session::load(&path).map_err(|e| e.to_string())?;
            super::auth::bind_identity(
                path.parent().ok_or("missing account directory")?,
                &session.stable_identity(),
            )?;
            bili_config.cookie = Some(session.cookie_header());
        }
        if scope.platform == Platform::Tieba
            && let Some(path) = super::auth::saved_session(&self.state, "tieba", &alias)?
        {
            let session = platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
            super::auth::bind_identity(
                path.parent().ok_or("missing account directory")?,
                &session.stable_identity(),
            )?;
            tieba_config.bduss = Some(session.bduss().to_owned());
        }
        let next_bili = Client::new(bili_config).map_err(|e| e.to_string())?;
        let old_account = std::mem::replace(&mut self.account, alias);
        let old_bili = std::mem::replace(&mut self.bili, next_bili);
        let old_tieba =
            std::mem::replace(&mut self.tieba, super::tieba::TiebaState::new(tieba_config));
        let guard = AccountGuard {
            runtime: self,
            account: Some(old_account),
            bili: Some(old_bili),
            tieba: Some(old_tieba),
        };
        action(guard.runtime)
    }

    fn comment_scope(&self, name: &str) -> Result<Scope> {
        if self.account == "public" {
            return Err("Comments require an explicit personal account alias".into());
        }
        Ok(self.scope(platform(name)?))
    }
    pub(super) fn tieba_comment_session(&self) -> Result<(PathBuf, platform_tieba::Session)> {
        let path = super::auth::saved_session(&self.state, "tieba", &self.account)?
            .ok_or("No saved Tieba login for this alias")?;
        let session = platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
        super::auth::bind_identity(
            path.parent().ok_or("account directory missing")?,
            &session.stable_identity(),
        )?;
        Ok((path, session))
    }

    pub fn prepare_latest_comment(&mut self, name: &str, id: &str, message: &str) -> Result<Value> {
        self.prepare_comment_request(name, id, message, &CommentRequest::default())
    }

    fn prepare_comment_request(
        &mut self,
        name: &str,
        id: &str,
        message: &str,
        request: &CommentRequest,
    ) -> Result<Value> {
        let scope = self.comment_scope(name)?;
        let outbox = Outbox::open(&self.state)?;
        if outbox.exists(id)? {
            let existing = outbox.load(id, &scope)?;
            if existing.message != message || existing.request != request.frozen()? {
                return Err(
                    "Operation ID already belongs to another message, image or reply target".into(),
                );
            }
            return value(&existing);
        }
        if let Some(id) = request.image_resource.as_deref() {
            self.comment_image_bytes(id)?;
        }
        let mut record = match scope.platform {
            Platform::Bilibili => {
                self.call_bilibili_account("bilibili.me")?;
                let list = self
                    .bili
                    .my_publications(1, 20)
                    .map_err(|e| e.to_string())?;
                let latest = list
                    .items
                    .iter()
                    .max_by_key(|item| item.created_at)
                    .ok_or("No own published videos")?;
                if latest.created_at == 0 {
                    return Err("Video creation time unavailable; latest target not guessed".into());
                }
                let video: VideoId = latest
                    .bvid
                    .parse()
                    .map_err(|e: platform_bilibili::Error| e.to_string())?;
                let prepared = self.prepare_bili_comment(&video, message, request, Vec::new())?;
                let target = prepared.target();
                Operation::new(
                    id,
                    scope,
                    target.actor_mid().to_string(),
                    value(target)?,
                    message,
                )?
            }
            Platform::Tieba => {
                let (path, mut session) = self.tieba_comment_session()?;
                let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
                let list = api
                    .self_publications(
                        &mut session,
                        platform_tieba::PublicationKind::Threads,
                        1,
                        20,
                    )
                    .map_err(|e| e.to_string())?;
                if list.items.iter().any(|item| item.created_at.is_none()) {
                    return Err("Topic creation time unavailable; latest target not guessed".into());
                }
                let latest = list
                    .items
                    .iter()
                    .filter(|item| item.content_type == "thread")
                    .max_by_key(|item| item.created_at)
                    .ok_or("No own topic threads")?;
                let prepared = api
                    .prepare_own_reply(
                        &mut session,
                        platform_tieba::ThreadId(latest.thread_id),
                        message,
                    )
                    .map_err(|e| e.to_string())?;
                session.save(&path).map_err(|e| e.to_string())?;
                let target = prepared.target();
                Operation::new(
                    id,
                    scope,
                    target.owner_uid.to_string(),
                    value(target)?,
                    message,
                )?
            }
            _ => unreachable!(),
        };
        record.request = request.frozen()?;
        outbox.insert(&record)?;
        value(&record)
    }

    pub fn comment_status(&self, name: &str, id: &str) -> Result<Value> {
        let scope = self.comment_scope(name)?;
        value(&Outbox::open(&self.state)?.load(id, &scope)?)
    }

    pub fn submit_comment(&mut self, name: &str, id: &str) -> Result<Value> {
        let scope = self.comment_scope(name)?;
        let outbox = Outbox::open(&self.state)?;
        let mut record = outbox.load(id, &scope)?;
        match record.status {
            Status::Sent|Status::Verified=>return value(&record),
            Status::Prepared=>{},
            _=>return Err("This operation was already attempted; inspect or verify it, never resend automatically".into()),
        }
        match scope.platform {
            Platform::Bilibili => {
                let expected: platform_bilibili::CommentTarget = target(&record.target)?;
                let request = CommentRequest::from_record(&record)?;
                let video: VideoId = expected
                    .bvid
                    .parse()
                    .map_err(|e: platform_bilibili::Error| e.to_string())?;
                let mut prepared =
                    self.prepare_bili_comment(&video, &record.message, &request, Vec::new())?;
                if !same_bili_target(prepared.target(), &record.target)?
                    || prepared.target().actor_mid().to_string() != record.actor_id
                {
                    return Err("Prepared video target or account changed; no message sent".into());
                }
                let dir = account_directory(&self.state, "bilibili", &self.account)?;
                super::auth::bind_identity(&dir, &record.actor_id)?;
                if let Some(resource) = request.image_resource.as_deref() {
                    let picture = self.upload_comment_image(&record, resource)?;
                    prepared = self.prepare_bili_comment(
                        &video,
                        &record.message,
                        &request,
                        vec![picture],
                    )?;
                    if !same_bili_target(prepared.target(), &record.target)? {
                        return Err("comment target changed after image upload".into());
                    }
                }
                record.attempt_context =
                    Some(json!({"root":prepared.root(),"parent":prepared.parent(),
                    "pictures":prepared.pictures()}));
                outbox.begin_attempt(&mut record)?;
                match self.bili.submit_prepared_comment(prepared) {
                    Ok(receipt) => {
                        record.status = Status::Sent;
                        record.receipt = Some(value(&receipt)?);
                    }
                    Err(error) => {
                        record.status = bili_error_status(&error);
                        record.error = Some(error.to_string());
                    }
                }
            }
            Platform::Tieba => {
                let expected: platform_tieba::ReplyTarget = target(&record.target)?;
                let (path, mut session) = self.tieba_comment_session()?;
                let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
                let prepared = api
                    .prepare_own_reply(
                        &mut session,
                        platform_tieba::ThreadId(expected.thread_id),
                        &record.message,
                    )
                    .map_err(|e| e.to_string())?;
                if value(prepared.target())? != record.target
                    || prepared.target().owner_uid.to_string() != record.actor_id
                {
                    return Err("Prepared topic target or account changed; no message sent".into());
                }
                record.attempt_context = Some(
                    json!({"before_post_ids":prepared.before_post_ids(),"prepared_at":prepared.prepared_at()}),
                );
                session.save(&path).map_err(|e| e.to_string())?;
                outbox.begin_attempt(&mut record)?;
                match api.send_prepared_reply(&mut session, prepared) {
                    Ok(receipt) => {
                        record.status = Status::Sent;
                        record.receipt = Some(value(&receipt)?);
                    }
                    Err(error) => {
                        record.status = match error {
                            platform_tieba::Error::Platform(_)
                            | platform_tieba::Error::Challenge
                            | platform_tieba::Error::LoginRequired => Status::Rejected,
                            _ => Status::Unknown,
                        };
                        record.error = Some(error.to_string());
                    }
                }
                // The durable attempt already forbids resending even if session persistence fails.
                let saved = session.save(&path).map_err(|e| e.to_string());
                outbox.finish(&record)?;
                saved?;
                return value(&record);
            }
            _ => unreachable!(),
        }
        outbox.finish(&record)?;
        value(&record)
    }

    pub fn verify_comment(&mut self, name: &str, id: &str) -> Result<Value> {
        let scope = self.comment_scope(name)?;
        let outbox = Outbox::open(&self.state)?;
        let mut record = outbox.load(id, &scope)?;
        if matches!(record.status, Status::Verified | Status::Rejected) {
            return value(&record);
        }
        if record.status == Status::Prepared {
            return Err("No send was attempted; nothing to verify".into());
        }
        if record.status == Status::Attempting {
            // Recovery after an interrupted process must never repeat its POST.
            record.status = Status::Unknown;
            record.error = Some("Previous send was interrupted; checking visibility only".into());
            outbox.finish(&record)?;
        }
        if scope.platform == Platform::Bilibili && record.status == Status::Unknown {
            return value(&record);
        }
        match scope.platform {
            Platform::Bilibili => {
                let receipt: platform_bilibili::CommentReceipt =
                    target(record.receipt.as_ref().ok_or("missing comment receipt")?)?;
                let request = CommentRequest::from_record(&record)?;
                let readback = if request.root == 0 {
                    self.bili.comment_by_id(receipt.oid, receipt.rpid)
                } else {
                    self.bili
                        .reply_by_id(receipt.oid, request.root, receipt.rpid)
                };
                match readback {
                    Ok(Some(comment))
                        if comment.message == record.message
                            && comment.author_mid.to_string() == record.actor_id
                            && comment.root == request.root
                            && comment.parent == request.parent
                            && comment_pictures_match(&record, &value(&comment)?) =>
                    {
                        record.verification = Some(value(&comment)?);
                        record.status = Status::Verified;
                        record.error = None;
                    }
                    Ok(_) => record.error = Some(
                        "Comment not readable with matching author and exact content; not resent"
                            .into(),
                    ),
                    Err(error) => record.error = Some(error.to_string()),
                }
            }
            Platform::Tieba => {
                let query = tieba_verification(&record)?;
                let without_receipt = record.receipt.is_none();
                let (path, mut session) = self.tieba_comment_session()?;
                let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
                match api.verify_reply(&mut session, &query) {
                    Ok(Some(post)) => {
                        let mut verified = value(&post)?;
                        verified["reconciled_without_receipt"] = json!(without_receipt);
                        record.verification = Some(verified);
                        record.status = Status::Verified;
                        record.error = None;
                    }
                    Ok(None) => {
                        record.error = Some(
                            "Reply not readable with matching author and exact content; not resent"
                                .into(),
                        )
                    }
                    Err(error) => record.error = Some(error.to_string()),
                }
                session.save(&path).map_err(|e| e.to_string())?;
            }
            _ => unreachable!(),
        }
        outbox.finish(&record)?;
        value(&record)
    }
}

fn emote_page_params(params: &Value) -> Result<(usize, usize)> {
    let offset = number(params, "offset", 0)?;
    let limit = number(params, "limit", 64)?;
    if offset > 4096 || limit == 0 || limit > 128 {
        return Err("emote page exceeds offset/limit bounds".into());
    }
    Ok((offset as usize, limit as usize))
}

fn emote_json_page(
    items: Vec<Value>,
    offset: usize,
    limit: usize,
) -> Result<(Vec<Value>, Option<usize>)> {
    let total = items.len();
    let mut page = Vec::new();
    let mut bytes = 0;
    for item in items.into_iter().skip(offset).take(limit) {
        let size = serde_json::to_vec(&item)
            .map_err(|_| "emote projection encoding")?
            .len();
        if bytes + size > 24 * 1024 {
            if page.is_empty() {
                return Err("single emote entry exceeds tool budget".into());
            }
            break;
        }
        bytes += size;
        page.push(item);
    }
    let end = offset + page.len();
    Ok((page, (end < total).then_some(end)))
}

fn bili_error_status(error: &platform_bilibili::Error) -> Status {
    match error {
        platform_bilibili::Error::Platform(_)
        | platform_bilibili::Error::RiskControl(_)
        | platform_bilibili::Error::LoginRequired => Status::Rejected,
        _ => Status::Unknown,
    }
}

fn comment_pictures_match(record: &Operation, actual: &Value) -> bool {
    let expected = record
        .attempt_context
        .as_ref()
        .and_then(|c| c.get("pictures"));
    let expected = expected
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let actual = actual
        .get("pictures")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if record
        .request
        .as_ref()
        .and_then(|r| r.get("image_resource"))
        .is_some_and(|v| !v.is_null())
        && expected.len() != 1
    {
        return false;
    }
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(a, b)| {
            ["img_src", "img_width", "img_height"]
                .iter()
                .all(|k| !a[*k].is_null() && a[*k] == b[*k])
        })
}

fn tieba_verification(record: &Operation) -> Result<platform_tieba::ReplyVerification> {
    let expected: platform_tieba::ReplyTarget = target(&record.target)?;
    if expected.owner_uid == 0 || expected.owner_uid.to_string() != record.actor_id {
        return Err("reply verification owner mismatch".into());
    }
    let context = record
        .attempt_context
        .as_ref()
        .ok_or("missing reply attempt context; no automatic resend")?;
    let before_post_ids: Vec<u64> = serde_json::from_value(context["before_post_ids"].clone())
        .map_err(|_| "invalid reply attempt baseline")?;
    if before_post_ids.len() > 1024 {
        return Err("reply attempt baseline limit".into());
    }
    let post_id = if let Some(raw) = record.receipt.as_ref() {
        let receipt: platform_tieba::ReplyReceipt = target(raw)?;
        if receipt.thread_id != expected.thread_id
            || receipt.owner_uid != expected.owner_uid
            || receipt.message != record.message
            || receipt.before_post_ids != before_post_ids
        {
            return Err("reply receipt differs from durable attempt".into());
        }
        receipt.post_id
    } else {
        None
    };
    Ok(platform_tieba::ReplyVerification {
        thread_id: expected.thread_id,
        post_id,
        owner_uid: expected.owner_uid,
        message: record.message.clone(),
        before_post_ids,
        attempted_at: record
            .attempt_started_at
            .ok_or("missing reply attempt time")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emote_pages_keep_continuations_at_the_last_returned_item() {
        let items = (0..4)
            .map(|id| json!({"id":id,"padding":"x".repeat(10_000)}))
            .collect::<Vec<_>>();
        let (first, next) = emote_json_page(items.clone(), 0, 4).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(next, Some(2));
        let (second, next) = emote_json_page(items, 2, 4).unwrap();
        assert_eq!(second[0]["id"], 2);
        assert_eq!(second.len(), 2);
        assert_eq!(next, None);
        assert!(emote_page_params(&json!({"limit":0})).is_err());
    }

    #[test]
    fn rich_request_coordinates_and_images_are_frozen_and_validated() {
        let request = CommentRequest::from_params(
            "bilibili",
            &json!({"root":8,"image_resource":"a".repeat(32)}),
        )
        .unwrap();
        assert_eq!(request.root, 8);
        assert_eq!(request.parent, 8);
        assert!(request.frozen().unwrap().is_some());
        assert_eq!(CommentRequest::default().frozen().unwrap(), None);
        for params in [
            json!({"parent":8}),
            json!({"root":8,"parent":0}),
            json!({"root":-1}),
            json!({"image_resource":"https://example.com/image.png"}),
        ] {
            assert!(CommentRequest::from_params("bilibili", &params).is_err());
        }
        assert!(CommentRequest::from_params("tieba", &json!({"root":8})).is_err());
    }

    #[test]
    fn picture_readback_needs_the_exact_uploaded_url_and_dimensions() {
        let mut record = Operation::new(
            "picture-check",
            Scope {
                platform: Platform::Bilibili,
                account: "test".into(),
            },
            "42".into(),
            json!({"aid":7}),
            "test",
        )
        .unwrap();
        let picture = json!({"img_src":"https://i0.hdslb.com/bfs/new/test.png","img_width":160,"img_height":160,"img_size":1.0});
        record.attempt_context = Some(json!({"pictures":[picture.clone()]}));
        let mut actual = json!({"pictures":[picture]});
        actual["pictures"][0]["img_size"] = json!(1.1);
        assert!(comment_pictures_match(&record, &actual));
        actual["pictures"][0]["img_width"] = json!(80);
        assert!(!comment_pictures_match(&record, &actual));
        assert!(!comment_pictures_match(&record, &json!({"pictures":[]})));
        actual["pictures"][0]["img_width"] = json!(160);
        actual["pictures"][0]["img_src"] = json!("https://i0.hdslb.com/bfs/old/unrelated.png");
        assert!(!comment_pictures_match(&record, &actual));
        record.request = Some(json!({"image_resource":"a".repeat(32)}));
        record.attempt_context = None;
        assert!(!comment_pictures_match(&record, &json!({"pictures":[]})));
    }

    #[test]
    fn unknown_reply_uses_durable_attempt_without_inventing_a_receipt() {
        let mut record = Operation::new(
            "lost-reply",
            Scope {
                platform: Platform::Tieba,
                account: "test".into(),
            },
            "42".into(),
            json!({"thread_id":7,"title":"own","forum_id":8,"forum_name":"test","owner_uid":42}),
            "晚安喵（test）",
        )
        .unwrap();
        record.status = Status::Unknown;
        record.attempt_started_at = Some(1000);
        record.attempt_context = Some(json!({"before_post_ids":[10,11],"prepared_at":998}));
        let query = tieba_verification(&record).unwrap();
        assert_eq!(query.attempted_at, 1000);
        assert_eq!(query.before_post_ids, vec![10, 11]);
        assert_eq!(query.post_id, None);
        assert!(record.receipt.is_none());
        record.receipt = Some(json!({"thread_id":7,"post_id":12,"owner_uid":42,
            "message":"a different reply","before_post_ids":[10,11],"accepted_at":1001,
            "echo_content":null}));
        assert!(tieba_verification(&record).is_err());
        record.receipt = None;
        record.attempt_context = None;
        assert!(tieba_verification(&record).is_err());
    }
}
