// SPDX-License-Identifier: MIT
//! Task-level Tieba discovery and typed writes. No platform URL or CSRF token is model input.
use super::tasks::{TaskTarget, check_account};
use super::*;
use crate::outbox::{Operation, Outbox, Status};
use platform_tieba::{ActionReceipt, ActionTarget, Forum, UploadedImage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const FORUM: &str = "task-tieba-forum";
const REPLY: &str = "task-tieba-reply";
const UPLOAD: &str = "task-tieba-uploaded";
const CONTINUATION: &str = "task-tieba-discovery-page";
#[derive(Clone, Serialize, Deserialize)]
struct ReplyLocation {
    thread: u64,
    parent: u64,
    child: Option<u64>,
}
#[derive(Serialize, Deserialize)]
struct Continuation {
    action: String,
    params: Value,
    page: u32,
    seen: BTreeSet<String>,
    pending: Vec<Value>,
    has_more: bool,
}

fn input(params: &Value) -> Result<Value> {
    let mut map = params
        .as_object()
        .ok_or("Tieba task parameters must be an object")?
        .clone();
    map.remove("operation_id");
    map.remove("account");
    Ok(Value::Object(map))
}
fn write_action(action: &str) -> bool {
    matches!(action, "create_thread" | "reply_floor" | "upload_image")
}
fn outcome(record: &Operation, reused: bool) -> Value {
    json!({"status":record.status,"scope":record.scope,"operation_id":record.id,"reused_operation":reused,"verified":record.status==Status::Verified,
        "acknowledged":matches!(record.status,Status::Sent|Status::Verified),"unknown":matches!(record.status,Status::Unknown|Status::Attempting),
        "automatic_resend":false,"error":record.error,"action":record.request.as_ref().and_then(|v|v.get("action")),
        "thread_id":record.receipt.as_ref().and_then(|r|r.get("thread_id")),"post_id":record.receipt.as_ref().and_then(|r|r.get("post_id"))})
}
fn images(params: &Value) -> Result<Vec<String>> {
    if params.get("image_ref").is_some() && params.get("image_refs").is_some() {
        return Err("choose image_ref or image_refs".into());
    }
    let refs = if let Some(v) = params.get("image_refs") {
        serde_json::from_value::<Vec<String>>(v.clone())
            .map_err(|_| "image_refs must be an array of opaque references")?
    } else {
        params
            .get("image_ref")
            .map(|v| {
                v.as_str()
                    .map(|v| vec![v.to_owned()])
                    .ok_or("image_ref must be text")
            })
            .transpose()?
            .unwrap_or_default()
    };
    if refs.len() > 4
        || refs.iter().any(|r| {
            r.len() != 32
                || !r
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err("at most four opaque image references are accepted".into());
    }
    Ok(refs)
}

impl Runtime {
    pub(super) fn tieba_forum_read_matches(&self, method: &str, params: &Value) -> bool {
        (method == "social.open" && params["reference"].as_str().is_some_and(|r| self.locators.scope_for(FORUM, r).is_ok()))
            || (method == "social.discover" && params["continuation"].as_str().is_some_and(|r| self.locators.scope_for(CONTINUATION, r).is_ok()))
    }
    pub(super) fn open_tieba_forum_read(&mut self, params: &Value) -> Result<Value> {
        if params.get("platform").is_some_and(|v| v != "tieba") {
            return Err("platform differs from the selected Tieba forum".into());
        }
        if params.get("source").is_some() || params.get("feed_ref").is_some() {
            return Err("selected forum continuation is separate from home or following feeds".into());
        }
        let mut request = params.clone();
        request["action"] = json!("forum");
        if let Some(reference) = params.get("reference") {
            request["forum_ref"] = reference.clone();
            request.as_object_mut().unwrap().remove("reference");
        }
        let mut result = self.task_tieba_features(&request)?;
        result["source"] = json!("tieba_selected_forum");
        if let Some(cursor) = result["continuation"].as_str() {
            result["next"] = json!({"tool":"social_discover","arguments":{"continuation":cursor}});
        }
        Ok(result)
    }
    pub(super) fn task_tieba_features(&mut self, params: &Value) -> Result<Value> {
        let action = required_str(params, "action")?;
        if write_action(action) {
            let id = required_str(params, "operation_id")?;
            let outbox = Outbox::open(&self.state)?;
            if outbox.exists(id)? {
                let record = outbox.inspect(id)?;
                check_account(params, &record.scope)?;
                if record.scope.platform != Platform::Tieba
                    || record.request.as_ref()
                        != Some(&json!({"action":action,"input":input(params)?}))
                {
                    return Err(
                        "operation_id belongs to a different action, account or payload".into(),
                    );
                }
                return self.with_task_scope(&record.scope, |runtime| {
                    runtime.project_tieba_operation(&record, true)
                });
            }
        }
        if let Some(reference) = params.get("continuation").and_then(Value::as_str) {
            let scope = self.locators.scope_for(CONTINUATION, reference)?;
            check_account(params, &scope)?;
            return self.with_task_scope(&scope, |rt| rt.tieba_discovery_page(params));
        }
        for (field, kind) in [
            ("forum_ref", FORUM),
            ("reply_ref", REPLY),
            ("subpost_ref", REPLY),
        ] {
            if let Some(reference) = params.get(field).and_then(Value::as_str) {
                let scope = self.locators.scope_for(kind, reference)?;
                check_account(params, &scope)?;
                return self.with_task_scope(&scope, |rt| rt.tieba_features_current(params));
            }
        }
        if let Some(reference) = params.get("target_ref").and_then(Value::as_str) {
            let (scope, target) = self.load_task_target(reference)?;
            if target.platform() != Platform::Tieba {
                return Err("Tieba action requires a Tieba target".into());
            }
            check_account(params, &scope)?;
            return self.with_task_scope(&scope, |rt| rt.tieba_features_current(params));
        }
        self.with_platform_account("tieba", params, write_action(action), |rt| {
            rt.tieba_features_current(params)
        })
    }

    fn tieba_features_current(&mut self, params: &Value) -> Result<Value> {
        match required_str(params, "action")? {
            "search" | "forum" => self.tieba_discovery_page(params),
            "open_reply" => {
                let reference = params
                    .get("reply_ref")
                    .or_else(|| params.get("subpost_ref"))
                    .and_then(Value::as_str)
                    .ok_or("open_reply needs reply_ref")?;
                let location: ReplyLocation =
                    self.locators
                        .get(&self.scope(Platform::Tieba), REPLY, reference)?;
                let request = if let Some(child) = location.child {
                    json!({"thread":location.thread.to_string(),"post":location.parent,"comment":child})
                } else {
                    json!({"thread":location.thread.to_string(),"post":location.parent,"page":1})
                };
                let mut result = self.call_tieba(
                    if location.child.is_some() {
                        "tieba.comment_context"
                    } else {
                        "tieba.comments"
                    },
                    &request,
                )?;
                self.annotate_task_content(
                    &mut result,
                    &Platform::Tieba,
                    None,
                    Some(&location.thread.to_string()),
                )?;
                Ok(
                    json!({"status":"ok","scope":self.scope(Platform::Tieba),"reply_ref":reference,"content":result}),
                )
            }
            action if write_action(action) => self.tieba_write_feature(params),
            _ => Err("unknown Tieba task action".into()),
        }
    }

    fn tieba_discovery_page(&mut self, params: &Value) -> Result<Value> {
        let scope = self.scope(Platform::Tieba);
        let limit = usize::try_from(number(params, "limit", 20)?).map_err(|_| "invalid limit")?;
        if !(1..=50).contains(&limit) {
            return Err("Tieba limit must be 1..50".into());
        }
        let mut state = if let Some(reference) = params.get("continuation").and_then(Value::as_str)
        {
            self.locators
                .get::<Continuation>(&scope, CONTINUATION, reference)?
        } else {
            let action = required_str(params, "action")?;
            if !matches!(action, "search" | "forum") {
                return Err("continuation belongs to Tieba discovery".into());
            }
            Continuation {
                action: action.into(),
                params: params.clone(),
                page: 1,
                seen: BTreeSet::new(),
                pending: Vec::new(),
                has_more: true,
            }
        };
        if state.action != required_str(params, "action")? {
            return Err("continuation action mismatch".into());
        }
        let mut metadata = json!({});
        let mut stop = None;
        if state.pending.is_empty() && state.has_more {
            let forum =
                if let Some(reference) = state.params.get("forum_ref").and_then(Value::as_str) {
                    Some(self.locators.get::<Forum>(&scope, FORUM, reference)?.name)
                } else {
                    state
                        .params
                        .get("forum")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                };
            if state.action == "forum" {
                let forum = forum.ok_or("forum action needs a forum name or forum_ref")?;
                let sort = match state
                    .params
                    .get("order")
                    .and_then(Value::as_str)
                    .unwrap_or("reply")
                {
                    "reply" => 6,
                    "newest" => 1,
                    "hot" => 3,
                    _ => return Err("forum order must be reply, newest or hot".into()),
                };
                let featured = state
                    .params
                    .get("featured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let page = self
                    .tieba_client()?
                    .forum_threads(&forum, state.page, 50, sort, featured)
                    .map_err(|e| e.to_string())?;
                let forum_ref = self.locators.put_reusable(&scope, FORUM, &page.forum)?;
                metadata = json!({"forum":page.forum,"forum_ref":forum_ref,"page":page.page});
                state.has_more = page.page.has_more;
                if page.page.current_page != 0 && page.page.current_page != u64::from(state.page) {
                    state.has_more = false;
                    stop = Some("server_page_mismatch")
                }
                for thread in page.threads {
                    let key = format!("thread:{}", thread.thread_id.0);
                    if !state.seen.insert(key) {
                        continue;
                    }
                    let target = self.save_task_target(&TaskTarget::Thread {
                        thread: thread.thread_id.0.to_string(),
                    })?;
                    let mut item = serde_json::to_value(&thread).map_err(|e| e.to_string())?;
                    item["target_ref"] = json!(target);
                    // SDK media serde hides URLs. Re-open the thread before selecting media.
                    item["media_expansion"] = json!("open_target_for_media");
                    state.pending.push(item);
                }
            } else {
                let query = required_str(&state.params, "query")?;
                let page = if let Some(forum) = forum {
                    self.tieba_client()?.search_forum(
                        &forum,
                        query,
                        state.page,
                        50,
                        state
                            .params
                            .get("only_threads")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    )
                } else {
                    self.tieba_client()?.search_global(
                        query,
                        state.page,
                        50,
                        state.params.get("order").and_then(Value::as_str) == Some("newest"),
                    )
                }
                .map_err(|e| e.to_string())?;
                metadata = json!({"query":page.query,"forum":page.forum,"page":page.current_page,"has_more":page.has_more});
                state.has_more = page.has_more == Some(true);
                if page.has_more.is_none() {
                    stop = Some("server_pagination_unknown")
                }
                if page.current_page != state.page {
                    state.has_more = false;
                    stop = Some("server_page_mismatch")
                }
                for hit in page.items {
                    let key = format!("{}:{}", hit.thread_id.0, hit.post_id.unwrap_or(0));
                    if !state.seen.insert(key) {
                        continue;
                    }
                    let mut item = serde_json::to_value(&hit).map_err(|e| e.to_string())?;
                    item["target_ref"] = json!(self.save_task_target(&TaskTarget::Thread {
                        thread: hit.thread_id.0.to_string()
                    })?);
                    if let Some(parent) = hit.parent_post_id.or(hit.post_id) {
                        item["reply_ref"] = json!(self.locators.put_reusable(
                            &scope,
                            REPLY,
                            &ReplyLocation {
                                thread: hit.thread_id.0,
                                parent,
                                child: hit.parent_post_id.and(hit.post_id)
                            }
                        )?);
                        item["open_action"] = json!("open_reply");
                    }
                    state.pending.push(item);
                }
            }
            state.page = state.page.checked_add(1).ok_or("Tieba page exhausted")?;
            if state.pending.is_empty() {
                state.has_more = false;
                stop = Some("no_new_items")
            }
            if state.page > 100000 || state.seen.len() > 2048 {
                state.has_more = false;
                stop = Some("discovery_budget")
            }
        }
        let take = limit.min(state.pending.len());
        let items = state.pending.drain(..take).collect::<Vec<_>>();
        let more = !state.pending.is_empty() || state.has_more;
        let continuation = if more {
            Some(self.locators.put(&scope, CONTINUATION, &state)?)
        } else {
            None
        };
        Ok(
            json!({"status":"ok","scope":scope,"items":items,"metadata":metadata,"continuation":continuation,"has_more":more,"stop_reason":stop,"media_downloaded":false}),
        )
    }

    fn project_tieba_operation(&self, record: &Operation, reused: bool) -> Result<Value> {
        let mut value = outcome(record, reused);
        if matches!(record.status, Status::Sent | Status::Verified) {
            if record
                .request
                .as_ref()
                .and_then(|r| r.get("action"))
                .and_then(Value::as_str)
                == Some("upload_image")
            {
                if let Some(raw) = record.receipt.as_ref() {
                    let image: UploadedImage = serde_json::from_value(raw.clone())
                        .map_err(|_| "invalid saved Tieba image receipt")?;
                    let reference = self.locators.put_reusable(&record.scope, UPLOAD, &image)?;
                    value["uploaded_image_ref"] = json!(reference);
                    value["width"] = json!(image.width);
                    value["height"] = json!(image.height);
                }
            } else if let Some(thread) = record
                .receipt
                .as_ref()
                .and_then(|r| r.get("thread_id"))
                .and_then(Value::as_u64)
            {
                value["target_ref"] = json!(self.save_task_target(&TaskTarget::Thread {
                    thread: thread.to_string()
                })?);
            }
        }
        Ok(value)
    }

    fn tieba_write_feature(&mut self, params: &Value) -> Result<Value> {
        let action = required_str(params, "action")?;
        let id = required_str(params, "operation_id")?;
        let scope = self.scope(Platform::Tieba);
        if scope.account == "public" {
            return Err("Tieba writing requires a personal account".into());
        }
        let (path, mut session) = self.tieba_comment_session()?;
        let auth = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
        let actor = auth
            .self_identity(&mut session)
            .map_err(|e| e.to_string())?;
        let image_refs = images(params)?;
        let mut forum = if let Some(reference) = params.get("forum_ref").and_then(Value::as_str) {
            Some(self.locators.get::<Forum>(&scope, FORUM, reference)?)
        } else if let Some(name) = params.get("forum").and_then(Value::as_str) {
            Some(
                auth.resolve_forum(&mut session, name)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let reply = if action == "reply_floor" {
            if let Some(reference) = params
                .get("reply_ref")
                .or_else(|| params.get("subpost_ref"))
                .and_then(Value::as_str)
            {
                Some(
                    self.locators
                        .get::<ReplyLocation>(&scope, REPLY, reference)?,
                )
            } else {
                let reference = required_str(params, "target_ref")?;
                let (target_scope, target) = self.load_task_target(reference)?;
                if target_scope != scope {
                    return Err("reply target belongs to another account".into());
                }
                let TaskTarget::TiebaPost { thread, post } = target else {
                    return Err("reply_floor needs an exact floor target_ref or reply_ref".into());
                };
                Some(ReplyLocation {
                    thread: thread.parse().map_err(|_| "invalid thread")?,
                    parent: post,
                    child: None,
                })
            }
        } else {
            None
        };
        let message = if action == "upload_image" {
            "upload_image"
        } else {
            required_str(params, "message")?
        };
        if message.trim().is_empty() || message.chars().count() > 1000 || message.contains('\0') {
            return Err("message must contain 1..1000 characters".into());
        }
        if action == "create_thread" {
            let title = required_str(params, "title")?;
            if title.trim().is_empty() || title.len() > 300 || title.chars().any(char::is_control) {
                return Err("thread title is empty or too large".into());
            }
        }
        if action == "upload_image" && image_refs.len() != 1 {
            return Err("upload_image needs exactly one image_ref".into());
        }
        if action != "reply_floor" && forum.is_none() {
            return Err("forum or forum_ref is required".into());
        }
        let target = if let Some(location) = &reply {
            json!({"kind":"floor_reply","thread":location.thread,"parent":location.parent,"child":location.child})
        } else {
            json!({"kind":action,"forum":forum})
        };
        let outbox = Outbox::open(&self.state)?;
        let mut record = Operation::new(
            id,
            scope.clone(),
            actor.user_id.to_string(),
            target,
            message,
        )?;
        record.request = Some(json!({"action":action,"input":input(params)?}));
        outbox.insert(&record)?;
        // Preparation uses reads only. Media upload and final submit are both
        // covered by the durable attempt; a lost outcome never re-enters here.
        let prepared = if let Some(reply) = reply.as_ref() {
            Some(
                auth.prepare_floor_reply(
                    &mut session,
                    platform_tieba::ThreadId(reply.thread),
                    reply.parent,
                    reply.child,
                    message,
                )
                .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        if let Some(prepared) = prepared.as_ref() {
            if let ActionTarget::FloorReply {
                forum_id,
                forum_name,
                ..
            } = prepared.target()
            {
                if forum.as_ref().is_some_and(|f| f.id != *forum_id) {
                    return Err("reply belongs to another forum".into());
                }
                forum = Some(Forum {
                    id: *forum_id,
                    name: forum_name.clone(),
                });
            }
        }
        outbox.begin_attempt(&mut record)?;
        let execution = (|| -> std::result::Result<Value, platform_tieba::Error> {
            let mut uploaded = Vec::new();
            for reference in &image_refs {
                if self.locators.scope_for(UPLOAD, reference).ok().as_ref() == Some(&scope) {
                    let image: UploadedImage = self
                        .locators
                        .get(&scope, UPLOAD, reference)
                        .map_err(|_| platform_tieba::Error::InvalidConfig)?;
                    if image.forum_id != forum.as_ref().unwrap().id {
                        return Err(platform_tieba::Error::InvalidConfig);
                    }
                    uploaded.push(image);
                    continue;
                }
                let resource = self
                    .resolve_send_image(reference)
                    .map_err(|_| platform_tieba::Error::InvalidConfig)?;
                let image_path = self
                    .image_path(&resource)
                    .map_err(|_| platform_tieba::Error::InvalidConfig)?;
                let bytes = read_bounded(&image_path, 2 * 1024 * 1024)
                    .map_err(|_| platform_tieba::Error::Limit)?;
                let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    "image/png"
                } else {
                    "image/jpeg"
                };
                uploaded.push(auth.upload_image(
                    &mut session,
                    forum.as_ref().unwrap().id,
                    &bytes,
                    mime,
                )?);
            }
            if action == "upload_image" {
                return serde_json::to_value(&uploaded[0])
                    .map_err(|_| platform_tieba::Error::InvalidConfig);
            }
            let prepared = if let Some(prepared) = prepared {
                auth.attach_floor_images(&mut session, prepared, uploaded)?
            } else {
                auth.prepare_thread(
                    &mut session,
                    &forum.as_ref().unwrap().name,
                    params["title"].as_str().unwrap(),
                    message,
                    uploaded,
                )?
            };
            let receipt = auth.send_action(&mut session, prepared)?;
            serde_json::to_value(receipt).map_err(|_| platform_tieba::Error::WriteResultUnknown)
        })();
        match execution {
            Ok(receipt) => {
                record.status = Status::Sent;
                record.receipt = Some(receipt);
            }
            Err(platform_tieba::Error::Platform(code)) => {
                record.status = Status::Rejected;
                record.error = Some(format!("Tieba rejected action ({code})"));
            }
            Err(error) => {
                record.status = Status::Unknown;
                record.error = Some(error.to_string());
            }
        }
        outbox.finish(&record)?;
        session.save(&path).map_err(|e| e.to_string())?;
        if action != "upload_image" && record.status == Status::Sent {
            let receipt: ActionReceipt = serde_json::from_value(record.receipt.clone().unwrap())
                .map_err(|_| "invalid action receipt")?;
            if auth.verify_action(&mut session, &receipt).unwrap_or(false) {
                record.status = Status::Verified;
                record.verification = Some(
                    json!({"exact_actor_and_content":true,"parent_checked":matches!(receipt.target,ActionTarget::FloorReply{..})}),
                );
                outbox.finish(&record)?;
            }
        }
        self.project_tieba_operation(&record, false)
    }
}
