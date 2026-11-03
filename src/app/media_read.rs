// SPDX-License-Identifier: MIT
//! One media-reading action: resolve the active account's reference, fetch only
//! when needed, inspect the actual bytes, and choose the bounded decoder.
use super::*;

struct MediaInput {
    scope: Scope,
    resource: ResourceRef,
    path: PathBuf,
    download: Value,
}

#[derive(Clone, Copy)]
enum MediaKind {
    Image,
    Gif,
    Video,
    Audio,
}
struct Detection {
    kind: MediaKind,
    mime: &'static str,
    embedded_audio: bool,
    animated_static_preview: bool,
    duration_ms: Option<u64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WatchCache {
    schema: u32,
    bvid: String,
    aid: u64,
    page: u32,
    video: ResourceRef,
    audio: Option<ResourceRef>,
    manifest: ResourceRef,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    quality: platform_bilibili::VideoQuality,
}
const WATCH_CACHE: &str = "task-video-cache";
const MEDIA_CACHE: &str = "task-media-cache";
const CACHE_TTL_SECONDS: u64 = 300;

#[derive(serde::Serialize, serde::Deserialize)]
struct MediaCache {
    schema: u32,
    asset: String,
    kind: String,
    resource: ResourceRef,
    created_at: u64,
}
fn cache_time() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| "system clock is before epoch".into())
}
fn cache_fresh(created_at: u64, now: u64) -> bool {
    now.checked_sub(created_at)
        .is_some_and(|age| age < CACHE_TTL_SECONDS)
}

fn reference(value: &str) -> Result<()> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(
            "media reference must be an existing opaque resource/asset ID, not a URL or path"
                .into(),
        );
    }
    Ok(())
}

fn detect_media(path: &Path) -> Result<Detection> {
    detect_media_with_limits(path, &runtime_media::Limits::default())
}
fn detect_media_with_limits(path: &Path, limits: &runtime_media::Limits) -> Result<Detection> {
    let mut file = open_regular(path, Budget::default().max_resource_bytes)?;
    let mut magic = [0; 32];
    let length = file.read(&mut magic).map_err(|e| e.to_string())?;
    let magic = &magic[..length];
    let image = |mime, animated_static_preview| Detection {
        kind: MediaKind::Image,
        mime,
        embedded_audio: false,
        animated_static_preview,
        duration_ms: None,
    };
    let audio = |mime| Detection {
        kind: MediaKind::Audio,
        mime,
        embedded_audio: false,
        animated_static_preview: false,
        duration_ms: None,
    };
    if magic.starts_with(b"GIF87a") || magic.starts_with(b"GIF89a") {
        return Ok(Detection {
            kind: MediaKind::Gif,
            mime: "image/gif",
            embedded_audio: false,
            animated_static_preview: false,
            duration_ms: None,
        });
    }
    if magic.starts_with(b"\xff\xd8\xff") {
        return Ok(image("image/jpeg", false));
    }
    if magic.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(image("image/png", false));
    }
    if magic.starts_with(b"RIFF") && magic.get(8..12) == Some(b"WEBP") {
        let animated =
            magic.get(12..16) == Some(b"VP8X") && magic.get(20).is_some_and(|flags| flags & 2 != 0);
        return Ok(image("image/webp", animated));
    }
    if magic.starts_with(b"RIFF") && magic.get(8..12) == Some(b"WAVE") {
        return Ok(audio("audio/wav"));
    }
    if magic.starts_with(b"OggS") {
        return Ok(audio("audio/ogg"));
    }
    if magic.starts_with(b"ID3") {
        return Ok(audio("audio/mpeg"));
    }
    if magic.starts_with(b"#!AMR") {
        return Ok(audio("audio/amr"));
    }
    if magic.starts_with(b"#!SILK") || magic.starts_with(b"\x02#!SILK") {
        return Ok(audio("audio/silk"));
    }
    if magic
        .get(4..8)
        .is_some_and(|v| v == b"ftyp" || v == b"styp")
    {
        if magic.get(8..12).is_some_and(|brand| {
            [
                b"avif".as_slice(),
                b"avis",
                b"heic",
                b"heix",
                b"mif1",
                b"msf1",
            ]
            .contains(&brand)
        }) {
            return Err("AVIF/HEIF image decoding is unavailable".into());
        }
        let probe = runtime_media::video::probe_video(path, limits).map_err(|e| e.to_string())?;
        let embedded_audio = probe.tracks.iter().any(|track| track.audio);
        if probe.tracks.iter().any(|track| {
            !track.audio
                && track.width.is_some_and(|value| value > 0)
                && track.height.is_some_and(|value| value > 0)
        }) {
            return Ok(Detection {
                kind: MediaKind::Video,
                mime: "video/mp4",
                embedded_audio,
                animated_static_preview: false,
                duration_ms: Some(probe.duration_ms),
            });
        }
        if embedded_audio {
            return Ok(audio("audio/mp4"));
        }
        return Err("MP4 has no supported audio or video track".into());
    }
    if magic.len() >= 2 && magic[0] == 0xff && magic[1] & 0xe0 == 0xe0 {
        return Ok(audio(if magic[1] & 0xf6 == 0xf0 {
            "audio/aac"
        } else {
            "audio/mpeg"
        }));
    }
    Err("media signature is unsupported; no content was claimed as observed".into())
}

