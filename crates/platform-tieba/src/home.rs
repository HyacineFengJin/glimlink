// SPDX-License-Identifier: MIT
//! Authenticated native home recommendations. Protocol field facts are checked
//! against TiebaLite's Personalized.proto at 7d2a3a2aa4bc78a7129c03c7a09234ef24571f60.
//! This is not a public hot list, forum listing or local recommender.
use crate::{
    Author, Client, Error, Forum, Fragment, MediaKind, MediaResource, Result, ThreadId, model,
    wire::{self, Fields},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecommendedThread {
    pub thread_id: ThreadId,
    pub title: String,
    pub author: Author,
    pub forum: Forum,
    pub reply_count: Option<u64>,
    pub view_count: Option<u64>,
    pub like_count: Option<u64>,
    pub created_at: Option<u64>,
    pub provider_source: Option<String>,
    pub fragments: Vec<Fragment>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecommendationPage {
    pub requested_page: u32,
    pub server_item_count: usize,
    pub omitted_live_count: usize,
    pub items: Vec<RecommendedThread>,
}
fn check(page: u32, count: usize) -> Result<()> {
    if !(1..=100).contains(&page) {
        return Err(Error::InvalidPage);
    }
    if !(1..=20).contains(&count) {
        return Err(Error::Limit);
    }
    Ok(())
}
fn request(common: &[u8], page: u32, count: usize) -> Result<Vec<u8>> {
    check(page, count)?;
    let mut data = Vec::new();
    wire::bytes(1, common, &mut data);
    wire::number(4, if page == 1 { 1 } else { 2 }, &mut data);
    wire::number(5, count as u64, &mut data);
    wire::number(6, page.into(), &mut data);
    wire::number(11, 1, &mut data);
    wire::number(23, 1, &mut data);
    let mut outer = Vec::new();
    wire::bytes(1, &data, &mut outer);
    Ok(outer)
}
fn text(fields: &Fields<'_>, field: u32, max: usize) -> Result<String> {
    let Some(bytes) = fields.bytes(field) else {
        return Ok(String::new());
    };
    if bytes.len() > max {
        return Err(Error::Limit);
    }
    let value = std::str::from_utf8(bytes).map_err(|_| Error::InvalidProto)?;
    if value.contains('\0') {
        return Err(Error::InvalidProto);
    }
    Ok(value.into())
}
fn image(fields: &Fields<'_>, output: &mut Vec<Fragment>) -> Result<()> {
    for field in [15, 3, 8, 18] {
        let url = text(fields, field, 4096)?;
        if url.is_empty() {
            continue;
        }
        if let Ok(url) = crate::transport::normalize_media_url(&url) {
            output.push(Fragment::Media {
                label: "feed image".into(),
                resource: MediaResource {
                    kind: MediaKind::Image,
                    url,
                    mime: None,
                    width: u32::try_from(fields.num(10)).ok().filter(|n| *n > 0),
                    height: u32::try_from(fields.num(11)).ok().filter(|n| *n > 0),
                    duration_seconds: None,
                },
            });
            return Ok(());
        }
    }
    output.push(Fragment::Unsupported {
        kind: 3,
        text: String::new(),
        reason: "Feed media has no supported Tieba CDN URL; open the full thread for context"
            .into(),
    });
    Ok(())
}
pub(crate) fn parse_thread(
    raw: &[u8],
    provider_source: Option<String>,
) -> Result<Option<RecommendedThread>> {
    let fields = Fields::parse(raw)?;
    if fields.bytes(113).is_some() || fields.bytes(72).is_some() {
        return Ok(None);
    }
    let thread_id = ThreadId(fields.num_opt(1).or_else(|| fields.num_opt(2)).unwrap_or(0));
    thread_id.validate()?;
    let author = if let Some(raw) = fields.bytes(18) {
        let user = Fields::parse(raw)?;
        Author {
            id: user.num(2),
            name: text(&user, 3, 512)?,
            display_name: text(&user, 4, 512)?,
        }
    } else {
        Author {
            id: fields.num(56),
            ..Default::default()
        }
    };
    let mut fragments = model::fragments(&fields, 142)?;
    // Older home responses use the smaller Abstract message, which has the
    // same type/text/link/src wire coordinates as PbContent.
    if fragments.is_empty() {
        fragments = model::fragments(&fields, 21)?;
    }
    if fragments.len() > 40 {
        return Err(Error::Limit);
    }
    for raw in fields.all(22) {
        if fragments.len() >= 40 {
            return Err(Error::Limit);
        }
        image(&Fields::parse(raw)?, &mut fragments)?;
    }
    if let Some(raw) = fields.bytes(79) {
        let video = Fields::parse(raw)?;
        let url = text(&video, 2, 4096)?;
        if let Ok(url) = crate::transport::normalize_media_url(&url) {
            fragments.push(Fragment::Media {
                label: "feed video".into(),
                resource: MediaResource {
                    kind: MediaKind::Video,
                    url,
                    mime: None,
                    width: u32::try_from(video.num(4)).ok().filter(|n| *n > 0),
                    height: u32::try_from(video.num(5)).ok().filter(|n| *n > 0),
                    duration_seconds: video.num_opt(3),
                },
            });
        }
    }
    Ok(Some(RecommendedThread {
        thread_id,
        title: text(&fields, 3, 2048)?,
        author,
        forum: Forum {
            id: fields.num(27),
            name: text(&fields, 28, 512)?,
        },
        reply_count: fields.num_opt(4),
        view_count: fields.num_opt(5),
        like_count: fields.num_opt(124),
        created_at: fields.num_opt(45),
        provider_source,
        fragments,
    }))
}
fn parse(raw: &[u8], page: u32) -> Result<RecommendationPage> {
    check(page, 20)?;
    let data = model::response(raw)?;
    let mut sources = BTreeMap::new();
    for (index, raw) in data.all(7).enumerate() {
        if index >= 100 {
            return Err(Error::Limit);
        }
        let fields = Fields::parse(raw)?;
        let source = text(&fields, 3, 512)?;
        if !source.is_empty() {
            sources.insert(fields.num(1), source);
        }
        // Extra/weight/dislike tracking fields deliberately stay private.
    }
    let mut out = RecommendationPage {
        requested_page: page,
        server_item_count: 0,
        omitted_live_count: 0,
        items: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    for raw in data.all(2) {
        out.server_item_count += 1;
        if out.server_item_count > 100 {
            return Err(Error::Limit);
        }
        if let Some(mut thread) = parse_thread(raw, None)? {
            if seen.insert(thread.thread_id.0) {
                thread.provider_source = sources.remove(&thread.thread_id.0);
                out.items.push(thread);
            }
        } else {
            out.omitted_live_count += 1;
        }
    }
    Ok(out)
}
impl Client {
    /// Each call asks the logged-in account's actual home recommendation
    /// endpoint. Pages are refresh/load-more batches, not a stable public list.
    pub fn home_recommendations(&self, page: u32, count: usize) -> Result<RecommendationPage> {
        check(page, count)?;
        if self.transport.bduss().is_none() {
            return Err(Error::LoginRequired);
        }
        let body = request(&self.common("12.64.1.1"), page, count)?;
        let raw = self
            .transport
            .api("/c/f/excellent/personalized?cmd=309264", &body)?;
        parse(&raw, page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: u64) -> Vec<u8> {
        let mut t = Vec::new();
        wire::number(1, id, &mut t);
        wire::bytes(3, b"Synthetic recommendation", &mut t);
        wire::number(27, 7, &mut t);
        wire::bytes(28, "合成吧".as_bytes(), &mut t);
        let mut a = Vec::new();
        wire::number(2, 8, &mut a);
        wire::bytes(4, b"Synthetic author", &mut a);
        wire::bytes(29, b"private-synthetic-bduss", &mut a);
        wire::bytes(36, b"not-projected", &mut a);
        wire::bytes(18, &a, &mut t);
        t
    }
    fn envelope(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        wire::bytes(2, data, &mut out);
        out
    }
    #[test]
    fn request_requires_auth_and_distinguishes_refresh_from_load_more() {
        for (page, load) in [(1, 1), (2, 2)] {
            let request = request(b"synthetic-common", page, 3).unwrap();
            let outer = Fields::parse(&request).unwrap();
            let f = Fields::parse(outer.bytes(1).unwrap()).unwrap();
            assert_eq!(f.num(4), load);
            assert_eq!(f.num(6), u64::from(page));
            assert_eq!(f.num(5), 3);
        }
        let client = Client::new(crate::Config::default()).unwrap();
        assert_eq!(
            client.home_recommendations(1, 3).unwrap_err(),
            Error::LoginRequired
        );
        assert!(request(b"common", 0, 3).is_err());
        assert!(request(b"common", 1, 21).is_err());
    }
    #[test]
    fn home_projection_keeps_threads_and_lazy_media_without_private_user_fields() {
        let mut t = entry(42);
        let mut media = Vec::new();
        wire::bytes(15, b"https://tiebapic.baidu.com/synthetic.gif", &mut media);
        wire::bytes(22, &media, &mut t);
        let mut live = entry(99);
        wire::bytes(113, b"live marker", &mut live);
        let mut source = Vec::new();
        wire::number(1, 42, &mut source);
        wire::bytes(3, "个性推荐".as_bytes(), &mut source);
        wire::bytes(6, b"private-tracking-extra", &mut source);
        let mut data = Vec::new();
        wire::bytes(2, &t, &mut data);
        wire::bytes(2, &t, &mut data);
        wire::bytes(2, &live, &mut data);
        wire::bytes(7, &source, &mut data);
        let page = parse(&envelope(&data), 1).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.omitted_live_count, 1);
        assert_eq!(page.server_item_count, 3);
        assert_eq!(page.items[0].forum.name, "合成吧");
        assert!(
            matches!(&page.items[0].fragments[0],Fragment::Media{resource,..}if resource.url.ends_with("synthetic.gif"))
        );
        let out = serde_json::to_string(&page).unwrap();
        for private in ["bduss", "not-projected", "private-tracking", "https://"] {
            assert!(!out.contains(private));
        }
        assert!(out.contains("个性推荐"));
    }
    #[test]
    fn malformed_or_challenged_responses_are_never_successful_empty_feeds() {
        assert!(parse(b"<html>verify</html>", 1).is_err());
        assert!(parse(b"{}", 1).is_err());
        assert!(parse(&[], 1).is_err());
        let mut error = Vec::new();
        wire::number(1, 1, &mut error);
        wire::bytes(2, "请先登录".as_bytes(), &mut error);
        let mut out = Vec::new();
        wire::bytes(1, &error, &mut out);
        assert_eq!(parse(&out, 1).unwrap_err(), Error::LoginRequired);
        let mut data = Vec::new();
        wire::bytes(2, &entry(0), &mut data);
        assert!(parse(&envelope(&data), 1).is_err());
    }
}
