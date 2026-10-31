// SPDX-License-Identifier: MIT
//! Authorized own-video comments. Host tools journal each operation before POST.
//! Protocol source: bilibili-api-collect docs/comment/{action,list}.md.
use crate::{Client, Error, Result, UploadedCommentImage, VideoId, api_data};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentTarget {
    pub aid: u64,
    pub bvid: String,
    pub title: String,
    pub owner_mid: u64,
    #[serde(default)]
    pub actor_mid: u64,
}

impl CommentTarget {
    /// Older own-video records did not distinguish author and authenticated actor.
    pub fn actor_mid(&self) -> u64 {
        if self.actor_mid == 0 {
            self.owner_mid
        } else {
            self.actor_mid
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommentPicture {
    pub img_src: String,
    pub img_width: u32,
    pub img_height: u32,
    #[serde(default)]
    pub img_size: f64,
}
impl CommentPicture {
    pub(crate) fn validate(&self) -> Result<()> {
        picture_url(&self.img_src)?;
        if self.img_width == 0
            || self.img_height == 0
            || self.img_width > 4096
            || self.img_height > 4096
            || u64::from(self.img_width) * u64::from(self.img_height) > 8_000_000
            || !self.img_size.is_finite()
            || !(0.0..=16384.0).contains(&self.img_size)
        {
            return Err(Error::InvalidComment);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentEmote {
    pub text: String,
    pub id: Option<u64>,
    pub url: Option<String>,
    #[serde(default)]
    pub package_id: Option<u64>,
    #[serde(default, rename = "type")]
    pub kind: Option<u32>,
    #[serde(default)]
    pub attr: Option<u64>,
    #[serde(default)]
    pub meta: CommentEmoteMeta,
    #[serde(default)]
    pub flags: CommentEmoteFlags,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentEmoteMeta {
    pub size: Option<u32>,
    pub alias: Option<String>,
    #[serde(default)]
    pub suggest: Vec<String>,
}

/// Preserve explicit false separately from missing. In particular, `unlocked`
/// is an observed service flag, not a documented purchase/access verdict.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentEmoteFlags {
    pub added: Option<bool>,
    pub no_access: Option<bool>,
    pub preview: Option<bool>,
    pub unlocked: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentEmotePackage {
    pub id: u64,
    pub text: String,
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<u32>,
    pub attr: Option<u64>,
    pub meta: CommentEmoteMeta,
    pub flags: CommentEmoteFlags,
    pub emote: Vec<CommentEmote>,
}

/// Consumed by the only write method. No Clone or public serialization.
pub struct PreparedComment {
    target: CommentTarget,
    message: String,
    csrf: String,
    cookie_binding: String,
    root: u64,
    parent: u64,
    pictures: Vec<CommentPicture>,
}
impl fmt::Debug for PreparedComment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedComment")
            .field("target", &self.target)
            .field("private_payload", &"[redacted]")
            .finish()
    }
}
impl PreparedComment {
    pub fn target(&self) -> &CommentTarget {
        &self.target
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn root(&self) -> u64 {
        self.root
    }
    pub fn parent(&self) -> u64 {
        self.parent
    }
    pub fn pictures(&self) -> &[CommentPicture] {
        &self.pictures
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VideoComment {
    pub rpid: u64,
    pub oid: u64,
    pub author_mid: u64,
    pub message: String,
    pub state: Option<i64>,
    #[serde(default)]
    pub root: u64,
    #[serde(default)]
    pub parent: u64,
    #[serde(default)]
    pub pictures: Vec<CommentPicture>,
    #[serde(default)]
    pub emote: Vec<CommentEmote>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentReplyPage {
    pub root: VideoComment,
    pub replies: Vec<VideoComment>,
    pub page: u32,
    pub page_size: u32,
    pub total: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentReceipt {
    pub rpid: u64,
    pub oid: u64,
    pub bvid: String,
    pub echoed: Option<VideoComment>,
    pub echo_matches: Option<bool>,
    pub need_captcha: Option<bool>,
}

impl Client {
    /// Read-only preflight; the embedding runtime must reserve its ledger next.
    pub fn prepare_own_video_comment(
        &self,
        id: &VideoId,
        message: &str,
    ) -> Result<PreparedComment> {
        self.prepare_own_video_comment_with_pictures(id, message, Vec::new())
    }

    pub fn prepare_own_video_comment_with_pictures(
        &self,
        id: &VideoId,
        message: &str,
        pictures: Vec<UploadedCommentImage>,
    ) -> Result<PreparedComment> {
        self.prepare_comment(id, 0, 0, message, pictures, true)
    }

    pub fn prepare_own_video_reply(
        &self,
        id: &VideoId,
        root: u64,
        parent: u64,
        message: &str,
        pictures: Vec<UploadedCommentImage>,
    ) -> Result<PreparedComment> {
        validate_id(root)?;
        validate_id(parent)?;
        self.prepare_comment(id, root, parent, message, pictures, true)
    }

    pub fn prepare_video_comment_with_pictures(
        &self,
        id: &VideoId,
        message: &str,
        pictures: Vec<UploadedCommentImage>,
    ) -> Result<PreparedComment> {
        self.prepare_comment(id, 0, 0, message, pictures, false)
    }
    pub fn prepare_video_reply(
        &self,
        id: &VideoId,
        root: u64,
        parent: u64,
        message: &str,
        pictures: Vec<UploadedCommentImage>,
    ) -> Result<PreparedComment> {
        validate_id(root)?;
        validate_id(parent)?;
        self.prepare_comment(id, root, parent, message, pictures, false)
    }
    pub(crate) fn prepare_comment_target(
        &self,
        id: &VideoId,
    ) -> Result<(CommentTarget, String, String)> {
        let cookie = self.transport.cookie_header().ok_or(Error::LoginRequired)?;
        let csrf = csrf_from_cookie(cookie)?;
        let actor = self.current_user()?;
        let video = self.video_info(id)?;
        if video.owner.id != actor.mid {
            return Err(Error::NotOwnVideo);
        }
        video.bvid.parse::<VideoId>()?;
        Ok((
            CommentTarget {
                aid: video.aid,
                bvid: video.bvid,
                title: video.title,
                owner_mid: video.owner.id,
                actor_mid: actor.mid,
            },
            csrf,
            cookie.to_owned(),
        ))
    }
    pub(crate) fn prepare_public_comment_target(
        &self,
        id: &VideoId,
    ) -> Result<(CommentTarget, String, String)> {
        let cookie = self.transport.cookie_header().ok_or(Error::LoginRequired)?;
        let csrf = csrf_from_cookie(cookie)?;
        let actor = self.current_user()?;
        let video = self.video_info(id)?;
        video.bvid.parse::<VideoId>()?;
        Ok((
            CommentTarget {
                aid: video.aid,
                bvid: video.bvid,
                title: video.title,
                owner_mid: video.owner.id,
                actor_mid: actor.mid,
            },
            csrf,
            cookie.to_owned(),
        ))
    }

    fn prepare_comment(
        &self,
        id: &VideoId,
        root: u64,
        parent: u64,
        message: &str,
        images: Vec<UploadedCommentImage>,
        own_only: bool,
    ) -> Result<PreparedComment> {
        validate_message(message)?;
        if images.len() > 3 {
            return Err(Error::InvalidComment);
        }
        let (target, csrf, cookie_binding) = if own_only {
            self.prepare_comment_target(id)?
        } else {
            self.prepare_public_comment_target(id)?
        };
        let mut pictures = Vec::with_capacity(images.len());
        for image in images {
            if image.actor_mid() != target.actor_mid() {
                return Err(Error::CommentPreparationChanged);
            }
            image.picture().validate()?;
            pictures.push(image.picture().clone());
        }
        if root != 0 {
            let head = self
                .comment_by_id(target.aid, root)?
                .ok_or(Error::Schema("reply root missing"))?;
            if (own_only && head.author_mid != target.actor_mid())
                || head.root != 0
                || head.parent != 0
            {
                return Err(Error::NotOwnVideo);
            }
            if parent != root {
                let child = self
                    .reply_by_id(target.aid, root, parent)?
                    .ok_or(Error::Schema("reply parent missing"))?;
                if own_only && child.author_mid != target.actor_mid() {
                    return Err(Error::NotOwnVideo);
                }
            }
        }
        Ok(PreparedComment {
            target,
            message: message.to_owned(),
            csrf,
            cookie_binding,
            root,
            parent,
            pictures,
        })
    }

    /// One POST only. Does no network preflight, retry, redirect or readback.
    /// Any failure after dispatch requires reconciliation, never blind resending.
    pub fn submit_prepared_comment(&self, prepared: PreparedComment) -> Result<CommentReceipt> {
        if self.transport.cookie_header() != Some(prepared.cookie_binding.as_str()) {
            return Err(Error::CommentPreparationChanged);
        }
        validate_message(&prepared.message)?;
        let body = comment_form(&prepared)?;
        let response = self
            .transport
            .post_root_comment(&body, &prepared.target.bvid)?;
        parse_receipt(response, &prepared)
    }

    /// Read the exact root comment by ID using the documented reply endpoint.
    /// An absent result is not an invitation to resend.
    pub fn comment_by_id(&self, aid: u64, rpid: u64) -> Result<Option<VideoComment>> {
        validate_id(aid)?;
        validate_id(rpid)?;
        let url = format!(
            "https://api.bilibili.com/x/v2/reply/reply?type=1&oid={aid}&root={rpid}&ps=1&pn=1"
        );
        let value = self.transport.json(&url)?;
        if value.get("code").and_then(Value::as_i64) == Some(12006) {
            return Ok(None);
        }
        let data = api_data(value, false)?;
        match data.get("root") {
            None => Err(Error::Schema("missing comment root")),
            Some(Value::Null) => Ok(None),
            Some(root) => {
                let comment = parse_comment(root)?;
                if comment.rpid != rpid || comment.oid != aid {
                    return Err(Error::Schema("comment readback target mismatch"));
                }
                Ok(Some(comment))
            }
        }
    }

    pub fn reply_page(
        &self,
        aid: u64,
        root: u64,
        page: u32,
        page_size: u32,
    ) -> Result<CommentReplyPage> {
        validate_id(aid)?;
        validate_id(root)?;
        if page == 0 || page > 100000 || page_size == 0 || page_size > 20 {
            return Err(Error::InvalidPage);
        }
        let url = format!(
            "https://api.bilibili.com/x/v2/reply/reply?type=1&oid={aid}&root={root}&pn={page}&ps={page_size}"
        );
        parse_reply_page(
            api_data(self.transport.json(&url)?, false)?,
            aid,
            root,
            page,
            page_size,
        )
    }

    /// Reads one bounded page and, when needed, one native seek query. Missing
    /// or ambiguous relationships never trigger a write.
    pub fn reply_by_id(&self, aid: u64, root: u64, rpid: u64) -> Result<Option<VideoComment>> {
        validate_id(rpid)?;
        if rpid == root {
            return Err(Error::InvalidId);
        }
        let page = self.reply_page(aid, root, 1, 20)?;
        if let Some(found) = page.replies.into_iter().find(|r| r.rpid == rpid) {
            return Ok(Some(found));
        }
        // Do not make callers traverse pages merely to reply to a returned
        // target. The service seek result must attest the exact thread.
        let nav = api_data(
            self.transport
                .json("https://api.bilibili.com/x/web-interface/nav")?,
            true,
        )?;
        let key = crate::wbi::WbiKey::from_nav(&nav)?;
        let params = [
            ("type", "1".into()),
            ("oid", aid.to_string()),
            ("mode", "3".into()),
            ("pagination_str", "{\"offset\":\"\"}".into()),
            ("plat", "1".into()),
            ("seek_rpid", rpid.to_string()),
            ("web_location", "1315875".into()),
        ];
        let query = key.sign(&params, crate::wbi::unix_seconds()?)?;
        let data = api_data(
            self.transport.json(&format!(
                "https://api.bilibili.com/x/v2/reply/wbi/main?{query}"
            ))?,
            false,
        )?;
        let Some(head) = data.get("seek_root_reply").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let head_comment = parse_comment(head)?;
        if head_comment.oid != aid || head_comment.rpid != root || head_comment.root != 0 {
            return Err(Error::Schema("seek reply root mismatch"));
        }
        let Some(items) = head.get("replies").and_then(Value::as_array) else {
            return Ok(None);
        };
        if items.len() > 100 {
            return Err(Error::JsonLimit);
        }
        for raw in items {
            let c = parse_comment(raw)?;
            if c.oid != aid || c.root != root {
                return Err(Error::Schema("seek reply relationship"));
            }
            if c.rpid == rpid {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }

    pub fn comment_emotes(&self) -> Result<Vec<CommentEmote>> {
        parse_emote_catalog(self.comment_emote_data()?)
    }

    /// This is the authenticated user panel, not a purchase history. Package and
    /// entry flags retain missing/false/true rather than inferring entitlement.
    pub fn comment_emote_packages(&self) -> Result<Vec<CommentEmotePackage>> {
        parse_emote_packages(self.comment_emote_data()?)
    }

    fn comment_emote_data(&self) -> Result<Value> {
        self.transport.cookie_header().ok_or(Error::LoginRequired)?;
        api_data(
            self.transport
                .json("https://api.bilibili.com/x/emote/user/panel/web?business=reply")?,
            false,
        )
    }
}

pub(crate) fn picture_url(raw: &str) -> Result<String> {
    let normalized = if let Some(tail) = raw.strip_prefix("http://") {
        format!("https://{tail}")
    } else if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_owned()
    };
    let uri = crate::http::parse_url(&normalized, crate::http::UrlRole::Resource)?;
    let host = uri.host().ok_or(Error::UrlPolicy)?;
    if !(host == "hdslb.com" || host.ends_with(".hdslb.com"))
        || !uri.path().starts_with("/bfs/")
        || uri.path().contains("..")
        || uri.query().is_some()
    {
        return Err(Error::UrlPolicy);
    }
    Ok(normalized)
}

fn parse_reply_page(
    data: Value,
    aid: u64,
    root: u64,
    page: u32,
    page_size: u32,
) -> Result<CommentReplyPage> {
    let head = parse_comment(data.get("root").ok_or(Error::Schema("reply root"))?)?;
    if head.oid != aid || head.rpid != root || head.root != 0 {
        return Err(Error::Schema("reply page root mismatch"));
    }
    let mut replies = Vec::new();
    match data.get("replies") {
        Some(Value::Array(items)) if items.len() <= page_size as usize => {
            for raw in items {
                let reply = parse_comment(raw)?;
                if reply.oid != aid || reply.root != root || reply.parent == 0 {
                    return Err(Error::Schema("reply relationship mismatch"));
                }
                replies.push(reply);
            }
        }
        Some(Value::Null) => {}
        _ => return Err(Error::Schema("reply list")),
    }
    let total = data
        .pointer("/page/count")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
    Ok(CommentReplyPage {
        root: head,
        replies,
        page,
        page_size,
        total,
    })
}

fn parse_emote(raw: &Value, fallback: Option<&str>) -> Result<CommentEmote> {
    let text = raw
        .get("text")
        .and_then(Value::as_str)
        .or(fallback)
        .ok_or(Error::Schema("emote text"))?;
    if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
        return Err(Error::Schema("emote text"));
    }
    let id = match raw.get("id") {
        None | Some(Value::Null) => None,
        Some(_) => Some(integer(raw, "id")?),
    };
    let url = match raw.get("url") {
        None | Some(Value::Null) => None,
        Some(Value::String(url))
            if raw.get("type").and_then(Value::as_u64) == Some(4)
                && !(url.starts_with("http://")
                    || url.starts_with("https://")
                    || url.starts_with("//")) =>
        {
            None
        }
        Some(v) => Some(picture_url(v.as_str().ok_or(Error::Schema("emote url"))?)?),
    };
    Ok(CommentEmote {
        text: text.into(),
        id,
        url,
        package_id: match raw.get("package_id") {
            None | Some(Value::Null) => None,
            Some(_) => Some(integer(raw, "package_id")?),
        },
        kind: optional_u32(raw, "type")?,
        attr: optional_u64(raw, "attr")?,
        meta: parse_emote_meta(raw.get("meta"))?,
        flags: parse_emote_flags(raw.get("flags"))?,
    })
}

fn optional_u64(raw: &Value, name: &str) -> Result<Option<u64>> {
    match raw.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str()?.parse().ok())
            .map(Some)
            .ok_or(Error::Schema("emote number")),
    }
}
fn optional_u32(raw: &Value, name: &str) -> Result<Option<u32>> {
    optional_u64(raw, name)?
        .map(|n| u32::try_from(n).map_err(|_| Error::Schema("emote number")))
        .transpose()
}
fn optional_text(raw: &Value, name: &str) -> Result<Option<String>> {
    match raw.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let text = value
                .as_str()
                .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
                .ok_or(Error::Schema("emote metadata text"))?;
            Ok(Some(text.into()))
        }
    }
}
fn parse_emote_meta(raw: Option<&Value>) -> Result<CommentEmoteMeta> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else {
        return Ok(CommentEmoteMeta::default());
    };
    if !raw.is_object() {
        return Err(Error::Schema("emote meta"));
    }
    let suggest = match raw.get("suggest") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) if items.len() <= 32 => items
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
                    .map(str::to_owned)
                    .ok_or(Error::Schema("emote suggestion"))
            })
            .collect::<Result<Vec<_>>>()?,
        _ => return Err(Error::Schema("emote suggestions")),
    };
    Ok(CommentEmoteMeta {
        size: optional_u32(raw, "size")?,
        alias: optional_text(raw, "alias")?,
        suggest,
    })
}
fn parse_emote_flags(raw: Option<&Value>) -> Result<CommentEmoteFlags> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else {
        return Ok(CommentEmoteFlags::default());
    };
    if !raw.is_object() {
        return Err(Error::Schema("emote flags"));
    }
    let flag = |name: &str| -> Result<Option<bool>> {
        match raw.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_bool().map(Some).ok_or(Error::Schema("emote flag")),
        }
    };
    Ok(CommentEmoteFlags {
        added: flag("added")?,
        no_access: flag("no_access")?,
        preview: flag("preview")?,
        unlocked: flag("unlocked")?,
    })
}

