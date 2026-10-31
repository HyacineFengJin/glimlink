// SPDX-License-Identifier: MIT
//! Verified self publications, distinct from incoming notification targets.
//! Protocol: aiotieba get_selfinfo_moindex + get_user_contents/{get_threads,get_posts}.
use crate::{
    Error, Result, model,
    wire::{self, Fields},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationKind {
    Threads,
    Replies,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublicationOwner {
    pub user_id: u64,
    pub portrait: String,
    pub name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Publication {
    /// thread, post, comment or unknown; the parent thread is context for replies.
    pub content_type: String,
    pub thread_id: u64,
    pub post_id: Option<u64>,
    pub forum_id: Option<u64>,
    pub forum_name: String,
    pub title: String,
    pub text: String,
    pub created_at: Option<u64>,
    pub unexpanded_rich_context: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublicationPage {
    pub owner: PublicationOwner,
    pub kind: PublicationKind,
    pub requested_page: u32,
    /// This particular protobuf response does not provide a page/total indicator.
    pub has_more: Option<bool>,
    pub server_group_count: usize,
    pub server_item_count: usize,
    pub truncated: bool,
    pub items: Vec<Publication>,
}
fn integer(value: &Value) -> Result<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
        .filter(|v| *v > 0 && *v <= i64::MAX as u64)
        .ok_or(Error::Schema("self account UID"))
}
fn portrait(value: &str) -> Result<&str> {
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(Error::Schema("self account portrait"));
    }
    let base = value.split('?').next().unwrap_or(value);
    if base.is_empty() {
        return Err(Error::Schema("self account portrait"));
    }
    Ok(base)
}
pub(crate) fn self_owner(
    raw: &[u8],
    bound_portrait: &str,
    bound_uid: Option<u64>,
) -> Result<PublicationOwner> {
    if raw.len() > 1024 * 1024 {
        return Err(Error::Limit);
    }
    if raw.iter().copied().find(|c| !c.is_ascii_whitespace()) != Some(b'{') {
        return Err(Error::Challenge);
    }
    let root: Value =
        serde_json::from_slice(raw).map_err(|_| Error::Schema("self account JSON"))?;
    let code = root
        .get("no")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or(Error::Schema("self account result"))?;
    if code != 0 {
        return Err(Error::Platform(code));
    }
    let data = root
        .get("data")
        .filter(|v| v.is_object())
        .ok_or(Error::Schema("self account data"))?;
    let user_id = integer(data.get("id").ok_or(Error::LoginRequired)?)?;
    let user_portrait = data
        .get("portrait")
        .and_then(Value::as_str)
        .ok_or(Error::LoginRequired)?;
    if portrait(user_portrait)? != portrait(bound_portrait)?
        || bound_uid.is_some_and(|uid| uid != user_id)
    {
        return Err(Error::Schema("self account identity mismatch"));
    }
    let name = match data.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(v)) if v.len() <= 1024 => Some(v.clone()),
        _ => return Err(Error::Schema("self account name")),
    };
    Ok(PublicationOwner {
        user_id,
        portrait: portrait(user_portrait)?.into(),
        name,
    })
}
pub(crate) fn request(
    common: &[u8],
    uid: u64,
    kind: PublicationKind,
    page: u32,
    limit: usize,
) -> Result<Vec<u8>> {
    if uid == 0 || uid > i64::MAX as u64 {
        return Err(Error::InvalidId);
    }
    if page == 0 || page > 100000 {
        return Err(Error::InvalidPage);
    }
    if !(1..=50).contains(&limit) {
        return Err(Error::Limit);
    }
    let mut data = Vec::new();
    wire::number(1, uid, &mut data);
    if kind == PublicationKind::Replies {
        wire::number(2, limit as u64, &mut data);
    }
    if kind == PublicationKind::Threads {
        wire::number(4, 1, &mut data);
        wire::number(33, 1, &mut data);
    }
    wire::number(5, 1, &mut data);
    wire::number(26, page.into(), &mut data);
    wire::bytes(27, common, &mut data);
    let mut request = Vec::new();
    wire::bytes(1, &data, &mut request);
    Ok(request)
}
fn optional_id(value: u64) -> Result<Option<u64>> {
    if value > i64::MAX as u64 {
        return Err(Error::Schema("publication identity range"));
    }
    Ok((value > 0).then_some(value))
}
fn bounded_text(fields: &Fields<'_>, field: u32, limit: usize) -> Result<String> {
    let value = fields.text(field)?;
    if value.len() > limit {
        return Err(Error::Limit);
    }
    Ok(value)
}
fn append_text(out: &mut String, text: &str) -> Result<()> {
    if out.len().saturating_add(text.len()) > 32768 {
        return Err(Error::Limit);
    }
    out.push_str(text);
    Ok(())
}
fn first_post_text(group: &Fields<'_>) -> Result<(String, bool)> {
    let mut text = String::new();
    let mut rich = false;
    let mut count = 0;
    for raw in group.all(49) {
        count += 1;
        if count > 512 {
            return Err(Error::Limit);
        }
        let f = Fields::parse(raw)?;
        if matches!(f.num(1), 0 | 1 | 4 | 9 | 18 | 27 | 40) {
            append_text(&mut text, &f.text(2)?)?;
        } else {
            rich = true;
        }
    }
    Ok((
        text,
        rich || group.all(16).next().is_some()
            || group.all(23).next().is_some()
            || group.bytes(29).is_some(),
    ))
}
fn reply_text(content: &Fields<'_>) -> Result<(String, bool)> {
    let mut text = String::new();
    let mut rich = false;
    let mut count = 0;
    for raw in content.all(1) {
        count += 1;
        if count > 512 {
            return Err(Error::Limit);
        }
        let f = Fields::parse(raw)?;
        if !f.text(4)?.is_empty() {
            rich = true;
        } else if matches!(f.num(1), 0 | 1 | 4 | 9 | 18 | 27 | 40) {
            append_text(&mut text, &f.text(2)?)?;
        } else {
            rich = true;
        }
    }
    Ok((text, rich))
}
pub(crate) fn parse(
    raw: &[u8],
    owner: PublicationOwner,
    kind: PublicationKind,
    page: u32,
    limit: usize,
) -> Result<PublicationPage> {
    let data = model::response(raw)?;
    let mut server_group_count = 0;
    let mut server_item_count = 0;
    let mut items = Vec::new();
    for raw in data.all(1) {
        server_group_count += 1;
        if server_group_count > 100 {
            return Err(Error::Limit);
        }
        let group = Fields::parse(raw)?;
        let thread_id =
            optional_id(group.num(2))?.ok_or(Error::Schema("publication thread identity"))?;
        if group
            .num_opt(18)
            .is_some_and(|id| id != 0 && id != owner.user_id)
        {
            return Err(Error::Schema("publication author mismatch"));
        }
        let forum_id = optional_id(group.num(1))?;
        let forum_name = bounded_text(&group, 6, 1024)?;
        let title = bounded_text(&group, 7, 4096)?;
        if kind == PublicationKind::Threads {
            server_item_count += 1;
            if items.len() < limit {
                let (text, rich) = first_post_text(&group)?;
                items.push(Publication {
                    content_type: "thread".into(),
                    thread_id,
                    post_id: optional_id(group.num(3))?,
                    forum_id,
                    forum_name,
                    title,
                    text,
                    created_at: group.num_opt(5).filter(|v| *v > 0),
                    unexpanded_rich_context: rich,
                });
            }
        } else {
            let mut group_items = 0;
            for raw in group.all(8) {
                group_items += 1;
                server_item_count += 1;
                if group_items > 128 || server_item_count > 2048 {
                    return Err(Error::Limit);
                }
                let content = Fields::parse(raw)?;
                let post_id = optional_id(content.num(4))?
                    .ok_or(Error::Schema("publication reply identity"))?;
                if items.len() < limit {
                    let (text, rich) = reply_text(&content)?;
                    let content_type = match content.num(3) {
                        0 => "post",
                        1 => "comment",
                        _ => "unknown",
                    }
                    .into();
                    items.push(Publication {
                        content_type,
                        thread_id,
                        post_id: Some(post_id),
                        forum_id,
                        forum_name: forum_name.clone(),
                        title: title.clone(),
                        text,
                        created_at: content.num_opt(2).filter(|v| *v > 0),
                        unexpanded_rich_context: rich,
                    });
                }
            }
            if group_items == 0 {
                return Err(Error::Schema("publication reply group lacks own content"));
            }
        }
    }
    Ok(PublicationPage {
        owner,
        kind,
        requested_page: page,
        has_more: None,
        server_group_count,
        server_item_count,
        truncated: server_item_count > limit,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn owner() -> PublicationOwner {
        PublicationOwner {
            user_id: 42,
            portrait: "tb.self".into(),
            name: Some("me".into()),
        }
    }
    fn envelope(group: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        wire::bytes(1, group, &mut data);
        let mut out = Vec::new();
        wire::bytes(2, &data, &mut out);
        out
    }
    fn base_group() -> Vec<u8> {
        let mut out = Vec::new();
        wire::number(1, 7, &mut out);
        wire::number(2, 700, &mut out);
        wire::number(3, 701, &mut out);
        wire::number(5, 1700000000, &mut out);
        wire::bytes(6, b"forum", &mut out);
        wire::bytes(7, b"actual title", &mut out);
        wire::number(18, 42, &mut out);
        out
    }
    #[test]
    fn authenticated_self_uid_requires_the_same_bound_portrait() {
        let raw=serde_json::to_vec(&json!({"no":0,"data":{"id":"42","portrait":"tb.self?t=1700000000","name":"me","BDUSS":"secret","tbs":"secret"}})).unwrap();
        let result = self_owner(&raw, "tb.self", None).unwrap();
        assert_eq!(result.user_id, 42);
        assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
        assert!(self_owner(&raw, "tb.other", None).is_err());
        assert!(self_owner(&raw, "tb.self", Some(99)).is_err());
        assert!(self_owner(b"{\"no\":0,\"data\":{}}", "tb.self", None).is_err());
    }
    #[test]
    fn thread_and_reply_requests_use_separate_modes_and_only_verified_uid() {
        let t = request(b"common", 42, PublicationKind::Threads, 2, 20).unwrap();
        let t = Fields::parse(&t).unwrap();
        let t = Fields::parse(t.bytes(1).unwrap()).unwrap();
        assert_eq!(t.num(1), 42);
        assert_eq!(t.num(4), 1);
        assert_eq!(t.num(33), 1);
        assert_eq!(t.num(26), 2);
        let r = request(b"common", 42, PublicationKind::Replies, 1, 20).unwrap();
        let r = Fields::parse(&r).unwrap();
        let r = Fields::parse(r.bytes(1).unwrap()).unwrap();
        assert_eq!(r.num(4), 0);
        assert_eq!(r.num(2), 20);
        assert_eq!(r.num(5), 1);
        assert!(request(b"", 0, PublicationKind::Threads, 1, 20).is_err());
    }
    #[test]
    fn own_reply_content_is_not_the_parent_thread_or_incoming_notification() {
        let mut group = base_group();
        for (id, type_) in [(801, 0), (802, 1)] {
            let mut content = Vec::new();
            let mut fragment = Vec::new();
            wire::bytes(2, b"my reply", &mut fragment);
            wire::bytes(1, &fragment, &mut content);
            wire::number(2, 1700000010, &mut content);
            wire::number(3, type_, &mut content);
            wire::number(4, id, &mut content);
            wire::bytes(8, &content, &mut group);
        }
        let parsed = parse(&envelope(&group), owner(), PublicationKind::Replies, 1, 20).unwrap();
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.items[0].post_id, Some(801));
        assert_eq!(parsed.items[0].content_type, "post");
        assert_eq!(parsed.items[1].content_type, "comment");
        assert_eq!(parsed.items[0].text, "my reply");
        assert_eq!(parsed.has_more, None);
        assert!(
            parse(
                &envelope(&base_group()),
                owner(),
                PublicationKind::Replies,
                1,
                20
            )
            .is_err()
        );
    }
    #[test]
    fn threads_keep_media_unexpanded_and_reject_a_foreign_author() {
        let mut group = base_group();
        let mut fragment = Vec::new();
        wire::bytes(2, b"my text", &mut fragment);
        wire::bytes(49, &fragment, &mut group);
        wire::bytes(16, b"media-kept-private", &mut group);
        let parsed = parse(&envelope(&group), owner(), PublicationKind::Threads, 1, 20).unwrap();
        assert_eq!(parsed.items[0].text, "my text");
        assert!(parsed.items[0].unexpanded_rich_context);
        assert_eq!(parsed.items[0].content_type, "thread");
        wire::number(18, 99, &mut group);
        assert!(parse(&envelope(&group), owner(), PublicationKind::Threads, 1, 20).is_err());
        assert!(
            parse(
                b"<html>challenge</html>",
                owner(),
                PublicationKind::Threads,
                1,
                20
            )
            .is_err()
        );
    }
}
