// SPDX-License-Identifier: MIT
//! Bounded discussion reading. Provider cursors and cached-page tails stay in
//! account-scoped continuations; the model only passes capability references.
use super::tasks::{TaskTarget, check_account};
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const NAMESPACE: &str = "task-discussion";
const MAX_SEEN: usize = 2048;
const MAX_PAGES: usize = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Position {
    Cursor { value: Option<String> },
    Page { number: u32 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    id: String,
    resource: String,
    pointer: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Progress {
    schema: u32,
    scope: Scope,
    target_ref: String,
    target: TaskTarget,
    order: String,
    position: Position,
    seen: BTreeSet<String>,
    pending: Vec<Pending>,
    context: Value,
    #[serde(default)]
    context_ref: Option<Pending>,
    stopped: Option<String>,
    platform_has_more: Option<bool>,
    total_pages_read: u32,
}
impl Progress {
    fn new(scope: Scope, target_ref: String, target: TaskTarget, order: String) -> Result<Self> {
        let position = match &target {
            TaskTarget::Video { .. } => Position::Cursor { value: None },
            TaskTarget::BiliComment { .. }
            | TaskTarget::Thread { .. }
            | TaskTarget::TiebaPost { .. } => Position::Page { number: 1 },
            _ => {
                return Err(
                    "this target has no supported discussion; open a video, thread or comment"
                        .into(),
                );
            }
        };
        Ok(Self {
            schema: 1,
            scope,
            target_ref,
            target,
            order,
            position,
            seen: BTreeSet::new(),
            pending: Vec::new(),
            context: Value::Null,
            context_ref: None,
            stopped: None,
            platform_has_more: None,
            total_pages_read: 0,
        })
    }
    fn validate(&self, scope: &Scope) -> Result<()> {
        if self.schema != 1
            || &self.scope != scope
            || self.target.platform() != scope.platform
            || !matches!(self.order.as_str(), "hot" | "time")
            || self.seen.len() > MAX_SEEN
            || self.pending.len() > 100
        {
            return Err("invalid or out-of-scope discussion continuation".into());
        }
        match (&self.target, &self.position) {
            (TaskTarget::Video { .. }, Position::Cursor { value })
                if value
                    .as_ref()
                    .is_none_or(|v| v.len() <= 4096 && !v.chars().any(char::is_control)) => {}
            (
                TaskTarget::BiliComment { .. }
                | TaskTarget::Thread { .. }
                | TaskTarget::TiebaPost { .. },
                Position::Page { number },
            ) if (1..=100_000).contains(number) => {}
            _ => return Err("discussion continuation target/page mismatch".into()),
        }
        if self.seen.iter().any(|id| id.is_empty() || id.len() > 64)
            || self.pending.iter().any(|p| {
                p.id.is_empty()
                    || p.id.len() > 64
                    || p.pointer.len() > 256
                    || !p.pointer.starts_with('/')
            })
        {
            return Err("invalid discussion progress references".into());
        }
        Ok(())
    }

    fn offer(&mut self, frame: Frame) {
        self.context = frame.context;
        self.context_ref = frame.context_ref;
        self.platform_has_more = frame.has_more;
        let mut added = 0usize;
        for pending in frame.entries {
            if self.seen.contains(&pending.id) {
                continue;
            }
            if self.seen.len() >= MAX_SEEN {
                self.stopped = Some("seen_limit".into());
                break;
            }
            self.seen.insert(pending.id.clone());
            self.pending.push(pending);
            added += 1;
        }
        if self.stopped.is_none() {
            self.stopped = if frame.truncated {
                Some("provider_page_truncated".into())
            } else if frame.has_more == Some(false) {
                Some("platform_end".into())
            } else if frame.empty {
                Some("empty_page".into())
            } else if added == 0 {
                Some("no_new_items".into())
            } else if self.seen.len() >= MAX_SEEN {
                Some("seen_limit".into())
            } else if frame.next.is_none() {
                Some("pagination_unavailable".into())
            } else {
                None
            };
        }
        if let Some(next) = frame.next {
            self.position = next;
        }
    }
}

struct Frame {
    entries: Vec<Pending>,
    next: Option<Position>,
    has_more: Option<bool>,
    empty: bool,
    truncated: bool,
    context: Value,
    context_ref: Option<Pending>,
}

fn selected_order(params: &Value) -> Result<String> {
    match params.get("order") {
        None => Ok("hot".into()),
        Some(Value::String(v)) if matches!(v.as_str(), "hot" | "time") => Ok(v.clone()),
        _ => Err("discussion order must be hot or time".into()),
    }
}
fn item_id(value: &Value) -> Result<String> {
    value
        .get("rpid")
        .or_else(|| value.get("id"))
        .and_then(Value::as_u64)
        .filter(|v| *v > 0 && *v <= i64::MAX as u64)
        .map(|v| v.to_string())
        .ok_or("discussion item identity missing".into())
}
fn entries(data: &Value, field: &str, prefix: &str, resource: &str) -> Result<Vec<Pending>> {
    let rows = data
        .get(field)
        .and_then(Value::as_array)
        .ok_or("discussion provider list missing")?;
    if rows.len() > 100 {
        return Err("discussion provider page limit".into());
    }
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            Ok(Pending {
                id: item_id(value)?,
                resource: resource.into(),
                pointer: format!("{prefix}/{field}/{index}"),
            })
        })
        .collect()
}
fn optional_bool(data: &Value, key: &str) -> Result<Option<bool>> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(*v)),
        _ => Err("discussion provider flag malformed".into()),
    }
}
fn optional_string(data: &Value, key: &str) -> Result<Option<String>> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) if v.len() <= 4096 && !v.chars().any(char::is_control) => {
            Ok(Some(v.clone()))
        }
        _ => Err("discussion provider cursor malformed".into()),
    }
}

