// SPDX-License-Identifier: MIT
//! Bilibili task workflows. Fixed native operations; IDs and paging stay private.
use super::*;
use crate::outbox::{Operation, Outbox, Status};
use platform_bilibili::FeatureCommand as Command;
use serde::{Deserialize, Serialize};
const TARGET: &str = "task-bilibili-feature";
const CURSOR: &str = "task-bilibili-feature-page";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    Video { video: String },
    Folder { id: u64 },
    Dynamic { id: String },
    DynamicComment { id: String, root: u64, parent: u64 },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    action: String,
    target: Target,
    page: u32,
}
fn dynamic_id(raw: &str) -> Result<String> {
    let id = raw
        .strip_prefix("https://t.bilibili.com/")
        .or_else(|| raw.strip_prefix("https://www.bilibili.com/opus/"))
        .unwrap_or(raw)
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if id.is_empty()
        || id.len() > 24
        || !id.bytes().all(|c| c.is_ascii_digit())
        || id.bytes().all(|c| c == b'0')
    {
        return Err("invalid Bilibili dynamic link".into());
    }
    Ok(id.into())
}
fn allowed(params: &Value, keys: &[&str]) -> Result<()> {
    let p = params.as_object().ok_or("expected action object")?;
    if p.keys().any(|k| !keys.contains(&k.as_str())) {
        return Err("unexpected Bilibili action parameter".into());
    }
    Ok(())
}
fn boolean(p: &Value, key: &str, default: bool) -> Result<bool> {
    p.get(key)
        .map(|v| v.as_bool().ok_or_else(|| format!("{key} must be boolean")))
        .transpose()
        .map(|v| v.unwrap_or(default))
}
fn int(p: &Value, key: &str, default: u64) -> Result<u32> {
    u32::try_from(number(p, key, default)?).map_err(|_| format!("invalid {key}"))
}
fn value<T: Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}
fn outcome(record: &Operation, reused: bool) -> Value {
    json!({"operation_id":record.id,"scope":record.scope,"status":record.status,"action":record.message,"write_attempted":record.attempt_started_at.is_some(),"reused":reused,"receipt":record.receipt,"verification":record.verification,"error":record.error,"automatic_resubmission":false,"next_tools":[{"tool":"runtime_operation","operation_id":record.id}]})
}
fn bind_actor(rt: &Runtime, mid: u64) -> Result<()> {
    let path = crate::private_state::account_directory(&rt.state, "bilibili", &rt.account)?;
    super::auth::bind_identity(&path, &mid.to_string())
}
impl Runtime {
    pub(super) fn bili_feature_open(&mut self, p: &Value) -> Result<Value> {
        let (scope, target) = self.load_bili_feature(required_str(p, "target_ref")?)?;
        super::tasks::check_account(p, &scope)?;
        let action = match target {
            Target::Folder { .. } => "favorite_contents",
            Target::Dynamic { .. } | Target::DynamicComment { .. } => "dynamic_open",
            _ => return Err("use social_open for ordinary videos".into()),
        };
        self.with_task_scope(&scope, |rt| {
            rt.bili_feature_read(action, Some(&target), 1, p)
        })
    }
    pub(super) fn bili_dynamic_feature_ref(&self, id: &str) -> Result<String> {
        self.save_bili_feature(&Target::Dynamic {
            id: dynamic_id(id)?,
        })
    }
    fn save_bili_feature(&self, target: &Target) -> Result<String> {
        self.locators
            .put_reusable(&self.scope(Platform::Bilibili), TARGET, target)
    }
    fn load_bili_feature(&self, reference: &str) -> Result<(Scope, Target)> {
        if let Ok((scope, target)) = self.load_task_target(reference) {
            let target =
                match target {
                    super::tasks::TaskTarget::Video { video } => Target::Video { video },
                    _ => return Err(
                        "this Bilibili action requires a video, dynamic or favorite folder target"
                            .into(),
                    ),
                };
            if scope.platform != Platform::Bilibili {
                return Err("Bilibili target required".into());
            }
            return Ok((scope, target));
        }
        let scope = self.locators.scope_for(TARGET, reference)?;
        if scope.platform != Platform::Bilibili {
            return Err("Bilibili target required".into());
        }
        let target: Target = self.locators.get(&scope, TARGET, reference)?;
        match &target {
            Target::Video { video } => {
                video.parse::<VideoId>().map_err(|e| e.to_string())?;
            }
            Target::Folder { id } if *id == 0 => return Err("invalid favorite target".into()),
            Target::Dynamic { id } | Target::DynamicComment { id, .. } => {
                dynamic_id(id)?;
            }
            _ => {}
        }
        if matches!(
            target,
            Target::DynamicComment { root: 0, .. } | Target::DynamicComment { parent: 0, .. }
        ) {
            return Err("invalid dynamic comment target".into());
        }
        Ok((scope, target))
    }
    pub(super) fn call_bilibili_features(&mut self, params: &Value) -> Result<Value> {
        let action = required_str(params, "action")?;
        let extra: &[&str] = match action {
            "favorites" => &[],
            "favorite_contents" => &["continuation", "limit"],
            "dynamic_open" => &["reference"],
            "dynamic_discussion" => &["continuation", "limit"],
            "danmaku_read" => &["page", "start_ms", "duration_ms", "limit", "continuation"],
            "favorite" => &["folder_ref", "add", "operation_id"],
            "coin" => &["count", "operation_id"],
            "danmaku" => &[
                "page",
                "progress_ms",
                "message",
                "color",
                "mode",
                "operation_id",
            ],
            "dynamic_create" => &["message", "image_refs", "image_ref", "operation_id"],
            "dynamic_like" => &["like", "operation_id"],
            "dynamic_reply" => &["message", "operation_id"],
            _ => return Err("unknown Bilibili feature action".into()),
        };
        let mut keys = vec!["action", "target_ref", "account"];
        keys.extend_from_slice(extra);
        allowed(params, &keys)?;
        let write = matches!(
            action,
            "favorite" | "coin" | "danmaku" | "dynamic_create" | "dynamic_like" | "dynamic_reply"
        );
        if let Some(reference) = params.get("continuation").and_then(Value::as_str) {
            if action == "danmaku_read" {
                return self.bili_danmaku_continue(params, reference);
            }
            let scope = self.locators.scope_for(CURSOR, reference)?;
            super::tasks::check_account(params, &scope)?;
            let cursor: Cursor = self.locators.get(&scope, CURSOR, reference)?;
            if cursor.action != action {
                return Err("continuation belongs to another Bilibili action".into());
            }
            if params.get("target_ref").is_some() {
                return Err("continue with continuation only".into());
            }
            return self.with_task_scope(&scope, |rt| {
                rt.bili_feature_read(action, Some(&cursor.target), cursor.page, params)
            });
        }
        if let Some(reference) = params.get("target_ref") {
            let (scope, target) = self
                .load_bili_feature(reference.as_str().ok_or("target_ref must be opaque text")?)?;
            super::tasks::check_account(params, &scope)?;
            if write && scope.account == "public" {
                return Err(
                    "reopen this Bilibili target under a saved personal account before writing"
                        .into(),
                );
            }
            return self.with_task_scope(&scope, |rt| {
                if write {
                    rt.bili_feature_write(action, Some(&target), params)
                } else {
                    rt.bili_feature_read(action, Some(&target), 1, params)
                }
            });
        }
        if write && action != "dynamic_create" {
            return Err("this action needs a returned target_ref".into());
        }
        self.with_platform_account("bilibili", params, write || action == "favorites", |rt| {
            if write {
                rt.bili_feature_write(action, None, params)
            } else {
                rt.bili_feature_read(action, None, 1, params)
            }
        })
    }
    fn bili_feature_read(
        &mut self,
        action: &str,
        target: Option<&Target>,
        page: u32,
        p: &Value,
    ) -> Result<Value> {
        let limit = int(p, "limit", 20)?;
        if !(1..=20).contains(&limit) {
            return Err("limit must be1..20".into());
        }
        match action {
            "favorites" => {
                let actor = self.bili.current_user().map_err(|e| e.to_string())?.mid;
                bind_actor(self, actor)?;
                let folders = self
                    .bili
                    .favorite_folders(actor)
                    .map_err(|e| e.to_string())?;
                let states = if let Some(Target::Video { video }) = target {
                    Some(
                        self.bili
                            .video_folders(
                                &video.parse::<VideoId>().map_err(|e| e.to_string())?,
                                actor,
                            )
                            .map_err(|e| e.to_string())?,
                    )
                } else {
                    None
                };
                let mut items = Vec::new();
                for f in folders {
                    let mut item = value(&f)?;
                    item["folder_ref"] =
                        json!(self.save_bili_feature(&Target::Folder { id: f.id })?);
                    item["next_tools"] = json!([{"tool":"social_bilibili","action":"favorite_contents","target_ref":item["folder_ref"]}]);
                    if let Some(v) = &states {
                        item["favorited"] = v["list"]
                            .as_array()
                            .and_then(|a| a.iter().find(|x| x["id"].as_u64() == Some(f.id)))
                            .and_then(|x| x["fav_state"].as_u64())
                            .map(|v| json!(v != 0))
                            .unwrap_or(Value::Null)
                    }
                    items.push(item)
                }
                Ok(
                    json!({"scope":self.scope(Platform::Bilibili),"items":items,"content_writes":false}),
                )
            }
            "favorite_contents" => {
                let Some(Target::Folder { id }) = target else {
                    return Err(
                        "favorite_contents needs a returned folder_ref as target_ref".into(),
                    );
                };
                let data = self
                    .bili
                    .favorite_contents(*id, page, limit)
                    .map_err(|e| e.to_string())?;
                let mut items = Vec::new();
                for raw in data["items"].as_array().ok_or("favorite items absent")? {
                    let mut item = json!({"title":raw["title"],"duration_seconds":raw["duration"],"creator":{"mid":raw["upper"]["mid"],"name":raw["upper"]["name"]},"counts":raw["cnt_info"],"media_type":raw["type"],"media":[]});
                    if raw["type"].as_u64() == Some(2) {
                        if let Some(bvid) = raw["bvid"].as_str() {
                            let video = bvid.parse::<VideoId>().map_err(|e| e.to_string())?;
                            item["target_ref"] =
                                json!(self.save_task_target(&super::tasks::TaskTarget::Video {
                                    video: video.to_string()
                                })?);
                        }
                    }
                    if let Some(url) = raw["cover"].as_str() {
                        if platform_bilibili::validate_resource_url(url).is_ok() {
                            item["media"] = json!([{"url":url,"kind":"cover"}])
                        }
                    }
                    items.push(item);
                }
                let mut result =
                    json!({"items":items,"folder":data["folder"],"content_writes":false});
                // The folder cover is metadata, never an automatically fetched image.
                if let Some(obj) = result["folder"].as_object_mut() {
                    for key in ["cover", "intro", "upper", "cover_type"] {
                        obj.remove(key);
                    }
                }
                let mut assets = self.locators.batch();
                super::browsing::replace_media(
                    &mut result,
                    &mut assets,
                    &self.scope(Platform::Bilibili),
                    &mut 0,
                    "bilibili-comment-media",
                )?;
                assets.commit();
                result["continuation"] = if data["has_more"] == true {
                    json!(self.locators.put(
                        &self.scope(Platform::Bilibili),
                        CURSOR,
                        &Cursor {
                            action: action.into(),
                            target: target.unwrap().clone(),
                            page: page + 1
                        }
                    )?)
                } else {
                    Value::Null
                };
                Ok(result)
            }
            "dynamic_open" => {
                let id = match target {
                    Some(Target::Dynamic { id } | Target::DynamicComment { id, .. }) => id.clone(),
                    None => dynamic_id(required_str(p, "reference")?)?,
                    _ => return Err("dynamic_open needs a dynamic target or link".into()),
                };
                let item = self.bili.dynamic_detail(&id).map_err(|e| e.to_string())?;
                let target_ref = self.bili_dynamic_feature_ref(&id)?;
                let mut data =
                    json!({"items":[item],"target_ref":target_ref,"content_writes":false});
                data = self.cache_bili_dynamic_page(data)?;
                data["next_tools"] = json!([{"tool":"social_bilibili","action":"dynamic_discussion","target_ref":target_ref},{"tool":"social_bilibili","action":"dynamic_reply","target_ref":target_ref},{"tool":"social_bilibili","action":"dynamic_like","target_ref":target_ref}]);
                Ok(data)
            }
            "dynamic_discussion" => {
                let (id, root) = match target {
                    Some(Target::Dynamic { id }) => (id, 0),
                    Some(Target::DynamicComment { id, root, .. }) => (id, *root),
                    _ => {
                        return Err(
                            "dynamic_discussion needs a returned dynamic/comment target".into()
                        );
                    }
                };
                let data = self
                    .bili
                    .dynamic_discussion(id, root, page, limit)
                    .map_err(|e| e.to_string())?;
                let mut items = Vec::new();
                if let Some(rows) = data["replies"].as_array() {
                    if rows.len() > 20 {
                        return Err("dynamic discussion limit".into());
                    }
                    for raw in rows {
                        items.push(self.bili_dynamic_comment(id, raw, root, 0)?);
                    }
                }
                let mut result = json!({"items":items,"root":null,"content_writes":false});
                if root != 0 {
                    if let Some(raw) = data.get("root").filter(|v| !v.is_null()) {
                        result["root"] = self.bili_dynamic_comment(id, raw, 0, 0)?;
                    }
                }
                let mut assets = self.locators.batch();
                super::browsing::replace_media(
                    &mut result,
                    &mut assets,
                    &self.scope(Platform::Bilibili),
                    &mut 0,
                    "bilibili-comment-media",
                )?;
                assets.commit();
                let next = if root == 0 {
                    if data["cursor"]["is_end"] == false {
                        data["cursor"]["next"]
                            .as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .and_then(|n| n.checked_add(1))
                            .filter(|n| *n > page)
                    } else {
                        None
                    }
                } else {
                    data["page"]["count"]
                        .as_u64()
                        .filter(|n| *n > u64::from(page) * u64::from(limit))
                        .map(|_| page + 1)
                };
                result["continuation"] = match next {
                    Some(next) => json!(self.locators.put(
                        &self.scope(Platform::Bilibili),
                        CURSOR,
                        &Cursor {
                            action: action.into(),
                            target: target.unwrap().clone(),
                            page: next
                        }
                    )?),
                    None => Value::Null,
                };
                Ok(result)
            }
            "danmaku_read" => {
                let Some(Target::Video { video }) = target else {
                    return Err("danmaku_read needs a video target".into());
                };
                self.bili_danmaku_read(video, p)
            }
            _ => Err("read action and target do not match".into()),
        }
    }
    fn bili_dynamic_comment(
        &self,
        id: &str,
        raw: &Value,
        expected_root: u64,
        depth: u8,
    ) -> Result<Value> {
        let rpid = raw["rpid"]
            .as_u64()
            .or_else(|| raw["rpid_str"].as_str()?.parse().ok())
            .filter(|v| *v > 0)
            .ok_or("dynamic comment identity missing")?;
        let root = raw["root"]
            .as_u64()
            .ok_or("dynamic comment relationship absent")?;
        if root != expected_root {
            return Err("dynamic comment belongs to a different discussion root".into());
        }
        let target = Target::DynamicComment {
            id: id.into(),
            root: if root == 0 { rpid } else { root },
            parent: rpid,
        };
        let mut media = Vec::new();
        if let Some(pictures) = raw["content"]["pictures"].as_array() {
            if pictures.len() > 9 {
                return Err("dynamic comment picture limit".into());
            }
            for pic in pictures {
                if let Some(url) = pic["img_src"].as_str() {
                    media.push(json!({"kind":"picture","url":url,"width":pic["img_width"],"height":pic["img_height"]}));
                }
            }
        }
        if let Some(emotes) = raw["content"]["emote"].as_object() {
            if emotes.len() > 100 {
                return Err("dynamic comment emote limit".into());
            }
            for (text, e) in emotes {
                if let Some(url) = e["url"].as_str() {
                    media.push(json!({"kind":"emote","text":text,"url":url}));
                }
            }
        }
        let mut result = json!({"target_ref":self.save_bili_feature(&target)?,"author":{"name":raw["member"]["uname"],"mid":raw["member"]["mid"]},"text":raw["content"]["message"],"like_count":raw["like"],"created_at":raw["ctime"],"reply_count":raw["rcount"],"media":media,"preview_replies":[],"content_is_untrusted":true,"next_tools":[{"tool":"social_bilibili","action":"dynamic_discussion"},{"tool":"social_bilibili","action":"dynamic_reply"}]});
        if depth == 0 {
            if let Some(replies) = raw["replies"].as_array() {
                if replies.len() > 20 {
                    return Err("dynamic reply preview limit".into());
                }
                result["preview_replies"] = json!(
                    replies
                        .iter()
                        .map(|r| self.bili_dynamic_comment(
                            id,
                            r,
                            if root == 0 { rpid } else { root },
                            1
                        ))
                        .collect::<Result<Vec<_>>>()?
                );
            }
        }
        Ok(result)
    }
    fn bili_feature_write(
        &mut self,
        action: &str,
        target: Option<&Target>,
        p: &Value,
    ) -> Result<Value> {
        let id = required_str(p, "operation_id")?;
        let scope = self.scope(Platform::Bilibili);
        let mut input = p.clone();
        for k in ["operation_id", "account"] {
            input.as_object_mut().unwrap().remove(k);
        }
        let outbox = Outbox::open(&self.state)?;
        let mut existing = None;
        // Resolve durable outcomes before touching credentials or the network.
        if outbox.exists(id)? {
            let mut r = outbox.load(id, &scope)?;
            if r.request
                .as_ref()
                .is_none_or(|r| r["kind"] != "bilibili_feature" || r["input"] != input)
            {
                return Err("operation_id belongs to another Bilibili action or payload".into());
            }
            if r.status == Status::Attempting {
                r.status = Status::Unknown;
                r.error = Some("previous process interrupted; no write was repeated".into());
                outbox.finish(&r)?;
            }
            if r.status != Status::Prepared {
                return Ok(outcome(&r, true));
            }
            existing = Some(r);
        }
        let message = p
            .get("message")
            .map(|v| v.as_str().ok_or("message must be text"))
            .transpose()?
            .unwrap_or("");
        let command = match (action, target) {
            ("favorite", Some(Target::Video { video })) => {
                let (scope2, folder) = self.load_bili_feature(required_str(p, "folder_ref")?)?;
                if scope2 != scope {
                    return Err("favorite folder belongs to another account".into());
                }
                let Target::Folder { id } = folder else {
                    return Err("favorite requires a returned folder_ref".into());
                };
                Command::Favorite {
                    video: video.clone(),
                    folder_id: id,
                    add: boolean(p, "add", true)?,
                }
            }
            ("coin", Some(Target::Video { video })) => Command::Coin {
                video: video.clone(),
                count: int(p, "count", 1)?,
            },
            ("danmaku", Some(Target::Video { video })) => Command::Danmaku {
                video: video.clone(),
                page: int(p, "page", 1)?,
                progress_ms: number(p, "progress_ms", 0)?,
                text: message.into(),
                color: int(p, "color", 0xffffff)?,
                mode: int(p, "mode", 1)?,
            },
            ("dynamic_like", Some(Target::Dynamic { id } | Target::DynamicComment { id, .. })) => {
                Command::DynamicLike {
                    dynamic_id: id.clone(),
                    like: boolean(p, "like", true)?,
                }
            }
            ("dynamic_reply", Some(Target::Dynamic { id })) => Command::DynamicReply {
                dynamic_id: id.clone(),
                message: message.into(),
                root: 0,
                parent: 0,
            },
            ("dynamic_reply", Some(Target::DynamicComment { id, root, parent })) => {
                Command::DynamicReply {
                    dynamic_id: id.clone(),
                    message: message.into(),
                    root: *root,
                    parent: *parent,
                }
            }
            ("dynamic_create", None) => Command::DynamicCreate {
                text: message.into(),
                pictures: vec![],
            },
            _ => return Err("Bilibili action does not match the returned exact target".into()),
        };
        if p.get("image_refs").is_some() && p.get("image_ref").is_some() {
            return Err("choose image_ref or image_refs".into());
        }
        let images = match p.get("image_refs") {
            None => match p.get("image_ref") {
                None => vec![],
                Some(Value::String(s)) => vec![s.clone()],
                _ => return Err("image_ref must be a cached opaque resource".into()),
            },
            Some(Value::Array(a)) if a.len() <= 9 => a
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_owned).ok_or_else(|| {
                        String::from("image_refs must contain cached opaque resources")
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            _ => return Err("image_refs must contain at most9 cached image resources".into()),
        };
        let pending_pictures = matches!(&command,Command::DynamicCreate{text,pictures} if text.trim().is_empty()&&pictures.is_empty()&&!images.is_empty());
        if !pending_pictures {
            command.validate().map_err(|e| e.to_string())?;
        }
        // Validate every local image before the first side effect, one at a time.
        for image in &images {
            let bytes = self.comment_image_bytes(image)?;
            self.bili
                .prepare_dynamic_image(&bytes)
                .map_err(|e| e.to_string())?;
        }
        let prepared = if pending_pictures {
            self.bili.prepare_dynamic_publication(message, images.len())
        } else {
            self.bili.prepare_feature(command.clone())
        }
        .map_err(|e| e.to_string())?;
        bind_actor(self, prepared.actor_mid())?;
        let actor = prepared.actor_mid();
        let native_target = prepared.target().clone();
        let before = prepared.state_before().clone();
        let mut record = if let Some(r) = existing {
            if r.actor_id != actor.to_string() || r.target != native_target {
                return Err("prepared feature account or target changed".into());
            }
            r
        } else {
            let mut r = Operation::new(
                id,
                scope.clone(),
                actor.to_string(),
                native_target.clone(),
                action,
            )?;
            r.request = Some(json!({"kind":"bilibili_feature","input":input,"command":command}));
            r.attempt_context = Some(json!({"state_before":before}));
            outbox.insert(&r)?;
            r
        };
        let satisfied = match &command {
            Command::Favorite { add, .. } => before["favorited"].as_bool() == Some(*add),
            Command::DynamicLike { like, .. } => before["liked"].as_bool() == Some(*like),
            _ => false,
        };
        if satisfied {
            record.status = Status::AlreadySatisfied;
            record.verification = Some(
                json!({"verified":true,"desired_state_already_present":true,"request_sent":false}),
            );
            outbox.finish(&record)?;
            return Ok(outcome(&record, false));
        }
        if let Command::Coin { count, .. } = &command {
            if before["coins"]
                .as_u64()
                .is_some_and(|n| n + u64::from(*count) > 2)
            {
                return Err(
                    "this coin amount exceeds the video's remaining per-account coin allowance"
                        .into(),
                );
            }
        }
        outbox.begin_attempt(&mut record)?;
        let mut executed = command.clone();
        if !images.is_empty() {
            let mut pictures = Vec::new();
            for image in &images {
                let uploaded = (|| {
                    let bytes = self.comment_image_bytes(image)?;
                    let p = self
                        .bili
                        .prepare_dynamic_image(&bytes)
                        .map_err(|e| e.to_string())?;
                    if p.actor_mid() != actor {
                        return Err("dynamic image actor changed".into());
                    }
                    self.bili.submit_dynamic_image(p).map_err(|e| e.to_string())
                })();
                match uploaded {
                    Ok(receipt) => {
                        pictures.push(receipt.into_picture().map_err(|e| e.to_string())?);
                        record.receipt = Some(
                            json!({"uploaded_images":pictures.len(),"publication_attempted":false}),
                        );
                    }
                    Err(error) => {
                        record.status = Status::Unknown;
                        record.error = Some(error);
                        outbox.finish(&record)?;
                        return Ok(outcome(&record, false));
                    }
                }
            }
            executed = Command::DynamicCreate {
                text: message.into(),
                pictures,
            };
        }
        let sent = if images.is_empty() {
            self.bili.submit_feature(prepared)
        } else {
            match self.bili.prepare_feature(executed.clone()) {
                Ok(p) if p.actor_mid() == actor && p.target() == &native_target => {
                    self.bili.submit_feature(p)
                }
                Ok(_) => Err(platform_bilibili::Error::CommentPreparationChanged),
                Err(e) => Err(e),
            }
        };
        let receipt = match sent {
            Ok(v) => v,
            Err(error) => {
                record.status = if matches!(
                    error,
                    platform_bilibili::Error::Platform(_)
                        | platform_bilibili::Error::RiskControl(_)
                        | platform_bilibili::Error::LoginRequired
                ) && images.is_empty()
                {
                    Status::Rejected
                } else {
                    Status::Unknown
                };
                record.error = Some(error.to_string());
                outbox.finish(&record)?;
                return Ok(outcome(&record, false));
            }
        };
        // Public projections contain acknowledgements, never private image URLs.
        let mut safe = json!({"acknowledged":true,"uploaded_images":images.len()});
        for key in [
            "dynamic_id",
            "rpid",
            "rpid_str",
            "dmid",
            "dmid_str",
            "count",
            "message",
        ] {
            if let Some(v) = receipt.get(key) {
                safe[key] = v.clone();
            }
        }
        if let Some(id) = receipt["dynamic_id"].as_str() {
            safe["target_ref"] = json!(self.bili_dynamic_feature_ref(id)?);
        }
        record.receipt = Some(safe);
        record.status = Status::Sent;
        record.error = None;
        outbox.finish(&record)?;
        match self
            .bili
            .feature_readback(&executed, &native_target, &receipt, actor, &before)
        {
            Ok(verified) => {
                if verified["verified"] == true {
                    record.status = Status::Verified
                }
                record.verification = Some(verified)
            }
            Err(e) => record.error = Some(format!("acknowledged; readback unavailable: {e}")),
        };
        outbox.finish(&record)?;
        Ok(outcome(&record, false))
    }
    fn bili_danmaku_read(&self, video: &str, p: &Value) -> Result<Value> {
        let page = int(p, "page", 1)?;
        let start = number(p, "start_ms", 0)?;
        let duration = number(p, "duration_ms", 360000)?;
        if duration == 0 || duration > 360000 || start > 86_400_000 {
            return Err("danmaku window must be at most360000ms within one day".into());
        }
        let end = start
            .checked_add(duration)
            .ok_or("danmaku window overflow")?;
        let first = u32::try_from(start / 360000 + 1).map_err(|_| "danmaku segment range")?;
        let last = u32::try_from((end - 1) / 360000 + 1).map_err(|_| "danmaku segment range")?;
        let mut items = Vec::new();
        for segment in first..=last {
            let rows = self
                .bili
                .danmaku_segment(
                    &video.parse::<VideoId>().map_err(|e| e.to_string())?,
                    page,
                    segment,
                )
                .map_err(|e| e.to_string())?;
            items.extend(
                rows.into_iter()
                    .filter(|r| r.progress_ms >= start && r.progress_ms < end),
            );
        }
        items.sort_by_key(|r| r.progress_ms);
        let bytes = serde_json::to_vec(&items).map_err(|e| e.to_string())?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("danmaku projection exceeds cache budget; request a shorter window".into());
        }
        let resource = self.store.import(
            &self.scope(Platform::Bilibili),
            "danmaku-window",
            "application/json",
            &mut bytes.as_slice(),
        )?;
        let cursor = DanmakuCursor {
            resource: resource.id,
            offset: 0,
            video: video.into(),
            page,
            start,
            end,
        };
        self.bili_danmaku_page(&cursor, p)
    }
    fn bili_danmaku_continue(&mut self, p: &Value, reference: &str) -> Result<Value> {
        if ["target_ref", "page", "start_ms", "duration_ms"]
            .iter()
            .any(|k| p.get(*k).is_some())
        {
            return Err("continue danmaku with continuation and optional limit only".into());
        }
        let scope = self.locators.scope_for(DANMAKU_CURSOR, reference)?;
        super::tasks::check_account(p, &scope)?;
        let cursor: DanmakuCursor = self.locators.get(&scope, DANMAKU_CURSOR, reference)?;
        self.with_task_scope(&scope, |rt| rt.bili_danmaku_page(&cursor, p))
    }
    fn bili_danmaku_page(&self, c: &DanmakuCursor, p: &Value) -> Result<Value> {
        let limit = usize::try_from(number(p, "limit", 20)?).map_err(|_| "invalid limit")?;
        if !(1..=100).contains(&limit) || c.offset > 12000 {
            return Err("danmaku page limit".into());
        }
        let (_, path) = self
            .store
            .resolve(&self.scope(Platform::Bilibili), &c.resource)?;
        let bytes = read_bounded(&path, 2 * 1024 * 1024)?;
        let items: Vec<platform_bilibili::DanmakuItem> =
            serde_json::from_slice(&bytes).map_err(|_| "invalid cached danmaku window")?;
        let next = c.offset.saturating_add(limit).min(items.len());
        let selected = items
            .get(c.offset..next)
            .ok_or("danmaku continuation offset")?;
        let continuation = if next < items.len() {
            Some(self.locators.put(
                &self.scope(Platform::Bilibili),
                DANMAKU_CURSOR,
                &DanmakuCursor {
                    offset: next,
                    ..c.clone()
                },
            )?)
        } else {
            None
        };
        Ok(
            json!({"items":selected,"continuation":continuation,"window":{"start_ms":c.start,"end_ms":c.end,"page":c.page},"content_is_untrusted":true,"content_writes":false}),
        )
    }
}
const DANMAKU_CURSOR: &str = "task-bilibili-danmaku-page";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DanmakuCursor {
    resource: String,
    offset: usize,
    video: String,
    page: u32,
    start: u64,
    end: u64,
}