impl Runtime {
    /// Materialize an opaque attachment without resizing, sampling or model work.
    pub(super) fn materialize_send_media(
        &mut self,
        id: &str,
    ) -> Result<(Scope, ResourceRef, PathBuf)> {
        let scope = self.media_reference_scope(id)?;
        self.with_task_scope(&scope, |rt| {
            let input = rt.resolve_media_input(id)?;
            Ok((input.scope, input.resource, input.path))
        })
    }
    /// Resolve an attachment without changing its content. This is deliberately
    /// separate from read_media: viewing may resize/sample; sending must not.
    pub(super) fn resolve_send_image(&mut self, id: &str) -> Result<String> {
        let input = self.resolve_media_input(id)?;
        if input.scope.account != self.account
            || !matches!(
                input.scope.platform,
                Platform::Local | Platform::Bilibili | Platform::Tieba
            )
        {
            return Err("send image belongs to another account or an unavailable provider".into());
        }
        if input.resource.bytes > 2 * 1024 * 1024 {
            return Err("send image must be at most 2 MiB".into());
        }
        let actual = detect_media(&input.path)?;
        if !matches!(actual.kind, MediaKind::Image)
            || !matches!(actual.mime, "image/png" | "image/jpeg")
        {
            return Err(
                "send image supports original PNG/JPEG only; GIF or other media was not converted"
                    .into(),
            );
        }
        runtime_media::pictures::probe_image(&input.path, &runtime_media::Limits::default())
            .map_err(|e| e.to_string())?;
        if input.resource.mime == actual.mime {
            return Ok(input.resource.id);
        }
        // Correct cache metadata with a byte-for-byte copy. The source remains
        // available and neither resizing nor transcoding is performed.
        let corrected = self.store.import(
            &input.scope,
            "send-image-original",
            actual.mime,
            &mut File::open(&input.path).map_err(|e| e.to_string())?,
        )?;
        Ok(corrected.id)
    }

    /// Public task target references are resolved before any media is fetched;
    /// raw video IDs remain an internal convenience for existing entry points.
    pub(super) fn watch_video_window(&mut self, params: &Value) -> Result<Value> {
        if let Some(id) = params.get("target_ref") {
            if params.get("video").is_some() {
                return Err("provide target_ref or video, not both".into());
            }
            let id = id.as_str().ok_or("target_ref must be a reference string")?;
            let (scope, target) = self.load_task_target(id)?;
            if scope.platform != Platform::Bilibili {
                return Err("media.watch currently supports Bilibili video targets".into());
            }
            if let Some(account) = params.get("account") {
                if account.as_str() != Some(scope.account.as_str()) {
                    return Err("target_ref and requested account differ".into());
                }
            }
            let video = match target {
                super::tasks::TaskTarget::Video { video }
                | super::tasks::TaskTarget::BiliComment { video, .. } => video,
                _ => return Err("target_ref does not identify a video".into()),
            };
            let mut resolved = params.clone();
            if let Some(hint) = self.target_playback_hint(id)? {
                if resolved.get("page").is_none() {
                    resolved["page"] = json!(hint.page);
                }
                if resolved.get("start_ms").is_none() {
                    resolved["start_ms"] = json!(hint.start_ms);
                }
            }
            resolved
                .as_object_mut()
                .ok_or("watch parameters must be an object")?
                .remove("target_ref");
            resolved["video"] = json!(video);
            let mut result =
                self.with_task_scope(&scope, |runtime| runtime.watch_video_current(&resolved))?;
            result["target_ref"] = json!(id);
            return Ok(result);
        }
        self.watch_video_current(params)
    }