fn parse_emote_packages(data: Value) -> Result<Vec<CommentEmotePackage>> {
    let packages = data
        .get("packages")
        .and_then(Value::as_array)
        .ok_or(Error::Schema("emote packages"))?;
    if packages.len() > 256 {
        return Err(Error::JsonLimit);
    }
    let mut result = Vec::new();
    let mut count = 0usize;
    for package in packages {
        let items = match package.get("emote") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(items)) => items.as_slice(),
            _ => return Err(Error::Schema("package emote list")),
        };
        let header = parse_emote(package, None)?;
        let id = header.id.ok_or(Error::Schema("emote package id"))?;
        let mut emote = Vec::new();
        for item in items {
            if count >= 4096 {
                return Err(Error::JsonLimit);
            }
            let entry = parse_emote(item, None)?;
            if entry.package_id.is_some_and(|pid| pid != id) {
                return Err(Error::Schema("emote package mismatch"));
            }
            emote.push(entry);
            count += 1;
        }
        result.push(CommentEmotePackage {
            id,
            text: header.text,
            url: header.url,
            kind: header.kind,
            attr: header.attr,
            meta: header.meta,
            flags: header.flags,
            emote,
        });
    }
    Ok(result)
}

fn parse_emote_catalog(data: Value) -> Result<Vec<CommentEmote>> {
    Ok(parse_emote_packages(data)?
        .into_iter()
        .filter(|p| p.flags.no_access != Some(true))
        .flat_map(|p| p.emote)
        .filter(|e| e.flags.no_access != Some(true))
        .collect())
}
fn validate_id(value: u64) -> Result<()> {
    if value == 0 || value > i64::MAX as u64 {
        Err(Error::InvalidId)
    } else {
        Ok(())
    }
}
fn validate_message(message: &str) -> Result<()> {
    if message.is_empty()
        || message.chars().count() > 1000
        || message.len() > 4000
        || message.contains('\0')
    {
        Err(Error::InvalidComment)
    } else {
        Ok(())
    }
}
pub(crate) fn csrf_from_cookie(cookie: &str) -> Result<String> {
    let mut result = None;
    for item in cookie.split(';') {
        let Some((name, value)) = item.trim().split_once('=') else {
            continue;
        };
        if name == "bili_jct" {
            if result.is_some()
                || value.len() != 32
                || !value.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(Error::LoginState);
            }
            result = Some(value.to_owned());
        }
    }
    result.ok_or(Error::LoginRequired)
}
fn encode(value: &str) -> String {
    let mut result = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            result.push(b as char);
        } else {
            result.push_str(&format!("%{b:02X}"));
        }
    }
    result
}
fn comment_form(prepared: &PreparedComment) -> Result<Vec<u8>> {
    validate_id(prepared.target.aid)?;
    validate_message(&prepared.message)?;
    Ok(format!(
        "oid={}&type=1&message={}&plat=1&root={}&parent={}&csrf={}",
        prepared.target.aid,
        encode(&prepared.message),
        prepared.root,
        prepared.parent,
        encode(&prepared.csrf)
    )
    .into_bytes())
    .and_then(|mut body| {
        if !prepared.pictures.is_empty() {
            let pictures =
                serde_json::to_string(&prepared.pictures).map_err(|_| Error::InvalidComment)?;
            body.extend_from_slice(format!("&pictures={}", encode(&pictures)).as_bytes());
        }
        Ok(body)
    })
}
fn integer(value: &Value, key: &str) -> Result<u64> {
    let number = value
        .get(key)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or(Error::Schema("comment integer"))?;
    validate_id(number)?;
    Ok(number)
}
fn parse_comment(value: &Value) -> Result<VideoComment> {
    if !value.is_object() {
        return Err(Error::Schema("comment object"));
    }
    let rpid = integer(value, "rpid")?;
    let oid = integer(value, "oid")?;
    let author_mid = if value.get("mid").is_some() {
        integer(value, "mid")?
    } else {
        integer(
            value.get("member").ok_or(Error::Schema("comment member"))?,
            "mid",
        )?
    };
    let message = value
        .pointer("/content/message")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 16384)
        .ok_or(Error::Schema("comment message"))?
        .to_owned();
    let state = match value.get("state") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().ok_or(Error::Schema("comment state"))?),
    };
    let coordinate = |name: &str| -> Result<u64> {
        match value.get(name) {
            None | Some(Value::Null) => Ok(0),
            Some(v) => v
                .as_u64()
                .or_else(|| v.as_str()?.parse().ok())
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or(Error::Schema("comment relationship")),
        }
    };
    let mut pictures = Vec::new();
    if let Some(raw) = value.pointer("/content/pictures").filter(|v| !v.is_null()) {
        let items = raw.as_array().ok_or(Error::Schema("comment pictures"))?;
        if items.len() > 9 {
            return Err(Error::Schema("comment pictures limit"));
        }
        for item in items {
            let mut picture: CommentPicture = serde_json::from_value(item.clone())
                .map_err(|_| Error::Schema("comment picture"))?;
            picture.img_src = picture_url(&picture.img_src)?;
            picture.validate()?;
            pictures.push(picture);
        }
    }
    let mut emote = Vec::new();
    if let Some(raw) = value.pointer("/content/emote").filter(|v| !v.is_null()) {
        let items = raw.as_object().ok_or(Error::Schema("comment emote"))?;
        if items.len() > 128 {
            return Err(Error::Schema("comment emote limit"));
        }
        for (text, item) in items {
            emote.push(parse_emote(item, Some(text))?);
        }
    }
    Ok(VideoComment {
        rpid,
        oid,
        author_mid,
        message,
        state,
        root: coordinate("root")?,
        parent: coordinate("parent")?,
        pictures,
        emote,
    })
}
fn parse_receipt(response: Value, prepared: &PreparedComment) -> Result<CommentReceipt> {
    let code = response
        .get("code")
        .and_then(Value::as_i64)
        .ok_or(Error::WriteOutcomeUnknown("missing_result_code"))?;
    if code != 0 {
        return Err(match code {
            -101 => Error::LoginRequired,
            -352 | -412 | -401 | -509 | 12015 => Error::RiskControl(code),
            _ => Error::Platform(code),
        });
    }
    let data = response
        .get("data")
        .filter(|v| v.is_object())
        .ok_or(Error::WriteOutcomeUnknown("missing_receipt"))?;
    let rpid =
        integer(data, "rpid").map_err(|_| Error::WriteOutcomeUnknown("missing_comment_id"))?;
    // ID survives a missing/partial echo so the caller can reconcile by GET.
    let echoed = data
        .get("reply")
        .filter(|v| !v.is_null())
        .and_then(|v| parse_comment(v).ok());
    let echo_matches = echoed.as_ref().map(|v| {
        v.rpid == rpid
            && v.oid == prepared.target.aid
            && v.author_mid == prepared.target.actor_mid()
            && v.message == prepared.message
            && v.root == prepared.root
            && v.parent == prepared.parent
            && v.pictures == prepared.pictures
    });
    let need_captcha = data.get("need_captcha").and_then(Value::as_bool);
    Ok(CommentReceipt {
        rpid,
        oid: prepared.target.aid,
        bvid: prepared.target.bvid.clone(),
        echoed,
        echo_matches,
        need_captcha,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn prepared() -> PreparedComment {
        PreparedComment {
            target: CommentTarget {
                aid: 42,
                bvid: "BV17x411w7KC".into(),
                title: "owned".into(),
                owner_mid: 7,
                actor_mid: 7,
            },
            message: "晚安喵（test）".into(),
            csrf: "0123456789abcdef0123456789abcdef".into(),
            cookie_binding: "private-cookie".into(),
            root: 0,
            parent: 0,
            pictures: vec![],
        }
    }
    #[test]
    fn precise_unicode_and_root_target_survive_form_encoding_without_secret_debug() {
        let p = prepared();
        let body = String::from_utf8(comment_form(&p).unwrap()).unwrap();
        assert!(body.contains("message=%E6%99%9A%E5%AE%89%E5%96%B5%EF%BC%88test%EF%BC%89"));
        assert!(body.starts_with("oid=42&type=1&"));
        assert!(body.contains("&root=0&parent=0&"));
        assert!(!format!("{p:?}").contains("0123456789abcdef"));
        assert!(!format!("{p:?}").contains("private-cookie"));
        assert!(validate_message("").is_err());
        assert!(validate_message(&"x".repeat(1001)).is_err());
        assert!(csrf_from_cookie("SESSDATA=opaque").is_err());
        assert!(csrf_from_cookie("bili_jct=x; bili_jct=x").is_err());
    }
    #[test]
    fn acknowledgement_keeps_id_even_without_echo_and_never_fakes_visibility() {
        let p = prepared();
        let receipt = parse_receipt(
            json!({"code":0,"data":{"rpid":123,"reply":null,"need_captcha":false}}),
            &p,
        )
        .unwrap();
        assert_eq!(receipt.rpid, 123);
        assert!(receipt.echoed.is_none());
        assert!(receipt.echo_matches.is_none());
        assert!(matches!(
            parse_receipt(json!({"code":0,"data":{}}), &p),
            Err(Error::WriteOutcomeUnknown(_))
        ));
        assert_eq!(
            parse_receipt(json!({"code":12015,"message":"private"}), &p).unwrap_err(),
            Error::RiskControl(12015)
        );
        let echoed = json!({"rpid":123,"oid":42,"mid":7,"state":17,"content":{"message":"晚安喵（test）"},"SESSDATA":"not-public"});
        let receipt =
            parse_receipt(json!({"code":0,"data":{"rpid":123,"reply":echoed}}), &p).unwrap();
        assert_eq!(receipt.echo_matches, Some(true));
        assert_eq!(receipt.echoed.unwrap().state, Some(17));
        assert!(
            !serde_json::to_string(&parse_comment(&echoed).unwrap())
                .unwrap()
                .contains("not-public")
        );
    }
    #[test]
    fn prepared_comment_cannot_be_sent_with_another_clients_credentials() {
        let client = Client::new(crate::Config::default()).unwrap();
        assert_eq!(
            client.submit_prepared_comment(prepared()).unwrap_err(),
            Error::CommentPreparationChanged
        );
        assert!(client.comment_by_id(0, 7).is_err());
        let root =
            json!({"rpid":999,"oid":42,"member":{"mid":"7"},"content":{"message":"readback"}});
        let parsed = parse_comment(&root).unwrap();
        assert_eq!(parsed.author_mid, 7);
        assert_eq!(parsed.rpid, 999);
    }

    #[test]
    fn reply_and_picture_form_preserves_coordinates_and_scoped_image_fields() {
        let mut p = prepared();
        p.root = 100;
        p.parent = 101;
        p.message = "回复测试[doge]".into();
        p.pictures = vec![CommentPicture {
            img_src: "https://i0.hdslb.com/bfs/new_dyn/test.png".into(),
            img_width: 160,
            img_height: 160,
            img_size: 0.5,
        }];
        let body = String::from_utf8(comment_form(&p).unwrap()).unwrap();
        assert!(body.contains("&root=100&parent=101&"));
        assert!(body.contains("&pictures=%5B%7B%22img_src%22"));
        assert!(body.contains("img_width%22%3A160"));
        assert!(body.contains("%5Bdoge%5D"));
        assert!(picture_url("https://evil.example/bfs/x.png").is_err());
        assert!(picture_url("https://i0.hdslb.com/bfs/x.png?credential=private").is_err());
        assert!(picture_url("https://i0.hdslb.com/bfs/../private").is_err());
    }

    #[test]
    fn comment_rich_readback_whitelists_media_and_emotes_and_defaults_old_records() {
        let raw = json!({"rpid":102,"oid":42,"mid":7,"root":100,"parent":101,"state":0,
            "content":{"message":"test[doge]","pictures":[{"img_src":"http://i0.hdslb.com/bfs/new_dyn/test.png",
                "img_width":160,"img_height":160,"img_size":0.5,"private":"not-public"}],
                "emote":{"[doge]":{"id":26,"text":"[doge]","url":"//i0.hdslb.com/bfs/emote/doge.png","private":"not-public"}}}});
        let comment = parse_comment(&raw).unwrap();
        assert_eq!((comment.root, comment.parent), (100, 101));
        assert_eq!(comment.pictures[0].img_width, 160);
        assert!(comment.pictures[0].img_src.starts_with("https://"));
        assert_eq!(comment.emote[0].text, "[doge]");
        assert!(
            !serde_json::to_string(&comment)
                .unwrap()
                .contains("not-public")
        );
        let old: VideoComment = serde_json::from_value(
            json!({"rpid":1,"oid":42,"author_mid":7,"message":"old","state":0}),
        )
        .unwrap();
        assert!(
            old.emote.is_empty() && old.pictures.is_empty() && old.root == 0 && old.parent == 0
        );
    }

    #[test]
    fn reply_pages_cannot_cross_root_or_resource_and_catalog_uses_real_tokens() {
        let root =
            json!({"rpid":100,"oid":42,"mid":7,"root":0,"parent":0,"content":{"message":"root"}});
        let reply = json!({"rpid":101,"oid":42,"mid":7,"root":100,"parent":100,"content":{"message":"reply"}});
        let data = json!({"root":root,"replies":[reply],"page":{"count":1}});
        let parsed = parse_reply_page(data.clone(), 42, 100, 1, 20).unwrap();
        assert_eq!(parsed.replies[0].parent, 100);
        assert_eq!(parsed.total, Some(1));
        assert!(parse_reply_page(data.clone(), 43, 100, 1, 20).is_err());
        let mut wrong = data;
        wrong["replies"][0]["root"] = json!(99);
        assert!(parse_reply_page(wrong, 42, 100, 1, 20).is_err());
        let catalog = parse_emote_catalog(json!({"packages":[{"id":1,"text":"test pack","emote":[{"id":26,"text":"[doge]",
            "url":"http://i0.hdslb.com/bfs/emote/doge.png","unknown":"not-public"},
            {"id":27,"text":"[locked]","url":"http://i0.hdslb.com/bfs/emote/locked.png","flags":{"no_access":true}},
            {"id":28,"text":"(=^･ω･^=)","url":"(=^･ω･^=)","type":4}]}]}))
        .unwrap();
        assert_eq!(catalog[0].text, "[doge]");
        assert_eq!(catalog.len(), 2);
        assert!(catalog[1].url.is_none());
        assert!(
            !serde_json::to_string(&catalog)
                .unwrap()
                .contains("not-public")
        );
        assert!(parse_emote_catalog(json!({"packages":{}})).is_err());
    }

    #[test]
    fn package_projection_preserves_observed_access_flags_without_purchase_inference() {
        let data = json!({"setting":{"private":"not-public"},"packages":[{
            "id":900,"text":"complex pack","type":3,"attr":28,
            "url":"http://i0.hdslb.com/bfs/emote/icon.png",
            "flags":{"added":true,"preview":false,"private":"not-public"},
            "meta":{"size":2,"item_id":123,"item_url":"https://example.invalid/?uid=private","asset_id":456},
            "emote":[{"id":901,"package_id":900,"text":"[complex_rock]","type":3,"attr":0,
                "url":"http://i0.hdslb.com/bfs/emote/rock.png","flags":{"unlocked":false},
                "meta":{"size":2,"alias":"rock","suggest":["rock",""],"unknown":"not-public"}},
                {"id":902,"package_id":900,"text":"[complex_locked]","type":3,
                "url":"http://i0.hdslb.com/bfs/emote/locked.png","flags":{"no_access":true,"unlocked":true}}]},
            {"id":910,"text":"unadded","flags":{"added":false},"emote":[]}]});
        let packages = parse_emote_packages(data.clone()).unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].flags.added, Some(true));
        assert_eq!(packages[0].kind, Some(3));
        assert_eq!(packages[0].meta.size, Some(2));
        assert_eq!(packages[0].emote[0].flags.unlocked, Some(false));
        assert_eq!(packages[0].emote[0].flags.no_access, None);
        assert_eq!(packages[1].flags.added, Some(false));
        let flat = parse_emote_catalog(data).unwrap();
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].text, "[complex_rock]");
        let encoded = serde_json::to_string(&packages).unwrap();
        assert!(
            !encoded.contains("private")
                && !encoded.contains("item_url")
                && !encoded.contains("asset_id")
        );
    }

    #[test]
    fn package_metadata_requires_correct_types_and_old_emote_receipts_remain_loadable() {
        let base = json!({"packages":[{"id":1,"text":"small faces","flags":{},"emote":[]}]});
        let package = parse_emote_packages(base.clone()).unwrap().remove(0);
        assert_eq!(package.flags.added, None);
        assert_eq!(package.flags.unlocked, None);
        assert_eq!(package.kind, None);
        let old: CommentEmote =
            serde_json::from_value(json!({"id":26,"text":"[doge]","url":null})).unwrap();
        assert!(old.meta.size.is_none() && old.flags.added.is_none() && old.package_id.is_none());
        let mut invalid = base.clone();
        invalid["packages"][0]["flags"]["added"] = json!("true");
        assert!(parse_emote_packages(invalid).is_err());
        let mut invalid = base.clone();
        invalid["packages"][0]["meta"] = json!({"suggest":false});
        assert!(parse_emote_packages(invalid).is_err());
        let mut invalid = base;
        invalid["packages"][0]["emote"] = json!([{"id":2,"package_id":999,"text":"wrong"}]);
        assert!(parse_emote_packages(invalid).is_err());
    }
}
