//! Bounded notification reads. No unread acknowledgement or content writes.
use crate::{Client, Error, Result, api_data};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

const MAX_SERVER_ITEMS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Reply,
    At,
    Like,
}
impl NotificationKind {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::At => "at",
            Self::Like => "like",
        }
    }
    fn time_parameter(self) -> &'static str {
        match self {
            Self::Reply => "reply_time",
            Self::At => "at_time",
            Self::Like => "like_time",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationCursor {
    pub id: u64,
    pub time: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationUnread {
    pub at: Option<u64>,
    pub reply: Option<u64>,
    pub like: Option<u64>,
    pub recv_reply: Option<u64>,
    pub recv_like: Option<u64>,
    pub sys_msg: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationActor {
    pub mid: Option<u64>,
    pub nickname: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationTarget {
    pub item_id: Option<u64>,
    pub subject_id: Option<u64>,
    pub root_id: Option<u64>,
    pub source_id: Option<u64>,
    pub target_id: Option<u64>,
    pub business_id: Option<u64>,
    pub content_type: Option<String>,
    pub business: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub detail_title: Option<String>,
    pub root_reply_content: Option<String>,
    pub source_content: Option<String>,
    pub target_reply_content: Option<String>,
    pub message: Option<String>,
    /// Untrusted link data, never fetched or executed by this reader.
    pub uri: Option<String>,
    /// Reference only: does not mean an image was fetched or seen by a model.
    pub image: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub kind: NotificationKind,
    /// Like notifications have distinct latest/total server sections.
    pub section: String,
    pub created_at: u64,
    pub actors: Vec<NotificationActor>,
    pub actors_unavailable: bool,
    pub count: Option<u64>,
    pub target: NotificationTarget,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationPage {
    pub kind: NotificationKind,
    pub items: Vec<Notification>,
    pub next_cursor: Option<NotificationCursor>,
    pub server_cursor: Option<NotificationCursor>,
    pub server_is_end: bool,
    pub last_view_at: Option<u64>,
    pub server_item_count: usize,
    pub latest_item_count: usize,
    pub truncated: bool,
    pub pagination_note: Option<String>,
    /// No acknowledgement is sent. The server's implicit behavior is unverified.
    pub read_state_effect: String,
    pub media_downloaded: bool,
}

impl Client {
    pub fn notification_unread(&self) -> Result<NotificationUnread> {
        parse_unread(api_data(
            self.transport
                .json("https://api.bilibili.com/x/msgfeed/unread?build=0&mobi_app=web")?,
            false,
        )?)
    }

    /// One page only; cursor reads of likes return the total section only.
    /// A smaller output limit uses the actual returned item's cursor, never a
    /// server cursor that would skip rows withheld from the caller.
    pub fn notifications(
        &self,
        kind: NotificationKind,
        cursor: Option<NotificationCursor>,
        limit: u32,
    ) -> Result<NotificationPage> {
        let url = notification_url(kind, cursor, limit)?;
        parse_page(
            api_data(self.transport.json(&url)?, false)?,
            kind,
            cursor.is_some(),
            limit as usize,
        )
    }
}

fn notification_url(
    kind: NotificationKind,
    cursor: Option<NotificationCursor>,
    limit: u32,
) -> Result<String> {
    if !(1..=20).contains(&limit) {
        return Err(Error::InvalidPage);
    }
    let mut url = format!(
        "https://api.bilibili.com/x/msgfeed/{}?platform=web&build=0&mobi_app=web&ps={limit}",
        kind.endpoint()
    );
    if let Some(cursor) = cursor {
        if cursor.id == 0 || cursor.time == 0 {
            return Err(Error::InvalidPage);
        }
        url.push_str(&format!(
            "&id={}&{}={}",
            cursor.id,
            kind.time_parameter(),
            cursor.time
        ));
    }
    Ok(url)
}
fn optional_number(value: &Value, key: &str) -> Result<Option<u64>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .or_else(|| {
                value.as_str().and_then(|v| {
                    (!v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse().ok())
                        .flatten()
                })
            })
            .map(Some)
            .ok_or(Error::Schema("notification numeric field")),
    }
}
fn number(value: &Value, key: &str) -> Result<u64> {
    optional_number(value, key)?.ok_or(Error::Schema("missing notification numeric field"))
}
fn text(value: &Value, key: &str, maximum: usize) -> Result<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= maximum && !value.contains('\0') => {
            Ok(Some(value.clone()))
        }
        Some(Value::String(_)) => Err(Error::JsonLimit),
        _ => Err(Error::Schema("notification text field")),
    }
}
fn actor(value: &Value) -> Result<NotificationActor> {
    if !value.is_object() {
        return Err(Error::Schema("notification actor"));
    }
    Ok(NotificationActor {
        mid: optional_number(value, "mid")?,
        nickname: text(value, "nickname", 1024)?,
        avatar: text(value, "avatar", 16384)?,
    })
}
fn notification(value: &Value, kind: NotificationKind, section: &str) -> Result<Notification> {
    let id = number(value, "id")?;
    if id == 0 {
        return Err(Error::Schema("notification identity"));
    }
    let mut actors = Vec::new();
    let actors_unavailable;
    match kind {
        NotificationKind::Reply | NotificationKind::At => match value.get("user") {
            Some(Value::Null) => {
                actors_unavailable = true;
            }
            Some(user) => {
                actors.push(actor(user)?);
                actors_unavailable = false;
            }
            None => return Err(Error::Schema("missing notification actor")),
        },
        NotificationKind::Like => {
            let users = value
                .get("users")
                .and_then(Value::as_array)
                .ok_or(Error::Schema("notification like actors"))?;
            if users.len() > 20 {
                return Err(Error::JsonLimit);
            }
            actors_unavailable = users.is_empty();
            for user in users {
                actors.push(actor(user)?);
            }
        }
    }
    let item = value
        .get("item")
        .filter(|v| v.is_object())
        .ok_or(Error::Schema("notification item"))?;
    // Empty/unrecognized items are protocol failures, not empty successful text.
    if ![
        "type",
        "business_id",
        "item_id",
        "subject_id",
        "title",
        "source_content",
        "uri",
    ]
    .iter()
    .any(|key| item.get(*key).is_some())
    {
        return Err(Error::Schema("unrecognized notification item"));
    }
    Ok(Notification {
        id,
        kind,
        section: section.into(),
        created_at: number(value, kind.time_parameter())?,
        actors,
        actors_unavailable,
        count: optional_number(value, "counts")?,
        target: NotificationTarget {
            item_id: optional_number(item, "item_id")?,
            subject_id: optional_number(item, "subject_id")?,
            root_id: optional_number(item, "root_id")?,
            source_id: optional_number(item, "source_id")?,
            target_id: optional_number(item, "target_id")?,
            business_id: optional_number(item, "business_id")?,
            content_type: text(item, "type", 256)?,
            business: text(item, "business", 1024)?,
            title: text(item, "title", 32768)?,
            description: text(item, "desc", 32768)?,
            detail_title: text(item, "detail_title", 32768)?,
            root_reply_content: text(item, "root_reply_content", 32768)?,
            source_content: text(item, "source_content", 32768)?,
            target_reply_content: text(item, "target_reply_content", 32768)?,
            message: text(item, "message", 32768)?,
            uri: text(item, "uri", 16384)?,
            image: text(item, "image", 16384)?,
        },
    })
}
fn parse_unread(value: Value) -> Result<NotificationUnread> {
    let result = NotificationUnread {
        at: optional_number(&value, "at")?,
        reply: optional_number(&value, "reply")?,
        like: optional_number(&value, "like")?,
        recv_reply: optional_number(&value, "recv_reply")?,
        recv_like: optional_number(&value, "recv_like")?,
        sys_msg: optional_number(&value, "sys_msg")?,
    };
    if [
        result.at,
        result.reply,
        result.like,
        result.recv_reply,
        result.recv_like,
        result.sys_msg,
    ]
    .iter()
    .all(Option::is_none)
    {
        return Err(Error::Schema("unrecognized notification unread counts"));
    }
    Ok(result)
}
fn cursor(value: &Value) -> Result<(bool, Option<NotificationCursor>)> {
    let value = value
        .get("cursor")
        .filter(|v| v.is_object())
        .ok_or(Error::Schema("missing notification cursor"))?;
    let is_end = value
        .get("is_end")
        .and_then(Value::as_bool)
        .ok_or(Error::Schema("notification cursor end"))?;
    let id = optional_number(value, "id")?;
    let time = optional_number(value, "time")?;
    let cursor = match (id, time) {
        (Some(id), Some(time)) if id > 0 && time > 0 => Some(NotificationCursor { id, time }),
        (None, None) | (Some(0), Some(0)) if is_end => None,
        _ => return Err(Error::Schema("notification cursor identity")),
    };
    Ok((is_end, cursor))
}
fn rows<'a>(value: &'a Value, is_end: bool) -> Result<&'a [Value]> {
    match value.get("items") {
        Some(Value::Array(rows)) if rows.len() <= MAX_SERVER_ITEMS => Ok(rows),
        Some(Value::Array(_)) => Err(Error::JsonLimit),
        Some(Value::Null) if is_end => Ok(&[]),
        _ => Err(Error::Schema("notification items")),
    }
}
fn parse_page(
    value: Value,
    kind: NotificationKind,
    cursor_request: bool,
    limit: usize,
) -> Result<NotificationPage> {
    let total = if kind == NotificationKind::Like {
        value
            .get("total")
            .filter(|v| v.is_object())
            .ok_or(Error::Schema("notification like total"))?
    } else {
        &value
    };
    let (server_is_end, server_cursor) = cursor(total)?;
    let total_rows = rows(total, server_is_end)?;
    let latest = if kind == NotificationKind::Like {
        let latest = value
            .get("latest")
            .filter(|v| v.is_object())
            .ok_or(Error::Schema("notification like latest"))?;
        rows(latest, true)?
    } else {
        &[]
    };
    let last_view_at =
        optional_number(&value, "last_view_at")?.or(if kind == NotificationKind::Like {
            optional_number(&value["latest"], "last_view_at")?
        } else {
            None
        });
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    let mut latest_truncated = false;
    if !cursor_request {
        for row in latest {
            let item = notification(row, kind, "latest")?;
            if !seen.insert(item.id) {
                continue;
            }
            if items.len() < limit {
                items.push(item);
            } else {
                latest_truncated = true;
            }
        }
    }
    let mut withheld_total = false;
    let mut last_emitted_total = None;
    for row in total_rows {
        let item = notification(row, kind, "total")?;
        if seen.contains(&item.id) {
            // A latest entry can also occupy a slot in the total section.
            if !withheld_total {
                last_emitted_total = Some(NotificationCursor {
                    id: item.id,
                    time: item.created_at,
                });
            }
            continue;
        }
        if items.len() < limit && !withheld_total {
            seen.insert(item.id);
            last_emitted_total = Some(NotificationCursor {
                id: item.id,
                time: item.created_at,
            });
            items.push(item);
        } else {
            withheld_total = true;
        }
    }
    let truncated = latest_truncated || withheld_total;
    let (next_cursor, pagination_note) = if latest_truncated {
        (None,Some("Latest like section exceeded the output budget. Its remaining rows cannot be resumed losslessly with the total cursor; no continuation is claimed.".into()))
    } else if withheld_total {
        if let Some(last) = last_emitted_total {
            (Some(last),Some("Output was limited; next_cursor is the last actually returned total entry, not the farther server cursor. Like cursor requests omit the repeated latest section.".into()))
        } else {
            (None,Some("Latest like entries filled the output budget before any total entry was returned. The total section remains unread; no lossless cursor is claimed.".into()))
        }
    } else if server_is_end {
        (None, None)
    } else {
        (server_cursor, None)
    };
    Ok(NotificationPage {
        kind,
        items,
        next_cursor,
        server_cursor,
        server_is_end,
        last_view_at,
        server_item_count: total_rows.len(),
        latest_item_count: latest.len(),
        truncated,
        pagination_note,
        read_state_effect: "server_behavior_unverified_no_ack_sent".into(),
        media_downloaded: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn item(id: u64, kind: NotificationKind) -> Value {
        let mut value = json!({"id":id,"user":{"mid":42,"nickname":"test","SESSDATA":"must-stay-private"},"users":[{"mid":42,"nickname":"test"}],"item":{"type":"reply","business_id":1,"title":"a title","source_content":"real reply","image":"https://i0.hdslb.com/fixture.jpg","uri":"https://www.bilibili.com/video/BV17x411w7KC","private_key":"must-stay-private"},"counts":1});
        value[kind.time_parameter()] = json!(1000 + id);
        value
    }
    fn page(items: Value) -> Value {
        json!({"cursor":{"is_end":false,"id":1,"time":1001},"items":items,"last_view_at":123})
    }
    #[test]
    fn notification_projection_keeps_real_context_and_drops_unknown_private_fields() {
        let huge = 9_007_199_254_740_999_u64;
        let parsed = parse_page(
            page(json!([item(huge, NotificationKind::Reply)])),
            NotificationKind::Reply,
            false,
            20,
        )
        .unwrap();
        assert_eq!(parsed.items[0].id, huge);
        assert_eq!(
            parsed.items[0].target.source_content.as_deref(),
            Some("real reply")
        );
        assert!(parsed.items[0].target.image.is_some());
        assert!(!parsed.media_downloaded);
        assert!(
            !serde_json::to_string(&parsed)
                .unwrap()
                .contains("must-stay-private")
        );
    }
    #[test]
    fn truncation_cursor_never_skips_unreturned_rows_and_like_sections_stay_explicit() {
        let value = page(json!([
            item(9, NotificationKind::At),
            item(8, NotificationKind::At),
            item(7, NotificationKind::At)
        ]));
        let parsed = parse_page(value, NotificationKind::At, false, 1).unwrap();
        assert_eq!(
            parsed.next_cursor,
            Some(NotificationCursor { id: 9, time: 1009 })
        );
        assert_eq!(
            parsed.server_cursor,
            Some(NotificationCursor { id: 1, time: 1001 })
        );
        assert!(parsed.truncated);
        let value = json!({"latest":{"items":[item(9,NotificationKind::Like)],"last_view_at":8},"total":page(json!([item(9,NotificationKind::Like),item(8,NotificationKind::Like),item(7,NotificationKind::Like)]))});
        let parsed = parse_page(value.clone(), NotificationKind::Like, false, 2).unwrap();
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.items[0].section, "latest");
        assert_eq!(parsed.items[1].id, 8);
        assert_eq!(
            parsed.next_cursor,
            Some(NotificationCursor { id: 8, time: 1008 })
        );
        let resumed = parse_page(value, NotificationKind::Like, true, 1).unwrap();
        assert_eq!(resumed.items[0].section, "total");
        let value = json!({"latest":{"items":[item(10,NotificationKind::Like),item(9,NotificationKind::Like)]},"total":page(json!([item(8,NotificationKind::Like)]))});
        let parsed = parse_page(value, NotificationKind::Like, false, 1).unwrap();
        assert!(parsed.truncated);
        assert!(parsed.next_cursor.is_none());
        assert!(parsed.pagination_note.is_some());
    }
    #[test]
    fn missing_schema_is_not_empty_success_and_unread_unknowns_are_not_zero() {
        assert!(parse_page(json!({}), NotificationKind::Reply, false, 20).is_err());
        assert!(parse_page(page(json!([{"id":1}])), NotificationKind::Reply, false, 20).is_err());
        let empty = json!({"items":null,"cursor":{"is_end":true,"id":0,"time":0}});
        assert!(
            parse_page(empty, NotificationKind::At, false, 20)
                .unwrap()
                .items
                .is_empty()
        );
        let unread =
            parse_unread(json!({"at":0,"reply":4,"like":2,"cookie":"must-not-leak"})).unwrap();
        assert_eq!(unread.at, Some(0));
        assert_eq!(unread.recv_reply, None);
        assert!(parse_unread(json!({"new_unsupported_field":2})).is_err());
        assert!(parse_unread(json!({"at":"wrong"})).is_err());
    }
    #[test]
    fn notification_requests_have_fixed_read_endpoints_and_bounded_parameters() {
        for kind in [
            NotificationKind::At,
            NotificationKind::Reply,
            NotificationKind::Like,
        ] {
            let url = notification_url(kind, Some(NotificationCursor { id: 42, time: 1000 }), 20)
                .unwrap();
            assert!(url.starts_with("https://api.bilibili.com/x/msgfeed/"));
            assert!(url.contains(&format!("{}=1000", kind.time_parameter())));
            assert!(!url.contains("clear") && !url.contains("csrf") && !url.contains("mark"));
        }
        for limit in [0, 21, u32::MAX] {
            assert!(notification_url(NotificationKind::At, None, limit).is_err());
        }
        assert!(
            notification_url(
                NotificationKind::At,
                Some(NotificationCursor { id: 0, time: 1000 }),
                1
            )
            .is_err()
        );
        assert!(
            parse_page(
                page(json!(vec![item(1, NotificationKind::Reply); 257])),
                NotificationKind::Reply,
                false,
                20
            )
            .is_err()
        );
    }
}
