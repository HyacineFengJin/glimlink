// SPDX-License-Identifier: MIT
use platform_bilibili::{Client, Config, MediaResource, SelectedMedia, VideoId, VideoInfo};
use runtime_core::{
    Platform, Result, Scope,
    content::{ContentBlock, Document, ResourceRef},
    resources::{
        Budget, FileLock, ResourceBatch, ResourceStore, lock_private, open_regular, private_dir,
    },
};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

mod animations;
mod auth;
mod bili_features;
mod browsing;
mod discussion;
mod emotes;
mod engagement;
mod github;
mod media_read;
mod media_view;
mod speech;
mod x;
mod pixiv;
pub use speech::model_directory as speech_model_directory;
mod notification_open;
mod notifications;
mod operator;
mod publications;
mod qq;
mod qq_import;
mod sending;
mod task_sending;
mod tasks;
mod tieba;
mod tieba_features;
mod watching;
#[cfg(feature = "weixin-live")]
mod weixin;
#[cfg(not(feature = "weixin-live"))]
#[path = "app/weixin_paused.rs"]
mod weixin;

#[derive(Default)]
pub struct RuntimeOptions<'a> {
    pub bili_cookie_file: Option<&'a Path>,
    pub tieba_bduss_file: Option<&'a Path>,
}

pub struct Runtime {
    #[cfg(feature = "memory-bench")]
    benchmark_trace: Vec<Value>,
    #[cfg(feature = "memory-bench")]
    benchmark_review_context: bool,
    state: PathBuf,
    account: String,
    store: ResourceStore,
    bili: Client,
    tieba: tieba::TiebaState,
    qq: qq::QqState,
    x: Option<x::XState>,
    call_depth: u32,
    mind_context: runtime_jobs::AgentContext,
    mind_job: Option<String>,
    mind_state: PathBuf,
    locators: crate::private_state::PrivateLocators,
    _lock: FileLock,
}

pub fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string parameter {key}"))
}
fn number(params: &Value, key: &str, default: u64) -> Result<u64> {
    match params.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("invalid integer parameter {key}")),
    }
}
pub fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let file = open_regular(path, max_bytes)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max_bytes {
        return Err("input grew beyond byte limit".into());
    }
    Ok(bytes)
}

fn download_mime(path: &Path, reported: Option<&str>, group: &str) -> Result<String> {
    let mut file = open_regular(path, Budget::default().max_resource_bytes)?;
    let mut header = [0; 16];
    let n = file.read(&mut header).map_err(|e| e.to_string())?;
    let head = &header[..n];
    let known = if head.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(b"GIF8") {
        Some("image/gif")
    } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WAVE") {
        Some("audio/wav")
    } else if head.starts_with(b"#!AMR") {
        Some("audio/amr")
    } else if head.starts_with(b"#!SILK") || head.starts_with(b"\x02#!SILK") {
        Some("audio/x-silk")
    } else if head.get(4..8) == Some(b"ftyp") {
        Some(if group == "audio" {
            "audio/mp4"
        } else {
            "video/mp4"
        })
    } else {
        None
    };
    if let Some(mime) = known {
        return Ok(mime.into());
    }
    if let Some(mime) = reported.filter(|v| {
        v.starts_with(&format!("{group}/")) && v.len() <= 128 && !v.chars().any(char::is_control)
    }) {
        return Ok(mime.into());
    }
    if n == 0 {
        return Err("downloaded media is empty".into());
    }
    Ok(format!("{group}/unknown"))
}

