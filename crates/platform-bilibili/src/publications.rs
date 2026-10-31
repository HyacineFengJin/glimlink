// SPDX-License-Identifier: MIT
//! Account and public-uploader space-visible video lists. No creator drafts or publishing.
use crate::{AccountInfo, Client, Error, MediaResource, Result, VideoId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedVideo {
    pub aid: u64,
    pub bvid: String,
    pub title: String,
    pub description: String,
    pub created_at: u64,
    pub duration_text: String,
    pub duration_seconds: Option<u64>,
    /// Reference only; no image is fetched, decoded or submitted to a model.
    pub cover: Option<MediaResource>,
    /// A space can include cooperation videos with a different primary author.
    pub author_mid: Option<u64>,
    pub author_name: Option<String>,
    pub is_cooperation: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublicationPage {
    pub owner: AccountInfo,
    pub items: Vec<PublishedVideo>,
    pub requested_page: u32,
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub has_more: bool,
    pub next_page: Option<u32>,
    pub media_downloaded: bool,
}

/// A selected public uploader, never a representation of the logged-in account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploaderVideoPage {
    pub uploader_mid: u64,
    pub uploader_name: String,
    pub items: Vec<PublishedVideo>,
    pub requested_page: u32,
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub has_more: bool,
    pub next_page: Option<u32>,
    pub media_downloaded: bool,
}

struct ParsedPublicationPage {
    items: Vec<PublishedVideo>,
    page: u32,
    page_size: u32,
    total: u64,
    has_more: bool,
    next_page: Option<u32>,
}

impl Client {
    /// Read one page of space-visible published videos for the current cookie's
    /// verified UID. Does not accept a user-supplied alternate account ID.
    pub fn my_publications(&mut self, page: u32, page_size: u32) -> Result<PublicationPage> {
        validate_page(page, page_size)?;
        let owner = self.current_user()?;
        let params = publication_params(owner.mid, page, page_size)?;
        let data = self.signed_get("/x/space/wbi/arc/search", &params)?;
        parse_page(data, owner, page, page_size)
    }

    /// Explicitly inspect another uploader's public space. The fixed public card
    /// verifies the requested MID; it is not substituted for current_user().
    pub fn uploader_videos(
        &mut self,
        mid: u64,
        page: u32,
        page_size: u32,
    ) -> Result<UploaderVideoPage> {
        let params = publication_params(mid, page, page_size)?;
        let uploader = self.uploader_info(mid)?;
        if uploader.mid != mid {
            return Err(Error::Schema("uploader publication identity"));
        }
        let data = self.signed_get("/x/space/wbi/arc/search", &params)?;
        parse_uploader_page(data, uploader.mid, uploader.name, page, page_size)
    }
}

fn validate_page(page: u32, size: u32) -> Result<()> {
    if page == 0 || page > 100_000 || !(1..=20).contains(&size) {
        return Err(Error::InvalidPage);
    }
    Ok(())
}
fn publication_params(mid: u64, page: u32, size: u32) -> Result<Vec<(&'static str, String)>> {
    validate_page(page, size)?;
    if mid == 0 || mid > i64::MAX as u64 {
        return Err(Error::InvalidId);
    }
    Ok(vec![
        ("mid", mid.to_string()),
        ("pn", page.to_string()),
        ("ps", size.to_string()),
        ("order", "pubdate".into()),
        ("tid", "0".into()),
        ("platform", "web".into()),
        ("web_location", "1550101".into()),
    ])
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
            .ok_or(Error::Schema("publication numeric field")),
    }
}
fn number(value: &Value, key: &str) -> Result<u64> {
    optional_number(value, key)?.ok_or(Error::Schema("missing publication numeric field"))
}
fn optional_text(value: &Value, key: &str, maximum: usize) -> Result<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= maximum && !value.contains('\0') => {
            Ok(Some(value.clone()))
        }
        Some(Value::String(_)) => Err(Error::JsonLimit),
        _ => Err(Error::Schema("publication text field")),
    }
}
fn text(value: &Value, key: &str, maximum: usize) -> Result<String> {
    optional_text(value, key, maximum)?.ok_or(Error::Schema("missing publication text field"))
}
fn flag(value: &Value, key: &str) -> Result<Option<bool>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(value) if value.as_u64() == Some(0) => Ok(Some(false)),
        Some(value) if value.as_u64() == Some(1) => Ok(Some(true)),
        _ => Err(Error::Schema("publication boolean field")),
    }
}
fn duration(value: &str) -> Option<u64> {
    let parts: Vec<_> = value.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut result = 0_u64;
    for (index, value) in parts.iter().enumerate() {
        if value.is_empty() || value.len() > 8 || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value = value.parse::<u64>().ok()?;
        if index > 0 && value >= 60 {
            return None;
        }
        result = result.checked_mul(60)?.checked_add(value)?;
    }
    Some(result)
}
fn cover(value: &str) -> Result<Option<MediaResource>> {
    if value.is_empty() {
        return Ok(None);
    }
    // Historical first-party space responses use HTTP cover URLs. Upgrade only
    // after the resource-domain policy accepts their HTTPS equivalent.
    let value = if let Some(value) = value.strip_prefix("http://") {
        format!("https://{value}")
    } else if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.into()
    };
    crate::http::validate_resource_url(&value)?;
    Ok(Some(MediaResource {
        url: value,
        backup_urls: Vec::new(),
        expected_bytes: None,
        mime_type: None,
    }))
}
fn parse_page(
    data: Value,
    owner: AccountInfo,
    requested_page: u32,
    requested_size: u32,
) -> Result<PublicationPage> {
    let parsed = parse_listing(data, requested_size)?;
    Ok(PublicationPage {
        owner,
        items: parsed.items,
        requested_page,
        page: parsed.page,
        page_size: parsed.page_size,
        total: parsed.total,
        has_more: parsed.has_more,
        next_page: parsed.next_page,
        media_downloaded: false,
    })
}