    fn watch_video_current(&mut self, params: &Value) -> Result<Value> {
        let quality = super::media_view::quality(params, platform_bilibili::VideoQuality::Preview)?;
        super::media_view::view(params)?;
        let video: VideoId = required_str(params, "video")?
            .parse()
            .map_err(|e: platform_bilibili::Error| e.to_string())?;
        let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid video page")?;
        let start = number(params, "start_ms", 0)?;
        let duration = number(params, "duration_ms", 10_000)?;
        let edge = number(params, "max_edge", u64::from(quality.default_edge()))?;
        let interval = number(params, "interval_ms", duration.div_ceil(4).max(1))?;
        let limits = runtime_media::Limits::default();
        limits
            .check_window(start, duration)
            .map_err(|e| e.to_string())?;
        if page == 0
            || page > 2048
            || edge == 0
            || edge > u64::from(limits.max_dimension)
            || interval == 0
            || duration.div_ceil(interval) > limits.max_frames as u64
        {
            return Err("watch page/window/sampling/resize exceeds supported bounds".into());
        }
        if params.get("manifest").is_some()
            || params.get("audio_resource").is_some()
            || params.get("resource").is_some()
        {
            return Err("media.watch manages media resources internally; provide only video target and time window".into());
        }
        let scope = self.scope(Platform::Bilibili);
        let cached = self.cached_watch_video_quality(&scope, &video, page, quality)?;
        let reused = cached.is_some();
        let cache = if let Some(cache) = cached {
            cache
        } else {
            let fetched =
                self.fetch_bili(&json!({"video":video.to_string(),"page":page,"quality":quality}))?;
            let content = &fetched["content"];
            let video:ResourceRef=serde_json::from_value(content["video"].clone())
                .map_err(|_|"video consists of multiple progressive files; combined timeline playback is not implemented")?;
            let audio = if content["audio"].is_null() {
                None
            } else {
                Some(
                    serde_json::from_value(content["audio"].clone())
                        .map_err(|_| "invalid fetched audio reference")?,
                )
            };
            let manifest: ResourceRef = serde_json::from_value(fetched["manifest"].clone())
                .map_err(|_| "missing fetched video manifest")?;
            WatchCache {
                schema: 1,
                bvid: content["info"]["bvid"]
                    .as_str()
                    .ok_or("missing fetched BV identity")?
                    .into(),
                aid: content["info"]["aid"]
                    .as_u64()
                    .ok_or("missing fetched AV identity")?,
                page,
                video,
                audio,
                manifest,
                created_at: cache_time()?,
                quality,
            }
        };
        let matches_requested = match &video {
            VideoId::Bv(id) => cache.bvid == *id,
            VideoId::Av(id) => cache.aid == *id,
        };
        if !matches_requested {
            return Err("fetched video identity differs from requested target".into());
        }
        let manifest = self.watch_manifest(&scope, &cache)?;
        let mut cache_note = None;
        if !reused {
            if let Err(error) = self.locators.put(&scope, WATCH_CACHE, &cache) {
                cache_note = Some(error);
            }
        }
        let preferences = super::watching::media_preferences(
            params,
            &crate::watch_sessions::MediaPreferences::default(),
        )?;
        let mut read = json!({"reference":cache.video.id,"start_ms":start,"duration_ms":duration,
            "interval_ms":interval,"max_edge":edge,"transcribe":preferences.transcribe,"include_audio":params.get("include_audio").and_then(Value::as_bool).unwrap_or(true)});
        for key in ["region", "format"] {
            if let Some(v) = params.get(key) {
                read[key] = v.clone();
            }
        }
        if params.get("_single_frame_detail") == Some(&json!(true)) {
            read["_single_frame_detail"] = json!(true);
        }
        if let Some(language) = &preferences.speech_language {
            read["speech_language"] = json!(language);
        }
        if let Some(audio) = &cache.audio {
            read["audio_resource"] = json!(audio.id);
        }
        let mut result = self.read_media(&read)?;
        if result["media_kind"] != "video" {
            return Err("fetched video did not contain a supported video track".into());
        }
        result["video"] = json!({"aid":cache.aid,"bvid":cache.bvid,"page":cache.page,
            "title":manifest["info"]["title"],"duration_seconds":manifest["info"]["duration_seconds"],
            "preview_only":manifest["preview_only"]});
        result["scope"] = json!(scope);
        result["source_quality"] = manifest["source_quality"].clone();
        if result["source_quality"].is_null() {
            result["source_quality"] = json!({"requested":quality});
        }
        if let Some(track) = result
            .pointer("/window/probe/tracks")
            .and_then(Value::as_array)
            .and_then(|tracks| tracks.iter().find(|t| t["audio"] == false))
        {
            let w = track["width"].as_u64().unwrap_or(0);
            let h = track["height"].as_u64().unwrap_or(0);
            let actual = json!({"width":w,"height":h,"codec":track["codec"]});
            result["source_quality"]["actual"] = actual;
            result["source_quality"]["request_met"] =
                json!(w > 0 && h > 0 && w.min(h) >= u64::from(quality.height()));
        }
        result["detail_note"] = json!(
            "max_edge limits output only. region is cropped from source pixels before resizing; no upscaling. Check source_quality.actual/request_met; an unavailable source quality is never replaced by an enlarged preview."
        );
        self.link_watch_frames(&mut result, &scope, &cache.bvid, page)?;
        result["download"] = json!({"performed":!reused,"source":"bilibili_video",
            "method":"bilibili.fetch","reused_cached_media":reused,"cached_at":cache.created_at,"cache_ttl_seconds":CACHE_TTL_SECONDS});
        result["media_manifest"] = json!(cache.manifest);
        result["subtitle_status"] = manifest["subtitles"].clone();
        result["full_video_watched"] = json!(false);
        self.attach_watch_subtitles(
            &mut result,
            params,
            &scope,
            &video.to_string(),
            page,
            start,
            duration,
        )?;
        result["watch_note"] = json!(
            "This is sampled evidence for the requested window, not a claim to have watched every event or understood audio."
        );
        if let Some(error) = cache_note {
            result["cache_note"] = json!(error);
        }
        Ok(result)
    }

