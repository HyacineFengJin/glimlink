// SPDX-License-Identifier: MIT
//! A bounded, read-only uploader dynamic feed. No media downloads, polling or
//! unread acknowledgements. Source: pskdje/bilibili-API-collect dynamic/space.md.
use crate::{Client, Error, Result, VideoId, api_data};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicAuthor {
    pub mid: Option<u64>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicStats {
    pub like_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub forward_count: Option<u64>,
    /// Existing viewer state from the response, never an instruction to like.
    pub liked: Option<bool>,
    /// Original compact display, including a label when the count is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like_count_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_count_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_count_display: Option<String>,
    /// True only for a normalized compact unit display. A false flag does not
    /// turn a None/unknown count into an exact count; ordinary numeric zero stays exact.
    #[serde(default, skip_serializing_if = "not_approximate")]
    pub like_count_approximate: bool,
    #[serde(default, skip_serializing_if = "not_approximate")]
    pub comment_count_approximate: bool,
    #[serde(default, skip_serializing_if = "not_approximate")]
    pub forward_count_approximate: bool,
}
fn not_approximate(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicRichNode {
    pub kind: String,
    pub text: Option<String>,
    pub orig_text: Option<String>,
    pub rid: Option<String>,
    pub mentioned_mid: Option<u64>,
    pub unexpanded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicMediaKind {
    Picture,
    Cover,
    Emote,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicMedia {
    pub kind: DynamicMediaKind,
    pub role: String,
    /// Private SDK locator; the runtime should substitute an account-scoped asset.
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Filename or service-field hint only, not a decoded animation claim.
    pub format_hint: Option<String>,
    pub animated: Option<bool>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicMajor {
    pub kind: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub aid: Option<u64>,
    pub bvid: Option<String>,
    pub article_id: Option<u64>,
    pub opus_id: Option<String>,
    /// Unsupported or incompletely projected branch, not a media-viewed flag.
    pub unexpanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicOriginalReference {
    pub id: String,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DynamicItem {
    pub id: String,
    pub kind: String,
    pub author: DynamicAuthor,
    pub published_at: Option<u64>,
    pub text: Option<String>,
    pub text_source: Option<String>,
    pub text_truncated: Option<bool>,
    pub rich_nodes: Vec<DynamicRichNode>,
    pub stat: DynamicStats,
    pub major: Option<DynamicMajor>,
    pub additional_type: Option<String>,
    pub additional_unexpanded: bool,
    pub media: Vec<DynamicMedia>,
    /// At most one expanded original. Its own author is never replaced by host_mid.
    pub orig: Option<Box<DynamicItem>>,
    pub orig_reference: Option<DynamicOriginalReference>,
    pub orig_unexpanded: bool,
    /// Only explicit basic.comment_id_str/comment_type fields, no type guessing.
    pub comment_oid: Option<u64>,
    pub comment_type: Option<u32>,
    pub pinned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DynamicFeedPage {
    pub owner_mid: u64,
    pub items: Vec<DynamicItem>,
    pub has_more: Option<bool>,
    pub next_offset: Option<String>,
    pub server_offset: Option<String>,
    pub truncated: bool,
    pub pagination_note: Option<String>,
    pub media_downloaded: bool,
}

impl Client {
    /// One current Web space-feed GET. Existing Cookie is reused by Transport;
    /// no login, device synthesis, deprecated endpoint fallback or pagination.
    pub fn uploader_dynamics(
        &self,
        mid: u64,
        offset: Option<&str>,
        limit: u32,
    ) -> Result<DynamicFeedPage> {
        let url = feed_url(mid, offset, limit)?;
        parse_feed(
            api_data(self.transport.json(&url)?, false)?,
            mid,
            offset,
            limit,
        )
    }

    /// The signed-in viewer's Following dynamics, separate from homepage
    /// recommendations and from a selected uploader's public space feed.
    pub fn following_dynamics(&self, offset: Option<&str>, limit: u32) -> Result<Value> {
        let url = following_url(offset, limit)?;
        let viewer = self.current_user()?;
        following_page(
            api_data(self.transport.json(&url)?, false)?,
            viewer.mid,
            offset,
        )
    }
}

fn following_page(mut raw: Value, viewer: u64, offset: Option<&str>) -> Result<Value> {
    valid_mid(viewer)?;
    // This endpoint chooses its own page size. Preserve the complete bounded
    // provider page; Runtime slices and caches its tail before using the offset.
    // Checking the original list prevents promoted filtering from hiding an
    // oversized provider response.
    list(raw.get("items"), 100)?;
    let mut omitted = 0usize;
    if let Some(items) = raw["items"].as_array_mut() {
        items.retain(|item| {
            let promoted = item["type"] == "DYNAMIC_TYPE_AD"
                || item
                    .pointer("/modules/module_tag/text")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.contains("推荐") || s.contains("广告"));
            if promoted {
                omitted += 1
            }
            !promoted
        });
    }
    let mut page = serde_json::to_value(parse_feed(raw, viewer, offset, 100)?)
        .map_err(|_| Error::InvalidJson)?;
    page.as_object_mut()
        .ok_or(Error::Schema("dynamic feed projection"))?
        .remove("owner_mid");
    page["viewer_mid"] = serde_json::json!(viewer);
    page["source"] = serde_json::json!("bilibili_following_dynamics");
    page["recommendation_kind"] = serde_json::json!("following_activity");
    page["signed_in_request"] = serde_json::json!(true);
    page["viewer_verified"] = serde_json::json!(true);
    page["omitted_promoted_items"] = serde_json::json!(omitted);
    Ok(page)
}

fn following_url(offset: Option<&str>, limit: u32) -> Result<String> {
    if !(1..=20).contains(&limit) { return Err(Error::InvalidPage); }
    let offset = offset.unwrap_or("");
    valid_offset(offset)?;
    Ok(format!("https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/all?type=all&offset={}&platform=web&features=itemOpusStyle&timezone_offset=-480", encode(offset)))
}

fn valid_mid(mid: u64) -> Result<()> {
    if mid == 0 || mid > i64::MAX as u64 {
        Err(Error::InvalidId)
    } else {
        Ok(())
    }
}
fn valid_offset(offset: &str) -> Result<()> {
    if offset.len() > 4096 || offset.chars().any(char::is_control) {
        Err(Error::InvalidPage)
    } else {
        Ok(())
    }
}
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn feed_url(mid: u64, offset: Option<&str>, limit: u32) -> Result<String> {
    valid_mid(mid)?;
    if !(1..=20).contains(&limit) {
        return Err(Error::InvalidPage);
    }
    let offset = offset.unwrap_or("");
    valid_offset(offset)?;
    Ok(format!(
        "https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/space?host_mid={mid}&offset={}&platform=web&features=itemOpusStyle&timezone_offset=-480&web_location=333.1387",
        encode(offset)
    ))
}
fn string(raw: &Value, key: &str, max: usize) -> Result<Option<String>> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= max && !value.contains('\0') => {
            Ok(Some(value.clone()))
        }
        _ => Err(Error::Schema("dynamic text field")),
    }
}
fn required_text(raw: &Value, key: &str, max: usize) -> Result<String> {
    string(raw, key, max)?
        .filter(|s| !s.is_empty())
        .ok_or(Error::Schema("dynamic required text"))
}
fn number(raw: &Value, key: &str) -> Result<Option<u64>> {
    match raw.get(key) {
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
            .ok_or(Error::Schema("dynamic numeric field")),
    }
}
fn number32(raw: &Value, key: &str) -> Result<Option<u32>> {
    number(raw, key)?
        .map(|n| u32::try_from(n).map_err(|_| Error::Schema("dynamic numeric field")))
        .transpose()
}
fn dimension(raw: &Value, key: &str) -> Result<Option<u32>> {
    if let Some(Value::Number(n)) = raw.get(key) {
        if let Some(v) = n
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0 && *n <= u32::MAX as f64 && n.fract() == 0.0)
        {
            return Ok(Some(v as u32));
        }
    }
    number32(raw, key)
}
fn flag(raw: &Value, key: &str) -> Result<Option<bool>> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(*v)),
        Some(v) if v.as_u64() == Some(0) => Ok(Some(false)),
        Some(v) if v.as_u64() == Some(1) => Ok(Some(true)),
        _ => Err(Error::Schema("dynamic flag")),
    }
}
fn dynamic_id(raw: &Value) -> Result<String> {
    let id = required_text(raw, "id_str", 20)?;
    if !id.bytes().all(|b| b.is_ascii_digit()) || id.parse::<u64>().ok().is_none_or(|n| n == 0) {
        return Err(Error::Schema("dynamic id"));
    }
    Ok(id)
}
fn list<'a>(value: Option<&'a Value>, max: usize) -> Result<&'a [Value]> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) if items.len() <= max => Ok(items),
        _ => Err(Error::Schema("dynamic list")),
    }
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
    ["gif", "webp", "png", "jpg", "jpeg", "avif"]
        .into_iter()
        .find(|s| path.ends_with(&format!(".{s}")))
        .map(str::to_owned)
}
fn push_media(
    output: &mut Vec<DynamicMedia>,
    kind: DynamicMediaKind,
    role: &str,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
    text: Option<String>,
    hint: Option<&str>,
) -> Result<()> {
    if url.is_empty() {
        return Ok(());
    }
    let url = media_url(url)?;
    if output.iter().any(|m| m.url == url && m.text == text) {
        return Ok(());
    }
    if output.len() >= 256 {
        return Err(Error::JsonLimit);
    }
    output.push(DynamicMedia {
        kind,
        role: role.into(),
        format_hint: hint.map(str::to_owned).or_else(|| format_hint(&url)),
        url,
        width,
        height,
        animated: None,
        text,
    });
    Ok(())
}
fn parse_nodes(desc: &Value, media: &mut Vec<DynamicMedia>) -> Result<Vec<DynamicRichNode>> {
    let mut nodes = Vec::new();
    for node in list(desc.get("rich_text_nodes"), 256)? {
        let kind = required_text(node, "type", 128)?;
        let text = string(node, "text", 65536)?;
        let orig_text = string(node, "orig_text", 65536)?;
        let rid = string(node, "rid", 128)?;
        let mentioned_mid = if kind == "RICH_TEXT_NODE_TYPE_AT" {
            rid.as_ref()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0)
        } else {
            None
        };
        let mut unexpanded = !matches!(
            kind.as_str(),
            "RICH_TEXT_NODE_TYPE_TEXT"
                | "RICH_TEXT_NODE_TYPE_AT"
                | "RICH_TEXT_NODE_TYPE_TOPIC"
                | "RICH_TEXT_NODE_TYPE_EMOJI"
        );
        if kind == "RICH_TEXT_NODE_TYPE_EMOJI" {
            if let Some(emoji) = node.get("emoji").filter(|v| v.is_object()) {
                let token = text.clone().or_else(|| orig_text.clone());
                for (field, hint) in [
                    ("icon_url", None),
                    ("url", None),
                    ("gif_url", Some("gif")),
                    ("webp_url", Some("webp")),
                ] {
                    if let Some(url) = string(emoji, field, 16384)?.filter(|s| !s.is_empty()) {
                        push_media(
                            media,
                            DynamicMediaKind::Emote,
                            "rich_node_emote",
                            &url,
                            None,
                            None,
                            token.clone(),
                            hint,
                        )?;
                    }
                }
            } else {
                unexpanded = true;
            }
        }
        if node
            .get("pics")
            .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
            || node.get("video").is_some_and(|v| !v.is_null())
        {
            unexpanded = true;
        }
        nodes.push(DynamicRichNode {
            kind,
            text,
            orig_text,
            rid,
            mentioned_mid,
            unexpanded,
        });
    }
    Ok(nodes)
}