impl Runtime {
    pub(super) fn task_discussion(&mut self, params: &Value) -> Result<Value> {
        let dynamic = params
            .get("target_ref")
            .and_then(Value::as_str)
            .is_some_and(|id| self.locators.scope_for("task-bilibili-feature", id).is_ok())
            || params
                .get("continuation")
                .and_then(Value::as_str)
                .is_some_and(|id| {
                    self.locators
                        .scope_for("task-bilibili-feature-page", id)
                        .is_ok()
                });
        if dynamic {
            let mut request = params.clone();
            request.as_object_mut().unwrap().remove("order");
            request["action"] = json!("dynamic_discussion");
            return self.call_bilibili_features(&request);
        }
        let limit = usize::try_from(number(params, "limit", 20)?)
            .map_err(|_| "invalid discussion limit")?;
        if !(1..=20).contains(&limit) {
            return Err("discussion limit must be 1..20".into());
        }
        let input = params
            .as_object()
            .ok_or("discussion parameters must be an object")?;
        if input.keys().any(|k| {
            !matches!(
                k.as_str(),
                "target_ref" | "continuation" | "order" | "limit" | "account"
            )
        }) {
            return Err("discussion accepts target_ref or continuation; provider IDs and page parameters stay inside Runtime".into());
        }
        let has_target = params.get("target_ref").is_some();
        let has_continuation = params.get("continuation").is_some();
        if has_target == has_continuation {
            return Err("provide exactly one of target_ref or continuation".into());
        }
        let mut progress = if has_continuation {
            let reference = required_str(params, "continuation")?;
            let scope = self.locators.scope_for(NAMESPACE, reference)?;
            check_account(params, &scope)?;
            let saved: Progress = self.locators.get(&scope, NAMESPACE, reference)?;
            saved.validate(&scope)?;
            let (target_scope, target) = self.load_task_target(&saved.target_ref)?;
            if target_scope != scope || target != saved.target {
                return Err("discussion target changed or belongs to another scope".into());
            }
            if params.get("order").is_some() && selected_order(params)? != saved.order {
                return Err(
                    "continuation order is fixed; start a new target discussion to change it"
                        .into(),
                );
            }
            saved
        } else {
            let reference = required_str(params, "target_ref")?;
            let (scope, target) = self.load_task_target(reference)?;
            check_account(params, &scope)?;
            Progress::new(scope, reference.into(), target, selected_order(params)?)?
        };
        let scope = progress.scope.clone();
        self.with_task_scope(&scope, |runtime| {
            runtime.read_task_discussion(&mut progress, limit)
        })
    }

