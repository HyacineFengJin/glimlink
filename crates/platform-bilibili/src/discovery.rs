// SPDX-License-Identifier: MIT
//! Bounded video discovery. Search protocol and fields:
//! pskdje/bilibili-API-collect docs/search/{search_request,search_response}.md;
//! popular list: docs/video_ranking/popular.md. No media is downloaded here.
use crate::model::{VideoStats, counter, video_stats};
use crate::{Client, Error, MediaResource, Owner, Result, VideoId, api_data};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchOrder {
    #[default]
    Relevance,
    View,
    Newest,
    Comments,
}
impl SearchOrder {
    fn parameter(self) -> &'static str {
        match self {
            Self::Relevance => "totalrank",
            Self::View => "click",
            Self::Newest => "pubdate",
            Self::Comments => "scores",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchDuration {
    #[default]
    All,
    UnderTenMinutes,
    TenToThirtyMinutes,
    ThirtyToSixtyMinutes,
    OverSixtyMinutes,
}
impl SearchDuration {
    fn parameter(self) -> u8 {
        match self {
            Self::All => 0,
            Self::UnderTenMinutes => 1,
            Self::TenToThirtyMinutes => 2,
            Self::ThirtyToSixtyMinutes => 3,
            Self::OverSixtyMinutes => 4,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchVideo {
    pub aid: u64,
    pub bvid: String,
    /// Plain text; only documented search highlighting is removed.
    pub title: String,
    pub description: String,
    pub owner: Owner,
    pub duration_text: String,
    pub duration_seconds: Option<u64>,
    pub published_at: Option<u64>,
    pub stat: VideoStats,
    /// Private SDK locator, not a fetched/observed image.
    pub cover: Option<MediaResource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoSearchPage {
    pub keyword: String,
    pub order: SearchOrder,
    pub duration: SearchDuration,
    pub items: Vec<SearchVideo>,
    pub page: u32,
    pub page_size: u32,
    pub total: Option<u64>,
    pub total_pages: Option<u32>,
    pub has_more: Option<bool>,
    pub next_page: Option<u32>,
    pub media_downloaded: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PopularVideoPage {
    pub source: &'static str,
    pub items: Vec<SearchVideo>,
    pub page: u32,
    pub page_size: u32,
    pub has_more: Option<bool>,
    pub next_page: Option<u32>,
    pub media_downloaded: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecommendedVideo {
    #[serde(flatten)]
    pub video: SearchVideo,
    pub recommendation_reason: Option<String>,
    pub promoted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecommendationPage {
    pub items: Vec<RecommendedVideo>,
    pub source: &'static str,
    pub signed_in_request: bool,
    pub personalization_verified: bool,
    pub refresh_index: u32,
    pub requested_size: u32,
    pub skipped_non_video_items: usize,
    pub has_more: Option<bool>,
    pub next_page: Option<u32>,
    pub pagination: &'static str,
    pub media_downloaded: bool,
}

impl Client {
    /// The web homepage feed, not the public popular list. A refresh is not a
    /// stable page. Existing Cookie/WBI state is reused; challenges never cause
    /// fabricated device data, another endpoint, or automatic login.
    pub fn home_recommendations(
        &mut self,
        refresh_index: u32,
        size: u32,
    ) -> Result<RecommendationPage> {
        if !(1..=100).contains(&refresh_index) || !(1..=20).contains(&size) {
            return Err(Error::InvalidPage);
        }
        if self.transport.cookie_header().is_none() {
            return Err(Error::LoginRequired);
        }
        self.current_user()?; // Do not label an expired Cookie's public fallback as signed in.
        let data = self.signed_get(
            "/x/web-interface/wbi/index/top/feed/rcmd",
            &[
                ("fresh_type", "3".into()),
                ("version", "1".into()),
                ("ps", size.to_string()),
                ("fresh_idx", refresh_index.to_string()),
                ("fresh_idx_1h", refresh_index.to_string()),
                ("brush", (refresh_index - 1).to_string()),
                ("homepage_ver", "1".into()),
            ],
        )?;
        recommendation_page(data, refresh_index, size)
    }
    /// Search is one signed request after any necessary read-only WBI key read.
    /// A normal saved cookie may lack buvid3: challenges are returned as errors,
    /// without synthesizing a fingerprint or switching to another endpoint.
    pub fn search_videos(
        &mut self,
        keyword: &str,
        page: u32,
        order: SearchOrder,
        duration: SearchDuration,
    ) -> Result<VideoSearchPage> {
        let params = search_params(keyword, page, order, duration)?;
        let data = self.signed_get("/x/web-interface/wbi/search/type", &params)?;
        search_page(data, keyword, page, order, duration)
    }

    /// Explicit independent discovery, never an automatic fallback for search.
    /// This endpoint is distinct from the homepage feed, even with a Cookie.
    pub fn popular_videos(&self, page: u32, page_size: u32) -> Result<PopularVideoPage> {
        validate_popular_page(page, page_size)?;
        let url =
            format!("https://api.bilibili.com/x/web-interface/popular?pn={page}&ps={page_size}");
        popular_page(
            api_data(self.transport.json(&url)?, false)?,
            page,
            page_size,
        )
    }
}

fn recommendation_page(data: Value, refresh_index: u32, size: u32) -> Result<RecommendationPage> {
    detect_challenge(&data)?;
    let raw = data
        .get("item")
        .and_then(Value::as_array)
        .ok_or(Error::Schema("homepage item list"))?;
    if raw.len() > 64 {
        return Err(Error::Schema("homepage batch exceeds bound"));
    }
    let mut items = Vec::new();
    let mut skipped = 0;
    for item in raw {
        if !matches!(
            item.get("goto").and_then(Value::as_str),
            Some("av" | "video")
        ) {
            skipped += 1;
            continue;
        }
        let normalized = serde_json::json!({"aid":item.get("id").or_else(||item.get("aid")),"bvid":item.get("bvid"),
            "title":item.get("title"),"desc":item.get("desc"),"pic":item.get("pic"),
            "duration":item.get("duration"),"pubdate":item.get("pubdate"),"owner":item.get("owner"),"stat":item.get("stat")});
        let video = popular_video(&normalized)?;
        if items.iter().any(|previous: &RecommendedVideo| {
            previous.video.aid == video.aid || previous.video.bvid == video.bvid
        }) {
            continue;
        }
        let reason = item
            .pointer("/rcmd_reason/content")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 2048)
            .map(str::to_owned);
        let promoted = item
            .get("is_ad")
            .is_some_and(|v| v == true || v.as_u64().is_some_and(|v| v != 0))
            || item.get("ad_info").is_some_and(|v| !v.is_null());
        items.push(RecommendedVideo {
            video,
            recommendation_reason: reason,
            promoted,
        });
    }
    let next_page = if !items.is_empty() && refresh_index < 100 {
        Some(refresh_index + 1)
    } else {
        None
    };
    Ok(RecommendationPage {
        items,
        source: "bilibili_home_recommendations",
        signed_in_request: true,
        personalization_verified: false,
        refresh_index,
        requested_size: size,
        skipped_non_video_items: skipped,
        has_more: None,
        next_page,
        pagination: "refresh_batches_not_stable_pages",
        media_downloaded: false,
    })
}

fn search_params(
    keyword: &str,
    page: u32,
    order: SearchOrder,
    duration: SearchDuration,
) -> Result<Vec<(&'static str, String)>> {
    if page == 0 || page > 50 {
        return Err(Error::InvalidPage);
    }
    if keyword.trim().is_empty()
        || keyword.len() > 1024
        || keyword.chars().count() > 256
        || keyword.chars().any(char::is_control)
    {
        return Err(Error::InvalidConfig);
    }
    Ok(vec![
        ("search_type", "video".into()),
        ("keyword", keyword.into()),
        ("page", page.to_string()),
        ("order", order.parameter().into()),
        ("duration", duration.parameter().to_string()),
        ("tids", "0".into()),
    ])
}
fn validate_popular_page(page: u32, size: u32) -> Result<()> {
    if page == 0 || page > 1000 || !(1..=20).contains(&size) {
        Err(Error::InvalidPage)
    } else {
        Ok(())
    }
}
fn text(value: &Value, name: &str, limit: usize) -> Result<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= limit && !s.contains('\0'))
        .map(str::to_owned)
        .ok_or(Error::Schema("discovery text"))
}
fn optional_text(value: &Value, name: &str, limit: usize) -> Result<Option<String>> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        _ => text(value, name, limit).map(Some),
    }
}
fn number(value: &Value, name: &str) -> Result<u64> {
    counter(value, name)?.ok_or(Error::Schema("discovery numeric field"))
}
fn id(value: &Value, name: &str) -> Result<u64> {
    let id = number(value, name)?;
    if id == 0 || id > i64::MAX as u64 {
        return Err(Error::Schema("discovery zero identity"));
    }
    Ok(id)
}
fn clean_title(raw: &str) -> String {
    raw.replace("<em class=\"keyword\">", "")
        .replace("<em class='keyword'>", "")
        .replace("</em>", "")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
fn duration_seconds(raw: &str) -> Option<u64> {
    let parts: Vec<_> = raw.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut seconds = 0_u64;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || part.len() > 8 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value = part.parse::<u64>().ok()?;
        if index > 0 && value >= 60 {
            return None;
        }
        seconds = seconds.checked_mul(60)?.checked_add(value)?;
    }
    Some(seconds)
}
fn cover(raw: Option<String>) -> Result<Option<MediaResource>> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let url = if let Some(tail) = raw.strip_prefix("http://") {
        format!("https://{tail}")
    } else if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw
    };
    crate::validate_resource_url(&url)?;
    Ok(Some(MediaResource {
        url,
        backup_urls: vec![],
        expected_bytes: None,
        mime_type: None,
    }))
}
fn identity(raw: &Value) -> Result<(u64, String)> {
    let aid = id(raw, "aid")?;
    let bvid = text(raw, "bvid", 32)?;
    if !matches!(bvid.parse::<VideoId>(), Ok(VideoId::Bv(_))) {
        return Err(Error::InvalidId);
    }
    Ok((aid, bvid))
}
fn search_video(raw: &Value) -> Result<SearchVideo> {
    if raw.get("type").and_then(Value::as_str) != Some("video") {
        return Err(Error::Schema("non-video discovery result"));
    }
    let (aid, bvid) = identity(raw)?;
    let duration_text = text(raw, "duration", 128)?;
    let stat = VideoStats {
        view: counter(raw, "play")?,
        like: counter(raw, "like")?,
        favorite: counter(raw, "favorites")?,
        reply: counter(raw, "review")?,
        danmaku: counter(raw, "video_review")?,
        ..Default::default()
    };
    Ok(SearchVideo {
        aid,
        bvid,
        title: clean_title(&text(raw, "title", 32768)?),
        description: optional_text(raw, "description", 65536)?.unwrap_or_default(),
        owner: Owner {
            id: id(raw, "mid")?,
            name: text(raw, "author", 1024)?,
        },
        duration_seconds: duration_seconds(&duration_text),
        duration_text,
        published_at: counter(raw, "pubdate")?,
        stat,
        cover: cover(optional_text(raw, "pic", 16384)?)?,
    })
}
fn popular_video(raw: &Value) -> Result<SearchVideo> {
    let (aid, bvid) = identity(raw)?;
    let owner = raw.get("owner").ok_or(Error::Schema("popular owner"))?;
    let seconds = number(raw, "duration")?;
    Ok(SearchVideo {
        aid,
        bvid,
        title: text(raw, "title", 32768)?,
        description: optional_text(raw, "desc", 65536)?.unwrap_or_default(),
        owner: Owner {
            id: id(owner, "mid")?,
            name: text(owner, "name", 1024)?,
        },
        duration_text: format!("{}:{:02}", seconds / 60, seconds % 60),
        duration_seconds: Some(seconds),
        published_at: counter(raw, "pubdate")?,
        stat: video_stats(raw.get("stat"), aid)?,
        cover: cover(optional_text(raw, "pic", 16384)?)?,
    })
}
fn detect_challenge(data: &Value) -> Result<()> {
    if data.get("v_voucher").is_some()
        || data.get("is_risk").and_then(Value::as_bool) == Some(true)
        || counter(data, "gaia_res_type")?.is_some_and(|n| n != 0)
    {
        return Err(Error::RiskControl(0));
    }
    Ok(())
}
fn search_page(
    data: Value,
    keyword: &str,
    page: u32,
    order: SearchOrder,
    duration: SearchDuration,
) -> Result<VideoSearchPage> {
    detect_challenge(&data)?;
    let actual_page = u32::try_from(number(&data, "page")?).map_err(|_| Error::InvalidPage)?;
    let size_legacy = counter(&data, "pagesize")?;
    let size_current = counter(&data, "page_size")?;
    if size_legacy.zip(size_current).is_some_and(|(a, b)| a != b) {
        return Err(Error::Schema("conflicting search page sizes"));
    }
    let page_size = size_legacy
        .or(size_current)
        .ok_or(Error::Schema("search page size"))?;
    if actual_page != page || !(1..=20).contains(&page_size) {
        return Err(Error::Schema("search page identity"));
    }
    let total = counter(&data, "numResults")?;
    let total_pages = counter(&data, "numPages")?
        .map(|n| u32::try_from(n).map_err(|_| Error::InvalidPage))
        .transpose()?;
    let list = data
        .get("result")
        .and_then(Value::as_array)
        .ok_or(Error::Schema("search result list"))?;
    if list.len() > page_size as usize {
        return Err(Error::Schema("search result exceeds page"));
    }
    if total.is_some_and(|n| n < list.len() as u64)
        || (!list.is_empty() && total_pages.is_some_and(|pages| page > pages))
    {
        return Err(Error::Schema("search result count mismatch"));
    }
    if list.is_empty() && (total.is_some_and(|n| n > 0) && total_pages.is_none_or(|n| page <= n)) {
        return Err(Error::Schema("search empty inconsistent page"));
    }
    let mut items = Vec::with_capacity(list.len());
    for raw in list {
        let video = search_video(raw)?;
        if items
            .iter()
            .any(|v: &SearchVideo| v.aid == video.aid || v.bvid == video.bvid)
        {
            return Err(Error::Schema("duplicate search video"));
        }
        items.push(video);
    }
    let has_more = total_pages.map(|pages| page < pages);
    let next_page = has_more
        .filter(|more| *more)
        .and_then(|_| page.checked_add(1))
        .filter(|n| *n <= 50);
    Ok(VideoSearchPage {
        keyword: keyword.into(),
        order,
        duration,
        items,
        page,
        page_size: page_size as u32,
        total,
        total_pages,
        has_more,
        next_page,
        media_downloaded: false,
    })
}
fn popular_page(data: Value, page: u32, page_size: u32) -> Result<PopularVideoPage> {
    detect_challenge(&data)?;
    let list = data
        .get("list")
        .and_then(Value::as_array)
        .ok_or(Error::Schema("popular list"))?;
    if list.len() > page_size as usize {
        return Err(Error::Schema("popular page exceeds limit"));
    }
    let has_more = match data.get("no_more") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(no_more)) => Some(!no_more),
        _ => return Err(Error::Schema("popular no_more flag")),
    };
    if list.is_empty() && has_more == Some(true) {
        return Err(Error::Schema("popular empty nonterminal page"));
    }
    let mut items = Vec::with_capacity(list.len());
    for raw in list {
        let video = popular_video(raw)?;
        if items
            .iter()
            .any(|v: &SearchVideo| v.aid == video.aid || v.bvid == video.bvid)
        {
            return Err(Error::Schema("duplicate popular video"));
        }
        items.push(video);
    }
    Ok(PopularVideoPage {
        source: "bilibili_popular",
        items,
        page,
        page_size,
        has_more,
        next_page: has_more
            .filter(|more| *more)
            .and_then(|_| page.checked_add(1)),
        media_downloaded: false,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn homepage_provenance_filters_non_video_and_preserves_reasons() {
        let row = serde_json::json!({"goto":"av","id":170001,"bvid":"BV17x411w7KC","title":"synthetic home item","duration":30,
            "owner":{"mid":42,"name":"synthetic"},"stat":{"view":5},"pic":"https://i0.hdslb.com/bfs/archive/test.jpg", "rcmd_reason":{"content":"synthetic reason"}});
        let parsed = super::recommendation_page(
            serde_json::json!({"item":[row.clone(),{"goto":"live"},row]}),
            1,
            20,
        )
        .unwrap();
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.skipped_non_video_items, 1);
        assert_eq!(
            parsed.items[0].recommendation_reason.as_deref(),
            Some("synthetic reason")
        );
        assert_eq!(parsed.source, "bilibili_home_recommendations");
        assert!(parsed.signed_in_request);
        assert!(!parsed.personalization_verified);
        assert_eq!(parsed.has_more, None);
        assert_eq!(parsed.next_page, Some(2));
        assert!(!parsed.media_downloaded);
        assert!(super::recommendation_page(serde_json::json!({"list":[]}), 1, 20).is_err());
        assert!(matches!(
            super::recommendation_page(serde_json::json!({"item":[],"is_risk":true}), 1, 20),
            Err(crate::Error::RiskControl(_))
        ));
    }
    use super::*;
    use serde_json::json;
    fn video() -> Value {
        json!({"type":"video","aid":170001,"bvid":"BV17x411w7KC",
        "title":"<em class=\"keyword\">猫</em> &amp; 小电视","description":"example",
        "mid":42,"author":"test","duration":"0:15","pubdate":123,
        "play":12,"review":3,"video_review":4,"favorites":5,
        "pic":"//i0.hdslb.com/bfs/archive/test.jpg","SESSDATA":"not-public"})
    }
    fn page(list: Value, total: u64, pages: u64) -> Value {
        json!({"page":1,"pagesize":20,"numResults":total,"numPages":pages,"result":list})
    }
    #[test]
    fn search_fields_keep_exact_counts_duration_and_unfetched_covers() {
        let parsed = search_page(
            page(json!([video()]), 1, 1),
            "猫",
            1,
            SearchOrder::View,
            SearchDuration::UnderTenMinutes,
        )
        .unwrap();
        assert_eq!(parsed.items[0].title, "猫 & 小电视");
        assert_eq!(parsed.items[0].duration_seconds, Some(15));
        assert_eq!(parsed.items[0].stat.view, Some(12));
        assert_eq!(parsed.items[0].stat.like, None);
        assert!(
            parsed.items[0]
                .cover
                .as_ref()
                .unwrap()
                .url
                .starts_with("https://")
        );
        assert!(!parsed.media_downloaded);
        assert_eq!(parsed.has_more, Some(false));
        assert!(
            !serde_json::to_string(&parsed)
                .unwrap()
                .contains("not-public")
        );
    }
    #[test]
    fn discovery_requests_and_pages_never_guess_or_skip_results() {
        let params = search_params(
            "猫",
            1,
            SearchOrder::Comments,
            SearchDuration::UnderTenMinutes,
        )
        .unwrap();
        assert!(
            params.contains(&("order", "scores".into()))
                && params.contains(&("duration", "1".into()))
        );
        assert!(search_params(" ", 1, SearchOrder::Relevance, SearchDuration::All).is_err());
        assert!(search_params("猫", 51, SearchOrder::Relevance, SearchDuration::All).is_err());
        assert!(validate_popular_page(1, 21).is_err());
        let more = search_page(
            page(json!([video()]), 21, 2),
            "猫",
            1,
            SearchOrder::Relevance,
            SearchDuration::All,
        )
        .unwrap();
        assert_eq!(more.next_page, Some(2));
        assert!(
            search_page(
                page(json!([]), 21, 2),
                "猫",
                1,
                SearchOrder::Relevance,
                SearchDuration::All
            )
            .is_err()
        );
        let mut risk = page(json!([]), 0, 0);
        risk["is_risk"] = json!(true);
        assert!(matches!(
            search_page(risk, "猫", 1, SearchOrder::Relevance, SearchDuration::All),
            Err(Error::RiskControl(_))
        ));
        let mut bad = video();
        bad["pic"] = json!("https://evil.example/image.jpg");
        assert!(search_video(&bad).is_err());
    }
    #[test]
    fn popular_stats_are_snapshot_counts_with_true_server_end_state() {
        let raw = json!({"aid":170001,"bvid":"BV17x411w7KC","title":"test","desc":"description",
            "duration":15,"owner":{"mid":42,"name":"test"},"stat":{"aid":170001,"like":9,"view":12},
            "pic":"https://i0.hdslb.com/bfs/archive/test.jpg"});
        let parsed = popular_page(json!({"list":[raw],"no_more":false}), 1, 20).unwrap();
        assert_eq!(parsed.items[0].stat.like, Some(9));
        assert_eq!(parsed.next_page, Some(2));
        assert_eq!(parsed.items[0].duration_seconds, Some(15));
        assert!(popular_page(json!({"list":[],"no_more":false}), 1, 20).is_err());
        assert_eq!(
            popular_page(json!({"list":[],"no_more":true}), 1, 20)
                .unwrap()
                .has_more,
            Some(false)
        );
    }
}
