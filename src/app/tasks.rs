// SPDX-License-Identifier: MIT
//! AI-facing social client. Platform IDs, account routing and continuations
//! stay here; provider routes remain available for host-side diagnostics.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum TaskTarget {
    Video {
        video: String,
    },
    Creator {
        mid: u64,
    },
    BiliComment {
        video: String,
        root: u64,
        parent: u64,
    },
    Thread {
        thread: String,
    },
    TiebaPost {
        thread: String,
        post: u64,
    },
}
impl TaskTarget {
    pub(super) fn platform(&self) -> Platform {
        match self {
            Self::Video { .. } | Self::Creator { .. } | Self::BiliComment { .. } => {
                Platform::Bilibili
            }
            Self::Thread { .. } | Self::TiebaPost { .. } => Platform::Tieba,
        }
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::Video { video } | Self::BiliComment { video, .. } => {
                video.parse::<VideoId>().map_err(|e| e.to_string())?;
            }
            Self::Thread { thread } | Self::TiebaPost { thread, .. } => {
                thread
                    .parse::<platform_tieba::ThreadId>()
                    .map_err(|e| e.to_string())?;
            }
            Self::Creator { mid } if *mid == 0 => return Err("invalid creator target".into()),
            _ => {}
        }
        if matches!(
            self,
            Self::BiliComment { root: 0, .. }
                | Self::BiliComment { parent: 0, .. }
                | Self::TiebaPost { post: 0, .. }
        ) {
            return Err("invalid discussion target".into());
        }
        Ok(())
    }
}

pub(super) fn check_account(params: &Value, scope: &Scope) -> Result<()> {
    if let Some(account) = params.get("account") {
        if account.as_str() != Some(scope.account.as_str()) {
            return Err(
                "account differs from the saved reference; reference was not retargeted".into(),
            );
        }
    }
    Ok(())
}
fn provider(params: &Value) -> Result<&str> {
    match required_str(params, "platform")? {
        name @ ("bilibili" | "tieba") => Ok(name),
        _ => Err("provider is unavailable: QQ is experimental and Weixin is paused".into()),
    }
}
fn platform_name(platform: &Platform) -> Result<&'static str> {
    match platform {
        Platform::Bilibili => Ok("bilibili"),
        Platform::Tieba => Ok("tieba"),
        _ => Err("unsupported social platform".into()),
    }
}
fn canonical_target(reference: &str, platform: Option<&str>) -> Result<TaskTarget> {
    if reference.len() > 4096 {
        return Err("content reference exceeds limit".into());
    }
    if platform != Some("tieba") {
        if let Ok(video) = reference.parse::<VideoId>() {
            return Ok(TaskTarget::Video {
                video: video.to_string(),
            });
        }
    }
    if platform != Some("bilibili") {
        if let Ok(thread) = reference.parse::<platform_tieba::ThreadId>() {
            return Ok(TaskTarget::Thread {
                thread: thread.0.to_string(),
            });
        }
    }
    Err("unsupported content reference; use a Runtime target_ref or a full Bilibili/Tieba content URL/ID".into())
}