    #[cfg(test)]
    fn cached_watch_video(
        &self,
        scope: &Scope,
        requested: &VideoId,
        page: u32,
    ) -> Result<Option<WatchCache>> {
        self.cached_watch_video_quality(
            scope,
            requested,
            page,
            platform_bilibili::VideoQuality::Preview,
        )
    }
    fn cached_watch_video_quality(
        &self,
        scope: &Scope,
        requested: &VideoId,
        page: u32,
        quality: platform_bilibili::VideoQuality,
    ) -> Result<Option<WatchCache>> {
        let now = cache_time()?;
        let mut stale: Vec<String> = Vec::new();
        for offset in (0..2048).step_by(128) {
            let records = self.locators.list(scope, offset, 128)?;
            let done = records.len() < 128;
            for record in records {
                if record["kind"] != WATCH_CACHE {
                    continue;
                }
                let Some(id) = record["asset"].as_str() else {
                    continue;
                };
                let Ok(cache) = self.locators.get::<WatchCache>(scope, WATCH_CACHE, id) else {
                    continue;
                };
                let same_video = match requested {
                    VideoId::Bv(id) => cache.bvid == *id,
                    VideoId::Av(id) => cache.aid == *id,
                };
                if cache.schema != 1
                    || cache.page != page
                    || !same_video
                    || cache.quality != quality
                {
                    continue;
                }
                let available = cache_fresh(cache.created_at, now)
                    && self.store.resolve(scope, &cache.video.id).is_ok()
                    && self.store.resolve(scope, &cache.manifest.id).is_ok()
                    && cache
                        .audio
                        .as_ref()
                        .is_none_or(|audio| self.store.resolve(scope, &audio.id).is_ok());
                if available && self.watch_manifest(scope, &cache).is_ok() {
                    for id in stale {
                        let _ = self.locators.remove(scope, WATCH_CACHE, &id);
                    }
                    return Ok(Some(cache));
                }
                stale.push(id.to_owned());
            }
            if done {
                break;
            }
        }
        for id in stale {
            let _ = self.locators.remove(scope, WATCH_CACHE, &id);
        }
        Ok(None)
    }

    fn cached_media_asset(
        &self,
        scope: &Scope,
        asset: &str,
        kind: &str,
    ) -> Result<Option<(ResourceRef, PathBuf)>> {
        let now = cache_time()?;
        let mut stale: Vec<String> = Vec::new();
        for offset in (0..2048).step_by(128) {
            let records = self.locators.list(scope, offset, 128)?;
            let done = records.len() < 128;
            for record in records {
                if record["kind"] != MEDIA_CACHE {
                    continue;
                }
                let Some(id) = record["asset"].as_str() else {
                    continue;
                };
                let Ok(cache) = self.locators.get::<MediaCache>(scope, MEDIA_CACHE, id) else {
                    continue;
                };
                if cache.schema != 1 || cache.asset != asset || cache.kind != kind {
                    continue;
                }
                if cache_fresh(cache.created_at, now) {
                    if let Ok((resource, path)) = self.store.resolve(scope, &cache.resource.id) {
                        if resource == cache.resource {
                            for id in stale {
                                let _ = self.locators.remove(scope, MEDIA_CACHE, &id);
                            }
                            return Ok(Some((resource, path)));
                        }
                    }
                }
                stale.push(id.to_owned());
            }
            if done {
                break;
            }
        }
        for id in stale {
            let _ = self.locators.remove(scope, MEDIA_CACHE, &id);
        }
        Ok(None)
    }

    fn watch_manifest(&self, scope: &Scope, cache: &WatchCache) -> Result<Value> {
        let (resource, path) = self.store.resolve(scope, &cache.manifest.id)?;
        if resource.mime != "application/json" {
            return Err("watch manifest has an invalid type".into());
        }
        let manifest: Value = serde_json::from_slice(&read_bounded(&path, 2 * 1024 * 1024)?)
            .map_err(|_| "invalid watch manifest")?;
        let audio_matches = match &cache.audio {
            Some(audio) => manifest["audio"]["id"].as_str() == Some(audio.id.as_str()),
            None => manifest["audio"].is_null(),
        };
        if manifest["schema"] != "agent-runtime.bilibili.v1"
            || manifest["info"]["bvid"] != cache.bvid
            || manifest["info"]["aid"] != cache.aid
            || manifest["page"] != cache.page
            || manifest["video"]["id"].as_str() != Some(cache.video.id.as_str())
            || !audio_matches
        {
            return Err("watch cache manifest and target identity differ".into());
        }
        Ok(manifest)
    }