impl Runtime {
    pub fn agent_context(&self) -> &runtime_jobs::AgentContext {
        &self.mind_context
    }
    pub fn set_agent_context(&mut self, context: runtime_jobs::AgentContext) -> Result<()> {
        context.validate()?;
        self.mind_context = context;
        Ok(())
    }
    /// Separate platform kernels may explicitly share only this mind store.
    pub fn set_mind_state(&mut self, state: &Path) -> Result<()> {
        if !state.is_absolute() {
            return Err("shared mind state must be an absolute path".into());
        }
        crate::mind_store::Store::open(state)?;
        self.mind_state = state.into();
        Ok(())
    }
    pub(crate) fn mind_state(&self) -> &Path {
        &self.mind_state
    }
    pub(crate) fn mind_job(&self) -> Option<&str> {
        self.mind_job.as_deref()
    }
    pub(crate) fn set_mind_job(&mut self, job: Option<String>) {
        self.mind_job = job;
    }
    pub(crate) fn state_dir(&self) -> &Path {
        &self.state
    }
    pub fn open(state: &Path, account: &str, cookie_file: Option<&Path>) -> Result<Self> {
        Self::open_with_options(
            state,
            account,
            RuntimeOptions {
                bili_cookie_file: cookie_file,
                tieba_bduss_file: None,
            },
        )
    }
    pub fn open_with_options(
        state: &Path,
        account: &str,
        options: RuntimeOptions<'_>,
    ) -> Result<Self> {
        private_dir(state)?;
        let state = state.canonicalize().map_err(|e| e.to_string())?;
        let lock = lock_private(&state.join("runtime.lock"))?;
        let work = state.join("work");
        if work.exists() {
            fs::remove_dir_all(&work).map_err(|e| e.to_string())?;
        }
        private_dir(&work)?;
        let image_work = state.join("agent-workspace/work");
        if image_work.exists() {
            fs::remove_dir_all(image_work).map_err(|e| e.to_string())?;
        }
        Scope {
            platform: Platform::Local,
            account: account.into(),
        }
        .validate()?;
        let mut config = Config::default();
        config.max_download_bytes = Budget::default().max_resource_bytes;
        if let Some(path) = options.bili_cookie_file {
            let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err("Cookie file must be a regular private file (0600)".into());
            }
            let value = String::from_utf8(read_bounded(path, 8192)?)
                .map_err(|_| "Cookie file must be UTF-8")?;
            config.cookie = Some(value.trim().to_owned());
            if account == "public" {
                return Err("an authenticated session needs an explicit account alias".into());
            }
        } else if let Some(path) = auth::saved_session(&state, "bilibili", account)? {
            let session = platform_bilibili::Session::load(&path).map_err(|e| e.to_string())?;
            auth::bind_identity(
                path.parent().ok_or("account directory missing")?,
                &session.stable_identity(),
            )?;
            config.cookie = Some(session.cookie_header());
        }
        let store = ResourceStore::open(&state.join("resources"), Budget::default())?;
        store.recover_orphans()?;
        let mut tieba_config = platform_tieba::Config::default();
        if let Some(path) = options.tieba_bduss_file {
            let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err("BDUSS file must be a regular private file (0600)".into());
            }
            if account == "public" {
                return Err("an authenticated session needs an explicit account alias".into());
            }
            tieba_config.bduss = Some(
                String::from_utf8(read_bounded(path, 4096)?)
                    .map_err(|_| "BDUSS file must be UTF-8")?
                    .trim()
                    .to_owned(),
            );
        } else if let Some(path) = auth::saved_session(&state, "tieba", account)? {
            let session = platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
            auth::bind_identity(
                path.parent().ok_or("account directory missing")?,
                &session.stable_identity(),
            )?;
            tieba_config.bduss = Some(session.bduss().to_owned());
        }
        let locators = crate::private_state::PrivateLocators::open(&state)?;
        Ok(Self {
            #[cfg(feature = "memory-bench")]
            benchmark_trace: Vec::new(),
            #[cfg(feature = "memory-bench")]
            benchmark_review_context: false,
            store,
            mind_state: state.clone(),
            state,
            account: account.into(),
            bili: Client::new(config).map_err(|e| e.to_string())?,
            tieba: tieba::TiebaState::new(tieba_config),
            qq: qq::QqState::default(),
            x: None,
            call_depth: 0,
            mind_context: runtime_jobs::AgentContext::default(),
            mind_job: None,
            locators,
            _lock: lock,
        })
    }

    pub fn scope(&self, platform: Platform) -> Scope {
        Scope {
            platform,
            account: self.account.clone(),
        }
    }

    fn resolve_any(&self, id: &str) -> Result<(Scope, ResourceRef, PathBuf)> {
        for platform in [
            Platform::Local,
            Platform::Bilibili,
            Platform::Tieba,
            Platform::Qq,
            Platform::Github,
            Platform::X,
            Platform::Pixiv,
            Platform::Weixin,
        ] {
            let scope = self.scope(platform);
            if let Ok((resource, path)) = self.store.resolve(&scope, id) {
                return Ok((scope, resource, path));
            }
        }
        Err("resource missing, expired or outside the active account".into())
    }

    pub fn import_file(&self, path: &Path, mime: &str) -> Result<ResourceRef> {
        self.store.maintain_cache()?;
        let mut file = open_regular(path, Budget::default().max_resource_bytes)?;
        self.store.import(
            &self.scope(Platform::Local),
            "local-import",
            mime,
            &mut file,
        )
    }

    fn import_output(
        &self,
        scope: &Scope,
        object_id: &str,
        path: &Path,
        mime: &str,
    ) -> Result<ResourceRef> {
        self.store.import(
            scope,
            object_id,
            mime,
            &mut File::open(path).map_err(|e| e.to_string())?,
        )
    }

    fn download(
        &self,
        batch: &mut ResourceBatch<'_>,
        resource: &MediaResource,
        object_id: &str,
        mime: &str,
    ) -> Result<ResourceRef> {
        batch.ingest(&self.scope(Platform::Bilibili), object_id, mime, |output| {
            self.bili
                .download_to_writer(resource, output, Budget::default().max_resource_bytes)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    fn normalize(&self, params: &Value, html: bool) -> Result<Value> {
        let (scope, source, _) = self.resolve_any(required_str(params, "resource")?)?;
        if source.bytes > runtime_content::MAX_INPUT as u64 {
            return Err("normalization input exceeds 1 MiB".into());
        }
        let data = self
            .store
            .read_range(&scope, &source.id, 0, runtime_content::MAX_INPUT)?;
        let mut blocks = vec![ContentBlock::Source {
            resource: source,
            label: "original source".into(),
        }];
        let mut notes = Vec::new();
        let mut batch = self.store.batch();
        if html {
            let text = std::str::from_utf8(&data).map_err(|_| "HTML input must be UTF-8")?;
            let projection = runtime_content::normalize_html(text)?;
            blocks.push(ContentBlock::Html {
                html: projection.html,
                truncated: projection.truncated,
            });
            for part in projection.sources {
                let mime = if part.kind == "script" {
                    "text/javascript"
                } else if part.kind == "style" {
                    "text/css"
                } else {
                    "text/plain"
                };
                let resource = batch.import(
                    &scope,
                    "extracted-source",
                    mime,
                    &mut part.content.as_bytes(),
                )?;
                blocks.push(ContentBlock::Source {
                    resource,
                    label: part.label,
                });
            }
            notes = projection.notes;
        } else {
            let projection = runtime_content::normalize_json(
                &data,
                params.get("pointer").and_then(Value::as_str).unwrap_or(""),
            )?;
            blocks.push(ContentBlock::Json { value: json!({"value":projection.value,"truncated":projection.truncated,"pointer":projection.pointer}) });
        }
        batch.commit();
        Ok(
            json!({"document":Document::new(scope, "normalized".into(), "Normalized content".into(), blocks),"notes":notes}),
        )
    }

    fn fetch_bili(&mut self, params: &Value) -> Result<Value> {
        let id: VideoId = required_str(params, "video")?
            .parse()
            .map_err(|e: platform_bilibili::Error| e.to_string())?;
        let info = self.bili.video_info(&id).map_err(|e| e.to_string())?;
        let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
        let quality = media_view::quality(params, platform_bilibili::VideoQuality::Preview)?;
        let playback = self
            .bili
            .playback_with_quality(&info, page, quality)
            .map_err(|e| e.to_string())?;
        let actual = match &playback.media {
            SelectedMedia::Dash { video, .. } => {
                json!({"quality":video.quality,"width":video.width,"height":video.height,"codec":video.codec})
            }
            _ => Value::Null,
        };
        let mut batch = self.store.batch();
        let mut blocks = vec![ContentBlock::Text {
            text: info.description.clone(),
        }];
        let (video, audio) = match &playback.media {
            SelectedMedia::Dash { video, audio } => {
                let video = self.download(
                    &mut batch,
                    &video.resource,
                    &format!("{}:{}:video", info.bvid, playback.cid),
                    "video/mp4",
                )?;
                let audio = self.download(
                    &mut batch,
                    &audio.resource,
                    &format!("{}:{}:audio", info.bvid, playback.cid),
                    "audio/mp4",
                )?;
                blocks.push(ContentBlock::Video {
                    resource: video.clone(),
                    duration_ms: Some(playback.duration_millis),
                    audio: Some(audio.clone()),
                });
                (Some(video), Some(audio))
            }
            SelectedMedia::Progressive { segments } => {
                let mut resources = Vec::new();
                for (index, segment) in segments.iter().enumerate() {
                    let resource = self.download(
                        &mut batch,
                        segment,
                        &format!("{}:{}:{index}", info.bvid, playback.cid),
                        "video/mp4",
                    )?;
                    blocks.push(ContentBlock::Video {
                        resource: resource.clone(),
                        duration_ms: None,
                        audio: None,
                    });
                    resources.push(resource);
                }
                // Do not concatenate MP4 files or pretend multiple fragments share one seek clock.
                if resources.len() == 1 {
                    (resources.pop(), None)
                } else {
                    (None, None)
                }
            }
        };
        let mut subtitle_status = json!({"tracks":0,"error":playback.subtitle_error});
        if let Some(index) = &playback.subtitles {
            subtitle_status =
                json!({"tracks":index.tracks.len(),"requires_login":index.requires_login});
        }
        let document = Document::new(
            self.scope(Platform::Bilibili),
            format!("{}:{}", info.bvid, playback.cid),
            info.title.clone(),
            blocks,
        );
        let manifest = json!({"schema":"agent-runtime.bilibili.v1","info":info,"page":page,
            "preview_only":playback.preview_only,"document":document,"video":video,"audio":audio,"subtitles":subtitle_status,
            "source_quality":{"requested":quality,"selected":actual,"available":playback.available_video},"model_observed":false});
        let resource = batch.import(
            &self.scope(Platform::Bilibili),
            "video-manifest",
            "application/json",
            &mut serde_json::to_vec(&manifest)
                .map_err(|e| e.to_string())?
                .as_slice(),
        )?;
        batch.commit();
        Ok(json!({"manifest":resource,"content":manifest}))
    }

    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value> {
        if !params.is_object() {
            return Err("tool params must be an object".into());
        }
        if self.call_depth >= 32 {
            return Err("nested Runtime task limit exceeded".into());
        }
        if self.call_depth == 0
            && !matches!(
                method,
                "runtime.accounts"
                    | "runtime.modules"
                    | "runtime.operation"
                    | "runtime.agent_status"
                    | "runtime.memory"
            )
        {
            self.store.maintain_cache()?;
        }
        self.call_depth += 1;
        let result = self.call_inner(method, params);
        self.call_depth -= 1;
        result
    }
    fn call_inner(&mut self, method: &str, params: &Value) -> Result<Value> {
        if !params.is_object() {
            return Err("tool params must be an object".into());
        }
        if matches!(method, "social.bilibili" | "bilibili.features") {
            return self.call_bilibili_features(params);
        }
        if method == "social.tieba" {
            return self.task_tieba_features(params);
        }
        if method.starts_with("github.") {
            return self.call_github_task(method, params);
        }
        if method.starts_with("x.") {
            return self.call_x(method, params);
        }
        if method.starts_with("pixiv.") {
            return self.call_pixiv(method, params);
        }
        if method.starts_with("social.")
            || matches!(method, "media.read" | "media.watch" | "runtime.expand")
        {
            return self.call_task(method, params).map(|mut value| {
                if value.get("status").is_none() {
                    value["status"] = json!("ok");
                }
                value
            });
        }
        match method {
            "runtime.memory" => {
                return crate::agent_mind::call_with_store(
                    &self.state,
                    &self.mind_state,
                    &self.mind_context,
                    self.mind_job.as_deref(),
                    params,
                );
            }
            "runtime.accounts" => return self.saved_accounts(),
            "runtime.agent_status" => {
                return match params.get("job_id") {
                    None => crate::agent_cli::summary(&self.state),
                    Some(value) => crate::agent_cli::job_status(
                        &self.state,
                        value.as_str().ok_or("invalid job_id")?,
                    ),
                };
            }
            "runtime.operation" => {
                let id = required_str(params, "operation_id")?;
                let record = crate::outbox::Outbox::open_existing(&self.state)?.inspect(id)?;
                tasks::check_account(params, &record.scope)?;
                return Ok(
                    json!({"status":"ok","operation":{"operation_id":record.id,"scope":record.scope,"status":record.status,"message":record.message,"created_at":record.created_at,"attempt_started_at":record.attempt_started_at,"receipt":record.receipt,"verification":record.verification,"error":record.error},"network_requests":0,"resubmitted":false,"in_flight_status_is_final":false}),
                );
            }
            "qq.status" => return self.qq_status(),
            "bilibili.emotes" => return self.call_comment_emotes(params),
            "bilibili.emote_packages" => return self.call_comment_emote_packages(params),
            "bilibili.comment_latest" => return self.call_latest_comment("bilibili", params),
            "bilibili.reply_latest" => {
                if number(params, "root", 0)? == 0 {
                    return Err("reply_latest requires a positive root comment ID".into());
                }
                return self.call_latest_comment("bilibili", params);
            }
            "tieba.reply_latest" => return self.call_latest_comment("tieba", params),
            "bilibili.like" | "bilibili.follow_uploader" | "bilibili.engagement" => {
                return self.call_bilibili_engagement(method, params);
            }
            _ => {}
        }
        if matches!(
            method,
            "bilibili.search"
                | "bilibili.uploader_content"
                | "bilibili.uploader_videos"
                | "bilibili.dynamics"
                | "bilibili.dynamic_media"
                | "bilibili.context"
                | "bilibili.popular"
                | "bilibili.recommended"
                | "bilibili.comments"
                | "bilibili.comment_replies"
                | "bilibili.comment_media"
                | "bilibili.subtitles"
        ) {
            return self.call_bilibili_browse(method, params);
        }
        if method == "media.gif" {
            return self.call_gif(params);
        }
        if matches!(method, "bilibili.publications" | "tieba.publications") {
            return self.call_publications(method, params);
        }
        if method == "bilibili.media" {
            return self.bilibili_asset(params);
        }
        if matches!(
            method,
            "bilibili.unread" | "bilibili.notifications" | "tieba.notifications"
        ) {
            return self.call_notifications(method, params);
        }
        if matches!(
            method,
            "bilibili.status" | "bilibili.me" | "bilibili.favorites"
        ) {
            return self.call_bilibili_account(method);
        }
        if matches!(method, "tieba.status" | "tieba.me") {
            return self.call_tieba_account(method);
        }
        if method.starts_with("tieba.") {
            return self.call_tieba(method, params);
        }
        if method.starts_with("weixin.") {
            return self.call_weixin(method, params);
        }
        if method == "asset.drop" {
            let kind = required_str(params, "kind")?;
            let platform = match kind {
                "bilibili-media" => Platform::Bilibili,
                "bilibili-comment-media" => Platform::Bilibili,
                "bilibili-dynamic-media" => Platform::Bilibili,
                "tieba-media" => Platform::Tieba,
                "weixin-media" | "weixin-payload" => Platform::Weixin,
                _ => return Err("unknown private asset kind".into()),
            };
            let asset = required_str(params, "asset")?;
            self.locators.remove(&self.scope(platform), kind, asset)?;
            return Ok(json!({"removed":asset}));
        }
        if method == "asset.list" {
            let offset =
                usize::try_from(number(params, "offset", 0)?).map_err(|_| "invalid offset")?;
            let limit =
                usize::try_from(number(params, "limit", 32)?).map_err(|_| "invalid limit")?;
            return Ok(
                json!({"bilibili":self.locators.list(&self.scope(Platform::Bilibili),offset,limit)?,"tieba":self.locators.list(&self.scope(Platform::Tieba),offset,limit)?,"weixin":self.locators.list(&self.scope(Platform::Weixin),offset,limit)?}),
            );
        }
        match method {
            "runtime.modules" => Ok(
                json!({"modules":crate::registry::modules(),"engines":[runtime_engine::codex_capabilities(),runtime_engine::Antigravity::capabilities()]}),
            ),
            "resource.read" => {
                let (scope, resource, _) = self.resolve_any(required_str(params, "resource")?)?;
                let encoding = params
                    .get("encoding")
                    .and_then(Value::as_str)
                    .unwrap_or("utf8");
                if !matches!(encoding, "utf8" | "base64") {
                    return Err("resource encoding must be utf8 or base64".into());
                }
                if encoding == "utf8"
                    && !(resource.mime.starts_with("text/") || resource.mime == "application/json")
                {
                    return Err(
                        "binary resource needs explicit encoding=base64 or a media tool".into(),
                    );
                }
                let offset = number(params, "offset", 0)?;
                let limit = usize::try_from(number(params, "limit", 16384)?)
                    .map_err(|_| "invalid range limit")?;
                let bytes = self.store.read_range(&scope, &resource.id, offset, limit)?;
                if encoding == "base64" {
                    use base64::Engine;
                    Ok(
                        json!({"resource":resource,"offset":offset,"next_offset":offset+bytes.len() as u64,"encoding":"base64","data":base64::engine::general_purpose::STANDARD.encode(&bytes),"truncated":(offset+bytes.len() as u64)<resource.bytes}),
                    )
                } else {
                    Ok(
                        json!({"resource":resource,"offset":offset,"next_offset":offset+bytes.len() as u64,"text":String::from_utf8_lossy(&bytes),"truncated":(offset+bytes.len() as u64)<resource.bytes}),
                    )
                }
            }
            "resource.list" => {
                let offset = usize::try_from(number(params, "offset", 0)?)
                    .map_err(|_| "invalid list offset")?;
                let limit = usize::try_from(number(params, "limit", 32)?)
                    .map_err(|_| "invalid list limit")?;
                Ok(
                    json!({"local":self.store.list(&self.scope(Platform::Local),offset,limit)?,"bilibili":self.store.list(&self.scope(Platform::Bilibili),offset,limit)?,"tieba":self.store.list(&self.scope(Platform::Tieba),offset,limit)?,"weixin":self.store.list(&self.scope(Platform::Weixin),offset,limit)?}),
                )
            }
            "resource.drop" => {
                let (scope, resource, _) = self.resolve_any(required_str(params, "resource")?)?;
                self.store.remove(&scope, &resource.id)?;
                Ok(json!({"removed":resource.id}))
            }
            "content.html" => self.normalize(params, true),
            "content.json" => self.normalize(params, false),
            "bilibili.inspect" => {
                let id: VideoId = required_str(params, "video")?
                    .parse()
                    .map_err(|e: platform_bilibili::Error| e.to_string())?;
                let info: VideoInfo = self.bili.video_info(&id).map_err(|e| e.to_string())?;
                Ok(json!({"info":info,"media_fetched":false,"model_observed":false}))
            }
            "bilibili.fetch" => self.fetch_bili(params),
            "media.image" => {
                let (scope, source, path) = self.resolve_any(required_str(params, "resource")?)?;
                let work = WorkDir::new(&self.state)?;
                let view = media_view::view(params)?;
                let output = work.path.join(match view.format {
                    runtime_media::pictures::OutputFormat::Jpeg => "image.jpg",
                    runtime_media::pictures::OutputFormat::Png => "image.png",
                });
                let edge = u32::try_from(number(params, "max_edge", 1280)?)
                    .map_err(|_| "invalid image size")?;
                let prepared = runtime_media::pictures::prepare_image_view(
                    &path,
                    &output,
                    edge,
                    view,
                    &runtime_media::Limits::default(),
                )
                .map_err(|e| e.to_string())?;
                let resource =
                    self.import_output(&scope, "prepared-image", &output, &prepared.mime)?;
                Ok(
                    json!({"source":source,"image":resource,"width":prepared.width,"height":prepared.height,"source_width":prepared.oriented_source_width,"source_height":prepared.oriented_source_height,"source_region":prepared.source_region,"upscaled":false,"model_observed":false}),
                )
            }
            "media.audio" => {
                let (scope, source, path) = self.resolve_any(required_str(params, "resource")?)?;
                let work = WorkDir::new(&self.state)?;
                let output = work.path.join("segment.wav");
                let segment = runtime_media::audio::segment_audio_wav(
                    &path,
                    &output,
                    number(params, "start_ms", 0)?,
                    number(params, "duration_ms", 10000)?,
                    &runtime_media::Limits::default(),
                )
                .map_err(|e| e.to_string())?;
                let resource = self.import_output(&scope, "audio-segment", &output, "audio/wav")?;
                Ok(
                    json!({"source":source,"audio":resource,"requested_start_ms":segment.requested_start_ms,
                    "requested_end_ms":segment.requested_end_ms,"actual_start_ms":segment.actual_start_ms,"actual_end_ms":segment.actual_end_ms,
                    "complete":segment.complete,"gaps":segment.gaps,"model_observed":false,"speech_recognition":false}),
                )
            }
            "media.video" => {
                let (scope, source, path) = self.resolve_any(required_str(params, "resource")?)?;
                let work = WorkDir::new(&self.state)?;
                let start = number(params, "start_ms", 0)?;
                let duration = number(params, "duration_ms", 10000)?;
                let interval = number(params, "interval_ms", 1000)?;
                let edge = u32::try_from(number(params, "max_edge", 640)?)
                    .map_err(|_| "invalid image size")?;
                let view = media_view::view(params)?;
                let window = runtime_media::video::sample_video_window_view(
                    &path,
                    &work.path.join("frames"),
                    start,
                    duration,
                    interval,
                    edge,
                    &view,
                    &if params.get("_single_frame_detail") == Some(&json!(true)) {
                        runtime_media::Limits::inspection()
                    } else {
                        runtime_media::Limits::default()
                    },
                )
                .map_err(|e| e.to_string())?;
                let mut batch = self.store.batch();
                let mut frames = Vec::new();
                for sample in &window.samples {
                    let resource = batch.import(
                        &scope,
                        "video-frame",
                        &sample.mime,
                        &mut File::open(&sample.path).map_err(|e| e.to_string())?,
                    )?;
                    frames.push(json!({"image":resource,"actual_pts_ms":sample.actual_pts_ms,"target_ms":sample.target_ms,"width":sample.width,"height":sample.height,"source_width":sample.source_width,"source_height":sample.source_height,"source_region":sample.source_region,"upscaled":false}));
                }
                let audio = if let Some(id) = params.get("audio_resource").and_then(Value::as_str) {
                    let (audio_scope, audio_source, audio_path) = self.resolve_any(id)?;
                    if audio_scope != scope {
                        return Err(
                            "video and audio must belong to the same account/platform".into()
                        );
                    }
                    let segment = runtime_media::audio::segment_audio_wav(
                        &audio_path,
                        &work.path.join("audio.wav"),
                        start,
                        duration,
                        &runtime_media::Limits::default(),
                    )
                    .map_err(|e| e.to_string())?;
                    let resource = batch.import(
                        &scope,
                        "video-audio-segment",
                        "audio/wav",
                        &mut File::open(&segment.path).map_err(|e| e.to_string())?,
                    )?;
                    json!({"resource":resource,"source":audio_source,"actual_start_ms":segment.actual_start_ms,"actual_end_ms":segment.actual_end_ms,"complete":segment.complete,"gaps":segment.gaps,"model_observed":false})
                } else {
                    json!({"available":false,"detail":"Provide audio_resource for the aligned audio track; visuals alone are not full video understanding"})
                };
                let result = json!({"source":source,"probe":window.probe,"requested_start_ms":window.requested_start_ms,
                    "requested_end_ms":window.requested_end_ms,"decoded_start_ms":window.decoded_start_ms,"decoded_end_ms":window.decoded_end_ms,
                    "frames_decoded":window.frames_decoded,"sampling_interval_ms":window.sampling_interval_ms,
                    "visual_coverage":window.visual_coverage,"frames":frames,"audio":audio,"gaps":window.gaps,"model_observed":false});
                let evidence = batch.import(
                    &scope,
                    "media-window-manifest",
                    "application/json",
                    &mut serde_json::to_vec(&result)
                        .map_err(|e| e.to_string())?
                        .as_slice(),
                )?;
                batch.commit();
                Ok(json!({"manifest":evidence,"window":result}))
            }
            _ => Err("unknown or unavailable tool".into()),
        }
    }

    pub fn image_path(&self, id: &str) -> Result<PathBuf> {
        let (_, resource, path) = self.resolve_any(id)?;
        if !resource.mime.starts_with("image/") {
            return Err("resource is not an image".into());
        }
        Ok(path)
    }

    /// The engine's transport identity is a host concern. New logical writes
    /// get a stable key without requiring the model to invent one. Explicit
    /// keys still allow the model to reconcile a prior logical operation.
    pub fn dispatch_engine_tool(
        &mut self,
        call: &runtime_engine::ToolCall,
    ) -> runtime_engine::ToolResult {
        let Some(method) = crate::tools::method(&call.tool) else {
            return runtime_engine::ToolResult::text("Unavailable task tool", false);
        };
        let mut params = call.arguments.clone();
        if crate::tools::is_write(&call.tool, &params)
            && params.is_object()
            && params.get("operation_id").is_none()
        {
            // Fixed FNV-1a domains keep IDs stable across processes and Rust
            // releases. These are idempotency identifiers, not access tokens.
            let mut a = 0xcbf29ce484222325_u64;
            let mut b = 0x84222325cbf29ce4_u64;
            for part in [&call.thread_id, &call.turn_id, &call.call_id] {
                for byte in (part.len() as u64)
                    .to_le_bytes()
                    .into_iter()
                    .chain(part.bytes())
                {
                    a = (a ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    b = (b ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
            params["operation_id"] = json!(format!("engine-{a:016x}{b:016x}"));
        }
        self.call_for_engine(&method, &params)
    }

    /// Convert tool resource references to actual image input, with an explicit attachment budget.
    pub fn call_for_engine(&mut self, method: &str, params: &Value) -> runtime_engine::ToolResult {
        let output = self.call_for_engine_inner(method, params);
        #[cfg(feature = "memory-bench")]
        if self.benchmark_trace.len() < 32 {
            use runtime_engine::ToolContent;
            let value = output
                .content_items
                .iter()
                .find_map(|item| match item {
                    ToolContent::InputText { text } => serde_json::from_str::<Value>(text).ok(),
                    _ => None,
                })
                .unwrap_or(Value::Null);
            let inferred = params
                .get("reference")
                .and_then(Value::as_str)
                .and_then(|reference| {
                    if reference.starts_with("https://tieba.baidu.com/") {
                        Some("tieba")
                    } else if [
                        "https://x.com/",
                        "https://www.x.com/",
                        "https://twitter.com/",
                        "https://www.twitter.com/",
                    ]
                    .iter()
                    .any(|prefix| reference.starts_with(prefix))
                    {
                        Some("x")
                    } else if reference.starts_with("https://github.com/") {
                        Some("github")
                    } else if reference.starts_with("https://www.bilibili.com/")
                        || reference.starts_with("BV")
                    {
                        Some("bilibili")
                    } else {
                        None
                    }
                })
                .or_else(|| method.starts_with("github.").then_some("github"));
            let inferred = inferred.map(|platform| json!(platform));
            let platform = value
                .pointer("/scope/platform")
                .or_else(|| value.pointer("/page/scope/platform"))
                .or_else(|| params.get("platform"))
                .or(inferred.as_ref());
            // Explicit opt-in for private comment-language review. Capture only
            // bounded normalized excerpts, never raw requests/headers/sessions.
            let review_context = if self.benchmark_review_context {
                let comment = |v: &Value| {
                    json!({"rpid":v.get("rpid"),"target_ref":v.get("target_ref"),
                    "message":v.get("message").and_then(Value::as_str).map(|s|crate::agent_mind::clip(s,512)),
                    "like_count":v.get("like_count"),"reply_count":v.get("reply_count"),"root":v.get("root"),"parent":v.get("parent")})
                };
                match method {
                    "social.notifications" => json!({"kind":params.get("kind"),
                        "items":value.get("items").and_then(Value::as_array).map(|xs|xs.iter().take(12).map(|v|json!({
                            "id":v.get("id"),"created_at":v.get("created_at"),"notification_ref":v.get("notification_ref"),
                            "source_content":v.pointer("/target/source_content").and_then(Value::as_str).map(|s|crate::agent_mind::clip(s,512)),
                            "target_reply_content":v.pointer("/target/target_reply_content").and_then(Value::as_str).map(|s|crate::agent_mind::clip(s,512)),
                            "message":v.pointer("/target/message").and_then(Value::as_str).map(|s|crate::agent_mind::clip(s,512))
                        })).collect::<Vec<_>>())}),
                    "social.discussion" => {
                        json!({"target_ref":params.get("target_ref"),"order":params.get("order"),
                        "root_comment":value.get("root_comment").or_else(||value.pointer("/context/root_comment")).map(&comment),
                        "items":value.get("items").and_then(Value::as_array).map(|xs|xs.iter().take(12).map(|v|{
                            let mut c=comment(v);c["replies"]=json!(v.get("replies").and_then(Value::as_array).map(|rs|rs.iter().take(2).map(&comment).collect::<Vec<_>>()));c
                        }).collect::<Vec<_>>())})
                    }
                    "social.open" => {
                        json!({"reference":params.get("reference"),"target_ref":value.get("target_ref"),
                        "notification_ref":value.get("notification_ref"),"context_ref":value.get("context_ref"),"exact_target_found":value.get("exact_target_found"),
                        "root_comment":value.pointer("/content/root_comment").map(&comment),"selected_comment":value.pointer("/content/selected_comment").map(&comment),
                        "replies":value.pointer("/content/replies").and_then(Value::as_array).map(|xs|xs.iter().take(12).map(&comment).collect::<Vec<_>>()),
                        "title":value.pointer("/content/info/title").or_else(||value.pointer("/content/video/title")),
                        "description":value.pointer("/content/info/desc").and_then(Value::as_str).map(|s|crate::agent_mind::clip(s,1024))})
                    }
                    "social.send" => json!({"target_ref":params.get("target_ref"),
                        "message":params.get("message").and_then(Value::as_str).map(|s|crate::agent_mind::clip(s,960)),"operation_id":params.get("operation_id")}),
                    _ => Value::Null,
                }
            } else {
                Value::Null
            };
            self.benchmark_trace.push(json!({"method":method,"success":output.success,"status":value.get("status"),
                "review_context":review_context,
                "memory_action":if method=="runtime.memory" {params.get("action")}else{None},
                "platform":platform,"items":value.get("items").and_then(Value::as_array).map(Vec::len),
                "target_ref_returned":value.get("target_ref").or_else(||value.pointer("/content/target_ref")).is_some(),
                "kind":value.get("kind"),"content_observed":value.get("content_observed"),
                "media_kind":value.get("media_kind"),"prepared":value.get("prepared"),
                "sampled_frames":value.pointer("/window/frames").and_then(Value::as_array).map(Vec::len),
                "speech_available":value.pointer("/speech/available"),"speech_chars":value.pointer("/speech/text").and_then(Value::as_str).map(str::len),
                "feed_source":value.pointer("/context/source"),
                "candidate_ids":value.get("items").and_then(Value::as_array).map(|items|items.iter().filter_map(|item|item.get("bvid").and_then(Value::as_str)).take(64).collect::<Vec<_>>()),
                "video_id":value.pointer("/video/bvid").or_else(||value.pointer("/content/info/bvid")).or_else(||value.pointer("/info/bvid")),
                "requested_start_ms":params.get("start_ms"),"requested_duration_ms":params.get("duration_ms"),
                "action":params.get("action"),"caller_quality":params.get("quality"),"region":params.get("region"),
                "inspection":value.get("inspection"),"input_frame_ref":params.get("frame_ref").or_else(||value.get("inspection").and_then(|_|params.get("reference"))),
                "download_performed":value.pointer("/download/performed"),
                "returned_frames":value.pointer("/window/frames").and_then(Value::as_array).map(|frames|frames.iter().take(4).map(|f|json!({"frame_ref":f.get("frame_ref"),"actual_pts_ms":f.get("actual_pts_ms"),"image":f.get("image")})).collect::<Vec<_>>()),
                "requested_quality":value.pointer("/source_quality/requested"),"source_dimensions":value.pointer("/source_quality/actual"),
                "quality_met":value.pointer("/source_quality/request_met"),"region_requested":params.get("region").is_some(),
                "sent_images":value.pointer("/engine_input/attachment_sources"),
                "attached_images":output.content_items.iter().filter(|item|matches!(item,ToolContent::InputImage{..})).count()}));
        }
        output
    }

    /// Metadata by default. Explicit review mode adds bounded normalized social
    /// excerpts; credentials/raw responses are never included.
    #[cfg(feature = "memory-bench")]
    pub fn take_benchmark_trace(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.benchmark_trace)
    }
    #[cfg(feature = "memory-bench")]
    pub fn enable_benchmark_review_context(&mut self) {
        self.benchmark_review_context = true;
    }

    fn call_for_engine_inner(
        &mut self,
        method: &str,
        params: &Value,
    ) -> runtime_engine::ToolResult {
        use base64::Engine;
        use runtime_engine::{ToolContent, ToolResult};
        let mut value = match self.call(method, params) {
            Ok(value) => value,
            Err(error) => {
                return ToolResult::text(
                    json!({"status":"failed","error":error}).to_string(),
                    false,
                );
            }
        };
        // Save large successful results so contact lists and protobuf expansions
        // do not abort the entire model turn at the engine's wire limit.
        let serialized = serde_json::to_vec(&value).unwrap_or_default();
        if serialized.len() > 48 * 1024 {
            let platform = match method.split('.').next() {
                Some("bilibili") => Platform::Bilibili,
                Some("tieba") => Platform::Tieba,
                Some("weixin") => Platform::Weixin,
                _ => Platform::Local,
            };
            let scope = value
                .get("scope")
                .or_else(|| value.pointer("/resolved_source/scope"))
                .or_else(|| value.pointer("/page/scope"))
                .and_then(|v| serde_json::from_value::<Scope>(v.clone()).ok())
                .unwrap_or_else(|| self.scope(platform));
            return match self.store.import(
                &scope,
                "large-tool-result",
                "application/json",
                &mut serialized.as_slice(),
            ) {
                Ok(resource) => {
                    let mut summary = json!({"status":value.get("status").unwrap_or(&json!("ok")),"scope":scope,"request_succeeded":true,"content_resource":resource,"expand_tool":"runtime_expand","detail":"Result saved because it exceeds the engine text budget. Expand this reference using a smaller range or JSON pointer; do not repeat the social action to recover output."});
                    for field in [
                        "target_ref",
                        "creator_ref",
                        "continuation",
                        "stop_reason",
                        "new_item_count",
                        "operation_id",
                        "actions",
                    ] {
                        if let Some(part) = value.get(field).filter(|v| v.to_string().len() <= 4096)
                        {
                            summary[field] = part.clone();
                        }
                    }
                    ToolResult::text(summary.to_string(), true)
                }
                Err(_) => ToolResult::text(
                    "Result exceeds engine budget and cache is full; request a smaller range or page.",
                    false,
                ),
            };
        }
        let images: Vec<String> = if method == "media.image"
            || (method == "media.read" && value.get("image").is_some())
        {
            value
                .pointer("/image/id")
                .and_then(Value::as_str)
                .map(|v| vec![v.to_owned()])
                .unwrap_or_default()
        } else if matches!(
            method,
            "media.video" | "media.gif" | "media.read" | "media.watch"
        ) {
            value
                .pointer("/window/frames")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| {
                    v.pointer("/image/id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut attachments = Vec::new();
        let mut attached_ids = Vec::new();
        let mut attachment_errors = Vec::new();
        let preserve_detail = value.get("inspection").is_some()
            || params.get("region").is_some()
            || params
                .get("quality")
                .and_then(Value::as_str)
                .is_some_and(|q| q != "preview")
            || params.get("format").and_then(Value::as_str) == Some("png")
            || params
                .get("max_edge")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 480);
        let mut remaining_bytes = if preserve_detail {
            512 * 1024
        } else {
            192 * 1024
        };
        for id in &images {
            if attachments.len() >= 4 {
                break;
            }
            let prepared = (|| -> Result<(Scope, Vec<u8>, u32, u32, bool)> {
                let scope = self.store.scope_for(id)?;
                let (resource, path) = self.store.resolve(&scope, id)?;
                if !resource.mime.starts_with("image/") {
                    return Err("attachment is not an image".into());
                }
                let limits = runtime_media::Limits::default();
                let original = runtime_media::pictures::probe_image(&path, &limits)
                    .map_err(|e| e.to_string())?;
                let budget = if preserve_detail {
                    remaining_bytes
                } else {
                    (64 * 1024).min(remaining_bytes)
                };
                match read_bounded(&path,budget as u64){
                    Ok(data)=>return Ok((scope,data,original.width,original.height,false)),
                    Err(_) if preserve_detail=>return Err("Detail image exceeds the bounded attachment allowance; choose a smaller source region, fewer frames, or explicitly format=jpeg. No detail downscaling was performed.".into()),
                    Err(_)=>(),
                }
                if budget >= 4096 {
                    let work = WorkDir::new(&self.state)?;
                    for edge in [384, 256, 192, 128, 64] {
                        let output = work.path.join(format!("attachment-{edge}.jpg"));
                        if let Ok(image) = runtime_media::pictures::prepare_image(
                            &path,
                            &output,
                            edge,
                            runtime_media::pictures::OutputFormat::Jpeg,
                            &limits,
                        ) {
                            if let Ok(data) = read_bounded(&output, budget as u64) {
                                return Ok((scope, data, image.width, image.height, true));
                            }
                        }
                    }
                }
                Err("Image cannot fit the preview attachment allowance".into())
            })();
            match prepared {
                Ok((scope, data, width, height, resized)) => {
                    let mime = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
                        "image/png"
                    } else {
                        "image/jpeg"
                    };
                    remaining_bytes -= data.len();
                    attached_ids.push(json!({"resource":id,"scope":scope,"width":width,"height":height,"mime":mime,"bytes":data.len(),"resized_for_wire":resized,"detail_preserved":preserve_detail}));
                    attachments.push(ToolContent::InputImage {
                        image_url: format!(
                            "data:{mime};base64,{}",
                            base64::engine::general_purpose::STANDARD.encode(data)
                        ),
                    });
                }
                Err(error) => attachment_errors.push(
                    json!({"resource":id,"error":error.chars().take(512).collect::<String>()}),
                ),
            }
        }
        if !attachment_errors.is_empty() {
            value["status"] = json!(if attachments.is_empty() {
                "input_unavailable"
            } else {
                "partial"
            });
        }
        value["engine_input"] = json!({"image_resources_attached":attached_ids.iter().map(|v|v["resource"].clone()).collect::<Vec<_>>(),"attachment_sources":attached_ids,"prepared_image_count":images.len(),
            "attachment_errors":attachment_errors,"omitted_by_count_budget":images.len().saturating_sub(4),
            "detail_preserved":preserve_detail,"note":"Only listed images entered the model. attachment_sources reports actual sent dimensions/MIME/bytes. Detail reads are never silently resized; on an allowance error choose a smaller region or explicitly JPEG. Compact previews may be downsized with resized_for_wire=true. Audio is not accepted by this image route."});
        let mut content_items = vec![ToolContent::InputText {
            text: value.to_string(),
        }];
        content_items.extend(attachments);
        let success = !matches!(
            value.get("status").and_then(Value::as_str),
            Some(
                "unsupported"
                    | "failed"
                    | "needs_auth"
                    | "permission_denied"
                    | "input_unavailable"
                    | "rate_limited"
            )
        );
        ToolResult {
            content_items,
            success,
        }
    }
}

pub struct WorkDir {
    pub path: PathBuf,
}
impl WorkDir {
    pub fn new(state: &Path) -> Result<Self> {
        let parent = state.join("work");
        private_dir(&parent)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = parent.join(format!("{}-{nonce}", std::process::id()));
        fs::DirBuilder::new()
            .create(&path)
            .map_err(|e| e.to_string())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(Self { path })
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn save_private_new(path: &Path, value: &Value) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