    fn read_task_discussion(&mut self, progress: &mut Progress, limit: usize) -> Result<Value> {
        progress.validate(&self.scope(progress.target.platform()))?;
        let mut output = Vec::new();
        let mut pages_read = 0usize;
        let mut error = None;
        let buffered_only = !progress.pending.is_empty();
        let mut page_cache: Option<(String, Value)> = None;
        loop {
            while output.len() < limit && !progress.pending.is_empty() {
                let pending = progress.pending[0].clone();
                if page_cache
                    .as_ref()
                    .is_none_or(|(id, _)| id != &pending.resource)
                {
                    let loaded = (|| -> Result<Value> {
                        let (_, path) = self.store.resolve(&progress.scope, &pending.resource)?;
                        serde_json::from_slice(&read_bounded(&path, 2 * 1024 * 1024)?)
                            .map_err(|_| "cached discussion page malformed".into())
                    })();
                    match loaded {
                        Ok(value) => page_cache = Some((pending.resource.clone(), value)),
                        Err(message) => {
                            progress.stopped = Some("cached_page_unavailable".into());
                            error = Some(message);
                            progress.pending.clear();
                            break;
                        }
                    }
                }
                let mut item = page_cache
                    .as_ref()
                    .unwrap()
                    .1
                    .pointer(&pending.pointer)
                    .cloned()
                    .ok_or("cached discussion item missing")?;
                if item_id(&item)? != pending.id {
                    return Err("cached discussion item identity changed".into());
                }
                let (video, thread) = match &progress.target {
                    TaskTarget::Video { video } | TaskTarget::BiliComment { video, .. } => {
                        (Some(video.as_str()), None)
                    }
                    TaskTarget::Thread { thread } | TaskTarget::TiebaPost { thread, .. } => {
                        (None, Some(thread.as_str()))
                    }
                    _ => unreachable!(),
                };
                self.annotate_task_content(&mut item, &progress.scope.platform, video, thread)?;
                output.push(item);
                progress.pending.remove(0);
            }
            if output.len() >= limit
                || progress.stopped.is_some()
                || pages_read >= MAX_PAGES
                || (buffered_only && progress.pending.is_empty())
            {
                break;
            }
            pages_read += 1;
            let frame = match self.discussion_frame(progress) {
                Ok(frame) => frame,
                Err(message) => {
                    progress.stopped = Some("provider_error".into());
                    error = Some(message);
                    break;
                }
            };
            progress.total_pages_read = progress.total_pages_read.saturating_add(1);
            progress.offer(frame);
        }
        if output.is_empty() {
            if let Some(message) = error.as_ref() {
                return Err(message.clone());
            }
        }
        let can_continue = !progress.pending.is_empty() || progress.stopped.is_none();
        let continuation = if can_continue {
            progress.validate(&progress.scope)?;
            Some(self.locators.put(&progress.scope, NAMESPACE, progress)?)
        } else {
            None
        };
        let stop_reason = if !progress.pending.is_empty() {
            "item_limit".to_owned()
        } else if let Some(reason) = progress.stopped.as_ref() {
            reason.clone()
        } else if buffered_only {
            "buffer_drained".to_owned()
        } else if pages_read >= MAX_PAGES {
            "page_budget".to_owned()
        } else {
            "item_limit".to_owned()
        };
        let mut context = progress.context.clone();
        if let Some(reference) = progress.context_ref.as_ref() {
            let value = if let Some((_, value)) = page_cache
                .as_ref()
                .filter(|(id, _)| id == &reference.resource)
            {
                value.clone()
            } else {
                let (_, path) = self.store.resolve(&progress.scope, &reference.resource)?;
                serde_json::from_slice::<Value>(&read_bounded(&path, 2 * 1024 * 1024)?)
                    .map_err(|_| "cached root comment malformed")?
            };
            let mut root = value
                .pointer(&reference.pointer)
                .cloned()
                .ok_or("cached root comment missing")?;
            if item_id(&root)? != reference.id {
                return Err("root comment cache identity changed".into());
            }
            let (video, thread) = match &progress.target {
                TaskTarget::BiliComment { video, .. } => (Some(video.as_str()), None),
                TaskTarget::TiebaPost { thread, .. } => (None, Some(thread.as_str())),
                _ => (None, None),
            };
            self.annotate_task_content(&mut root, &progress.scope.platform, video, thread)?;
            context[if progress.scope.platform == Platform::Tieba {
                "parent_post"
            } else {
                "root_comment"
            }] = root;
        }
        let new_count = output.len();
        Ok(
            json!({"status":if error.is_some(){"partial"}else{"ok"},"scope":progress.scope,"target_ref":progress.target_ref,
            "items":output,"context":context,"continuation":continuation,"stop_reason":stop_reason,
            "new_item_count":new_count,"seen_item_count":progress.seen.len(),"provider_pages_read":pages_read,
            "platform_has_more":progress.platform_has_more,"error":error,"media_downloaded":false,
            "discussion_scope":if matches!(progress.target,TaskTarget::BiliComment{..}){"root_reply_thread"}else{"target_discussion"}}),
        )
    }

