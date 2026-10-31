// SPDX-License-Identifier: MIT
//! Forum browsing and searches, based on aiotieba's published request schemas.
//! See README for provenance. No web page rendering or script execution.
use crate::{
    Author, Client, Error, Forum, Fragment, Page, Result, ThreadId, model,
    wire::{self, Fields},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread_id: ThreadId,
    pub first_post_id: Option<u64>,
    pub title: String,
    pub author: Author,
    pub reply_count: Option<u64>,
    pub view_count: Option<u64>,
    pub created_at: Option<u64>,
    pub last_reply_at: Option<u64>,
    pub pinned: bool,
    pub featured: bool,
    pub fragments: Vec<Fragment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumThreads {
    pub forum: Forum,
    pub page: Page,
    pub threads: Vec<ThreadSummary>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub thread_id: ThreadId,
    pub post_id: Option<u64>,
    pub parent_post_id: Option<u64>,
    pub kind: String,
    pub title: String,
    /// Search-provided excerpt, not a claim to have read the entire post.
    pub excerpt: String,
    pub forum: Forum,
    pub author: Author,
    pub created_at: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchPage {
    pub query: String,
    pub forum: Option<String>,
    pub requested_page: u32,
    pub current_page: u32,
    pub has_more: Option<bool>,
    pub items: Vec<SearchHit>,
}
pub(crate) fn text_input(value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(Error::InvalidConfig);
    }
    Ok(())
}
fn check(page: u32, limit: usize) -> Result<()> {
    if page == 0 || page > 100000 {
        return Err(Error::InvalidPage);
    }
    if !(1..=100).contains(&limit) {
        return Err(Error::Limit);
    }
    Ok(())
}
pub(crate) fn forum_request(
    common: &[u8],
    name: &str,
    page: u32,
    limit: usize,
    sort: u32,
    featured: bool,
) -> Result<Vec<u8>> {
    text_input(name, 256)?;
    check(page, limit)?;
    if !matches!(sort, 1 | 2 | 3 | 6) {
        return Err(Error::InvalidConfig);
    }
    let mut data = Vec::new();
    wire::bytes(39, common, &mut data);
    wire::bytes(1, name.as_bytes(), &mut data);
    wire::number(2, limit as u64, &mut data);
    wire::number(3, (limit + 5) as u64, &mut data);
    wire::number(4, u64::from(featured), &mut data);
    wire::number(15, if page == 1 { 0 } else { page as u64 }, &mut data);
    wire::number(47, sort as u64, &mut data);
    let mut out = Vec::new();
    wire::bytes(1, &data, &mut out);
    Ok(out)
}
pub(crate) fn parse_forum(raw: &[u8], requested: u32) -> Result<ForumThreads> {
    let data = model::response(raw)?;
    let f = Fields::parse(
        data.bytes(2)
            .ok_or(Error::Schema("missing forum metadata"))?,
    )?;
    let forum = Forum {
        id: f.num(1),
        name: f.text(2)?,
    };
    if forum.id == 0 || forum.name.is_empty() {
        return Err(Error::Schema("forum identity missing"));
    }
    text_input(&forum.name, 256)?;
    let page = model::page(
        data.bytes(4).ok_or(Error::Schema("missing forum page"))?,
        requested,
    )?;
    let mut users = BTreeMap::new();
    for raw in data.all(17) {
        if users.len() >= 512 {
            return Err(Error::Limit);
        }
        let user = model::author(raw)?;
        users.insert(user.id, user);
    }
    let mut threads = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in data.all(7) {
        if threads.len() >= 200 {
            return Err(Error::Limit);
        }
        let fields = Fields::parse(raw)?;
        let id = ThreadId(fields.num(1));
        id.validate()?;
        if !seen.insert(id.0) {
            continue;
        }
        let author = if let Some(raw) = fields.bytes(18) {
            model::author(raw)?
        } else {
            users.get(&fields.num(56)).cloned().unwrap_or_default()
        };
        threads.push(ThreadSummary {
            thread_id: id,
            first_post_id: fields.num_opt(40).filter(|id| *id > 0),
            title: fields.text(3)?,
            author,
            reply_count: fields.num_opt(4),
            view_count: fields.num_opt(5),
            created_at: fields.num_opt(45),
            last_reply_at: fields.num_opt(7),
            pinned: fields.num(9) != 0,
            featured: fields.num(10) != 0,
            fragments: model::fragments(&fields, 142)?,
        });
    }
    Ok(ForumThreads {
        forum,
        page,
        threads,
    })
}
fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn positive(value: &Value) -> Option<u64> {
    number(value).filter(|id| *id > 0 && *id <= i64::MAX as u64)
}
fn string(value: &Value, max: usize) -> Result<String> {
    let text = value.as_str().unwrap_or("");
    if text.len() > max || text.contains('\0') {
        return Err(Error::Limit);
    }
    Ok(text.to_owned())
}
fn bool_value(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| number(value).map(|v| v != 0))
}
fn excerpt(raw: &str) -> String {
    // Search highlight markup only. Preserve other characters as plain data.
    raw.replace("<em>", "")
        .replace("</em>", "")
        .replace("<em class=\"search_word\">", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}
pub(crate) fn parse_search(
    raw: &[u8],
    query: &str,
    forum: Option<&str>,
    requested: u32,
) -> Result<SearchPage> {
    let json: Value = serde_json::from_slice(raw).map_err(|_| Error::Challenge)?;
    let global = forum.is_none();
    let code = if global {
        json.get("no")
    } else {
        json.get("error_code")
    }
    .ok_or(Error::Schema("search status missing"))?;
    let code = code
        .as_i64()
        .or_else(|| code.as_str()?.parse().ok())
        .ok_or(Error::Schema("invalid search status"))?;
    if code != 0 {
        return Err(Error::Platform(code));
    }
    let data = if global {
        json.get("data")
            .ok_or(Error::Schema("search data missing"))?
    } else {
        &json
    };
    let raw_items = data
        .get("post_list")
        .and_then(Value::as_array)
        .ok_or(Error::Schema("search items missing"))?;
    if raw_items.len() > 200 {
        return Err(Error::Limit);
    }
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    for item in raw_items {
        let id = ThreadId(positive(&item["tid"]).ok_or(Error::Schema("search thread missing"))?);
        let is_comment = !global && bool_value(&item["is_floor"]).unwrap_or(false);
        let parent = positive(&item["pid"]);
        let post = if is_comment {
            positive(&item["cid"])
        } else {
            parent
        };
        if is_comment && post.is_none() {
            return Err(Error::Schema("search subreply identity missing"));
        }
        if !seen.insert((id.0, post)) {
            continue;
        }
        let user = if global {
            &item["user"]
        } else {
            &item["author"]
        };
        let name = string(
            if global {
                &user["user_name"]
            } else {
                &user["name_show"]
            },
            1024,
        )?;
        let display = if global {
            string(&user["show_nickname"], 1024)?
        } else {
            name.clone()
        };
        items.push(SearchHit {
            thread_id: id,
            post_id: post,
            parent_post_id: if is_comment { parent } else { None },
            kind: if is_comment { "comment" } else { "post" }.into(),
            title: excerpt(&string(&item["title"], 4096)?),
            excerpt: excerpt(&string(&item["content"], 16384)?),
            forum: Forum {
                id: positive(&item["forum_id"]).unwrap_or(0),
                name: if global {
                    string(&item["forum_name"], 256)?
                } else {
                    string(&item["fname"], 256)?
                },
            },
            author: Author {
                id: positive(&user["user_id"]).unwrap_or(0),
                name: name.clone(),
                display_name: if display.is_empty() { name } else { display },
            },
            created_at: positive(if global {
                &item["create_time"]
            } else {
                &item["time"]
            }),
        });
    }
    let page_data = if global { data } else { &data["page"] };
    let current = u32::try_from(number(&page_data["current_page"]).unwrap_or(requested as u64))
        .map_err(|_| Error::InvalidPage)?;
    if current == 0 || current > 100000 {
        return Err(Error::InvalidPage);
    }
    Ok(SearchPage {
        query: query.into(),
        forum: forum.map(str::to_owned),
        requested_page: requested,
        current_page: current,
        has_more: bool_value(&page_data["has_more"]),
        items,
    })
}
impl Client {
    pub fn forum_threads(
        &self,
        name: &str,
        page: u32,
        limit: usize,
        sort: u32,
        featured: bool,
    ) -> Result<ForumThreads> {
        let request = forum_request(&self.common("12.64.1.1"), name, page, limit, sort, featured)?;
        parse_forum(
            &self.transport.api("/c/f/frs/page?cmd=301001", &request)?,
            page,
        )
    }
    pub fn search_global(
        &self,
        query: &str,
        page: u32,
        limit: usize,
        newest: bool,
    ) -> Result<SearchPage> {
        text_input(query, 512)?;
        check(page, limit)?;
        let fields = vec![
            ("word", query.to_owned()),
            ("pn", page.to_string()),
            ("rn", limit.to_string()),
            ("st", if newest { "1" } else { "0" }.into()),
            ("tt", "1".into()),
            ("subapp_type", "pc".into()),
            ("_client_type", "20".into()),
        ];
        parse_search(&self.transport.search_global(&fields)?, query, None, page)
    }
    pub fn search_forum(
        &self,
        forum: &str,
        query: &str,
        page: u32,
        limit: usize,
        only_threads: bool,
    ) -> Result<SearchPage> {
        text_input(forum, 256)?;
        text_input(query, 512)?;
        check(page, limit)?;
        let fields = vec![
            ("_client_version", "22.12.1.0".into()),
            ("kw", forum.into()),
            ("only_thread", u8::from(only_threads).to_string()),
            ("pn", page.to_string()),
            ("rn", limit.to_string()),
            ("sm", "0".into()),
            ("word", query.into()),
        ];
        parse_search(
            &self.transport.search_forum(&fields)?,
            query,
            Some(forum),
            page,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forum_request_matches_published_schema_and_sort_values() {
        let raw = forum_request(b"common", "Rust", 1, 20, 6, true).unwrap();
        let envelope = Fields::parse(&raw).unwrap();
        let data = Fields::parse(envelope.bytes(1).unwrap()).unwrap();
        assert_eq!(data.bytes(39), Some(b"common".as_slice()));
        assert_eq!(data.text(1).unwrap(), "Rust");
        assert_eq!((data.num(2), data.num(3), data.num(4)), (20, 25, 1));
        assert_eq!((data.num(15), data.num(47)), (0, 6));
        let raw = forum_request(&[], "Rust", 2, 20, 3, false).unwrap();
        let envelope = Fields::parse(&raw).unwrap();
        let data = Fields::parse(envelope.bytes(1).unwrap()).unwrap();
        assert_eq!((data.num(15), data.num(47)), (2, 3));
        assert!(forum_request(&[], "Rust", 0, 20, 6, false).is_err());
        assert!(forum_request(&[], "Rust", 1, 101, 6, false).is_err());
        assert!(forum_request(&[], "Rust", 1, 20, 0, false).is_err());
    }

    #[test]
    fn forum_decodes_author_cache_deduplicates_and_preserves_nontext() {
        let mut forum = Vec::new();
        wire::number(1, 42, &mut forum);
        wire::bytes(2, b"Rust", &mut forum);
        let mut user = Vec::new();
        wire::number(2, 77, &mut user);
        wire::bytes(3, b"author", &mut user);
        let mut fragment = Vec::new();
        wire::number(1, 999, &mut fragment);
        wire::bytes(2, b"component", &mut fragment);
        let mut thread = Vec::new();
        wire::number(1, 123, &mut thread);
        wire::bytes(3, b"title", &mut thread);
        wire::number(56, 77, &mut thread);
        wire::number(40, 456, &mut thread);
        wire::bytes(142, &fragment, &mut thread);
        let mut page = Vec::new();
        wire::number(3, 1, &mut page);
        wire::number(6, 1, &mut page);
        let mut data = Vec::new();
        wire::bytes(2, &forum, &mut data);
        wire::bytes(4, &page, &mut data);
        wire::bytes(17, &user, &mut data);
        wire::bytes(7, &thread, &mut data);
        wire::bytes(7, &thread, &mut data);
        let mut raw = Vec::new();
        wire::bytes(2, &data, &mut raw);
        let result = parse_forum(&raw, 1).unwrap();
        assert_eq!(result.forum.id, 42);
        assert!(result.page.has_more);
        assert_eq!(result.threads.len(), 1);
        assert_eq!(result.threads[0].author.id, 77);
        assert_eq!(result.threads[0].first_post_id, Some(456));
        assert!(matches!(
            &result.threads[0].fragments[0],
            Fragment::Unsupported { kind: 999, .. }
        ));
        assert!(matches!(
            parse_forum(b"<html>challenge</html>", 1),
            Err(Error::Challenge)
        ));
    }

    #[test]
    fn forum_search_preserves_exact_subreply_coordinates_and_unknown_pagination() {
        let raw = json!({"error_code":"0","post_list":[{"tid":"123","pid":"456","cid":"789","is_floor":1,"fname":"Rust","title":"a<em>标题</em>","content":"图片 &lt;附件&gt;","author":{"name_show":"某人"},"time":99}],"page":{"current_page":2}});
        let result =
            parse_search(&serde_json::to_vec(&raw).unwrap(), "标题", Some("Rust"), 2).unwrap();
        assert_eq!(result.current_page, 2);
        assert_eq!(result.has_more, None);
        assert_eq!(result.items[0].post_id, Some(789));
        assert_eq!(result.items[0].parent_post_id, Some(456));
        assert_eq!(result.items[0].title, "a标题");
        assert_eq!(result.items[0].excerpt, "图片 <附件>");
    }

    #[test]
    fn global_search_requires_status_and_essential_ids() {
        let raw = json!({"no":0,"data":{"current_page":1,"has_more":false,"post_list":[{"tid":123,"pid":456,"user":{"user_id":77,"show_nickname":"显示名"},"forum_id":42,"forum_name":"Rust","create_time":99}]}});
        let parsed = parse_search(&serde_json::to_vec(&raw).unwrap(), "query", None, 1).unwrap();
        assert_eq!(parsed.items[0].author.display_name, "显示名");
        assert_eq!(parsed.has_more, Some(false));
        assert!(matches!(
            parse_search(br#"{"no":123}"#, "q", None, 1),
            Err(Error::Platform(123))
        ));
        assert!(parse_search(br#"{"data":{"post_list":[]}}"#, "q", None, 1).is_err());
        let mut bad = raw;
        bad["data"]["post_list"][0]["tid"] = json!(0);
        assert!(parse_search(&serde_json::to_vec(&bad).unwrap(), "q", None, 1).is_err());
    }
}