fn parse_uploader_page(
    data: Value,
    uploader_mid: u64,
    uploader_name: String,
    requested_page: u32,
    requested_size: u32,
) -> Result<UploaderVideoPage> {
    if uploader_mid == 0
        || uploader_mid > i64::MAX as u64
        || uploader_name.is_empty()
        || uploader_name.len() > 1024
        || uploader_name.contains('\0')
    {
        return Err(Error::Schema("uploader publication identity"));
    }
    let parsed = parse_listing(data, requested_size)?;
    Ok(UploaderVideoPage {
        uploader_mid,
        uploader_name,
        items: parsed.items,
        requested_page,
        page: parsed.page,
        page_size: parsed.page_size,
        total: parsed.total,
        has_more: parsed.has_more,
        next_page: parsed.next_page,
        media_downloaded: false,
    })
}

fn parse_listing(data: Value, requested_size: u32) -> Result<ParsedPublicationPage> {
    if flag(&data, "is_risk")? == Some(true)
        || optional_number(&data, "gaia_res_type")?.is_some_and(|v| v > 0)
    {
        return Err(Error::RiskControl(0));
    }
    let pagination = data
        .get("page")
        .filter(|v| v.is_object())
        .ok_or(Error::Schema("publication page"))?;
    let page = u32::try_from(number(pagination, "pn")?).map_err(|_| Error::InvalidPage)?;
    let page_size = u32::try_from(number(pagination, "ps")?).map_err(|_| Error::InvalidPage)?;
    if page == 0 || page_size == 0 || page_size > 50 {
        return Err(Error::Schema("publication page identity"));
    }
    let total = number(pagination, "count")?;
    let list = data
        .get("list")
        .and_then(|v| v.get("vlist"))
        .and_then(Value::as_array)
        .ok_or(Error::Schema("publication video list"))?;
    // Page-number APIs cannot safely resume an output-truncated page, so reject
    // unexpected overproduction instead of skipping withheld videos via pn+1.
    if list.len() > requested_size as usize || list.len() > page_size as usize {
        return Err(Error::JsonLimit);
    }
    let offset = (u64::from(page) - 1)
        .checked_mul(u64::from(page_size))
        .ok_or(Error::InvalidPage)?;
    if (list.is_empty() && offset < total) || (!list.is_empty() && offset >= total) {
        return Err(Error::Schema("inconsistent publication list count"));
    }
    let mut items = Vec::with_capacity(list.len());
    for value in list {
        let aid = number(value, "aid")?;
        let bvid = text(value, "bvid", 32)?;
        if aid == 0 || !matches!(bvid.parse::<VideoId>(), Ok(VideoId::Bv(_))) {
            return Err(Error::Schema("publication video identity"));
        }
        let duration_text = text(value, "length", 128)?;
        let pic = optional_text(value, "pic", 16384)?.unwrap_or_default();
        items.push(PublishedVideo {
            aid,
            bvid,
            title: text(value, "title", 32768)?,
            description: text(value, "description", 65536)?,
            created_at: number(value, "created")?,
            duration_seconds: duration(&duration_text),
            duration_text,
            cover: cover(&pic)?,
            author_mid: optional_number(value, "mid")?,
            author_name: optional_text(value, "author", 1024)?,
            is_cooperation: flag(value, "is_union_video")?,
        });
    }
    let has_more = offset.saturating_add(items.len() as u64) < total;
    let next_page = if has_more { page.checked_add(1) } else { None };
    Ok(ParsedPublicationPage {
        items,
        page,
        page_size,
        total,
        has_more,
        next_page,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn owner() -> AccountInfo {
        AccountInfo {
            mid: 42,
            uname: "synthetic".into(),
            level: None,
            vip_status: None,
        }
    }
    fn video() -> Value {
        json!({"aid":170001,"bvid":"BV17x411w7KC","title":"published video","description":"real description","created":1234,"length":"01:23","pic":"http://i0.hdslb.com/bfs/archive/test.jpg","mid":42,"author":"synthetic","is_union_video":0,"private_token":"must-stay-private"})
    }
    fn page(items: Value, count: u64) -> Value {
        json!({"list":{"vlist":items},"page":{"pn":1,"ps":20,"count":count},"is_risk":false,"gaia_res_type":0})
    }
    #[test]
    fn published_projection_preserves_cooperation_and_media_references_without_private_fields() {
        let mut v = video();
        v["mid"] = json!(99);
        v["is_union_video"] = json!(1);
        let result = parse_page(page(json!([v]), 1), owner(), 1, 20).unwrap();
        assert_eq!(result.owner.mid, 42);
        assert_eq!(result.items[0].author_mid, Some(99));
        assert_eq!(result.items[0].is_cooperation, Some(true));
        assert_eq!(result.items[0].duration_seconds, Some(83));
        assert!(
            result.items[0]
                .cover
                .as_ref()
                .unwrap()
                .url
                .starts_with("https://i0.hdslb.com/")
        );
        assert!(!result.media_downloaded);
        assert!(!result.has_more);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("must-stay-private")
        );
    }
    #[test]
    fn pagination_and_risk_failures_cannot_become_empty_or_skipped_publications() {
        let result = parse_page(page(json!([video()]), 21), owner(), 1, 20).unwrap();
        assert!(result.has_more);
        assert_eq!(result.next_page, Some(2));
        let empty = parse_page(page(json!([]), 0), owner(), 1, 20).unwrap();
        assert!(empty.items.is_empty());
        assert!(!empty.has_more);
        assert!(parse_page(page(json!([]), 21), owner(), 1, 20).is_err());
        let mut risk = page(json!([]), 0);
        risk["is_risk"] = json!(true);
        assert!(matches!(
            parse_page(risk, owner(), 1, 20),
            Err(Error::RiskControl(0))
        ));
        let mut risk = page(json!([]), 0);
        risk["gaia_res_type"] = json!(1);
        assert!(matches!(
            parse_page(risk, owner(), 1, 20),
            Err(Error::RiskControl(0))
        ));
        assert!(parse_page(json!({}), owner(), 1, 20).is_err());
        assert!(parse_page(page(json!([video(), video()]), 2), owner(), 1, 1).is_err());
    }
    #[test]
    fn request_scope_duration_and_cover_validation_remain_bounded() {
        let params = publication_params(42, 1, 20).unwrap();
        assert!(params.contains(&("mid", "42".into())));
        assert!(params.contains(&("order", "pubdate".into())));
        for (page, size) in [(0, 20), (1, 0), (1, 21), (100001, 20)] {
            assert!(publication_params(42, page, size).is_err());
        }
        assert!(publication_params(0, 1, 20).is_err());
        assert_eq!(duration("1:02:03"), Some(3723));
        assert_eq!(duration("123:59"), Some(7439));
        assert_eq!(duration("00:60"), None);
        assert_eq!(duration("unknown"), None);
        assert!(cover("http://evil.invalid/a.jpg").is_err());
        assert!(cover("https://i0.hdslb.com.evil.invalid/a.jpg").is_err());
    }

    #[test]
    fn public_uploader_pages_are_not_current_account_identity_and_keep_cooperation() {
        let mut item = video();
        item["mid"] = json!(99);
        item["author"] = json!("primary collaborator");
        item["is_union_video"] = json!(1);
        let result = parse_uploader_page(
            page(json!([item]), 21),
            42,
            "selected public uploader".into(),
            1,
            20,
        )
        .unwrap();
        assert_eq!(result.uploader_mid, 42);
        assert_eq!(result.uploader_name, "selected public uploader");
        assert_eq!(result.items[0].author_mid, Some(99));
        assert_eq!(
            result.items[0].author_name.as_deref(),
            Some("primary collaborator")
        );
        assert_eq!(result.items[0].is_cooperation, Some(true));
        assert_eq!(result.next_page, Some(2));
        assert!(!result.media_downloaded);
        let value = serde_json::to_value(&result).unwrap();
        assert!(value.get("owner").is_none());
        assert!(!value.to_string().contains("must-stay-private"));
    }

    #[test]
    fn public_uploader_requests_validate_before_network_and_share_strict_page_limits() {
        let mut client = Client::new(crate::Config::default()).unwrap();
        assert!(matches!(
            client.uploader_videos(0, 1, 20),
            Err(Error::InvalidId)
        ));
        assert!(matches!(
            client.uploader_videos(42, 0, 20),
            Err(Error::InvalidPage)
        ));
        assert!(matches!(
            client.uploader_videos(42, 1, 21),
            Err(Error::InvalidPage)
        ));
        let empty = parse_uploader_page(page(json!([]), 0), 42, "public".into(), 1, 20).unwrap();
        assert_eq!(empty.total, 0);
        assert!(!empty.has_more);
        let mut risk = page(json!([]), 0);
        risk["gaia_res_type"] = json!(1);
        assert!(matches!(
            parse_uploader_page(risk, 42, "public".into(), 1, 20),
            Err(Error::RiskControl(_))
        ));
        assert!(
            parse_uploader_page(
                page(json!([video(), video()]), 2),
                42,
                "public".into(),
                1,
                1
            )
            .is_err()
        );
        assert!(parse_uploader_page(page(json!([]), 0), 0, "public".into(), 1, 20).is_err());
    }
}
