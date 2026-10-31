// SPDX-License-Identifier: MIT
//! Read-only root/reply pages. Media are validated locators, never downloads.
//! Protocol: pskdje/bilibili-API-collect docs/comment/{list,readme}.md and
//! inorilzy/bilibili-api comment.get_comments_lazy.
use crate::{Client, Error, Result, api_data};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentOrder {
    Hot,
    Time,
}
impl CommentOrder {
    fn mode(self) -> u64 {
        match self {
            Self::Hot => 3,
            Self::Time => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentSource {
    Regular,
    Hot,
    PinnedUpper,
    PinnedAdmin,
    PinnedVote,
    PinnedTop,
    ReplyList,
    ReplyPreview,
}
impl CommentSource {
    fn pinned(self) -> bool {
        matches!(
            self,
            Self::PinnedUpper | Self::PinnedAdmin | Self::PinnedVote | Self::PinnedTop
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentMediaKind {
    Picture,
    Emote,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentReadAuthor {
    pub mid: Option<u64>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentReadMedia {
    pub kind: CommentMediaKind,
    /// The embedding runtime must keep this in a private asset locator.
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Filename hint only; not a decoded format or animation claim.
    pub format_hint: Option<String>,
    pub animated: Option<bool>,
    pub text: Option<String>,
    pub emote_id: Option<u64>,
    pub package_id: Option<u64>,
    pub emote_type: Option<u32>,
    pub size: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentRead {
    pub rpid: u64,
    pub oid: u64,
    pub author: CommentReadAuthor,
    pub message: String,
    pub created_at: Option<u64>,
    pub like_count: Option<u64>,
    pub reply_count: Option<u64>,
    pub up_liked: Option<bool>,
    /// Raw service action: 0 none, 1 liked, 2 disliked; missing stays unknown.
    pub user_action: Option<u32>,
    pub root: Option<u64>,
    pub parent: Option<u64>,
    pub state: Option<i64>,
    pub sources: Vec<CommentSource>,
    pub pinned: bool,
    pub media: Vec<CommentReadMedia>,
    pub replies: Vec<CommentRead>,
    pub reply_preview_truncated: bool,
    pub unknown_content_keys: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentReadPage {
    pub aid: u64,
    pub order: CommentOrder,
    pub items: Vec<CommentRead>,
    /// Extra placement records only. IDs already in items merge their sources.
    pub pinned: Vec<CommentRead>,
    pub hots: Vec<CommentRead>,
    pub has_more: Option<bool>,
    pub next_cursor: Option<String>,
    pub server_next_cursor: Option<String>,
    /// Opaque tokens may repeat while the returned comment page still changes.
    #[serde(default)]
    pub cursor_reused: bool,
    pub total: Option<u64>,
    pub truncated: bool,
    pub pagination_note: Option<String>,
    pub media_downloaded: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentReadReplies {
    pub aid: u64,
    pub root: u64,
    pub root_comment: CommentRead,
    pub items: Vec<CommentRead>,
    pub page: u32,
    pub page_size: u32,
    pub total: Option<u64>,
    pub has_more: Option<bool>,
    pub next_page: Option<u32>,
    pub truncated: bool,
    pub pagination_note: Option<String>,
    pub media_downloaded: bool,
}

impl Client {
    /// One WBI-signed page, mode 3 hot or mode 2 time. No automatic pagination.
    pub fn comment_page(
        &mut self,
        aid: u64,
        order: CommentOrder,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<CommentReadPage> {
        let params = comment_params(aid, order, cursor, limit)?;
        let data = self.signed_get("/x/v2/reply/wbi/main", &params)?;
        parse_page(data, aid, order, cursor, limit)
    }

    /// Read rich reply metadata without the own-author restriction of write preflight.
    pub fn comment_replies(
        &self,
        aid: u64,
        root: u64,
        page: u32,
        limit: u32,
    ) -> Result<CommentReadReplies> {
        valid_id(aid)?;
        valid_id(root)?;
        valid_limit(limit)?;
        if page == 0 || page > 100_000 {
            return Err(Error::InvalidPage);
        }
        let url = format!(
            "https://api.bilibili.com/x/v2/reply/reply?type=1&oid={aid}&root={root}&pn={page}&ps={limit}"
        );
        parse_replies(
            api_data(self.transport.json(&url)?, false)?,
            aid,
            root,
            page,
            limit,
        )
    }
}

fn valid_id(id: u64) -> Result<()> {
    if id == 0 || id > i64::MAX as u64 {
        Err(Error::InvalidId)
    } else {
        Ok(())
    }
}
fn valid_limit(limit: u32) -> Result<()> {
    if (1..=20).contains(&limit) {
        Ok(())
    } else {
        Err(Error::InvalidPage)
    }
}
fn valid_cursor(cursor: &str) -> Result<()> {
    if cursor.len() > 4096 || cursor.chars().any(char::is_control) {
        Err(Error::InvalidPage)
    } else {
        Ok(())
    }
}
fn comment_params(
    aid: u64,
    order: CommentOrder,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Vec<(&'static str, String)>> {
    valid_id(aid)?;
    valid_limit(limit)?;
    let cursor = cursor.unwrap_or("");
    valid_cursor(cursor)?;
    let pagination =
        serde_json::to_string(&json!({"offset":cursor})).map_err(|_| Error::InvalidPage)?;
    Ok(vec![
        ("oid", aid.to_string()),
        ("type", "1".into()),
        ("mode", order.mode().to_string()),
        ("pagination_str", pagination),
        ("plat", "1".into()),
        ("web_location", "1315875".into()),
    ])
}

fn optional_number(raw: &Value, name: &str) -> Result<Option<u64>> {
    match raw.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .or_else(|| {
                v.as_str()
                    .filter(|s| {
                        !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
                    })?
                    .parse()
                    .ok()
            })
            .map(Some)
            .ok_or(Error::Schema("comment read number")),
    }
}
fn number(raw: &Value, name: &str) -> Result<u64> {
    optional_number(raw, name)?.ok_or(Error::Schema("comment read missing number"))
}
fn optional_u32(raw: &Value, name: &str) -> Result<Option<u32>> {
    optional_number(raw, name)?
        .map(|v| u32::try_from(v).map_err(|_| Error::Schema("comment read number")))
        .transpose()
}
fn text(raw: &Value, name: &str, max: usize) -> Result<Option<String>> {
    match raw.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.len() <= max && !s.contains('\0') => Ok(Some(s.clone())),
        _ => Err(Error::Schema("comment read text")),
    }
}
fn flag(raw: &Value, name: &str) -> Result<Option<bool>> {
    match raw.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(*v)),
        Some(v) if v.as_u64() == Some(0) => Ok(Some(false)),
        Some(v) if v.as_u64() == Some(1) => Ok(Some(true)),
        _ => Err(Error::Schema("comment read flag")),
    }
}
fn challenge(data: &Value) -> Result<()> {
    if !data.is_object() {
        return Err(Error::Schema("comment read data"));
    }
    if data.get("v_voucher").is_some()
        || flag(data, "need_captcha")? == Some(true)
        || flag(data, "need_verify")? == Some(true)
    {
        return Err(Error::RiskControl(0));
    }
    Ok(())
}
fn media_url(raw: &str) -> Result<String> {
    let url = if let Some(tail) = raw.strip_prefix("http://") {
        format!("https://{tail}")
    } else if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.into()
    };
    let uri = crate::http::parse_url(&url, crate::http::UrlRole::Resource)?;
    let host = uri.host().ok_or(Error::UrlPolicy)?;
    if !(host == "hdslb.com" || host.ends_with(".hdslb.com")) {
        return Err(Error::UrlPolicy);
    }
    Ok(url)
}
fn format_hint(url: &str) -> Option<String> {
    let uri = url.parse::<ureq::http::Uri>().ok()?;
    let path = uri.path().split('@').next()?.to_ascii_lowercase();
    ["gif", "png", "jpg", "jpeg", "webp", "avif"]
        .into_iter()
        .find(|suffix| path.ends_with(&format!(".{suffix}")))
        .map(str::to_owned)
}
fn empty_media(kind: CommentMediaKind, url: String) -> CommentReadMedia {
    CommentReadMedia {
        kind,
        format_hint: format_hint(&url),
        url,
        width: None,
        height: None,
        animated: None,
        text: None,
        emote_id: None,
        package_id: None,
        emote_type: None,
        size: None,
    }
}
fn parse_media(content: &Value) -> Result<Vec<CommentReadMedia>> {
    let mut result = Vec::new();
    let mut seen_urls = BTreeSet::new();
    if let Some(raw) = content.get("pictures").filter(|v| !v.is_null()) {
        let items = raw.as_array().ok_or(Error::Schema("comment pictures"))?;
        if items.len() > 9 {
            return Err(Error::JsonLimit);
        }
        for item in items {
            let url =
                text(item, "img_src", 16384)?.ok_or(Error::Schema("comment picture source"))?;
            let mut media = empty_media(CommentMediaKind::Picture, media_url(&url)?);
            media.width = optional_u32(item, "img_width")?;
            media.height = optional_u32(item, "img_height")?;
            if seen_urls.insert(media.url.clone()) {
                result.push(media);
            }
        }
    }
    if let Some(raw) = content.get("emote").filter(|v| !v.is_null()) {
        let items = raw.as_object().ok_or(Error::Schema("comment emotes"))?;
        if items.len() > 128 {
            return Err(Error::JsonLimit);
        }
        for (token, item) in items {
            if token.len() > 256 {
                return Err(Error::JsonLimit);
            }
            let kind = optional_u32(item, "type")?;
            for field in ["url", "gif_url", "webp_url"] {
                let Some(url) = text(item, field, 16384)?.filter(|url| !url.is_empty()) else {
                    continue;
                };
                if field == "url"
                    && kind == Some(4)
                    && !(url.starts_with("http://")
                        || url.starts_with("https://")
                        || url.starts_with("//"))
                {
                    continue;
                }
                let mut media = empty_media(CommentMediaKind::Emote, media_url(&url)?);
                // These are service format hints, still not decoded animation facts.
                match field {
                    "gif_url" => media.format_hint = Some("gif".into()),
                    "webp_url" => media.format_hint = Some("webp".into()),
                    _ => {}
                }
                media.text = Some(token.clone());
                media.emote_id = optional_number(item, "id")?;
                media.package_id = optional_number(item, "package_id")?;
                media.emote_type = kind;
                if let Some(meta) = item.get("meta").filter(|v| !v.is_null()) {
                    media.size = optional_u32(meta, "size")?;
                }
                if seen_urls.insert(media.url.clone()) {
                    result.push(media);
                }
            }
        }
    }
    Ok(result)
}

pub(crate) fn parse_comment(
    raw: &Value,
    aid: u64,
    source: CommentSource,
    preview: bool,
) -> Result<CommentRead> {
    let rpid = number(raw, "rpid")?;
    valid_id(rpid)?;
    let oid = number(raw, "oid")?;
    if oid != aid {
        return Err(Error::Schema("comment oid mismatch"));
    }
    let member = raw.get("member").unwrap_or(&Value::Null);
    let top_mid = optional_number(raw, "mid")?.filter(|v| *v > 0);
    let member_mid = optional_number(member, "mid")?.filter(|v| *v > 0);
    if top_mid.zip(member_mid).is_some_and(|(a, b)| a != b) {
        return Err(Error::Schema("comment author mismatch"));
    }
    let content = raw
        .get("content")
        .filter(|v| v.is_object())
        .ok_or(Error::Schema("comment content"))?;
    let message = text(content, "message", 65536)?.ok_or(Error::Schema("comment message"))?;
    let state = match raw.get("state") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().ok_or(Error::Schema("comment state"))?),
    };
    let reply_count = optional_number(raw, "rcount")?.or(optional_number(raw, "count")?);
    let root = optional_number(raw, "root")?;
    let parent = optional_number(raw, "parent")?;
    let mut replies = Vec::new();
    let mut reply_preview_truncated = false;
    if let Some(children) = raw.get("replies").filter(|v| !v.is_null()) {
        let children = children
            .as_array()
            .ok_or(Error::Schema("comment reply preview"))?;
        if children.len() > 100 {
            return Err(Error::JsonLimit);
        }
        reply_preview_truncated = !children.is_empty();
        if preview {
            let mut seen = BTreeSet::new();
            for child in children.iter().take(3) {
                let parsed = parse_comment(child, aid, CommentSource::ReplyPreview, false)?;
                if parsed.root != Some(rpid) || parsed.parent.is_none_or(|n| n == 0) {
                    return Err(Error::Schema("comment preview relationship"));
                }
                if seen.insert(parsed.rpid) {
                    replies.push(parsed);
                }
            }
            reply_preview_truncated =
                children.len() > 3 || reply_count.is_some_and(|n| n > replies.len() as u64);
        }
    } else if reply_count.is_some_and(|n| n > 0) {
        reply_preview_truncated = true;
    }
    let unknown_content_keys = content
        .as_object()
        .ok_or(Error::Schema("comment content"))?
        .iter()
        .filter(|(key, value)| {
            ![
                "message", "pictures", "emote", "plat", "device", "members", "max_line",
            ]
            .contains(&key.as_str())
                && !value.is_null()
                && !value.as_object().is_some_and(|v| v.is_empty())
                && !value.as_array().is_some_and(|v| v.is_empty())
                && value.as_str() != Some("")
        })
        .map(|(key, _)| key.chars().take(128).collect::<String>())
        .take(32)
        .collect();
    Ok(CommentRead {
        rpid,
        oid,
        author: CommentReadAuthor {
            mid: top_mid.or(member_mid),
            name: text(member, "uname", 512)?,
        },
        message,
        created_at: optional_number(raw, "ctime")?,
        like_count: optional_number(raw, "like")?,
        reply_count,
        up_liked: flag(raw.get("up_action").unwrap_or(&Value::Null), "like")?,
        user_action: optional_u32(raw, "action")?,
        root,
        parent,
        state,
        sources: vec![source],
        pinned: source.pinned(),
        media: parse_media(content)?,
        replies,
        reply_preview_truncated,
        unknown_content_keys,
    })
}

fn root_comment(raw: &Value, aid: u64, source: CommentSource) -> Result<CommentRead> {
    let comment = parse_comment(raw, aid, source, true)?;
    if comment.root.is_some_and(|n| n != 0) || comment.parent.is_some_and(|n| n != 0) {
        return Err(Error::Schema("non-root comment in root list"));
    }
    Ok(comment)
}
fn push_unique(
    records: &mut Vec<CommentRead>,
    indices: &mut BTreeMap<u64, usize>,
    record: CommentRead,
) -> Result<()> {
    if let Some(index) = indices.get(&record.rpid) {
        let stored = &mut records[*index];
        if stored.oid != record.oid
            || stored
                .author
                .mid
                .zip(record.author.mid)
                .is_some_and(|(a, b)| a != b)
        {
            return Err(Error::Schema("duplicate comment identity mismatch"));
        }
        for source in record.sources {
            if !stored.sources.contains(&source) {
                stored.sources.push(source);
            }
        }
        stored.pinned |= record.pinned;
    } else {
        indices.insert(record.rpid, records.len());
        records.push(record);
    }
    Ok(())
}
fn raw_list<'a>(
    value: Option<&'a Value>,
    name: &'static str,
    maximum: usize,
) -> Result<&'a [Value]> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) if items.len() <= maximum => Ok(items),
        _ => Err(Error::Schema(name)),
    }
}

fn parse_page(
    data: Value,
    aid: u64,
    order: CommentOrder,
    current: Option<&str>,
    limit: u32,
) -> Result<CommentReadPage> {
    challenge(&data)?;
    let cursor = data
        .get("cursor")
        .filter(|v| v.is_object())
        .ok_or(Error::Schema("comment cursor"))?;
    if optional_number(cursor, "mode")?.is_some_and(|n| n != order.mode()) {
        return Err(Error::Schema("comment sort mismatch"));
    }
    let end = flag(cursor, "is_end")?;
    let total = optional_number(cursor, "all_count")?;
    let server_next_cursor = match cursor.pointer("/pagination_reply/next_offset") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            valid_cursor(s).map_err(|_| Error::Schema("comment server cursor"))?;
            (!s.is_empty()).then(|| s.clone())
        }
        _ => return Err(Error::Schema("comment server cursor")),
    };
    if !data.as_object().unwrap().contains_key("replies") {
        return Err(Error::Schema("comment replies missing"));
    }
    let regular = raw_list(data.get("replies"), "comment replies", 100)?;
    if data.get("replies") == Some(&Value::Null) && end != Some(true) && total != Some(0) {
        return Err(Error::Schema("null comment list without empty proof"));
    }
    let truncated = regular.len() > limit as usize;
    let mut records = Vec::new();
    let mut indices = BTreeMap::new();
    for raw in regular.iter().take(limit as usize) {
        push_unique(
            &mut records,
            &mut indices,
            root_comment(raw, aid, CommentSource::Regular)?,
        )?;
    }
    let mut pinned_raw = Vec::new();
    for (path, source) in [
        ("/upper/top", CommentSource::PinnedUpper),
        ("/top/upper", CommentSource::PinnedUpper),
        ("/top/admin", CommentSource::PinnedAdmin),
        ("/top/vote", CommentSource::PinnedVote),
    ] {
        if let Some(raw) = data.pointer(path).filter(|v| !v.is_null()) {
            pinned_raw.push((raw, source));
        }
    }
    if let Some(raw) = data.get("top_replies").filter(|v| !v.is_null()) {
        if raw.is_object() && raw.get("rpid").is_some() {
            pinned_raw.push((raw, CommentSource::PinnedTop));
        } else {
            for item in raw_list(Some(raw), "pinned comments", 16)? {
                pinned_raw.push((item, CommentSource::PinnedTop));
            }
        }
    }
    if pinned_raw.len() > 20 {
        return Err(Error::JsonLimit);
    }
    for (raw, source) in pinned_raw {
        push_unique(&mut records, &mut indices, root_comment(raw, aid, source)?)?;
    }
    for raw in raw_list(data.get("hots"), "hot comments", 20)? {
        push_unique(
            &mut records,
            &mut indices,
            root_comment(raw, aid, CommentSource::Hot)?,
        )?;
    }
    let mut items = Vec::new();
    let mut pinned = Vec::new();
    let mut hots = Vec::new();
    for record in records {
        if record.sources.contains(&CommentSource::Regular) {
            items.push(record);
        } else if record.pinned {
            pinned.push(record);
        } else {
            hots.push(record);
        }
    }
    let has_more = end.map(|v| !v);
    let cursor_reused = server_next_cursor
        .as_deref()
        .is_some_and(|n| Some(n) == current);
    let next_cursor = if truncated || end == Some(true) || items.is_empty() {
        None
    } else {
        server_next_cursor.clone()
    };
    let pagination_note = if truncated {
        Some("The server's regular page exceeded the requested limit; next_cursor is withheld to avoid skipping unreturned comments. Re-read this cursor with limit=20.".into())
    } else if items.is_empty() {
        Some("The regular comment page is empty; next_cursor is withheld to avoid an empty-page loop. Do not infer full exhaustion if the server still reports more comments.".into())
    } else if cursor_reused && next_cursor.is_some() {
        Some("The server reused an opaque cursor; different pages can share this token. Deduplicate by rpid and stop on is_end, an empty regular page, or a page with no new comment IDs. Keep reads bounded; never loop indefinitely on this token.".into())
    } else if has_more == Some(true) && next_cursor.is_none() {
        Some("The server reports more comments but supplied no usable next_offset; pagination is incomplete.".into())
    } else if has_more.is_none() {
        Some("The server did not provide is_end; end-of-list is unknown.".into())
    } else {
        None
    };
    Ok(CommentReadPage {
        aid,
        order,
        items,
        pinned,
        hots,
        has_more,
        next_cursor,
        server_next_cursor,
        cursor_reused,
        total,
        truncated,
        pagination_note,
        media_downloaded: false,
    })
}

fn parse_replies(
    data: Value,
    aid: u64,
    root: u64,
    requested_page: u32,
    limit: u32,
) -> Result<CommentReadReplies> {
    challenge(&data)?;
    let root_comment = root_comment(
        data.get("root").ok_or(Error::Schema("reply root"))?,
        aid,
        CommentSource::Regular,
    )?;
    if root_comment.rpid != root {
        return Err(Error::Schema("reply root mismatch"));
    }
    if !data.as_object().unwrap().contains_key("replies") {
        return Err(Error::Schema("reply items missing"));
    }
    let raw = raw_list(data.get("replies"), "reply items", 100)?;
    let page_meta = data.get("page").unwrap_or(&Value::Null);
    let server_page = optional_u32(page_meta, "num")?;
    let server_size = optional_u32(page_meta, "size")?;
    let total = optional_number(page_meta, "count")?;
    if data.get("replies") == Some(&Value::Null) && total != Some(0) {
        return Err(Error::Schema("null reply list without empty proof"));
    }
    if server_page == Some(0) || server_size == Some(0) {
        return Err(Error::Schema("reply page metadata"));
    }
    let truncated = raw.len() > limit as usize;
    let mut items = Vec::new();
    let mut indices = BTreeMap::new();
    for raw in raw.iter().take(limit as usize) {
        let record = parse_comment(raw, aid, CommentSource::ReplyList, false)?;
        if record.root != Some(root) || record.parent.is_none_or(|n| n == 0) {
            return Err(Error::Schema("reply relationship mismatch"));
        }
        push_unique(&mut items, &mut indices, record)?;
    }
    let page = server_page.unwrap_or(requested_page);
    let page_size = server_size.unwrap_or(limit);
    let has_more = server_page
        .zip(server_size)
        .zip(total)
        .map(|((page, size), total)| u64::from(page) * u64::from(size) < total);
    let mut next_page = if !truncated && has_more == Some(true) {
        page.checked_add(1)
    } else {
        None
    };
    let pagination_note = if truncated {
        Some("Reply page exceeded the requested limit; no next page is given to avoid skipping omitted replies.".into())
    } else if page != requested_page {
        next_page = None;
        Some("The server returned a different page number; no continuation was inferred.".into())
    } else if has_more.is_none() {
        Some("The server did not provide complete page metadata; continuation is unknown.".into())
    } else {
        None
    };
    Ok(CommentReadReplies {
        aid,
        root,
        root_comment,
        items,
        page,
        page_size,
        total,
        has_more,
        next_page,
        truncated,
        pagination_note,
        media_downloaded: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn comment(id: u64) -> Value {
        json!({"rpid":id,"oid":42,"mid":7,"root":0,"parent":0,"ctime":1700000000,
        "like":12,"rcount":0,"action":1,"member":{"mid":"7","uname":"reader","private":"hidden"},
        "up_action":{"like":true},"content":{"message":"actual text","private_extension":{"value":"hidden"}},"replies":null})
    }
    fn page(items: Vec<Value>) -> Value {
        json!({"cursor":{"mode":3,"is_end":false,"all_count":30,
        "pagination_reply":{"next_offset":"opaque-next"}},"replies":items,"hots":[],"top_replies":[]})
    }
    #[test]
    fn wbi_parameters_preserve_opaque_cursor_and_cannot_route_writes() {
        let raw = "{\"type\":3,\"Data\":{\"cursor\":4}}";
        let params = comment_params(42, CommentOrder::Time, Some(raw), 20).unwrap();
        let json: Value = serde_json::from_str(
            &params
                .iter()
                .find(|(k, _)| *k == "pagination_str")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(json["offset"], raw);
        assert!(params.contains(&("mode", "2".into())));
        assert!(comment_params(0, CommentOrder::Hot, None, 20).is_err());
        assert!(comment_params(42, CommentOrder::Hot, None, 21).is_err());
        assert!(comment_params(42, CommentOrder::Hot, Some("bad\n"), 20).is_err());
    }
    #[test]
    fn root_lists_deduplicate_placements_without_spending_the_regular_budget() {
        let mut raw = page(vec![comment(1), comment(2)]);
        raw["top_replies"] = json!([comment(1), comment(3)]);
        raw["top"] = json!({"upper":comment(3)});
        raw["hots"] = json!([comment(2), comment(4)]);
        let parsed = parse_page(raw, 42, CommentOrder::Hot, None, 2).unwrap();
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.pinned.len(), 1);
        assert_eq!(parsed.hots.len(), 1);
        assert!(parsed.items[0].pinned);
        assert!(parsed.items[1].sources.contains(&CommentSource::Hot));
        assert_eq!(parsed.items[0].like_count, Some(12));
        assert_eq!(parsed.items[0].up_liked, Some(true));
        assert_eq!(parsed.items[0].author.name.as_deref(), Some("reader"));
        assert_eq!(parsed.next_cursor.as_deref(), Some("opaque-next"));
        assert!(!parsed.media_downloaded);
        assert!(!serde_json::to_string(&parsed).unwrap().contains("hidden"));
    }
    #[test]
    fn truncation_and_unknown_pagination_never_invent_a_safe_continuation() {
        let parsed = parse_page(
            page(vec![comment(1), comment(2)]),
            42,
            CommentOrder::Hot,
            None,
            1,
        )
        .unwrap();
        assert!(parsed.truncated && parsed.next_cursor.is_none());
        assert_eq!(parsed.server_next_cursor.as_deref(), Some("opaque-next"));
        let mut raw = page(vec![]);
        raw["cursor"].as_object_mut().unwrap().remove("is_end");
        let parsed = parse_page(raw, 42, CommentOrder::Hot, None, 20).unwrap();
        assert_eq!(parsed.has_more, None);
        assert!(parsed.pagination_note.is_some());
        assert!(
            parse_page(
                json!({"cursor":{},"replies":null}),
                42,
                CommentOrder::Hot,
                None,
                20
            )
            .is_err()
        );
        assert!(
            parse_page(
                json!({"cursor":{},"need_captcha":true,"replies":[]}),
                42,
                CommentOrder::Hot,
                None,
                20
            )
            .is_err()
        );
        assert!(parse_page(json!({"replies":[]}), 42, CommentOrder::Hot, None, 20).is_err());
    }
    #[test]
    fn rich_media_are_references_and_gif_names_do_not_prove_animation() {
        let mut raw = comment(1);
        raw["content"] = json!({"message":"[complex]","pictures":[{"img_src":"http://i0.hdslb.com/bfs/new_dyn/file.gif","img_width":160,"img_height":120}],
            "emote":{"[complex]":{"id":99,"package_id":88,"type":3,"url":"https://i0.hdslb.com/bfs/emote/complex.png","meta":{"size":2}}}});
        let parsed = root_comment(&raw, 42, CommentSource::Regular).unwrap();
        assert_eq!(parsed.media.len(), 2);
        assert_eq!(parsed.media[0].format_hint.as_deref(), Some("gif"));
        assert_eq!(parsed.media[0].animated, None);
        assert_eq!(parsed.media[1].size, Some(2));
        assert!(parsed.media[0].url.starts_with("https://"));
        raw["content"]["pictures"][0]["img_src"] = json!("https://evil.example/a.gif");
        assert!(root_comment(&raw, 42, CommentSource::Regular).is_err());
    }
    #[test]
    fn replies_keep_rich_fields_and_reject_cross_video_or_root_data() {
        let mut child = comment(2);
        child["root"] = json!(1);
        child["parent"] = json!(1);
        let raw = json!({"root":comment(1),"replies":[child],"page":{"num":1,"size":20,"count":1}});
        let parsed = parse_replies(raw.clone(), 42, 1, 1, 20).unwrap();
        assert_eq!(parsed.items[0].like_count, Some(12));
        assert_eq!(parsed.has_more, Some(false));
        let mut wrong = raw.clone();
        wrong["replies"][0]["oid"] = json!(43);
        assert!(parse_replies(wrong, 42, 1, 1, 20).is_err());
        let mut wrong = raw;
        wrong["replies"][0]["root"] = json!(999);
        assert!(parse_replies(wrong, 42, 1, 1, 20).is_err());
        let mut missing = comment(1);
        missing.as_object_mut().unwrap().remove("like");
        assert_eq!(
            root_comment(&missing, 42, CommentSource::Regular)
                .unwrap()
                .like_count,
            None
        );
    }

    #[test]
    fn dynamic_emote_variants_remain_lazy_distinct_and_unverified() {
        let mut content = json!({"message":"[dynamic_expect]","emote":{"[dynamic_expect]":{
            "id":99,"package_id":88,"type":3,"meta":{"size":2},
            "url":"https://i0.hdslb.com/bfs/garb/poster.png",
            "gif_url":"http://i0.hdslb.com/bfs/garb/motion.gif",
            "webp_url":"https://i0.hdslb.com/bfs/garb/motion.webp"}}});
        let media = parse_media(&content).unwrap();
        assert_eq!(media.len(), 3);
        assert_eq!(
            media
                .iter()
                .map(|m| m.format_hint.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("png"), Some("gif"), Some("webp")]
        );
        assert!(media.iter().all(|m| m.kind == CommentMediaKind::Emote
            && m.text.as_deref() == Some("[dynamic_expect]")
            && m.emote_id == Some(99)
            && m.package_id == Some(88)
            && m.animated.is_none()));
        content["emote"]["[dynamic_expect]"]["webp_url"] =
            json!("https://i0.hdslb.com/bfs/garb/motion.gif");
        assert_eq!(parse_media(&content).unwrap().len(), 2);
        content["emote"]["[dynamic_expect]"]["url"] = Value::Null;
        assert_eq!(parse_media(&content).unwrap().len(), 1);
        content["emote"]["[dynamic_expect]"]["gif_url"] = json!("https://evil.example/motion.gif");
        assert!(parse_media(&content).is_err());
    }

    #[test]
    fn repeated_opaque_cursor_keeps_distinct_pages_but_empty_pages_stop() {
        let token = "opaque-next";
        let first = parse_page(
            page(vec![comment(1)]),
            42,
            CommentOrder::Hot,
            Some(token),
            20,
        )
        .unwrap();
        let second = parse_page(
            page(vec![comment(2)]),
            42,
            CommentOrder::Hot,
            Some(token),
            20,
        )
        .unwrap();
        assert_ne!(first.items[0].rpid, second.items[0].rpid);
        for parsed in [first, second] {
            assert!(parsed.cursor_reused);
            assert_eq!(parsed.next_cursor.as_deref(), Some(token));
            assert_eq!(parsed.has_more, Some(true));
            assert!(
                parsed
                    .pagination_note
                    .as_deref()
                    .unwrap()
                    .contains("no new comment IDs")
            );
        }
        let empty = parse_page(page(vec![]), 42, CommentOrder::Hot, Some(token), 20).unwrap();
        assert!(empty.items.is_empty() && empty.cursor_reused && empty.next_cursor.is_none());
        assert_eq!(empty.has_more, Some(true));
        assert_eq!(empty.server_next_cursor.as_deref(), Some(token));
        assert!(
            empty
                .pagination_note
                .as_deref()
                .unwrap()
                .contains("empty-page loop")
        );
        let mut ended = page(vec![comment(3)]);
        ended["cursor"]["is_end"] = json!(true);
        let ended = parse_page(ended, 42, CommentOrder::Hot, Some(token), 20).unwrap();
        assert_eq!(ended.has_more, Some(false));
        assert!(ended.next_cursor.is_none());
        let truncated = parse_page(
            page(vec![comment(1), comment(2)]),
            42,
            CommentOrder::Hot,
            Some(token),
            1,
        )
        .unwrap();
        assert!(truncated.cursor_reused && truncated.truncated && truncated.next_cursor.is_none());
    }
}
