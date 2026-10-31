//! Native signed JSON feeds. No raw response/user object is published.
//! Main protocol source: TiebaLite NewTiebaApi/MessageListBean, mirrored in
//! sanniou/Thread at 7d2a3a2aa4bc78a7129c03c7a09234ef24571f60.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Reply,
    At,
    Like,
}
impl NotificationKind {
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Reply => "/c/u/feed/replyme",
            Self::At => "/c/u/feed/atme",
            Self::Like => "/c/u/feed/agreeme",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationActor {
    pub user_id: Option<u64>,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub portrait: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationTarget {
    pub thread_id: u64,
    pub post_id: Option<u64>,
    /// This quoted/parent ID may itself be a subpost. Native readback, rather
    /// than the field name, establishes the floor root for a selected reply.
    pub parent_post_id: Option<u64>,
    pub forum_name: Option<String>,
    pub title: Option<String>,
    pub quote_content: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    pub notification_id: Option<String>,
    pub actor: Option<NotificationActor>,
    pub created_at: Option<u64>,
    pub content: Option<String>,
    pub target: NotificationTarget,
    pub unread: Option<bool>,
    pub is_comment: Option<bool>,
    /// Rich context is not silently treated as observed media. Open the target thread.
    pub unexpanded_rich_context: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationPage {
    pub kind: NotificationKind,
    pub requested_page: u32,
    pub current_page: Option<u32>,
    pub has_more: Option<bool>,
    pub has_prev: Option<bool>,
    pub server_item_count: usize,
    pub truncated: bool,
    pub items: Vec<Notification>,
    pub read_state_effect: String,
}
fn number(value: &Value) -> Result<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .ok_or(Error::Schema("notification integer"))
}
fn number_field(value: &Value, key: &str) -> Result<Option<u64>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) if v.is_empty() => Ok(None),
        Some(v) => Ok(Some(number(v)?)),
    }
}
fn positive(value: &Value, key: &str) -> Result<Option<u64>> {
    let value = number_field(value, key)?;
    if value.is_some_and(|v| v > i64::MAX as u64) {
        return Err(Error::Schema("notification identity range"));
    }
    Ok(value.filter(|v| *v > 0))
}
fn boolean(value: &Value, key: &str) -> Result<Option<bool>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(*v)),
        Some(v) => match number(v)? {
            0 => Ok(Some(false)),
            1 => Ok(Some(true)),
            _ => Err(Error::Schema("notification boolean")),
        },
    }
}
fn text(value: &Value, key: &str, limit: usize) -> Result<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) if v.len() <= limit => Ok(Some(v.clone())),
        _ => Err(Error::Schema("notification text")),
    }
}
fn actor(value: &Value, kind: NotificationKind) -> Result<Option<NotificationActor>> {
    let key = if kind == NotificationKind::Like {
        "agreeer"
    } else {
        "replyer"
    };
    let Some(value) = value.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    if !value.is_object() {
        return Err(Error::Schema("notification actor"));
    }
    Ok(Some(NotificationActor {
        user_id: positive(value, "id")?,
        name: text(value, "name", 1024)?,
        display_name: text(value, "name_show", 1024)?,
        portrait: text(value, "portrait", 4096)?,
    }))
}
pub(crate) fn parse(
    raw: &[u8],
    kind: NotificationKind,
    requested_page: u32,
    limit: usize,
) -> Result<NotificationPage> {
    if raw.len() > 1024 * 1024 || !(1..=100).contains(&limit) {
        return Err(Error::Limit);
    }
    let first = raw.iter().copied().find(|v| !v.is_ascii_whitespace());
    if first != Some(b'{') {
        return Err(Error::Challenge);
    }
    let root: Value =
        serde_json::from_slice(raw).map_err(|_| Error::Schema("notification JSON"))?;
    let code = root
        .get("error_code")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or(Error::Schema("notification result"))?;
    if code != 0 {
        return Err(Error::Platform(code));
    }
    let keys: &[&str] = match kind {
        NotificationKind::Reply => &["reply_list"],
        NotificationKind::At => &["at_list"],
        NotificationKind::Like => &["agree_list"],
    };
    let mut list = None;
    for key in keys {
        if let Some(value) = root.get(*key) {
            match value {
                Value::Array(items) => {
                    if list.is_some() {
                        return Err(Error::Schema("ambiguous notification lists"));
                    }
                    list = Some(items);
                }
                // Some official JSON lists encode empty as a primitive; only explicit
                // null/zero/empty are accepted, never arbitrary success-shaped data.
                Value::Null => {}
                Value::String(v) if v.is_empty() || v == "0" => {}
                Value::Number(n) if n.as_u64() == Some(0) => {}
                _ => return Err(Error::Schema("notification list")),
            }
        }
    }
    let list = if let Some(list) = list {
        list.as_slice()
    } else if keys.iter().any(|key| root.get(*key).is_some()) {
        &[][..]
    } else {
        return Err(Error::Schema("missing notification list"));
    };
    if list.len() > 256 {
        return Err(Error::Limit);
    }
    let pagination = root.get("page").filter(|v| !v.is_null());
    if pagination.is_some_and(|v| !v.is_object()) {
        return Err(Error::Schema("notification page"));
    }
    let current_page = if let Some(page) = pagination {
        positive(page, "current_page")?
            .map(|v| u32::try_from(v).map_err(|_| Error::Schema("notification page range")))
            .transpose()?
    } else {
        None
    };
    if pagination.is_some() && current_page.is_none() {
        return Err(Error::Schema("missing notification page identity"));
    }
    let mut items = Vec::new();
    for value in list.iter().take(limit) {
        if !value.is_object() {
            return Err(Error::Schema("notification item"));
        }
        let thread_id =
            positive(value, "thread_id")?.ok_or(Error::Schema("notification thread identity"))?;
        let context = if kind == NotificationKind::Like {
            match value.get("thread_info") {
                None | Some(Value::Null) => None,
                Some(v) if v.is_object() => Some(v),
                _ => return Err(Error::Schema("notification thread context")),
            }
        } else {
            Some(value)
        };
        let created_at = if kind == NotificationKind::Like {
            match number_field(value, "op_time")?.filter(|v| *v > 0) {
                Some(time) => Some(time),
                None => number_field(value, "time")?.filter(|v| *v > 0),
            }
        } else {
            number_field(value, "time")?.filter(|v| *v > 0)
        };
        items.push(Notification {
            notification_id: match value.get("id") {
                None | Some(Value::Null) => None,
                Some(Value::String(v)) if v.len() <= 128 => Some(v.clone()),
                Some(Value::Number(n)) => Some(n.to_string()),
                _ => return Err(Error::Schema("notification ID")),
            },
            actor: actor(value, kind)?,
            created_at,
            content: text(value, "content", 32768)?,
            target: NotificationTarget {
                thread_id,
                post_id: positive(value, "post_id")?,
                parent_post_id: positive(value, "quote_pid")?,
                forum_name: context
                    .map(|v| text(v, "fname", 1024))
                    .transpose()?
                    .flatten(),
                title: context
                    .map(|v| text(v, "title", 4096))
                    .transpose()?
                    .flatten(),
                quote_content: text(value, "quote_content", 32768)?,
            },
            unread: boolean(value, "unread")?,
            is_comment: boolean(value, "is_floor")?,
            unexpanded_rich_context: value.get("new_floor_info").is_some()
                || context
                    .and_then(|v| v.get("media"))
                    .is_some_and(|v| match v {
                        Value::Null => false,
                        Value::Array(items) => !items.is_empty(),
                        _ => true,
                    }),
        });
    }
    Ok(NotificationPage {
        kind,
        requested_page,
        current_page,
        has_more: if kind == NotificationKind::Like {
            boolean(&root, "has_more")?
        } else {
            pagination
                .map(|p| boolean(p, "has_more"))
                .transpose()?
                .flatten()
        },
        has_prev: pagination
            .map(|p| boolean(p, "has_prev"))
            .transpose()?
            .flatten(),
        server_item_count: list.len(),
        truncated: list.len() > limit,
        items,
        read_state_effect: "unknown".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn feed_projection_keeps_identity_context_and_drops_account_credentials() {
        let v = json!({"error_code":"0","reply_list":[{"id":"notice-1","thread_id":"700","post_id":"701","time":"1700000000","content":"回复你的内容","unread":"1","is_floor":"1","replyer":{"id":"42","name":"alice","name_show":"Alice","portrait":"tb.opaque","BDUSS":"synthetic-secret"},"quote_pid":"600","quote_content":"原文","fname":"example","new_floor_info":[],"STOKEN":"synthetic-token"}],"page":{"current_page":"1","has_more":"0","has_prev":"0"},"BDUSS":"root-secret"});
        let out = parse(
            &serde_json::to_vec(&v).unwrap(),
            NotificationKind::Reply,
            1,
            20,
        )
        .unwrap();
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].target.thread_id, 700);
        assert_eq!(out.items[0].actor.as_ref().unwrap().user_id, Some(42));
        assert_eq!(out.items[0].unread, Some(true));
        assert_eq!(out.has_more, Some(false));
        let public = serde_json::to_string(&out).unwrap();
        assert!(!public.contains("secret"));
        assert!(!public.contains("STOKEN"));
        assert_eq!(out.read_state_effect, "unknown");
    }
    #[test]
    fn feeds_do_not_fabricate_empty_success_from_errors_missing_lists_or_bad_pages() {
        for v in [
            json!({"error_code":5,"reply_list":[]}),
            json!({"error_code":0}),
            json!({"error_code":0,"reply_list":[],"page":{"current_page":0}}),
            json!({"error_code":0,"reply_list":[{"content":"no identity"}]}),
            json!({"error_code":0,"reply_list":"challenge"}),
        ] {
            assert!(
                parse(
                    &serde_json::to_vec(&v).unwrap(),
                    NotificationKind::Reply,
                    1,
                    20
                )
                .is_err()
            );
        }
        assert!(matches!(
            parse(b"<html>challenge</html>", NotificationKind::Reply, 1, 20),
            Err(Error::Challenge)
        ));
        assert_eq!(
            parse(
                b"{\"error_code\":0,\"at_list\":[]}",
                NotificationKind::At,
                1,
                20
            )
            .unwrap()
            .items
            .len(),
            0
        );
    }
    #[test]
    fn server_pagination_and_local_truncation_remain_separate() {
        let v = json!({"error_code":0,"at_list":[{"thread_id":1,"post_id":2},{"thread_id":1,"post_id":3}],"page":{"current_page":2,"has_more":1,"has_prev":1}});
        let result = parse(&serde_json::to_vec(&v).unwrap(), NotificationKind::At, 3, 1).unwrap();
        assert_eq!(result.current_page, Some(2));
        assert_eq!(result.requested_page, 3);
        assert_eq!(result.server_item_count, 2);
        assert!(result.truncated);
        assert_eq!(result.has_more, Some(true));
        let like = parse(
            b"{\"error_code\":\"0\",\"agree_list\":[]}",
            NotificationKind::Like,
            1,
            20,
        )
        .unwrap();
        assert!(like.items.is_empty());
        assert_eq!(like.has_more, None);
    }
    #[test]
    fn optional_empty_quote_ids_are_absent_but_missing_thread_is_rejected() {
        let v = json!({"error_code":0,"reply_list":[{"thread_id":"700","post_id":"701","quote_pid":"","time":"1700000000","replyer":{"id":""}}]});
        let out = parse(
            &serde_json::to_vec(&v).unwrap(),
            NotificationKind::Reply,
            1,
            20,
        )
        .unwrap();
        assert_eq!(out.items[0].target.parent_post_id, None);
        assert_eq!(out.items[0].actor.as_ref().unwrap().user_id, None);
        let mut invalid = v;
        invalid["reply_list"][0]["thread_id"] = json!("");
        assert!(
            parse(
                &serde_json::to_vec(&invalid).unwrap(),
                NotificationKind::Reply,
                1,
                20
            )
            .is_err()
        );
    }
    #[test]
    fn observed_agree_feed_has_its_own_actor_context_time_and_top_level_page_flag() {
        let v = json!({"error_code":"0","has_more":"1","agree_list":[{
            "id":"notice-42","thread_id":"700","post_id":"701","time":"1700000000","op_time":"1700000030","op_user_id":"42","type":"1","is_del":"0",
            "agreeer":{"id":"42","name":"alice","name_show":"Alice","portrait":"tb.opaque","BDUSS":"actor-secret"},
            "thread_info":{"fname":"example","title":"actual title","id":"700","fid":"77","media":[{"src":"not-published","STOKEN":"media-secret"}]}
        }],"BDUSS":"root-secret"});
        let out = parse(
            &serde_json::to_vec(&v).unwrap(),
            NotificationKind::Like,
            1,
            20,
        )
        .unwrap();
        let item = &out.items[0];
        assert_eq!(item.actor.as_ref().unwrap().user_id, Some(42));
        assert_eq!(item.created_at, Some(1700000030));
        assert_eq!(item.content, None);
        assert_eq!(item.target.title.as_deref(), Some("actual title"));
        assert_eq!(item.target.forum_name.as_deref(), Some("example"));
        assert!(item.unexpanded_rich_context);
        assert_eq!(out.has_more, Some(true));
        assert_eq!(out.current_page, None);
        let public = serde_json::to_string(&out).unwrap();
        assert!(!public.contains("secret"));
        assert!(!public.contains("not-published"));
        assert!(
            parse(
                b"{\"error_code\":0,\"reply_list\":[]}",
                NotificationKind::Like,
                1,
                20
            )
            .is_err()
        );
    }
}