    fn discussion_frame(&mut self, progress: &Progress) -> Result<Frame> {
        let (method, params, bili) = match (&progress.target, &progress.position) {
            (TaskTarget::Video { video }, Position::Cursor { value }) => {
                let mut params = json!({"video":video,"order":progress.order,"limit":20});
                if let Some(cursor) = value {
                    params["cursor"] = json!(cursor);
                }
                ("bilibili.comments", params, true)
            }
            (TaskTarget::BiliComment { video, root, .. }, Position::Page { number }) => (
                "bilibili.comment_replies",
                json!({"video":video,"root":root,"page":number,"limit":20}),
                true,
            ),
            (TaskTarget::Thread { thread }, Position::Page { number }) => (
                "tieba.thread",
                json!({"thread":thread,"page":number}),
                false,
            ),
            (TaskTarget::TiebaPost { thread, post }, Position::Page { number }) => (
                "tieba.comments",
                json!({"thread":thread,"post":post,"page":number}),
                false,
            ),
            _ => return Err("unsupported discussion continuation".into()),
        };
        let result = self.call(method, &params)?;
        let resource = result
            .pointer("/manifest/id")
            .and_then(Value::as_str)
            .ok_or("discussion provider manifest missing")?;
        if bili {
            let data = result
                .pointer("/page/data")
                .ok_or("discussion provider page missing")?;
            let mut rows = entries(data, "items", "/data", resource)?;
            let empty = rows.is_empty();
            if method == "bilibili.comments" {
                rows.extend(entries(data, "pinned", "/data", resource)?);
                rows.extend(entries(data, "hots", "/data", resource)?);
            }
            let has_more = optional_bool(data, "has_more")?;
            let context_ref = if method == "bilibili.comment_replies" {
                let root = data
                    .get("root_comment")
                    .ok_or("discussion root context missing")?;
                Some(Pending {
                    id: item_id(root)?,
                    resource: resource.into(),
                    pointer: "/data/root_comment".into(),
                })
            } else {
                None
            };
            let next = if method == "bilibili.comments" {
                optional_string(data, "next_cursor")?
                    .map(|value| Position::Cursor { value: Some(value) })
            } else {
                data.get("next_page")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0 && *n <= 100_000)
                    .map(|number| Position::Page { number })
            };
            Ok(Frame {
                entries: rows,
                next,
                has_more,
                empty,
                truncated: optional_bool(data, "truncated")?.unwrap_or(false),
                context_ref,
                context: json!({"aid":data["aid"],"total":data["total"],"focus_target_ref":progress.target_ref,
                    "selected_parent":match &progress.target {TaskTarget::BiliComment {parent,..}=>Some(*parent),_=>None},
                    "provider_pagination_note":data["pagination_note"],"order":progress.order}),
            })
        } else {
            let (data, prefix, field) = if method == "tieba.thread" {
                (
                    result
                        .pointer("/content/page")
                        .ok_or("Tieba page missing")?,
                    "/page",
                    "posts",
                )
            } else {
                (
                    result.get("content").ok_or("Tieba reply page missing")?,
                    "",
                    "comments",
                )
            };
            let rows = entries(data, field, prefix, resource)?;
            let empty = rows.is_empty();
            let page = data.get("page").ok_or("Tieba pagination missing")?;
            let current = page.get("current_page").and_then(Value::as_u64);
            let total = page.get("total_pages").and_then(Value::as_u64);
            let reported = optional_bool(page, "has_more")?;
            let has_more = if reported == Some(false) && total.is_none_or(|n| n == 0) {
                None
            } else {
                reported
            };
            let requested = params
                .get("page")
                .and_then(Value::as_u64)
                .ok_or("discussion requested page missing")?;
            let next = if has_more == Some(true) && current == Some(requested) {
                requested
                    .checked_add(1)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n <= 100_000)
                    .map(|number| Position::Page { number })
            } else {
                None
            };
            Ok(Frame {
                entries: rows,
                next,
                has_more,
                empty,
                truncated: false,
                context_ref: if method == "tieba.comments"
                    && data.get("parent_post").is_some_and(Value::is_object)
                {
                    Some(Pending {
                        id: item_id(&data["parent_post"])?,
                        resource: resource.into(),
                        pointer: "/parent_post".into(),
                    })
                } else {
                    None
                },
                context: json!({"thread_id":data["thread_id"],"title":data["title"],"forum":data["forum"],
                    "reply_count":data["reply_count"],"order":"platform_floor_order","focus_target_ref":progress.target_ref}),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            platform: Platform::Bilibili,
            account: "test".into(),
        }
    }
    #[test]
    fn progress_is_scoped_bounded_and_does_not_accept_provider_page_injection() {
        let mut state = Progress::new(
            scope(),
            "ref".into(),
            TaskTarget::Video {
                video: "BV17x411w7KC".into(),
            },
            "hot".into(),
        )
        .unwrap();
        assert!(state.validate(&scope()).is_ok());
        let other = Scope {
            account: "other".into(),
            ..scope()
        };
        assert!(state.validate(&other).is_err());
        state.position = Position::Page { number: 1 };
        assert!(state.validate(&scope()).is_err());
        state.position = Position::Cursor {
            value: Some("same-opaque-token".into()),
        };
        state.seen = (0..=MAX_SEEN).map(|n| n.to_string()).collect();
        assert!(state.validate(&scope()).is_err());
        assert!(
            Progress::new(
                scope(),
                "ref".into(),
                TaskTarget::Creator { mid: 42 },
                "hot".into()
            )
            .is_err()
        );
    }
    #[test]
    fn pending_records_keep_the_entire_thirty_floor_page_as_cache_pointers() {
        let data = json!({"posts":(1..=30).map(|id|json!({"id":id,"body":"not copied into continuation"})).collect::<Vec<_>>()});
        let mut pending = entries(&data, "posts", "/page", "cached-manifest").unwrap();
        assert_eq!(pending.len(), 30);
        let tail = pending.split_off(20);
        assert_eq!(tail.len(), 10);
        assert_eq!(tail[0].pointer, "/page/posts/20");
        assert_eq!(tail[0].id, "21");
        assert!(!serde_json::to_string(&tail).unwrap().contains("not copied"));
    }