#[derive(Serialize, Deserialize)]
struct ListContinuation {
    method: String,
    params: Value,
    seen: BTreeSet<String>,
    #[serde(default)]
    pending: Option<ListPending>,
}
#[derive(Serialize, Deserialize)]
struct ListPending {
    resource: String,
    offset: usize,
    fetch_more: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackHint {
    pub page: u32,
    pub start_ms: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewedTarget {
    schema: u32,
    target: TaskTarget,
    playback: PlaybackHint,
}

fn playback_from_link(reference: &str, target: &TaskTarget) -> Result<Option<PlaybackHint>> {
    if !matches!(target, TaskTarget::Video { .. }) || !reference.starts_with("https://") {
        return Ok(None);
    }
    let Some((_, query)) = reference.split_once('?') else {
        return Ok(None);
    };
    let mut hint = PlaybackHint {
        page: 1,
        start_ms: 0,
    };
    let mut seen = BTreeSet::new();
    for pair in query.split('#').next().unwrap_or(query).split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if !matches!(key, "p" | "t") {
            continue;
        }
        if !seen.insert(key) {
            return Err("ambiguous playback link: duplicate page/time parameter".into());
        }
        if key == "p" {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid video part in link".into());
            }
            hint.page = value.parse().map_err(|_| "invalid video part in link")?;
            if hint.page == 0 || hint.page > 2048 {
                return Err("video part exceeds supported bounds".into());
            }
        } else {
            let (whole, fraction) = value.split_once('.').map_or((value, ""), |(a, b)| (a, b));
            if whole.is_empty()
                || !whole.bytes().all(|b| b.is_ascii_digit())
                || !fraction.bytes().all(|b| b.is_ascii_digit())
                || (value.contains('.') && fraction.is_empty())
            {
                return Err("invalid playback time in link".into());
            }
            let seconds = whole.parse::<u64>().map_err(|_| "playback time overflow")?;
            let millis = fraction
                .bytes()
                .take(3)
                .fold(0_u64, |n, b| n * 10 + u64::from(b - b'0'))
                * 10_u64.pow(3 - fraction.len().min(3) as u32);
            hint.start_ms = seconds
                .checked_mul(1000)
                .and_then(|n| n.checked_add(millis))
                .ok_or("playback time overflow")?;
            runtime_media::Limits::default()
                .check_window(hint.start_ms, 1)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok((!seen.is_empty()).then_some(hint))
}

fn stored_target(value: Value) -> Result<(TaskTarget, Option<PlaybackHint>)> {
    if value.get("schema").is_some() {
        let viewed: ViewedTarget =
            serde_json::from_value(value).map_err(|_| "invalid playback target reference")?;
        if viewed.schema != 1
            || !matches!(viewed.target, TaskTarget::Video { .. })
            || viewed.playback.page == 0
            || viewed.playback.page > 2048
        {
            return Err("invalid playback target reference".into());
        }
        runtime_media::Limits::default()
            .check_window(viewed.playback.start_ms, 1)
            .map_err(|e| e.to_string())?;
        viewed.target.validate()?;
        Ok((viewed.target, Some(viewed.playback)))
    } else {
        let target: TaskTarget =
            serde_json::from_value(value).map_err(|_| "invalid task target reference")?;
        target.validate()?;
        Ok((target, None))
    }
}

fn list_item_key(method: &str, item: &Value) -> String {
    // Missing IDs must not collapse to the same JSON null key. A post ID is
    // scoped by its thread, and different replies in one topic stay distinct.
    for field in ["bvid", "notification_id", "id"] {
        if let Some(value) = item
            .get(field)
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
        {
            return format!("{method}:{field}:{value}");
        }
    }
    if method.ends_with(".publications") {
        if let Some(thread) = item.get("thread_id").filter(|v| !v.is_null()) {
            return format!(
                "{method}:{}:{thread}:{}",
                item["content_type"], item["post_id"]
            );
        }
    }
    // Provider projections are bounded. This complete semantic fallback also
    // distinguishes notification actors/timestamps when the service has no ID.
    use std::hash::{Hash, Hasher};
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    item.to_string().hash(&mut fingerprint);
    format!("{method}:snapshot:{:016x}", fingerprint.finish())
}

impl Runtime {
    pub(super) fn save_task_target(&self, target: &TaskTarget) -> Result<String> {
        target.validate()?;
        self.locators
            .put_reusable(&self.scope(target.platform()), "task-target", target)
    }
    pub(super) fn load_task_target(&self, id: &str) -> Result<(Scope, TaskTarget)> {
        let scope = self.locators.scope_for("task-target", id)?;
        let (target, _) = stored_target(self.locators.get(&scope, "task-target", id)?)?;
        target.validate()?;
        if scope.platform != target.platform() {
            return Err("target reference platform mismatch".into());
        }
        Ok((scope, target))
    }
    pub(super) fn target_playback_hint(&self, id: &str) -> Result<Option<PlaybackHint>> {
        let scope = self.locators.scope_for("task-target", id)?;
        let (_, hint) = stored_target(self.locators.get(&scope, "task-target", id)?)?;
        Ok(hint)
    }
    pub(super) fn with_task_scope<T>(
        &mut self,
        scope: &Scope,
        action: impl FnOnce(&mut Runtime) -> Result<T>,
    ) -> Result<T> {
        self.with_scope_context(scope, action)
    }

    /// Add directly usable actions to readable objects and media, never private URLs.
    pub(super) fn annotate_task_content(
        &self,
        node: &mut Value,
        platform: &Platform,
        video: Option<&str>,
        thread: Option<&str>,
    ) -> Result<()> {
        match node {
            Value::Array(items) => {
                for item in items {
                    self.annotate_task_content(item, platform, video, thread)?;
                }
            }
            Value::Object(_) => {
                let node_video = node.get("bvid").and_then(Value::as_str).map(str::to_owned);
                let node_thread = node
                    .get("thread_id")
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string());
                let video = node_video.as_deref().or(video);
                let thread = node_thread.as_deref().or(thread);
                let target = if *platform == Platform::Bilibili {
                    if let (Some(video), Some(parent)) = (
                        video,
                        node.get("rpid")
                            .filter(|_| {
                                node.get("message").is_some_and(Value::is_string)
                                    && node.get("oid").is_some_and(Value::is_u64)
                                    && node.get("author").is_some_and(Value::is_object)
                            })
                            .and_then(Value::as_u64)
                            .filter(|n| *n > 0),
                    ) {
                        let root = node
                            .get("root")
                            .and_then(Value::as_u64)
                            .filter(|n| *n > 0)
                            .unwrap_or(parent);
                        Some(TaskTarget::BiliComment {
                            video: video.into(),
                            root,
                            parent,
                        })
                    } else {
                        node_video
                            .as_ref()
                            .map(|v| TaskTarget::Video { video: v.clone() })
                    }
                } else if *platform == Platform::Tieba {
                    if let Some(thread) = thread {
                        let post = node
                            .get("parent_post_id")
                            .or_else(|| node.get("post_id"))
                            .or_else(|| node.get("id").filter(|_| node.get("fragments").is_some()))
                            .and_then(Value::as_u64)
                            .filter(|n| *n > 0);
                        match post {
                            Some(post) => Some(TaskTarget::TiebaPost {
                                thread: thread.into(),
                                post,
                            }),
                            None if node_thread.is_some() => Some(TaskTarget::Thread {
                                thread: thread.into(),
                            }),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(target) = target {
                    let child_context = *platform == Platform::Tieba
                        && node
                            .get("parent_post_id")
                            .and_then(Value::as_u64)
                            .is_some_and(|n| n > 0)
                        && (node.get("fragments").is_some() || node.get("comments").is_some());
                    let unresolved_publication = *platform == Platform::Tieba
                        && matches!(
                            node.get("content_type").and_then(Value::as_str),
                            Some("comment" | "unknown")
                        );
                    if unresolved_publication {
                        if let Some(thread) = thread {
                            node["context_ref"] =
                                json!(self.save_task_target(&TaskTarget::Thread {
                                    thread: thread.into()
                                })?);
                        }
                        node["exact_target_resolution"] = json!("requires_native_context_read");
                    } else if child_context {
                        node["discussion_ref"] = json!(self.save_task_target(&target)?);
                        let supported = node.get("reply_ref").and_then(Value::as_str).is_some();
                        node["reply_supported"] = json!(supported);
                        if supported {
                            node["reply_tool"] = json!("social_tieba");
                            node["reply_action"] = json!("reply_floor");
                        }
                    } else {
                        node["target_ref"] = json!(self.save_task_target(&target)?);
                    }
                }
                if let Some(asset) = node.get("asset").and_then(Value::as_str).map(str::to_owned) {
                    node["media_ref"] = json!(asset);
                    node["read_tool"] = json!("media_read");
                    node.as_object_mut().unwrap().remove("fetch_tool");
                }
                // Keep provider identities as useful metadata, but callers use creator refs.
                if *platform == Platform::Bilibili {
                    let creator = node.get("owner").or_else(|| node.get("author"));
                    if let Some(mid) = creator
                        .and_then(|v| v.get("mid").or_else(|| v.get("id")))
                        .and_then(Value::as_u64)
                        .filter(|n| *n > 0)
                    {
                        node["creator_ref"] =
                            json!(self.save_task_target(&TaskTarget::Creator { mid })?);
                    }
                }
                let notification = node.get("notification_ref").is_some();
                for (key, child) in node.as_object_mut().unwrap() {
                    if notification && key == "target" {
                        continue;
                    }
                    if !matches!(key.as_str(), "target_ref" | "creator_ref" | "scope") {
                        self.annotate_task_content(child, platform, video, thread)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn call_task(&mut self, method: &str, params: &Value) -> Result<Value> {
        if self.x_task_matches(method, params) {
            return self.call_x_social(method, params);
        }
        if self.pixiv_task_matches(method, params) {
            return self.call_pixiv_social(method, params);
        }
        if method == "social.favorites" && provider(params)? == "bilibili" {
            let mut request = json!({"action":"favorites"});
            if let Some(account) = params.get("account") {
                request["account"] = account.clone();
            }
            return self.call_bilibili_features(&request);
        }
        match method {
            "social.chats" => self.task_qq_chats(params),
            "social.messages" => self.task_qq_messages(params),
            "social.open" => self.task_open(params),
            "social.discussion" => self.task_discussion(params),
            "social.creator_content" => self.task_creator(params),
            "social.notifications"
            | "social.publications"
            | "social.search"
            | "social.discover" => self.task_list(method, params),
            "social.profile" | "social.favorites" => {
                let name = provider(params)?;
                if method == "social.favorites" && name != "bilibili" {
                    return Ok(
                        json!({"status":"unsupported","detail":"Favorite folders are implemented for Bilibili only"}),
                    );
                }
                let route = format!(
                    "{name}.{}",
                    if method == "social.profile" {
                        "me"
                    } else {
                        "favorites"
                    }
                );
                self.with_platform_account(name, params, true, |rt| {
                    let content = rt.call(&route, &json!({}))?;
                    Ok(json!({"status":"ok","scope":rt.scope(if name == "bilibili" {Platform::Bilibili} else {Platform::Tieba}),"content":content}))
                })
            }
            "social.send" => self.task_send(params),
            "social.emotes" => self.task_emotes(params),
            "social.like" | "social.follow" | "social.engagement" => {
                self.task_engage(method, params)
            }
            "media.read" => self.task_media_read(params),
            "media.watch" => self.watch_video(params),
            "runtime.expand" => self.task_expand(params),
            "social.subtitles" => self.task_subtitles(params),
            _ => Err("unknown task tool".into()),
        }
    }

    fn task_open(&mut self, params: &Value) -> Result<Value> {
        let reference = required_str(params, "reference")?;
        if self
            .locators
            .scope_for("task-bilibili-feature", reference)
            .is_ok()
        {
            return self.bili_feature_open(params);
        }
        if reference.starts_with("https://t.bilibili.com/")
            || reference.starts_with("https://www.bilibili.com/opus/")
        {
            let mut request = json!({"action":"dynamic_open","reference":reference});
            if let Some(account) = params.get("account") {
                request["account"] = account.clone();
            }
            return self.call_bilibili_features(&request);
        }
        if let Some(opened) = self.open_notification_reference(params)? {
            return Ok(opened);
        }
        if let Ok((scope, target)) = self.load_task_target(reference) {
            check_account(params, &scope)?;
            if params
                .get("platform")
                .is_some_and(|p| p.as_str() != platform_name(&scope.platform).ok())
            {
                return Err("platform differs from the saved target reference".into());
            }
            return self.with_task_scope(&scope, |rt| rt.open_task_target(&target, reference));
        }
        if reference.len() == 32 && reference.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("target reference is missing, invalid or expired".into());
        }
        let requested = params
            .get("platform")
            .map(|v| v.as_str().ok_or("platform must be a string"))
            .transpose()?;
        if requested.is_some_and(|s| !matches!(s, "bilibili" | "tieba")) {
            return Err("provider is unavailable or paused".into());
        }
        let target = canonical_target(reference, requested)?;
        let playback = playback_from_link(reference, &target)?;
        let name = platform_name(&target.platform())?;
        self.with_platform_account(name, params, false, |rt| {
            let id = if let Some(playback) = playback {
                rt.locators.put_reusable(
                    &rt.scope(target.platform()),
                    "task-target",
                    &ViewedTarget {
                        schema: 1,
                        target: target.clone(),
                        playback,
                    },
                )?
            } else {
                rt.save_task_target(&target)?
            };
            rt.open_task_target(&target, &id)
        })
    }
    fn open_task_target(&mut self, target: &TaskTarget, reference: &str) -> Result<Value> {
        let scope = self.scope(target.platform());
        match target {
            TaskTarget::Video { video } => {
                let value = self.call("bilibili.context", &json!({"video":video}))?;
                let mut content = value["page"]["data"].clone();
                self.annotate_task_content(&mut content, &scope.platform, Some(video), None)?;
                let mid = content["info"]["owner"]["id"]
                    .as_u64()
                    .ok_or("video uploader identity missing")?;
                let creator = self.save_task_target(&TaskTarget::Creator { mid })?;
                let can_send = super::auth::saved_session(&self.state, "bilibili", &scope.account)?
                    .and_then(|path| platform_bilibili::Session::load(&path).ok())
                    .is_some();
                let playback = self
                    .target_playback_hint(reference)?
                    .unwrap_or(PlaybackHint {
                        page: 1,
                        start_ms: 0,
                    });
                Ok(
                    json!({"status":"ok","scope":scope,"kind":"video","target_ref":reference,"creator_ref":creator,"playback":playback,"content":content,
                    "actions":{"watch":"media_watch","discussion":"social_discussion","creator_content":"social_creator_content",
                    "send":{"tool":"social_send","available":can_send,"constraint":"authorized_public_video_or_exact_reply_target"},
                    "like":"social_like","follow":"social_follow","subtitles":"social_subtitles"},"media_downloaded":false}),
                )
            }
            TaskTarget::Thread { thread } => {
                let value = self.call("tieba.thread", &json!({"thread":thread}))?;
                let mut content = value["content"].clone();
                // Opening gives the topic context and starter. The discussion
                // tool owns all floors and pagination, including page tails.
                let total_on_page = content["page"]["posts"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or(0);
                if let Some(posts) = content["page"]["posts"].as_array_mut() {
                    posts.truncate(1);
                }
                content["page"]["opening_preview"] = json!(true);
                content["page"]["posts_available_on_page"] = json!(total_on_page);
                self.annotate_task_content(&mut content, &scope.platform, None, Some(thread))?;
                Ok(
                    json!({"status":"ok","scope":scope,"kind":"thread","target_ref":reference,"content":content,"actions":{"discussion":"social_discussion","send":"social_send"},"media_downloaded":false}),
                )
            }
            TaskTarget::Creator { .. } => self.task_creator(&json!({"target_ref":reference})),
            TaskTarget::BiliComment { .. } | TaskTarget::TiebaPost { .. } => {
                self.task_discussion(&json!({"target_ref":reference}))
            }
        }
    }

    fn task_creator(&mut self, params: &Value) -> Result<Value> {
        if params.get("target_ref").is_some() == params.get("continuation").is_some() {
            return Err("provide exactly one of target_ref or continuation".into());
        }
        if params.get("continuation").is_some() {
            return self.task_list("social.creator_content", params);
        }
        let reference = required_str(params, "target_ref")?;
        let (scope, target) = self.load_task_target(reference)?;
        check_account(params, &scope)?;
        self.with_task_scope(&scope, |rt| {
            let mid = match target {
                TaskTarget::Creator { mid } => mid,
                TaskTarget::Video { video } | TaskTarget::BiliComment { video, .. } => {
                    let id: VideoId = video.parse().map_err(|e: platform_bilibili::Error| e.to_string())?;
                    rt.bili.video_info(&id).map_err(|e| e.to_string())?.owner.id
                }
                _ => return Ok(json!({"status":"unsupported","scope":scope,"detail":"Public creator feeds currently support Bilibili"})),
            };
            let kind = params.get("kind").and_then(Value::as_str).unwrap_or("all");
            if !matches!(kind, "all" | "videos" | "dynamics") { return Err("creator content kind must be all, videos or dynamics".into()); }
            let limit = number(params, "limit", 10)?;
            if !(1..=20).contains(&limit) { return Err("limit must be 1..20".into()); }
            let mut result = json!({"status":"ok","scope":scope,"target_ref":reference,"creator_ref":rt.save_task_target(&TaskTarget::Creator{mid})?,"media_downloaded":false,"sections":{},"errors":{}});
            let mut successes = 0;
            for (name, route) in [("videos", "bilibili.uploader_videos"), ("dynamics", "bilibili.dynamics")] {
                if kind != "all" && kind != name { continue; }
                let mut request = json!({"mid":mid,"limit":limit});
                if name == "videos" { request["page"] = json!(1); }
                match rt.task_list_page(route, &request, BTreeSet::new()) {
                    Ok(section) => { result["sections"][name] = section; successes += 1; }
                    Err(error) => result["errors"][name] = json!(error),
                }
            }
            if successes == 0 { return Err(format!("creator reads failed: {}", result["errors"])); }
            if result["errors"].as_object().is_some_and(|e| !e.is_empty()) { result["status"] = json!("partial"); }
            Ok(result)
        })
    }

    fn task_list(&mut self, method: &str, params: &Value) -> Result<Value> {
        if let Some(id) = params.get("continuation") {
            let id = id.as_str().ok_or("continuation must be an opaque string")?;
            let scope = self.locators.scope_for("task-list", id)?;
            check_account(params, &scope)?;
            let mut saved: ListContinuation = self.locators.get(&scope, "task-list", id)?;
            let allowed = match method {
                "social.notifications" => saved.method.ends_with(".notifications"),
                "social.publications" => saved.method.ends_with(".publications"),
                "social.search" => saved.method == "bilibili.search",
                "social.discover" => matches!(
                    saved.method.as_str(),
                    "bilibili.popular" | "bilibili.recommended"
                ),
                "social.creator_content" => matches!(
                    saved.method.as_str(),
                    "bilibili.uploader_videos" | "bilibili.dynamics"
                ),
                _ => false,
            };
            if !allowed {
                return Err("continuation belongs to another task".into());
            }
            if method == "social.discover"
                && let Some(source) = params.get("source")
            {
                let expected = if saved.method == "bilibili.recommended" {
                    "recommended"
                } else {
                    "popular"
                };
                if source.as_str() != Some(expected) {
                    return Err(
                        "continuation source is fixed; start a new read to change it".into(),
                    );
                }
            }
            if !saved
                .method
                .starts_with(&format!("{}.", platform_name(&scope.platform)?))
            {
                return Err("continuation provider mismatch".into());
            }
            for key in ["kind", "keyword", "order", "duration"] {
                if params
                    .get(key)
                    .is_some_and(|value| Some(value) != saved.params.get(key))
                {
                    return Err(format!(
                        "continuation {key} is fixed; start a new read to change it"
                    ));
                }
            }
            if params.get("limit").is_some() {
                let limit = number(params, "limit", 10)?;
                if !(1..=20).contains(&limit) {
                    return Err("limit must be 1..20".into());
                }
                saved.params["limit"] = json!(limit);
            }
            if params
                .get("platform")
                .and_then(Value::as_str)
                .is_some_and(|p| Some(p) != platform_name(&scope.platform).ok())
            {
                return Err("continuation platform mismatch".into());
            }
            return self.with_task_scope(&scope, |rt| {
                if saved.pending.is_some() {
                    rt.read_task_list_pending(saved)
                } else {
                    rt.task_list_page(&saved.method, &saved.params, saved.seen)
                }
            });
        }
        let name = if matches!(method, "social.search" | "social.discover") {
            "bilibili"
        } else {
            provider(params)?
        };
        let suffix = match method {
            "social.notifications" => "notifications",
            "social.publications" => "publications",
            "social.search" => "search",
            "social.discover" => match match params.get("source") {
                None => "popular",
                Some(value) => value.as_str().ok_or("discover source must be a string")?,
            } {
                "popular" => "popular",
                "recommended" => "recommended",
                _ => return Err("discover source must be recommended or popular".into()),
            },
            _ => return Err("invalid list task".into()),
        };
        let route = format!("{name}.{suffix}");
        if suffix == "notifications" && params.get("kind").and_then(Value::as_str) == Some("unread")
        {
            if name != "bilibili" {
                return Ok(
                    json!({"status":"unsupported","detail":"Tieba unread totals are not implemented"}),
                );
            }
            return self.with_platform_account(name, params, true, |rt| {
                let result = rt.call("bilibili.unread", &json!({}))?;
                Ok(json!({"status":"ok","scope":rt.scope(Platform::Bilibili),"counts":result["page"]["data"],"content_writes":false}))
            });
        }
        let login = matches!(suffix, "notifications" | "publications" | "recommended");
        let mut request = params.clone();
        let object = request.as_object_mut().unwrap();
        object.remove("platform");
        object.remove("account");
        let limit = number(&request, "limit", 10)?;
        if !(1..=20).contains(&limit) {
            return Err("limit must be 1..20".into());
        }
        request["limit"] = json!(limit);
        if suffix == "notifications" && request.get("kind").is_none() {
            request["kind"] = json!("reply");
        }
        self.with_platform_account(name, params, login, |rt| {
            rt.task_list_page(&route, &request, BTreeSet::new())
        })
    }

    fn task_list_page(
        &mut self,
        method: &str,
        params: &Value,
        seen: BTreeSet<String>,
    ) -> Result<Value> {
        if seen.len() > 2048 {
            return Err("continuation item budget exceeded".into());
        }
        let mut request = params.clone();
        // Provider page size stays fixed across continuations. The model's
        // limit controls only output slicing; changing it must never change
        // page-number arithmetic or lose a server-sized feed's tail.
        request["limit"] = json!(20);
        let result = self.call(method, &request)?;
        let data = result
            .pointer("/page/data")
            .ok_or("list data missing")?
            .clone();
        self.project_task_list(method, params, data, seen)
    }

    fn project_task_list(
        &mut self,
        method: &str,
        params: &Value,
        mut data: Value,
        mut seen: BTreeSet<String>,
    ) -> Result<Value> {
        let platform = if method.starts_with("bilibili.") {
            Platform::Bilibili
        } else {
            Platform::Tieba
        };
        let scope = self.scope(platform.clone());
        let all = data
            .get_mut("items")
            .and_then(Value::as_array_mut)
            .ok_or("list items missing")?;
        let server_items = all.len();
        all.retain(|item| {
            let key = list_item_key(method, item);
            seen.insert(key)
        });
        let new_count = all.len();
        if seen.len() > 2048 {
            return Err("continuation item budget exceeded; start a new read".into());
        }
        let mut next = params.clone();
        let has_more = data.get("has_more").and_then(Value::as_bool);
        let mut can_continue = has_more != Some(false) && server_items > 0 && new_count > 0;
        if method == "bilibili.notifications" {
            can_continue &= !data["server_is_end"].as_bool().unwrap_or(false);
            if let (Some(id), Some(time)) = (
                data["next_cursor"]["id"].as_u64(),
                data["next_cursor"]["time"].as_u64(),
            ) {
                next["cursor_id"] = json!(id);
                next["cursor_time"] = json!(time);
            } else {
                can_continue = false;
            }
        } else if method == "bilibili.dynamics" {
            if let Some(offset) = data["next_offset"].as_str() {
                next["offset"] = json!(offset);
            } else {
                can_continue = false;
            }
        } else if let Some(page) = data.get("next_page").and_then(Value::as_u64) {
            next["page"] = json!(page);
        } else if method.starts_with("tieba.") && !data["truncated"].as_bool().unwrap_or(false) {
            let page = number(params, "page", 1)?
                .checked_add(1)
                .ok_or("page overflow")?;
            can_continue &= page <= 100000;
            next["page"] = json!(page);
        } else {
            can_continue = false;
        }
        let stop_reason = if can_continue {
            "page_ready"
        } else if server_items == 0 {
            "empty_page"
        } else if new_count == 0 {
            "no_new_items"
        } else if has_more == Some(false) || data["server_is_end"] == true {
            "end"
        } else {
            "continuation_unavailable"
        };
        if method.ends_with(".notifications") {
            self.annotate_notification_targets(&mut data, &platform)?;
        }
        if method == "bilibili.notifications" {
            self.normalize_notification_media(&mut data)?;
        }
        self.annotate_task_content(&mut data, &platform, None, None)?;
        for field in [
            "next_cursor",
            "server_cursor",
            "next_page",
            "next_offset",
            "server_offset",
            "requested_page",
            "pagination_note",
        ] {
            data.as_object_mut().unwrap().remove(field);
        }
        let mut result = json!({"status":"ok","scope":scope,"items":data["items"],"context":data.as_object().unwrap().iter().filter(|(k,_)|k.as_str()!="items").map(|(k,v)|(k.clone(),v.clone())).collect::<serde_json::Map<_,_>>(),
            "continuation":null,"new_item_count":new_count,"stop_reason":stop_reason,"media_downloaded":false});
        let limit = number(params, "limit", 10)? as usize;
        if !(1..=20).contains(&limit) {
            return Err("limit must be 1..20".into());
        }
        let pending = if new_count > limit {
            let resource = self.store.import(
                &scope,
                "task-list-page",
                "application/json",
                &mut serde_json::to_vec(&result)
                    .map_err(|e| e.to_string())?
                    .as_slice(),
            )?;
            result["items"].as_array_mut().unwrap().truncate(limit);
            result["new_item_count"] = json!(limit);
            result["stop_reason"] = json!("item_limit");
            Some(ListPending {
                resource: resource.id,
                offset: limit,
                fetch_more: can_continue,
            })
        } else {
            None
        };
        if pending.is_some() || can_continue {
            result["continuation"] = json!(self.locators.put(
                &scope,
                "task-list",
                &ListContinuation {
                    method: method.into(),
                    params: next,
                    seen,
                    pending
                }
            )?);
        }
        Ok(result)
    }

    fn read_task_list_pending(&mut self, mut saved: ListContinuation) -> Result<Value> {
        let pending = saved.pending.take().ok_or("list tail is missing")?;
        let scope = self.scope(if saved.method.starts_with("bilibili.") {
            Platform::Bilibili
        } else {
            Platform::Tieba
        });
        let (_, path) = self.store.resolve(&scope, &pending.resource)?;
        let mut result: Value = serde_json::from_slice(&read_bounded(&path, 2 * 1024 * 1024)?)
            .map_err(|_| "cached list page malformed")?;
        let items = result["items"]
            .as_array()
            .ok_or("cached list items missing")?;
        let limit = number(&saved.params, "limit", 10)? as usize;
        if !(1..=20).contains(&limit) || pending.offset >= items.len() {
            return Err("invalid list tail continuation".into());
        }
        let end = (pending.offset + limit).min(items.len());
        let remaining = end < items.len();
        let count = end - pending.offset;
        result["items"] = json!(&items[pending.offset..end]);
        result["new_item_count"] = json!(count);
        result["provider_pages_read"] = json!(0);
        if remaining {
            result["stop_reason"] = json!("item_limit");
            saved.pending = Some(ListPending {
                offset: end,
                ..pending
            });
        }
        result["continuation"] = if remaining || pending.fetch_more {
            json!(self.locators.put(&scope, "task-list", &saved)?)
        } else {
            Value::Null
        };
        Ok(result)
    }

    pub(super) fn normalize_notification_media(&self, data: &mut Value) -> Result<()> {
        fn replace(rt: &Runtime, node: &mut Value) -> Result<()> {
            match node {
                Value::Array(items) => {
                    for item in items {
                        replace(rt, item)?;
                    }
                }
                Value::Object(map) => {
                    for key in ["avatar", "image"] {
                        if let Some(url) = map
                            .get(key)
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                        {
                            if platform_bilibili::validate_resource_url(&url).is_ok() {
                                let media = MediaResource {
                                    url,
                                    backup_urls: vec![],
                                    expected_bytes: None,
                                    mime_type: None,
                                };
                                let asset = rt.locators.put(
                                    &rt.scope(Platform::Bilibili),
                                    "bilibili-media",
                                    &media,
                                )?;
                                map.insert(key.into(), json!({"media_ref":asset,"read_tool":"media_read","fetched":false}));
                            } else {
                                map.insert(key.into(), json!({"status":"unsupported","detail":"Image location has no supported platform resolver"}));
                            }
                        }
                    }
                    for (_, child) in map {
                        replace(rt, child)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        replace(self, data)
    }
    fn annotate_notification_targets(&self, data: &mut Value, platform: &Platform) -> Result<()> {
        let kind = data["kind"].as_str().map(str::to_owned);
        if let Some(items) = data["items"].as_array_mut() {
            for item in items {
                if let Some(kind) = kind
                    .as_deref()
                    .or_else(|| item.get("kind").and_then(Value::as_str))
                {
                    let reference = self.notification_reference(platform, kind, item)?;
                    item["notification_ref"] = json!(reference);
                    item["open_tool"] = json!("social_open");
                }
                let target = &item["target"];
                let context = if *platform == Platform::Tieba {
                    let thread = target["thread_id"].as_u64().filter(|n| *n > 0);
                    thread.map(|thread| TaskTarget::Thread {
                        thread: thread.to_string(),
                    })
                } else {
                    target["uri"].as_str().and_then(|s| {
                        canonical_target(s.split('#').next().unwrap_or(s), Some("bilibili")).ok()
                    })
                };
                if let Some(target) = context {
                    item["context_ref"] = json!(self.save_task_target(&target)?);
                    item["open_tool"] = json!("social_open");
                }
                if item.get("target").is_some() {
                    item["target_resolution"] = json!("exact_reply_target_unresolved");
                }
            }
        }
        Ok(())
    }

    fn task_engage(&mut self, method: &str, params: &Value) -> Result<Value> {
        let reference = required_str(params, "target_ref")?;
        let (scope, target) = self.load_task_target(reference)?;
        check_account(params, &scope)?;
        let video = match target {
            TaskTarget::Video { video } => video,
            _ => {
                return Ok(
                    json!({"status":"unsupported","scope":scope,"detail":"Like/follow require a Bilibili video target; comment likes and following a comment author are not implemented"}),
                );
            }
        };
        let mut request = params.clone();
        request["video"] = json!(video);
        request["account"] = json!(scope.account);
        let route = match method {
            "social.like" => "bilibili.like",
            "social.follow" => "bilibili.follow_uploader",
            _ => "bilibili.engagement",
        };
        let mut result = self.call_bilibili_engagement(route, &request)?;
        result["target_ref"] = json!(reference);
        Ok(result)
    }

    fn task_send(&mut self, params: &Value) -> Result<Value> {
        if params.get("target_ref").is_some() == params.get("latest_own").is_some() {
            return Err("send needs exactly one target_ref or latest_own selector".into());
        }
        if let Some(name) = params.get("latest_own") {
            let name = name
                .as_str()
                .filter(|s| matches!(*s, "bilibili" | "tieba"))
                .ok_or("latest_own must be bilibili or tieba")?;
            return self.with_comment_account(name, params, |rt| {
                let id = required_str(params, "operation_id")?;
                let outbox = crate::outbox::Outbox::open(&rt.state)?;
                let platform = if name == "bilibili" {
                    Platform::Bilibili
                } else {
                    Platform::Tieba
                };
                let data = if outbox.exists(id)? {
                    outbox.load(id, &rt.scope(platform.clone()))?.target
                } else {
                    let page = rt.call_publications(
                        &format!("{name}.publications"),
                        &json!({"kind":"thread","limit":20}),
                    )?;
                    let items = page["page"]["data"]["items"]
                        .as_array()
                        .ok_or("own publications missing")?;
                    if items
                        .iter()
                        .any(|item| item["created_at"].as_u64().is_none_or(|n| n == 0))
                    {
                        return Err(
                            "creation times unavailable; latest target was not guessed".into()
                        );
                    }
                    items
                        .iter()
                        .max_by_key(|item| item["created_at"].as_u64())
                        .ok_or("no own published content")?
                        .clone()
                };
                let target = if name == "bilibili" {
                    TaskTarget::Video {
                        video: data["bvid"]
                            .as_str()
                            .ok_or("saved video target missing")?
                            .into(),
                    }
                } else {
                    TaskTarget::Thread {
                        thread: data["thread_id"]
                            .as_u64()
                            .ok_or("saved topic target missing")?
                            .to_string(),
                    }
                };
                let reference = rt.save_task_target(&target)?;
                let mut result = rt.send_task_target(&target, &reference, params)?;
                result["target_resolution"] = json!("own_latest_frozen_on_first_preparation");
                Ok(result)
            });
        }
        let reference = required_str(params, "target_ref")?;
        if self.locators.scope_for("task-qq-chat", reference).is_ok() {
            return self.task_qq_send(params);
        }
        let (scope, target) = self.load_task_target(reference)?;
        check_account(params, &scope)?;
        self.with_task_scope(&scope, |rt| rt.send_task_target(&target, reference, params))
    }

    fn task_media_read(&mut self, params: &Value) -> Result<Value> {
        let id = required_str(params, "reference")?;
        let scope = self.media_reference_scope(id)?;
        check_account(params, &scope)?;
        self.with_task_scope(&scope, |rt| rt.read_media(params))
    }
    pub(super) fn media_reference_scope(&self, id: &str) -> Result<Scope> {
        if let Ok(scope) = self.store.scope_for(id) {
            return Ok(scope);
        }
        for kind in [
            "bilibili-media",
            "bilibili-comment-media",
            "bilibili-dynamic-media",
            "tieba-media",
            "task-qq-media",
            "task-x-media",
            "task-pixiv-media",
        ] {
            if let Ok(scope) = self.locators.scope_for(kind, id) {
                return Ok(scope);
            }
        }
        Err(
            "media reference is missing, expired, or belongs to an unsupported/paused provider"
                .into(),
        )
    }

    fn task_expand(&mut self, params: &Value) -> Result<Value> {
        let reference = required_str(params, "reference")?;
        if self.locators.scope_for("task-qq-source", reference).is_ok() {
            return self.expand_qq_source(params);
        }
        let (scope, resource, path) = if let Ok(scope) = self.store.scope_for(reference) {
            let (resource, path) = self.store.resolve(&scope, reference)?;
            (scope, resource, path)
        } else {
            self.materialize_send_media(reference)?
        };
        check_account(params, &scope)?;
        self.with_task_scope(&scope,|rt|{
            let mut request=params.clone();request["resource"]=json!(resource.id);
            let mode=params.get("mode").and_then(Value::as_str).unwrap_or("auto");
            let mut sniff=[0u8;512];let length=File::open(&path).and_then(|mut file|file.read(&mut sniff)).map_err(|e|e.to_string())?;
            let prefix=sniff[..length].iter().position(|b|!b.is_ascii_whitespace()).map(|at|&sniff[at..length]).unwrap_or(&[]);
            let method=match mode {
                "auto" if resource.mime=="application/json" || prefix.starts_with(b"{") || prefix.starts_with(b"[")=>"content.json",
                "auto" if resource.mime=="text/html" || prefix.starts_with(b"<!DOCTYPE html") || prefix.starts_with(b"<!doctype html") || prefix.starts_with(b"<html")=>"content.html",
                "auto"|"raw"=>"resource.read", "html"=>"content.html", "json"=>"content.json",_=>return Err("expand mode must be auto, raw, html or json".into())
            };
            let content=rt.call(method,&request)?;
            Ok(json!({"status":"ok","scope":scope,"reference":reference,"resource":resource,"content":content,"scripts_executed":false}))
        })
    }
    fn task_subtitles(&mut self, params: &Value) -> Result<Value> {
        let reference = required_str(params, "target_ref")?;
        let (scope, target) = self.load_task_target(reference)?;
        check_account(params, &scope)?;
        let video = match target {
            TaskTarget::Video { video } | TaskTarget::BiliComment { video, .. } => video,
            _ => {
                return Ok(
                    json!({"status":"unsupported","scope":scope,"detail":"Subtitles require a Bilibili video"}),
                );
            }
        };
        let mut request = params.clone();
        request["video"] = json!(video);
        self.with_task_scope(&scope, |rt| {
            let content = rt.call("bilibili.subtitles", &request)?;
            Ok(json!({"status":"ok","scope":scope,"target_ref":reference,"content":content,"audio_understanding":false}))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_parsing_never_accepts_arbitrary_urls_or_ambiguous_providers() {
        assert!(matches!(
            canonical_target("BV17x411w7KC", None).unwrap(),
            TaskTarget::Video { .. }
        ));
        assert!(matches!(
            canonical_target("https://tieba.baidu.com/p/11145970964", None).unwrap(),
            TaskTarget::Thread { .. }
        ));
        for s in [
            "https://localhost/p/1",
            "https://www.bilibili.com.evil.invalid/video/av1",
            "javascript:alert(1)",
        ] {
            assert!(canonical_target(s, None).is_err());
        }
        assert!(canonical_target("av170001", Some("tieba")).is_err());
        assert!(
            TaskTarget::BiliComment {
                video: "av1".into(),
                root: 0,
                parent: 1
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn video_share_position_is_preserved_in_a_scoped_reference_with_legacy_compatibility() {
        let target = TaskTarget::Video {
            video: "av170001".into(),
        };
        let hint = playback_from_link(
            "https://www.bilibili.com/video/av170001/?p=2&t=12.345&share_source=copy",
            &target,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            hint,
            PlaybackHint {
                page: 2,
                start_ms: 12345
            }
        );
        for query in ["p=0", "p=2&p=3", "t=-1", "t=1.2.3", "t=NaN", "p=99999999"] {
            assert!(
                playback_from_link(
                    &format!("https://www.bilibili.com/video/av170001/?{query}"),
                    &target
                )
                .is_err()
            );
        }
        let dir = lab();
        let rt = Runtime::open(&dir.join("state"), "alice", None).unwrap();
        let ref_id = rt
            .locators
            .put_reusable(
                &rt.scope(Platform::Bilibili),
                "task-target",
                &ViewedTarget {
                    schema: 1,
                    target: target.clone(),
                    playback: hint.clone(),
                },
            )
            .unwrap();
        let legacy = rt.save_task_target(&target).unwrap();
        drop(rt);
        let rt = Runtime::open(&dir.join("state"), "public", None).unwrap();
        assert_eq!(rt.load_task_target(&ref_id).unwrap().1, target);
        assert_eq!(rt.target_playback_hint(&ref_id).unwrap(), Some(hint));
        assert!(rt.target_playback_hint(&legacy).unwrap().is_none());
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }

    fn lab() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "client-tasks-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        private_dir(&path).unwrap();
        path
    }
    #[test]
    fn homepage_source_and_cached_tail_survive_continuation_without_public_fallback() {
        let dir = lab();
        let mut rt = Runtime::open(&dir, "public", None).unwrap();
        let data = json!({"items":[{"aid":170001,"bvid":"BV17x411w7KC","title":"first"},{"aid":2,"bvid":"BV1KMsf6uEzK","title":"second"}],
            "source":"bilibili_home_recommendations","signed_in_request":true,"personalization_verified":false,"has_more":false});
        let first = rt
            .project_task_list(
                "bilibili.recommended",
                &json!({"source":"recommended","limit":1}),
                data,
                BTreeSet::new(),
            )
            .unwrap();
        let next = first["continuation"].as_str().unwrap();
        assert_eq!(first["context"]["source"], "bilibili_home_recommendations");
        assert!(
            rt.call(
                "social.discover",
                &json!({"source":"popular","continuation":next})
            )
            .unwrap_err()
            .contains("source is fixed")
        );
        let second = rt
            .call(
                "social.discover",
                &json!({"source":"recommended","continuation":next}),
            )
            .unwrap();
        assert_eq!(second["items"][0]["bvid"], "BV1KMsf6uEzK");
        assert_eq!(second["provider_pages_read"], 0);
        assert_eq!(second["context"]["source"], "bilibili_home_recommendations");
        assert!(second["continuation"].is_null());
        assert!(rt.call("social.discover", &json!({"source":42})).is_err());
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn persisted_targets_restore_scope_but_never_allow_an_account_override() {
        let dir = lab();
        let state = dir.join("state");
        let rt = Runtime::open(&state, "alice", None).unwrap();
        let id = rt
            .save_task_target(&TaskTarget::Video {
                video: "av170001".into(),
            })
            .unwrap();
        drop(rt);
        let mut rt = Runtime::open(&state, "public", None).unwrap();
        let (scope, target) = rt.load_task_target(&id).unwrap();
        assert_eq!(scope.account, "alice");
        assert_eq!(
            target,
            TaskTarget::Video {
                video: "av170001".into()
            }
        );
        assert!(
            rt.call("social.open", &json!({"reference":id,"account":"bob"}))
                .unwrap_err()
                .contains("account differs")
        );
        let result = rt
            .with_task_scope(&scope, |inner| {
                assert_eq!(inner.account, "alice");
                Ok(json!({"scope":inner.scope(Platform::Bilibili)}))
            })
            .unwrap();
        assert_eq!(result["scope"]["account"], "alice");
        assert_eq!(rt.account, "public");
        assert!(
            rt.with_task_scope(&scope, |_| Err::<Value, String>("synthetic failure".into()))
                .is_err()
        );
        assert_eq!(rt.account, "public");
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn notification_keys_without_service_ids_stay_distinct_and_continuations_reject_other_tasks() {
        let dir = lab();
        let mut rt = Runtime::open(&dir.join("state"), "alice", None).unwrap();
        let data = json!({"items":[
            {"notification_id":null,"created_at":1,"content":"first","target":{"thread_id":42}},
            {"notification_id":null,"created_at":2,"content":"second","target":{"thread_id":42}}
        ],"has_more":true,"truncated":false});
        let result = rt
            .project_task_list(
                "tieba.notifications",
                &json!({"kind":"reply","limit":20}),
                data.clone(),
                BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(result["new_item_count"], 2);
        let continuation = result["continuation"].as_str().unwrap();
        assert!(
            rt.call("social.publications", &json!({"continuation":continuation}))
                .unwrap_err()
                .contains("another task")
        );
        assert!(
            rt.call(
                "social.notifications",
                &json!({"continuation":continuation,"account":"bob"})
            )
            .unwrap_err()
            .contains("account differs")
        );
        let saved: ListContinuation = rt
            .locators
            .get(&rt.scope(Platform::Tieba), "task-list", continuation)
            .unwrap();
        let repeated = rt
            .project_task_list("tieba.notifications", &saved.params, data, saved.seen)
            .unwrap();
        assert_eq!(repeated["new_item_count"], 0);
        assert_eq!(repeated["stop_reason"], "no_new_items");
        assert!(repeated["continuation"].is_null());
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn server_sized_feed_pages_keep_small_requested_tails_without_another_request() {
        let dir = lab();
        let mut rt = Runtime::open(&dir.join("state"), "alice", None).unwrap();
        let data = json!({"items":(1..=13).map(|id|json!({"id":id.to_string(),"text":format!("post {id}")})).collect::<Vec<_>>(),"has_more":true,"next_offset":"next-server-page","truncated":false});
        let mut result = rt
            .project_task_list(
                "bilibili.dynamics",
                &json!({"mid":42,"limit":3}),
                data,
                BTreeSet::new(),
            )
            .unwrap();
        let mut ids = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].clone())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 3);
        for expected in [3, 3, 3, 1] {
            let next = result["continuation"].as_str().unwrap();
            result = rt
                .call("social.creator_content", &json!({"continuation":next}))
                .unwrap();
            assert_eq!(result["items"].as_array().unwrap().len(), expected);
            assert_eq!(result["provider_pages_read"], 0);
            ids.extend(
                result["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i["id"].clone()),
            );
        }
        assert_eq!(ids.len(), 13);
        assert_eq!(ids.first().unwrap(), "1");
        assert_eq!(ids.last().unwrap(), "13");
        let next = result["continuation"].as_str().unwrap();
        let state: ListContinuation = rt
            .locators
            .get(&rt.scope(Platform::Bilibili), "task-list", next)
            .unwrap();
        assert!(state.pending.is_none());
        assert_eq!(state.params["offset"], "next-server-page");
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn notification_context_never_impersonates_a_reply_target() {
        let dir = lab();
        let rt = Runtime::open(&dir.join("state"), "alice", None).unwrap();
        let mut tieba =
            json!({"items":[{"target":{"thread_id":42,"post_id":70,"parent_post_id":null}}]});
        rt.annotate_notification_targets(&mut tieba, &Platform::Tieba)
            .unwrap();
        assert!(tieba["items"][0].get("target_ref").is_none());
        let (_, context) = rt
            .load_task_target(tieba["items"][0]["context_ref"].as_str().unwrap())
            .unwrap();
        assert!(matches!(context, TaskTarget::Thread { .. }));
        let mut bili = json!({"items":[{"target":{"uri":"https://www.bilibili.com/video/av170001/#reply123"}}]});
        rt.annotate_notification_targets(&mut bili, &Platform::Bilibili)
            .unwrap();
        assert!(bili["items"][0].get("target_ref").is_none());
        assert!(bili["items"][0]["context_ref"].is_string());
        assert_eq!(
            bili["items"][0]["target_resolution"],
            "exact_reply_target_unresolved"
        );
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unresolved_comment_coordinates_never_become_actionable_comment_targets() {
        let dir = lab();
        let rt = Runtime::open(&dir.join("state"), "alice", None).unwrap();
        let mut context = json!({"location":{"comment":{"oid":1,"rpid":2,"root":null,"parent":null}},
            "selected_comment":{"oid":1,"rpid":3,"root":2,"parent":2,"message":"read back","author":{"mid":42}}});
        rt.annotate_task_content(&mut context, &Platform::Bilibili, Some("av1"), None)
            .unwrap();
        assert!(context["location"]["comment"].get("target_ref").is_none());
        assert!(context["selected_comment"]["target_ref"].is_string());
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn task_media_attaches_real_bytes_after_restoring_owner_without_a_decoder_chain() {
        let dir = lab();
        let state = dir.join("state");
        let file = dir.join("picture.png");
        image::DynamicImage::new_rgb8(8, 8).save(&file).unwrap();
        let rt = Runtime::open(&state, "alice", None).unwrap();
        let resource = rt.import_file(&file, "application/octet-stream").unwrap();
        drop(rt);
        let mut rt = Runtime::open(&state, "public", None).unwrap();
        let result = rt.call_for_engine("media.read", &json!({"reference":resource.id}));
        assert!(result.success);
        assert_eq!(
            result
                .content_items
                .iter()
                .filter(|i| matches!(i, runtime_engine::ToolContent::InputImage { .. }))
                .count(),
            1
        );
        let runtime_engine::ToolContent::InputText { text } = &result.content_items[0] else {
            panic!("missing projection")
        };
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["resolved_source"]["scope"]["account"], "alice");
        assert_eq!(value["download"]["performed"], false);
        assert_eq!(rt.account, "public");
        assert!(
            rt.call(
                "media.read",
                &json!({"reference":resource.id,"account":"bob"})
            )
            .is_err()
        );
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn default_engine_surface_hides_mechanical_routes_and_assigns_stable_write_ids() {
        let dir = lab();
        let mut rt = Runtime::open(&dir.join("state"), "alice", None).unwrap();
        let reference = rt
            .save_task_target(&TaskTarget::Creator { mid: 42 })
            .unwrap();
        let call = runtime_engine::ToolCall {
            thread_id: "t".into(),
            turn_id: "turn".into(),
            call_id: "call".into(),
            tool: "social_send".into(),
            arguments: json!({"target_ref":reference,"message":"not sent"}),
        };
        let one = rt.dispatch_engine_tool(&call);
        let two = rt.dispatch_engine_tool(&call);
        let get = |result: runtime_engine::ToolResult| {
            let runtime_engine::ToolContent::InputText { text } = &result.content_items[0] else {
                panic!("missing result")
            };
            serde_json::from_str::<Value>(text).unwrap()
        };
        let one = get(one);
        let two = get(two);
        assert_eq!(one["status"], "unsupported");
        assert_eq!(one["operation_id"], two["operation_id"]);
        assert!(one["operation_id"].as_str().unwrap().starts_with("engine-"));
        assert!(crate::tools::method("bilibili_fetch").is_none());
        assert_eq!(
            crate::tools::method("media_watch").as_deref(),
            Some("media.watch")
        );
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn compact_previews_are_fitted_to_engine_budget_without_a_model_resize_retry() {
        use base64::Engine;
        let dir = lab();
        let file = dir.join("noise.png");
        let mut pixels = image::RgbImage::new(512, 512);
        let mut noise = 0x13579bdf_u32;
        for pixel in pixels.pixels_mut() {
            for channel in &mut pixel.0 {
                noise = noise.wrapping_mul(1664525).wrapping_add(1013904223);
                *channel = (noise >> 24) as u8;
            }
        }
        pixels.save(&file).unwrap();
        let mut rt = Runtime::open(&dir.join("state"), "alice", None).unwrap();
        let resource = rt.import_file(&file, "image/png").unwrap();
        let result = rt.call_for_engine(
            "media.read",
            &json!({"reference":resource.id,"quality":"preview","max_edge":480}),
        );
        let images = result
            .content_items
            .iter()
            .filter_map(|item| match item {
                runtime_engine::ToolContent::InputImage { image_url } => Some(image_url),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(images.len(), 1);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(images[0].strip_prefix("data:image/jpeg;base64,").unwrap())
            .unwrap();
        assert!(bytes.len() <= 64 * 1024);
        assert!(bytes.starts_with(b"\xff\xd8\xff"));
        drop(rt);
        fs::remove_dir_all(dir).unwrap();
    }
}