fn parse_major(raw: Option<&Value>, media: &mut Vec<DynamicMedia>) -> Result<Option<DynamicMajor>> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let kind = required_text(raw, "type", 128)?;
    let mut major = DynamicMajor {
        kind: kind.clone(),
        title: None,
        description: None,
        aid: None,
        bvid: None,
        article_id: None,
        opus_id: None,
        unexpanded: false,
    };
    match kind.as_str() {
        "MAJOR_TYPE_ARCHIVE" => {
            let body = raw
                .get("archive")
                .filter(|v| v.is_object())
                .ok_or(Error::Schema("dynamic archive"))?;
            major.title = string(body, "title", 16384)?;
            major.description = string(body, "desc", 65536)?;
            major.aid = number(body, "aid")?.filter(|n| *n > 0);
            major.bvid = string(body, "bvid", 64)?.filter(|s| !s.is_empty());
            if let Some(bvid) = major.bvid.as_ref() {
                if !matches!(bvid.parse::<VideoId>()?, VideoId::Bv(_)) {
                    return Err(Error::InvalidId);
                }
            }
            if let Some(url) = string(body, "cover", 16384)? {
                push_media(
                    media,
                    DynamicMediaKind::Cover,
                    "archive_cover",
                    &url,
                    None,
                    None,
                    None,
                    None,
                )?;
            }
        }
        "MAJOR_TYPE_DRAW" => {
            let body = raw
                .get("draw")
                .filter(|v| v.is_object())
                .ok_or(Error::Schema("dynamic draw"))?;
            for image in list(body.get("items"), 64)? {
                let url = required_text(image, "src", 16384)?;
                push_media(
                    media,
                    DynamicMediaKind::Picture,
                    "draw_picture",
                    &url,
                    dimension(image, "width")?,
                    dimension(image, "height")?,
                    None,
                    None,
                )?;
                if string(image, "live_src", 16384)?.is_some_and(|s| !s.is_empty()) {
                    major.unexpanded = true;
                }
            }
        }
        "MAJOR_TYPE_OPUS" => {
            let body = raw
                .get("opus")
                .filter(|v| v.is_object())
                .ok_or(Error::Schema("dynamic opus"))?;
            major.title = string(body, "title", 16384)?;
            major.opus_id = string(body, "id_str", 20)?;
            for image in list(body.get("pics"), 64)? {
                let url = required_text(image, "url", 16384)?;
                push_media(
                    media,
                    DynamicMediaKind::Picture,
                    "opus_picture",
                    &url,
                    dimension(image, "width")?,
                    dimension(image, "height")?,
                    None,
                    None,
                )?;
            }
        }
        "MAJOR_TYPE_ARTICLE" => {
            let body = raw
                .get("article")
                .filter(|v| v.is_object())
                .ok_or(Error::Schema("dynamic article"))?;
            major.title = string(body, "title", 16384)?;
            major.description = string(body, "desc", 65536)?;
            major.article_id = number(body, "id")?.filter(|n| *n > 0);
            for cover in list(body.get("covers"), 16)? {
                let url = cover
                    .as_str()
                    .ok_or(Error::Schema("dynamic article cover"))?;
                push_media(
                    media,
                    DynamicMediaKind::Cover,
                    "article_cover",
                    url,
                    None,
                    None,
                    None,
                    None,
                )?;
            }
        }
        _ => major.unexpanded = true,
    }
    Ok(Some(major))
}
fn count(stat: &Value, name: &str) -> Result<Option<u64>> {
    let Some(block) = stat.get(name).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    if flag(block, "hidden")? == Some(true) {
        return Ok(None);
    }
    if !block.is_object() {
        return Err(Error::Schema("dynamic statistic block"));
    }
    match block.get("count") {
        Some(v) if v.as_i64().is_some_and(|n| n < 0) => Ok(None),
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => display_count(value, name),
        Some(value) if value.as_u64().is_some() => Ok(value.as_u64()),
        Some(Value::Number(value)) => value
            .as_f64()
            .filter(|n| {
                n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0
            })
            .map(|n| Some(n as u64))
            .ok_or(Error::Schema("dynamic statistic count")),
        _ => Err(Error::Schema("dynamic statistic count")),
    }
}
fn count_display_metadata(
    stat: &Value,
    name: &str,
    value: Option<u64>,
) -> Result<(Option<String>, bool)> {
    let block = stat.get(name).unwrap_or(&Value::Null);
    if flag(block, "hidden")? == Some(true) {
        return Ok((None, false));
    }
    let display = block["count"].as_str().map(str::to_owned);
    let approximate = value.is_some()
        && display.as_deref().is_some_and(|text| {
            matches!(
                text.trim().chars().last(),
                Some('万' | '亿' | '千' | 'w' | 'W' | 'k' | 'K' | 'm' | 'M')
            )
        });
    Ok((display, approximate))
}
fn display_count(value: &str, name: &str) -> Result<Option<u64>> {
    if value.len() > 64 || value.chars().any(char::is_control) {
        return Err(Error::Schema("dynamic statistic count"));
    }
    let value = value.trim();
    let action_label = match name {
        "like" => matches!(value, "点赞" | "赞"),
        "comment" => value == "评论",
        "forward" => value == "转发",
        _ => false,
    };
    // An action label or redacted display is not proof of an exact zero.
    if action_label
        || matches!(value, "" | "--" | "—" | "-")
        || value
            .strip_prefix('-')
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(None);
    }
    let (mantissa, multiplier) = match value.chars().last() {
        Some(unit @ ('万' | 'w' | 'W')) => (&value[..value.len() - unit.len_utf8()], 10_000u128),
        Some('亿') => (&value[..value.len() - '亿'.len_utf8()], 100_000_000),
        Some(unit @ ('千' | 'k' | 'K')) => (&value[..value.len() - unit.len_utf8()], 1_000),
        Some('m' | 'M') => (&value[..value.len() - 1], 1_000_000),
        _ => (value, 1),
    };
    let mantissa = mantissa.trim();
    let (whole, fraction) = mantissa
        .split_once('.')
        .map(|(a, b)| (a, Some(b)))
        .unwrap_or((mantissa, None));
    let parts: Vec<_> = whole.split(',').collect();
    if parts.len() > 1
        && (parts[0].is_empty() || parts[0].len() > 3 || parts.iter().skip(1).any(|s| s.len() != 3))
    {
        return Err(Error::Schema("dynamic statistic count"));
    }
    let whole = parts.concat();
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Schema("dynamic statistic count"));
    }
    let fraction = fraction.unwrap_or("");
    if mantissa.contains('.')
        && (fraction.is_empty()
            || fraction.len() > 8
            || !fraction.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Error::Schema("dynamic statistic count"));
    }
    let digits = format!("{whole}{fraction}");
    let numerator = digits
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or(Error::Schema("dynamic statistic count"))?;
    let denominator = 10u128.pow(fraction.len() as u32);
    if numerator % denominator != 0 {
        return Err(Error::Schema("dynamic statistic count"));
    }
    Ok(Some(
        u64::try_from(numerator / denominator)
            .map_err(|_| Error::Schema("dynamic statistic count"))?,
    ))
}
fn original_reference(raw: &Value) -> Result<Option<DynamicOriginalReference>> {
    match dynamic_id(raw) {
        Ok(id) => Ok(Some(DynamicOriginalReference {
            id,
            kind: string(raw, "type", 128)?,
        })),
        Err(_)
            if raw.get("id_str").and_then(Value::as_str) == Some("0")
                || raw.get("id_str").is_none() =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
pub(crate) fn parse_item(raw: &Value, depth: u8) -> Result<DynamicItem> {
    let id = dynamic_id(raw)?;
    let kind = required_text(raw, "type", 128)?;
    let modules = raw
        .get("modules")
        .filter(|v| v.is_object())
        .ok_or(Error::Schema("dynamic modules"))?;
    let author = modules.get("module_author").unwrap_or(&Value::Null);
    let dynamic = modules.get("module_dynamic").unwrap_or(&Value::Null);
    let stat = modules.get("module_stat").unwrap_or(&Value::Null);
    let mut media = Vec::new();
    let major = parse_major(dynamic.get("major"), &mut media)?;
    let desc = dynamic
        .get("desc")
        .filter(|v| !v.is_null())
        .map(|v| (v, "module_dynamic.desc"))
        .or_else(|| {
            dynamic
                .pointer("/major/opus/summary")
                .filter(|v| !v.is_null())
                .map(|v| (v, "major.opus.summary"))
        });
    let (text, text_source, text_truncated, rich_nodes) = if let Some((desc, source)) = desc {
        if !desc.is_object() {
            return Err(Error::Schema("dynamic description"));
        }
        (
            string(desc, "text", 131072)?,
            Some(source.into()),
            flag(desc, "has_more")?,
            parse_nodes(desc, &mut media)?,
        )
    } else {
        (None, None, None, Vec::new())
    };
    let additional = dynamic.get("additional").filter(|v| !v.is_null());
    let additional_type = additional
        .map(|raw| string(raw, "type", 128))
        .transpose()?
        .flatten();
    let original = raw.get("orig").filter(|v| !v.is_null());
    let mut orig = None;
    let mut orig_reference = None;
    let mut orig_unexpanded = false;
    if let Some(original) = original {
        orig_reference = original_reference(original)?;
        if depth == 0
            && orig_reference.is_some()
            && original.get("modules").is_some_and(Value::is_object)
        {
            orig = Some(Box::new(parse_item(original, 1)?));
        } else {
            orig_unexpanded = true;
        }
    } else if kind == "DYNAMIC_TYPE_FORWARD" {
        orig_unexpanded = true;
    }
    let basic = raw.get("basic").unwrap_or(&Value::Null);
    // An unavailable original can explicitly leave this optional interaction
    // target empty. Do not invent ID zero or relax uploader/author identities.
    let comment_oid = if basic.get("comment_id_str").and_then(Value::as_str) == Some("") {
        None
    } else {
        number(basic, "comment_id_str")?.filter(|n| *n > 0)
    };
    let comment_type = number32(basic, "comment_type")?;
    let pinned = flag(author, "is_top")?.unwrap_or(false)
        || modules.pointer("/module_tag/text").and_then(Value::as_str) == Some("置顶");
    let like_count = count(stat, "like")?;
    let comment_count = count(stat, "comment")?;
    let forward_count = count(stat, "forward")?;
    let (like_count_display, like_count_approximate) =
        count_display_metadata(stat, "like", like_count)?;
    let (comment_count_display, comment_count_approximate) =
        count_display_metadata(stat, "comment", comment_count)?;
    let (forward_count_display, forward_count_approximate) =
        count_display_metadata(stat, "forward", forward_count)?;
    Ok(DynamicItem {
        id,
        kind,
        author: DynamicAuthor {
            mid: number(author, "mid")?.filter(|n| *n > 0),
            name: string(author, "name", 512)?,
        },
        published_at: number(author, "pub_ts")?,
        text,
        text_source,
        text_truncated,
        rich_nodes,
        stat: DynamicStats {
            like_count,
            comment_count,
            forward_count,
            liked: flag(stat.get("like").unwrap_or(&Value::Null), "status")?,
            like_count_display,
            comment_count_display,
            forward_count_display,
            like_count_approximate,
            comment_count_approximate,
            forward_count_approximate,
        },
        major,
        additional_type,
        additional_unexpanded: additional.is_some(),
        media,
        orig,
        orig_reference,
        orig_unexpanded,
        comment_oid,
        comment_type,
        pinned,
    })
}
fn parse_feed(
    data: Value,
    owner_mid: u64,
    current: Option<&str>,
    limit: u32,
) -> Result<DynamicFeedPage> {
    if !data.is_object() {
        return Err(Error::Schema("dynamic feed data"));
    }
    if data.get("v_voucher").is_some() || flag(&data, "need_captcha")? == Some(true) {
        return Err(Error::RiskControl(0));
    }
    let has_more = flag(&data, "has_more")?;
    if !data.as_object().unwrap().contains_key("items") {
        return Err(Error::Schema("dynamic feed items missing"));
    }
    let raw = list(data.get("items"), 100)?;
    if data.get("items") == Some(&Value::Null) && has_more != Some(false) {
        return Err(Error::Schema("null dynamic feed without end proof"));
    }
    let server_offset = string(&data, "offset", 4096)?.filter(|s| !s.is_empty());
    if let Some(offset) = server_offset.as_ref() {
        valid_offset(offset).map_err(|_| Error::Schema("dynamic server offset"))?;
    }
    let truncated = raw.len() > limit as usize;
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in raw.iter().take(limit as usize) {
        let item = parse_item(raw, 0)?;
        if seen.insert(item.id.clone()) {
            items.push(item);
        }
    }
    let next_offset = if truncated || has_more == Some(false) || items.is_empty() {
        None
    } else {
        server_offset.clone()
    };
    let pagination_note = if truncated {
        Some("The feed exceeded the requested output limit; next_offset is withheld to avoid skipping unreturned items. Re-read the same offset with limit=20.".into())
    } else if items.is_empty() && has_more != Some(false) {
        Some("The feed page is empty but exhaustion is unconfirmed; no continuation is given to avoid an empty-page loop.".into())
    } else if next_offset.as_deref().is_some_and(|n| Some(n) == current) {
        Some("The service reused an opaque offset; deduplicate by dynamic ID and stop on an empty page, has_more=false, or no new IDs. Do not loop indefinitely.".into())
    } else if has_more == Some(true) && next_offset.is_none() {
        Some(
            "The service reports more items without a usable offset; continuation is unavailable."
                .into(),
        )
    } else if has_more.is_none() {
        Some("The service omitted has_more; end-of-feed is unknown.".into())
    } else {
        None
    };
    Ok(DynamicFeedPage {
        owner_mid,
        items,
        has_more,
        next_offset,
        server_offset,
        truncated,
        pagination_note,
        media_downloaded: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn item(id: &str, mid: u64) -> Value {
        json!({"id_str":id,"type":"DYNAMIC_TYPE_WORD","basic":{"comment_id_str":"300","comment_type":17},
        "modules":{"module_author":{"mid":mid,"name":"publisher","pub_ts":"1700000000","is_top":false},
            "module_dynamic":{"desc":{"text":"actual text","rich_text_nodes":[{"type":"RICH_TEXT_NODE_TYPE_TEXT","text":"actual text"}],"has_more":false},"major":null,"additional":null},
            "module_stat":{"like":{"count":0,"status":false},"comment":{"count":2},"forward":{"count":3}}},"orig":null})
    }
    #[test]
    fn requests_are_one_fixed_get_with_bounded_opaque_offset_and_no_device_synthesis() {
        let url = feed_url(42, Some("opaque&next=secret"), 20).unwrap();
        assert!(url.contains("host_mid=42") && url.contains("offset=opaque%26next%3Dsecret"));
        assert!(!url.contains("dm_img") && !url.contains("like") && !url.contains("modify"));
        assert!(feed_url(0, None, 20).is_err());
        assert!(feed_url(42, None, 21).is_err());
        assert!(feed_url(42, Some("bad\n"), 20).is_err());
    }
    #[test]
    fn feed_keeps_string_timestamps_pins_true_stats_and_explicit_comment_identifiers() {
        let mut entry = item("100", 42);
        entry["modules"]["module_tag"] = json!({"text":"置顶"});
        let data = json!({"items":[entry],"has_more":true,"offset":"opaque-next","total":"0","private":"not-public"});
        let parsed = parse_feed(data, 42, None, 20).unwrap();
        assert_eq!(parsed.items[0].published_at, Some(1700000000));
        assert!(parsed.items[0].pinned);
        assert_eq!(parsed.items[0].stat.like_count, Some(0));
        assert_eq!(parsed.items[0].stat.liked, Some(false));
        assert_eq!(parsed.items[0].comment_oid, Some(300));
        assert_eq!(parsed.items[0].comment_type, Some(17));
        assert!(parsed.items[0].orig.is_none() && !parsed.items[0].orig_unexpanded);
        assert_eq!(parsed.next_offset.as_deref(), Some("opaque-next"));
        assert!(!parsed.media_downloaded);
        assert!(
            !serde_json::to_string(&parsed)
                .unwrap()
                .contains("not-public")
        );
    }
    #[test]
    fn statistic_displays_distinguish_unknown_action_labels_from_numeric_zero() {
        for (name, label) in [
            ("like", "点赞"),
            ("like", "赞"),
            ("comment", "评论"),
            ("forward", "转发"),
        ] {
            let stat = json!({(name):{"count":label}});
            assert_eq!(count(&stat, name).unwrap(), None);
            assert_eq!(count(&json!({(name):{"count":0}}), name).unwrap(), Some(0));
            assert_eq!(
                count(&json!({(name):{"count":"0"}}), name).unwrap(),
                Some(0)
            );
        }
        for hidden in [
            Value::Null,
            json!(""),
            json!("--"),
            json!("—"),
            json!(-1),
            json!("-1"),
        ] {
            assert_eq!(
                count(&json!({"like":{"count":hidden}}), "like").unwrap(),
                None
            );
        }
        assert_eq!(
            count(
                &json!({"like":{"count":"not a number","hidden":true}}),
                "like"
            )
            .unwrap(),
            None
        );
        assert!(count(&json!({"like":{"count":"unknown metric"}}), "like").is_err());
        assert!(count(&json!({"like":{"count":"评论"}}), "like").is_err());
    }
    #[test]
    fn statistic_units_use_exact_decimal_arithmetic_and_ids_remain_strict() {
        for (display, expected) in [
            ("1.2万", 12000),
            ("3.45亿", 345000000),
            ("0.5k", 500),
            ("2W", 20000),
            ("1.25M", 1250000),
            ("1,234", 1234),
            ("1,234万", 12340000),
            ("0万", 0),
            ("1.0", 1),
            ("1.2345万", 12345),
            ("18446744073709551615", u64::MAX),
        ] {
            assert_eq!(
                count(&json!({"like":{"count":display}}), "like").unwrap(),
                Some(expected),
                "{display}"
            );
        }
        for display in [
            "1.2",
            "12,34",
            "1.2.3万",
            "1.00001万",
            "18446744073709551616",
            "1e4",
            "1万+",
            "NaN",
        ] {
            assert!(
                count(&json!({"like":{"count":display}}), "like").is_err(),
                "{display}"
            );
        }
        assert_eq!(
            count(&json!({"like":{"count":12.0}}), "like").unwrap(),
            Some(12)
        );
        assert!(count(&json!({"like":{"count":12.5}}), "like").is_err());
        assert!(number(&json!({"mid":"1.2万"}), "mid").is_err());
        assert!(number(&json!({"comment_id_str":"评论"}), "comment_id_str").is_err());
    }
    #[test]
    fn empty_optional_original_comment_id_never_becomes_a_fabricated_interaction_target() {
        let mut forwarded = item("100", 42);
        forwarded["type"] = json!("DYNAMIC_TYPE_FORWARD");
        let mut original = item("101", 99);
        original["basic"]["comment_id_str"] = json!("");
        forwarded["orig"] = original;
        let parsed = parse_item(&forwarded, 0).unwrap();
        assert_eq!(parsed.comment_oid, Some(300));
        assert_eq!(parsed.orig.as_ref().unwrap().comment_oid, None);
        assert_eq!(parsed.orig.as_ref().unwrap().author.mid, Some(99));
        assert_eq!(parsed.author.mid, Some(42));
        for key in ["mid", "pub_ts"] {
            let mut invalid = forwarded.clone();
            invalid["modules"]["module_author"][key] = json!("");
            assert!(parse_item(&invalid, 0).is_err(), "{key} must remain strict");
        }
        forwarded["orig"]["basic"]["comment_id_str"] = json!(" ");
        assert!(parse_item(&forwarded, 0).is_err());
    }
    #[test]
    fn compact_counts_keep_display_precision_and_do_not_relabel_unknown_or_exact_zero() {
        let mut entry = item("100", 42);
        entry["modules"]["module_stat"]["like"]["count"] = json!("1.2万");
        entry["modules"]["module_stat"]["comment"]["count"] = json!("评论");
        entry["modules"]["module_stat"]["forward"]["count"] = json!(0);
        let stat = parse_item(&entry, 0).unwrap().stat;
        assert_eq!(stat.like_count, Some(12000));
        assert_eq!(stat.like_count_display.as_deref(), Some("1.2万"));
        assert!(stat.like_count_approximate);
        assert_eq!(stat.comment_count, None);
        assert_eq!(stat.comment_count_display.as_deref(), Some("评论"));
        assert!(!stat.comment_count_approximate);
        assert_eq!(stat.forward_count, Some(0));
        assert_eq!(stat.forward_count_display, None);
        assert!(!stat.forward_count_approximate);
        let wire = serde_json::to_value(&stat).unwrap();
        assert_eq!(wire["like_count_approximate"], true);
        assert_eq!(wire["like_count_display"], "1.2万");
        assert_eq!(wire["forward_count"], 0);
        assert!(wire.get("forward_count_display").is_none());
        assert!(wire.get("forward_count_approximate").is_none());
    }
    #[test]
    fn following_keeps_the_whole_bounded_provider_page_for_runtime_tail_caching() {
        let mut entries = (100..125)
            .map(|id| item(&id.to_string(), 42))
            .collect::<Vec<_>>();
        entries.push(json!({"id_str":"999","type":"DYNAMIC_TYPE_AD","private":"not projected"}));
        let page = following_page(
            json!({"items":entries,"has_more":true,"offset":"provider-next"}),
            42,
            None,
        )
        .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 25);
        assert_eq!(page["truncated"], false);
        assert_eq!(page["next_offset"], "provider-next");
        assert_eq!(page["viewer_mid"], 42);
        assert!(page.get("owner_mid").is_none());
        assert_eq!(page["omitted_promoted_items"], 1);
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("not projected")
        );
        let oversized = vec![json!({"type":"DYNAMIC_TYPE_AD"}); 101];
        assert!(following_page(json!({"items":oversized,"has_more":true}), 42, None).is_err());
    }
    #[test]
    fn drawing_video_article_and_opus_are_references_without_invented_text_or_animation() {
        let mut entry = item("100", 42);
        entry["type"] = json!("DYNAMIC_TYPE_DRAW");
        entry["modules"]["module_dynamic"]["desc"] = Value::Null;
        entry["modules"]["module_dynamic"]["major"] = json!({"type":"MAJOR_TYPE_DRAW","archive":null,
            "draw":{"id":7,"items":[{"src":"http://i0.hdslb.com/bfs/new_dyn/a.gif","width":160,"height":160.0,"live_src":""}]}});
        let parsed = parse_item(&entry, 0).unwrap();
        assert!(parsed.text.is_none());
        assert_eq!(parsed.media[0].format_hint.as_deref(), Some("gif"));
        assert!(parsed.media[0].animated.is_none());
        assert_eq!(parsed.media[0].height, Some(160));
        entry["modules"]["module_dynamic"]["major"] = json!({"type":"MAJOR_TYPE_ARCHIVE","archive":{"aid":"170001","bvid":"BV17x411w7KC","title":"video","desc":"video summary","cover":"https://i0.hdslb.com/bfs/archive/cover.jpg"}});
        let parsed = parse_item(&entry, 0).unwrap();
        assert_eq!(parsed.major.as_ref().unwrap().aid, Some(170001));
        assert!(parsed.text.is_none());
        entry["modules"]["module_dynamic"]["major"] = json!({"type":"MAJOR_TYPE_ARTICLE","article":{"id":55,"title":"article","covers":["https://i0.hdslb.com/bfs/article/c.png"]}});
        assert_eq!(
            parse_item(&entry, 0).unwrap().major.unwrap().article_id,
            Some(55)
        );
        entry["modules"]["module_dynamic"]["major"] = json!({"type":"MAJOR_TYPE_OPUS","opus":{"title":"opus","summary":{"text":"actual summary","rich_text_nodes":[]},"pics":[{"url":"https://i0.hdslb.com/bfs/new_dyn/opus.webp","width":160,"height":160}]}});
        let parsed = parse_item(&entry, 0).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("actual summary"));
        assert_eq!(parsed.text_source.as_deref(), Some("major.opus.summary"));
    }
    #[test]
    fn rich_mentions_variants_and_forward_authors_stay_separate_with_one_orig_level() {
        let mut forwarded = item("100", 42);
        forwarded["type"] = json!("DYNAMIC_TYPE_FORWARD");
        let mut original = item("101", 99);
        original["orig"] = item("102", 77);
        forwarded["orig"] = original;
        forwarded["modules"]["module_dynamic"]["desc"] = json!({"text":"@other [hello]","rich_text_nodes":[
            {"type":"RICH_TEXT_NODE_TYPE_AT","text":"@other","rid":"99"},
            {"type":"RICH_TEXT_NODE_TYPE_EMOJI","text":"[hello]","emoji":{"icon_url":"https://i0.hdslb.com/bfs/emote/hello.png","gif_url":"https://i0.hdslb.com/bfs/emote/hello.gif","webp_url":"https://i0.hdslb.com/bfs/emote/hello.webp"}}]});
        let parsed = parse_item(&forwarded, 0).unwrap();
        assert_eq!(parsed.author.mid, Some(42));
        let original = parsed.orig.unwrap();
        assert_eq!(original.author.mid, Some(99));
        assert!(original.orig.is_none() && original.orig_unexpanded);
        assert_eq!(original.orig_reference.unwrap().id, "102");
        assert_eq!(parsed.rich_nodes[0].mentioned_mid, Some(99));
        assert_eq!(parsed.media.len(), 3);
        assert!(parsed.media.iter().all(|m| m.animated.is_none()));
    }
    #[test]
    fn pagination_and_unsupported_branches_never_become_fake_empty_or_complete_content() {
        let mut entry = item("100", 42);
        entry["modules"]["module_dynamic"]["major"] =
            json!({"type":"MAJOR_TYPE_UNKNOWN","secret":"not-public"});
        entry["modules"]["module_dynamic"]["additional"] =
            json!({"type":"ADDITIONAL_TYPE_UNKNOWN","secret":"not-public"});
        let parsed = parse_feed(
            json!({"items":[entry,item("101",42)],"has_more":1,"offset":"next"}),
            42,
            None,
            1,
        )
        .unwrap();
        assert!(parsed.truncated && parsed.next_offset.is_none());
        assert!(
            parsed.items[0].major.as_ref().unwrap().unexpanded
                && parsed.items[0].additional_unexpanded
        );
        assert!(
            !serde_json::to_string(&parsed)
                .unwrap()
                .contains("not-public")
        );
        assert!(parse_feed(json!({"has_more":true}), 42, None, 20).is_err());
        assert!(parse_feed(json!({"items":null,"has_more":true}), 42, None, 20).is_err());
        assert!(parse_feed(json!({"items":[],"need_captcha":true}), 42, None, 20).is_err());
        let empty = parse_feed(
            json!({"items":[],"has_more":true,"offset":"next"}),
            42,
            None,
            20,
        )
        .unwrap();
        assert!(empty.next_offset.is_none());
        assert_eq!(empty.has_more, Some(true));
    }
}