    #[test]
    fn repeated_cursor_with_new_ids_continues_and_a_repeated_page_stops() {
        let mut state = Progress::new(
            scope(),
            "ref".into(),
            TaskTarget::Video {
                video: "BV17x411w7KC".into(),
            },
            "hot".into(),
        )
        .unwrap();
        let frame = |id: &str| Frame {
            entries: vec![Pending {
                id: id.into(),
                resource: "cached".into(),
                pointer: "/data/items/0".into(),
            }],
            next: Some(Position::Cursor {
                value: Some("same-opaque-token".into()),
            }),
            has_more: Some(true),
            empty: false,
            truncated: false,
            context: json!({}),
            context_ref: None,
        };
        state.offer(frame("1"));
        assert!(state.stopped.is_none());
        state.pending.clear();
        state.offer(frame("2"));
        assert!(state.stopped.is_none());
        assert_eq!(state.seen.len(), 2);
        assert!(
            matches!(&state.position, Position::Cursor { value: Some(value) } if value == "same-opaque-token")
        );
        state.pending.clear();
        state.offer(frame("2"));
        assert_eq!(state.stopped.as_deref(), Some("no_new_items"));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn cached_thirty_floor_tail_is_returned_on_the_next_call_without_network() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let directory = std::env::temp_dir().join(format!(
            "discussion-tail-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut runtime = Runtime::open(&directory, "public", None).unwrap();
        let scope = runtime.scope(Platform::Tieba);
        let target = TaskTarget::Thread {
            thread: "123".into(),
        };
        let target_ref = runtime.save_task_target(&target).unwrap();
        let data = json!({"posts":(1..=30).map(|id|json!({"id":id,"floor":id,
            "author":{"id":42,"name":"synthetic"},"fragments":[{"type":"text","text":"cached floor"}]})).collect::<Vec<_>>()});
        let page = json!({"page":data});
        let bytes = serde_json::to_vec(&page).unwrap();
        let resource = runtime
            .store
            .import(
                &scope,
                "discussion-test-page",
                "application/json",
                &mut bytes.as_slice(),
            )
            .unwrap();
        let pending = entries(&data, "posts", "/page", &resource.id).unwrap();
        let mut progress = Progress::new(scope.clone(), target_ref, target, "hot".into()).unwrap();
        progress.seen = pending.iter().map(|p| p.id.clone()).collect();
        progress.pending = pending;
        progress.position = Position::Page { number: 2 };
        progress.platform_has_more = Some(true);
        let continuation = runtime.locators.put(&scope, NAMESPACE, &progress).unwrap();
        let first = runtime
            .task_discussion(&json!({"continuation":continuation}))
            .unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 20);
        assert_eq!(first["provider_pages_read"], 0);
        assert!(first["items"][0]["target_ref"].as_str().is_some());
        let second = runtime
            .task_discussion(&json!({"continuation":first["continuation"]}))
            .unwrap();
        assert_eq!(second["items"].as_array().unwrap().len(), 10);
        assert_eq!(second["items"][0]["id"], 21);
        assert_eq!(second["items"][9]["id"], 30);
        assert_eq!(second["provider_pages_read"], 0);
        assert_eq!(second["stop_reason"], "buffer_drained");
        assert!(second["continuation"].as_str().is_some());
        assert_eq!(runtime.scope(Platform::Tieba).account, "public");
        drop(runtime);
        fs::remove_dir_all(directory).unwrap();
    }
}