    fn resolve_media_input(&mut self, id: &str) -> Result<MediaInput> {
        reference(id)?;
        if let Ok((scope, resource, path)) = self.resolve_any(id) {
            return Ok(MediaInput {
                scope,
                resource,
                path,
                download: json!({"performed":false,"source":"cached_resource"}),
            });
        }
        if let Ok(scope) = self.locators.scope_for("task-qq-media", id) {
            if scope != self.scope(Platform::Qq) {
                return Err("QQ media belongs to another account".into());
            }
            if let Some((resource, path)) = self.cached_media_asset(&scope, id, "task-qq-media")? {
                return Ok(MediaInput {
                    scope,
                    resource,
                    path,
                    download: json!({"performed":false,"source":"cached_platform_asset","provider":"qq"}),
                });
            }
            let fetched = self.qq_media_download(&json!({"reference":id}))?;
            let resource_id = fetched
                .pointer("/resource/id")
                .and_then(Value::as_str)
                .ok_or("QQ media download returned no resource")?;
            let (actual, resource, path) = self.resolve_any(resource_id)?;
            if actual != scope {
                return Err("QQ media scope differs from locator".into());
            }
            let cached = self
                .locators
                .put(
                    &scope,
                    MEDIA_CACHE,
                    &MediaCache {
                        schema: 1,
                        asset: id.into(),
                        kind: "task-qq-media".into(),
                        resource: resource.clone(),
                        created_at: cache_time()?,
                    },
                )
                .is_ok();
            return Ok(MediaInput {
                scope,
                resource,
                path,
                download: json!({"performed":true,"provider":"qq","cache_stored":cached}),
            });
        }
        for (platform, kind, method) in [
            (Platform::X, "task-x-media", "x.media"),
            (Platform::Pixiv, "task-pixiv-media", "pixiv.media"),
            (Platform::Reddit,"task-reddit-media","reddit.media"),
            (Platform::Discord,"task-discord-media","discord.media"),
            (Platform::Bilibili, "bilibili-media", "bilibili.media"),
            (
                Platform::Bilibili,
                "bilibili-comment-media",
                "bilibili.comment_media",
            ),
            (
                Platform::Bilibili,
                "bilibili-dynamic-media",
                "bilibili.dynamic_media",
            ),
            (Platform::Tieba, "tieba-media", "tieba.media"),
        ] {
            let expected = self.scope(platform.clone());
            // Only this account's allowed locator kinds are considered. Private
            // URLs remain internal and are never copied into the tool result.
            if self.locators.get::<Value>(&expected, kind, id).is_err() {
                continue;
            }
            if let Some((resource, path)) = self.cached_media_asset(&expected, id, kind)? {
                return Ok(MediaInput {
                    scope: expected,
                    resource,
                    path,
                    download: json!({"performed":false,"source":"cached_platform_asset","provider":platform,
                        "method":method,"cache_ttl_seconds":CACHE_TTL_SECONDS}),
                });
            }
            let fetched = self.with_task_scope(&expected, |runtime| {
                runtime.call(method, &json!({"asset":id}))
            })?;
            let resource_id = fetched
                .pointer("/resource/id")
                .and_then(Value::as_str)
                .ok_or("media download did not return a cached resource")?;
            let (scope, resource, path) = self.resolve_any(resource_id)?;
            if scope != expected {
                return Err("media download returned a different account/platform scope".into());
            }
            let cache = MediaCache {
                schema: 1,
                asset: id.into(),
                kind: kind.into(),
                resource: resource.clone(),
                created_at: cache_time()?,
            };
            let stored = self.locators.put(&scope, MEDIA_CACHE, &cache).is_ok();
            return Ok(MediaInput {
                scope,
                resource,
                path,
                download: json!({"performed":true,"source":"platform_asset","provider":platform,"method":method,
                    "cache_stored":stored,"cache_ttl_seconds":CACHE_TTL_SECONDS}),
            });
        }
        Err("media reference is missing, outside the active account, or belongs to an unsupported/paused provider".into())
    }

    pub(super) fn read_media(&mut self, params: &Value) -> Result<Value> {
        let requested = required_str(params, "reference")?;
        reference(requested)?;
        super::media_view::view(params)?;
        if self
            .locators
            .scope_for(super::media_view::FRAME_ORIGIN, requested)
            .is_ok()
        {
            let mut request = params.clone();
            request.as_object_mut().unwrap().remove("reference");
            request["action"] = json!("inspect");
            request["frame_ref"] = json!(requested);
            return self.inspect_video(&request);
        }
        let start = number(params, "start_ms", 0)?;
        let duration = match params.get("duration_ms") {
            None => None,
            Some(_) => Some(number(params, "duration_ms", 0)?),
        };
        let interval = match params.get("interval_ms") {
            None => None,
            Some(_) => Some(number(params, "interval_ms", 0)?),
        };
        let quality = super::media_view::quality(params, platform_bilibili::VideoQuality::Preview)?;
        let edge = u32::try_from(number(
            params,
            "max_edge",
            u64::from(quality.default_edge()),
        )?)
        .map_err(|_| "invalid media resize edge")?;
        let limits = if params.get("_single_frame_detail") == Some(&json!(true)) {
            runtime_media::Limits::inspection()
        } else {
            runtime_media::Limits::default()
        };
        if edge == 0
            || edge > limits.max_dimension
            || interval.is_some_and(|n| n == 0 || n > limits.max_window_ms)
        {
            return Err("media resize edge or sampling interval exceeds supported bounds".into());
        }
        limits
            .check_window(start, duration.unwrap_or(10_000))
            .map_err(|e| e.to_string())?;
        let audio_reference = match params.get("audio_resource") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .ok_or("audio_resource must be an opaque reference")?,
            ),
        };
        if let Some(id) = audio_reference {
            reference(id)?;
        }
        let input = self.resolve_media_input(requested)?;
        let detection = detect_media_with_limits(&input.path, &limits)?;
        if matches!(detection.kind, MediaKind::Gif | MediaKind::Audio)
            && (params.get("region").is_some() || params.get("format").is_some())
        {
            return Err(
                "region/format rereading currently supports static images and video frames".into(),
            );
        }
        let (method, kind, default_duration) = match detection.kind {
            MediaKind::Image => ("media.image", "image", 0),
            MediaKind::Gif => ("media.gif", "gif", 3000),
            MediaKind::Video => ("media.video", "video", 10_000),
            MediaKind::Audio => ("media.audio", "audio", 10_000),
        };
        let mut request = json!({"resource":input.resource.id,"max_edge":edge});
        if params.get("_single_frame_detail") == Some(&json!(true)) {
            request["_single_frame_detail"] = json!(true);
        }
        for key in ["region", "format"] {
            if let Some(v) = params.get(key) {
                request[key] = v.clone();
            }
        }
        let mut audio_download = None;
        if default_duration > 0 {
            let duration = duration.unwrap_or(default_duration);
            request["start_ms"] = json!(start);
            request["duration_ms"] = json!(duration);
            // Four samples fit the existing engine's image-count budget by
            // default. Callers can explicitly request finer temporal evidence.
            request["interval_ms"] = json!(interval.unwrap_or(duration.div_ceil(4).max(1)));
        }
        if matches!(detection.kind, MediaKind::Video) {
            if params.get("include_audio") == Some(&json!(false)) {
                // Single-frame detail reads do not allocate/convert an audio window.
            } else if let Some(id) = audio_reference {
                let audio = self.resolve_media_input(id)?;
                if audio.scope != input.scope {
                    return Err(
                        "audio and video references must belong to the same account/platform"
                            .into(),
                    );
                }
                request["audio_resource"] = json!(audio.resource.id);
                audio_download = Some(audio.download);
            } else if detection.embedded_audio {
                // A muxed video already contains its own aligned audio track.
                request["audio_resource"] = json!(input.resource.id);
            }
        } else if audio_reference.is_some() {
            return Err("audio_resource is only applicable to a video reference".into());
        }
        let mut result = if detection
            .duration_ms
            .is_some_and(|duration| duration > 0 && start >= duration)
            && matches!(detection.kind, MediaKind::Video)
        {
            json!({"status":"end_of_media","window":{"frames":[],"probe":{"duration_ms":detection.duration_ms},"requested_start_ms":start,
                "requested_end_ms":start.checked_add(duration.unwrap_or(default_duration)).ok_or("media window overflow")?,"end_of_stream":true,"visual_coverage":"no_samples_after_end"}})
        } else {
            self.call(method, &request)?
        };
        self.add_speech_to_media(&mut result, params)?;
        let temporal = matches!(detection.kind, MediaKind::Gif | MediaKind::Video);
        let frame_count = result
            .pointer("/window/frames")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let at_end = result.pointer("/window/end_of_animation") == Some(&json!(true))
            || result
                .pointer("/window/probe/duration_ms")
                .and_then(Value::as_u64)
                .is_some_and(|duration| start >= duration);
        if matches!(detection.kind, MediaKind::Image) && params.get("quality").is_some() {
            let width = result["source_width"].as_u64().unwrap_or(0);
            let height = result["source_height"].as_u64().unwrap_or(0);
            result["source_quality"] = json!({"requested":quality,"actual":{"width":width,"height":height},
                "request_met":width>0 && height>0 && width.min(height)>=u64::from(quality.height()),
                "selection":"existing_image_pixels","stream_reacquired":false,
                "note":"This reference is an existing image. quality sets output bounds, but cannot add source pixels. To reread a video at higher source quality, pass its returned frame_ref to media_read, or media_watch action=inspect. A larger max_edge is not an HD stream upgrade."});
        }
        let object = result
            .as_object_mut()
            .ok_or("media processor result must be an object")?;
        object.insert("scope".into(), json!(input.scope.clone()));
        object.insert("reference".into(), json!(requested));
        object.insert(
            "resolved_source".into(),
            json!({"scope":input.scope,"resource":input.resource,"detected_mime":detection.mime}),
        );
        object.insert("download".into(), input.download);
        if let Some(download) = audio_download {
            object.insert("audio_download".into(), download);
        }
        object.insert("media_kind".into(), json!(kind));
        object.insert("processing_method".into(), json!(method));
        object.insert("prepared".into(), json!(!temporal || frame_count > 0));
        object.insert(
            "input_available".into(),
            json!(!temporal || frame_count > 0),
        );
        if temporal && frame_count == 0 {
            object.insert(
                "status".into(),
                json!(if at_end {
                    "end_of_media"
                } else {
                    "input_unavailable"
                }),
            );
            object.insert("reason".into(),json!(if at_end{"Requested window starts after the available timeline; no visual input was prepared."}else{"The requested window produced no visual input."}));
        }
        object.insert("model_observed".into(), json!(false));
        object.insert("audio_understanding".into(), json!(false));
        let recognized = object
            .get("speech")
            .and_then(|s| s.get("available"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        object.insert("speech_recognition".into(), json!(recognized));
        object.insert("audio_note".into(),json!("Prepared audio is transcribed locally when the optional Rust Whisper model is installed. Speech text does not establish understanding of music or other sounds."));
        if detection.animated_static_preview {
            object.insert("static_preview_only".into(), json!(true));
            object.insert("animation_note".into(),json!("Animated WebP detected; only a static preview is prepared because temporal WebP decoding is unavailable."));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    fn lab() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "media-read-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        root
    }
    #[test]
    fn cached_png_is_selected_by_signature_and_keeps_engine_image_structure() {
        let root = lab();
        let file = root.join("image.bin");
        image::DynamicImage::new_rgb8(8, 8)
            .save_with_format(&file, image::ImageFormat::Png)
            .unwrap();
        let mut runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let resource = runtime
            .import_file(&file, "application/octet-stream")
            .unwrap();
        let result = runtime
            .read_media(&json!({"reference":resource.id}))
            .unwrap();
        assert_eq!(result["media_kind"], "image");
        assert_eq!(result["resolved_source"]["detected_mime"], "image/png");
        assert_eq!(result["download"]["performed"], false);
        assert!(result["image"]["id"].as_str().is_some());
        assert_eq!(result["model_observed"], false);
        assert!(
            runtime
                .read_media(&json!({"reference":"https://example.com/a.png"}))
                .is_err()
        );
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cached_gif_automatically_returns_temporal_frames_and_rejects_other_accounts() {
        let root = lab();
        let file = root.join("animation.bin");
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&file).unwrap());
            for colour in [[255, 0, 0, 255], [0, 255, 0, 255]] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(8, 8, image::Rgba(colour)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let state = root.join("state");
        let mut runtime = Runtime::open(&state, "a", None).unwrap();
        let resource = runtime.import_file(&file, "image/png").unwrap();
        let result = runtime
            .read_media(&json!({"reference":resource.id,"duration_ms":200,"interval_ms":100}))
            .unwrap();
        assert_eq!(result["media_kind"], "gif");
        assert_eq!(result["window"]["frames"].as_array().unwrap().len(), 2);
        assert_eq!(result["download"]["performed"], false);
        assert_eq!(result["model_observed"], false);
        drop(runtime);
        let mut other = Runtime::open(&state, "b", None).unwrap();
        assert!(other.read_media(&json!({"reference":resource.id})).is_err());
        drop(other);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cached_audio_is_prepared_without_claiming_audio_understanding() {
        let root = lab();
        let file = root.join("sound.bin");
        let frames = 160_u32;
        let bytes = frames * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&16000_u32.to_le_bytes());
        wav.extend_from_slice(&32000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&bytes.to_le_bytes());
        wav.resize(44 + bytes as usize, 0);
        std::fs::write(&file, wav).unwrap();
        let mut runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let resource = runtime.import_file(&file, "video/mp4").unwrap();
        let result = runtime
            .read_media(&json!({"reference":resource.id,"duration_ms":10}))
            .unwrap();
        assert_eq!(result["media_kind"], "audio");
        assert!(result["audio"]["id"].as_str().is_some());
        assert_eq!(result["audio_understanding"], false);
        assert_eq!(result["speech_recognition"], false);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn watch_cache_binds_identity_page_and_scope_and_drops_expired_media() {
        let root = lab();
        let runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let scope = runtime.scope(Platform::Bilibili);
        let video = runtime
            .store
            .import(
                &scope,
                "test-video",
                "video/mp4",
                &mut b"cached opaque bytes".as_slice(),
            )
            .unwrap();
        let data = json!({"schema":"agent-runtime.bilibili.v1","info":{"aid":170001,"bvid":"BV17x411w7KC"},
            "page":1,"video":video,"audio":null});
        let manifest = runtime
            .store
            .import(
                &scope,
                "test-manifest",
                "application/json",
                &mut serde_json::to_vec(&data).unwrap().as_slice(),
            )
            .unwrap();
        let mut cache = WatchCache {
            schema: 1,
            bvid: "BV17x411w7KC".into(),
            aid: 170001,
            page: 1,
            video: video.clone(),
            audio: None,
            manifest,
            created_at: cache_time().unwrap(),
            quality: platform_bilibili::VideoQuality::Preview,
        };
        let locator = runtime.locators.put(&scope, WATCH_CACHE, &cache).unwrap();
        let requested: VideoId = "BV17x411w7KC".parse().unwrap();
        assert!(
            runtime
                .cached_watch_video_quality(
                    &scope,
                    &requested,
                    1,
                    platform_bilibili::VideoQuality::P1080
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            runtime
                .cached_watch_video(&scope, &requested, 1)
                .unwrap()
                .unwrap()
                .video
                .id,
            video.id
        );
        assert!(
            runtime
                .cached_watch_video(&scope, &requested, 2)
                .unwrap()
                .is_none()
        );
        assert!(
            runtime
                .cached_watch_video(
                    &Scope {
                        platform: Platform::Bilibili,
                        account: "b".into()
                    },
                    &requested,
                    1
                )
                .unwrap()
                .is_none()
        );
        runtime
            .locators
            .remove(&scope, WATCH_CACHE, &locator)
            .unwrap();
        cache.created_at = cache_time().unwrap().saturating_sub(CACHE_TTL_SECONDS + 1);
        let stale = runtime.locators.put(&scope, WATCH_CACHE, &cache).unwrap();
        assert!(
            runtime
                .cached_watch_video(&scope, &requested, 1)
                .unwrap()
                .is_none()
        );
        assert!(
            runtime
                .locators
                .get::<Value>(&scope, WATCH_CACHE, &stale)
                .is_err()
        );
        cache.created_at = cache_time().unwrap();
        let locator = runtime.locators.put(&scope, WATCH_CACHE, &cache).unwrap();
        runtime.store.remove(&scope, &video.id).unwrap();
        assert!(
            runtime
                .cached_watch_video(&scope, &requested, 1)
                .unwrap()
                .is_none()
        );
        assert!(
            runtime
                .locators
                .get::<Value>(&scope, WATCH_CACHE, &locator)
                .is_err()
        );
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn watch_target_validation_and_failed_window_restore_the_calling_account() {
        let root = lab();
        let state = root.join("state");
        let mut runtime = Runtime::open(&state, "a", None).unwrap();
        let target = runtime
            .save_task_target(&super::super::tasks::TaskTarget::Video {
                video: "BV17x411w7KC".into(),
            })
            .unwrap();
        let creator = runtime
            .save_task_target(&super::super::tasks::TaskTarget::Creator { mid: 42 })
            .unwrap();
        assert!(
            runtime
                .watch_video(&json!({"target_ref":target,"account":"b"}))
                .is_err()
        );
        assert!(runtime.watch_video(&json!({"target_ref":creator})).is_err());
        drop(runtime);
        let mut runtime = Runtime::open(&state, "public", None).unwrap();
        // Invalid windows fail before fetching; the context guard restores public.
        assert!(
            runtime
                .watch_video(&json!({"target_ref":target,"duration_ms":0}))
                .is_err()
        );
        assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
        assert!(
            runtime
                .watch_video(&json!({"video":"https://example.com/x","duration_ms":10}))
                .is_err()
        );
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn platform_asset_cache_reuses_only_live_scoped_entries_inside_ttl() {
        let root = lab();
        let file = root.join("source.png");
        image::DynamicImage::new_rgb8(8, 8)
            .save_with_format(&file, image::ImageFormat::Png)
            .unwrap();
        let mut runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let scope = runtime.scope(Platform::Bilibili);
        let resource = runtime
            .store
            .import(
                &scope,
                "test-png",
                "image/png",
                &mut File::open(&file).unwrap(),
            )
            .unwrap();
        let locator = platform_bilibili::MediaResource {
            url: "https://example.invalid/not-allowed.png".into(),
            backup_urls: vec![],
            expected_bytes: None,
            mime_type: Some("image/png".into()),
        };
        let asset = runtime
            .locators
            .put(&scope, "bilibili-comment-media", &locator)
            .unwrap();
        let mut cache = MediaCache {
            schema: 1,
            asset: asset.clone(),
            kind: "bilibili-comment-media".into(),
            resource: resource.clone(),
            created_at: cache_time().unwrap(),
        };
        let cached = runtime.locators.put(&scope, MEDIA_CACHE, &cache).unwrap();
        let result = runtime.read_media(&json!({"reference":asset})).unwrap();
        assert_eq!(result["media_kind"], "image");
        assert_eq!(result["download"]["performed"], false);
        assert_eq!(result["download"]["source"], "cached_platform_asset");
        runtime
            .locators
            .remove(&scope, MEDIA_CACHE, &cached)
            .unwrap();
        cache.created_at = cache_time().unwrap().saturating_sub(CACHE_TTL_SECONDS + 1);
        let expired = runtime.locators.put(&scope, MEDIA_CACHE, &cache).unwrap();
        // After expiry it attempts the normal provider path, whose URL policy
        // rejects this fixture before any network connection is possible.
        assert!(runtime.read_media(&json!({"reference":asset})).is_err());
        assert!(
            runtime
                .locators
                .get::<Value>(&scope, MEDIA_CACHE, &expired)
                .is_err()
        );
        assert!(!cache_fresh(
            cache_time().unwrap() + 60,
            cache_time().unwrap()
        ));
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn send_image_resolution_preserves_bytes_corrects_mime_and_rejects_animation() {
        let root = lab();
        let file = root.join("source.png");
        image::DynamicImage::new_rgb8(8, 8)
            .save_with_format(&file, image::ImageFormat::Png)
            .unwrap();
        let mut runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let original = runtime
            .import_file(&file, "application/octet-stream")
            .unwrap();
        let resolved = runtime.resolve_send_image(&original.id).unwrap();
        assert_ne!(resolved, original.id);
        let (scope, resource, path) = runtime.resolve_any(&resolved).unwrap();
        assert_eq!(scope.account, "a");
        assert_eq!(resource.mime, "image/png");
        assert_eq!(
            read_bounded(&path, 2 * 1024 * 1024).unwrap(),
            std::fs::read(&file).unwrap()
        );
        let (_, old, _) = runtime.resolve_any(&original.id).unwrap();
        assert_eq!(old.mime, "application/octet-stream");
        let gif = root.join("animation.gif");
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&gif).unwrap());
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    8,
                    8,
                    image::Rgba([255, 0, 0, 255]),
                )))
                .unwrap();
        }
        let source = runtime.import_file(&gif, "image/png").unwrap();
        let before = runtime
            .store
            .list(&runtime.scope(Platform::Local), 0, 128)
            .unwrap()
            .len();
        assert!(
            runtime
                .resolve_send_image(&source.id)
                .unwrap_err()
                .contains("not converted")
        );
        assert_eq!(
            runtime
                .store
                .list(&runtime.scope(Platform::Local), 0, 128)
                .unwrap()
                .len(),
            before
        );
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }
}
