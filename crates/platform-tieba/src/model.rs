// SPDX-License-Identifier: MIT
use crate::{Error, Result, ThreadId, wire::Fields};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

#[derive(Serialize)]
pub struct Fetched<T> {
    pub data: T,
    #[serde(skip_serializing)]
    pub raw_source: Vec<u8>,
    pub source_mime: &'static str,
    pub source_redacted: bool,
    pub response_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Author {
    pub id: u64,
    pub name: String,
    pub display_name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Forum {
    pub id: u64,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Page {
    pub requested_page: u32,
    pub current_page: u64,
    pub page_size: u64,
    pub total_pages: u64,
    pub total_count: Option<u64>,
    pub has_more: bool,
    pub has_previous: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Emoji,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct MediaResource {
    pub kind: MediaKind,
    #[serde(skip_serializing)]
    pub url: String,
    pub mime: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_seconds: Option<u64>,
}
impl fmt::Debug for MediaResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaResource")
            .field("kind", &self.kind)
            .field("url", &"[redacted]")
            .finish()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fragment {
    Text {
        text: String,
    },
    Link {
        text: String,
        href: String,
    },
    Mention {
        text: String,
        user_id: u64,
    },
    Media {
        resource: MediaResource,
        label: String,
    },
    Unsupported {
        kind: u64,
        text: String,
        reason: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub parent_post_id: u64,
    pub author: Author,
    pub created_at: u64,
    pub floor: u64,
    pub reply_to_user_id: Option<u64>,
    pub fragments: Vec<Fragment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub id: u64,
    pub floor: u64,
    pub created_at: u64,
    pub author: Author,
    pub reply_count: u64,
    pub fragments: Vec<Fragment>,
    pub comments: Vec<Comment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadPage {
    pub thread_id: ThreadId,
    pub title: String,
    pub forum: Forum,
    pub author: Author,
    pub reply_count: u64,
    pub page: Page,
    pub posts: Vec<Post>,
    pub comments_included: bool,
    pub notes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentPage {
    pub thread_id: ThreadId,
    pub parent_post_id: u64,
    pub forum: Forum,
    pub title: String,
    pub page: Page,
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub parent_post: Option<Post>,
    #[serde(default)]
    pub selected_comment_id: Option<u64>,
    #[serde(default)]
    pub selected_comment_found: Option<bool>,
}

pub(crate) fn author(data: &[u8]) -> Result<Author> {
    let f = Fields::parse(data)?;
    Ok(Author {
        id: f.num(2),
        name: f.text(3)?,
        display_name: f.text(4)?,
    })
}
fn forum(data: &[u8]) -> Result<Forum> {
    let f = Fields::parse(data)?;
    Ok(Forum {
        id: f.num(1),
        name: f.text(2)?,
    })
}
pub(crate) fn page(data: &[u8], requested: u32) -> Result<Page> {
    let f = Fields::parse(data)?;
    Ok(Page {
        requested_page: requested,
        current_page: f.num(3),
        page_size: f.num(1),
        total_pages: f.num(5),
        total_count: f.num_opt(4),
        has_more: f.num(6) != 0,
        has_previous: f.num(7) != 0,
    })
}
fn media(f: &Fields<'_>, kind: MediaKind) -> Result<MediaResource> {
    let fields = match kind {
        MediaKind::Image => &[25, 9, 8, 4][..],
        MediaKind::Emoji => &[4, 3][..],
        MediaKind::Audio | MediaKind::Video => &[3, 4][..],
    };
    for &field in fields {
        let url = f.text(field)?;
        if let Ok(url) = crate::transport::normalize_media_url(&url) {
            return Ok(MediaResource {
                kind,
                url,
                mime: None,
                width: u32::try_from(f.num(18)).ok().filter(|n| *n > 0),
                height: u32::try_from(f.num(19)).ok().filter(|n| *n > 0),
                duration_seconds: Some(f.num(13)).filter(|n| *n > 0),
            });
        }
    }
    Err(Error::UrlPolicy)
}
pub(crate) fn fragments(fields: &Fields<'_>, field: u32) -> Result<Vec<Fragment>> {
    let mut out = Vec::new();
    for value in fields.all(field) {
        if out.len() >= 512 {
            return Err(Error::Limit);
        }
        let f = Fields::parse(value)?;
        let kind = f.num(1);
        let text = f.text(2)?;
        let fragment = match kind {
            0 | 9 | 18 | 27 | 40 => Fragment::Text { text },
            1 => Fragment::Link {
                text,
                href: f.text(3)?,
            },
            4 => Fragment::Mention {
                text,
                user_id: f.num(15),
            },
            2 | 3 | 5 | 10 | 11 | 20 => {
                let k = match kind {
                    2 | 11 => MediaKind::Emoji,
                    5 => MediaKind::Video,
                    10 => MediaKind::Audio,
                    _ => MediaKind::Image,
                };
                match media(&f,k){Ok(resource)=>Fragment::Media{resource,label:text},Err(_)=>Fragment::Unsupported{kind,text,reason:"Media URL/format needs a platform-specific resolver; content retained in redacted protobuf source".into()}}
            }
            _ => Fragment::Unsupported {
                kind,
                text,
                reason: "Unknown fragment; content retained in redacted protobuf source".into(),
            },
        };
        out.push(fragment);
    }
    Ok(out)
}
fn comment(data: &[u8], pid: u64, users: &BTreeMap<u64, Author>) -> Result<Comment> {
    let f = Fields::parse(data)?;
    let aid = f.num(4);
    let a = if let Some(raw) = f.bytes(7) {
        author(raw)?
    } else {
        users.get(&aid).cloned().unwrap_or(Author {
            id: aid,
            ..Default::default()
        })
    };
    let fragments = fragments(&f, 2)?;
    let reply_to_user_id = if matches!(fragments.first(),Some(Fragment::Text{text})if text=="回复 ")
    {
        fragments.get(1).and_then(|v| {
            if let Fragment::Mention { user_id, .. } = v {
                Some(*user_id)
            } else {
                None
            }
        })
    } else {
        None
    };
    Ok(Comment {
        id: f.num(1),
        parent_post_id: pid,
        created_at: f.num(3),
        floor: f.num(6),
        author: a,
        reply_to_user_id,
        fragments,
    })
}
fn post(data: &[u8], users: &BTreeMap<u64, Author>) -> Result<Post> {
    let f = Fields::parse(data)?;
    let aid = f.num(19);
    let a = if let Some(raw) = f.bytes(23) {
        author(raw)?
    } else {
        users.get(&aid).cloned().unwrap_or(Author {
            id: aid,
            ..Default::default()
        })
    };
    let mut comments = Vec::new();
    if let Some(raw) = f.bytes(15) {
        let s = Fields::parse(raw)?;
        for value in s.all(2) {
            if comments.len() >= 100 {
                return Err(Error::Limit);
            }
            comments.push(comment(value, f.num(1), users)?);
        }
    }
    Ok(Post {
        id: f.num(1),
        floor: f.num(3),
        created_at: f.num(4),
        author: a,
        reply_count: f.num(13),
        fragments: fragments(&f, 5)?,
        comments,
    })
}
pub(crate) fn response(data: &[u8]) -> Result<Fields<'_>> {
    let f = match Fields::parse(data) {
        Ok(fields) => fields,
        Err(error) => {
            let leading = data.iter().copied().find(|v| !v.is_ascii_whitespace());
            if matches!(leading, Some(b'<' | b'{')) {
                return Err(Error::Challenge);
            }
            return Err(error);
        }
    };
    if let Some(error) = f.bytes(1) {
        let e = Fields::parse(error)?;
        let code = e.num(1) as i64;
        if code != 0 {
            let message = e.text(2)?;
            return Err(
                if ["请先登录", "未登录", "需要登录"]
                    .iter()
                    .any(|word| message.contains(word))
                {
                    Error::LoginRequired
                } else {
                    Error::Platform(code)
                },
            );
        }
    }
    Fields::parse(f.bytes(2).ok_or(Error::Schema("missing protobuf data"))?)
}
pub(crate) fn parse_thread(raw: &[u8], expected: ThreadId, requested: u32) -> Result<ThreadPage> {
    let f = response(raw)?;
    let thread = Fields::parse(f.bytes(8).ok_or(Error::Schema("missing thread"))?)?;
    if thread.num(1) != expected.0 {
        return Err(Error::Schema("thread ID mismatch"));
    }
    let mut users = BTreeMap::new();
    for value in f.all(13) {
        if users.len() >= 512 {
            return Err(Error::Limit);
        }
        let a = author(value)?;
        users.insert(a.id, a);
    }
    let mut posts = Vec::new();
    for value in f.all(6) {
        if posts.len() >= 100 {
            return Err(Error::Limit);
        }
        posts.push(post(value, &users)?);
    }
    let p = page(f.bytes(3).ok_or(Error::Schema("missing page"))?, requested)?;
    let forum = forum(f.bytes(2).unwrap_or_default())?;
    if p.current_page == 0
        || (posts.is_empty()
            && p.total_count.is_some_and(|n| n > 0)
            && p.current_page <= p.total_pages)
    {
        return Err(Error::Schema("missing posts or page identity"));
    }
    let comments_included = posts.iter().any(|p| !p.comments.is_empty());
    Ok(ThreadPage{thread_id:expected,title:thread.text(3)?,forum,author:author(thread.bytes(18).unwrap_or_default())?,reply_count:thread.num(4),page:p,posts,comments_included,
        notes:vec!["Page retrieval does not request embedded floor comments; use the explicit comments API.".into(),"Unknown content fields remain available in the credential-redacted protobuf source.".into()]})
}
pub(crate) fn parse_comments(
    raw: &[u8],
    expected: ThreadId,
    pid: u64,
    requested: u32,
) -> Result<CommentPage> {
    parse_comment_context(raw, expected, Some(pid), None, requested)
}
pub(crate) fn parse_comment_context(
    raw: &[u8],
    expected: ThreadId,
    expected_parent: Option<u64>,
    selected: Option<u64>,
    requested: u32,
) -> Result<CommentPage> {
    let f = response(raw)?;
    let thread = Fields::parse(f.bytes(5).ok_or(Error::Schema("missing thread"))?)?;
    if thread.num(1) != expected.0 {
        return Err(Error::Schema("thread ID mismatch"));
    }
    let parent = Fields::parse(f.bytes(3).ok_or(Error::Schema("missing parent post"))?)?;
    let pid = parent.num(1);
    if pid == 0 || expected_parent.is_some_and(|expected| expected != pid) {
        return Err(Error::Schema("parent post ID mismatch"));
    }
    let mut comments = Vec::new();
    for value in f.all(4) {
        if comments.len() >= 100 {
            return Err(Error::Limit);
        }
        comments.push(comment(value, pid, &BTreeMap::new())?);
    }
    let p = page(
        f.bytes(1).ok_or(Error::Schema("missing comment page"))?,
        requested,
    )?;
    if p.current_page == 0 && !(comments.is_empty() && parent.num(13) == 0 && !p.has_more) {
        return Err(Error::Schema("missing comment page identity"));
    }
    let parent_post = post(
        f.bytes(3).ok_or(Error::Schema("missing parent post"))?,
        &BTreeMap::new(),
    )?;
    let selected_comment_found = selected.map(|id| {
        comments
            .iter()
            .any(|comment| comment.id == id && comment.parent_post_id == pid)
    });
    Ok(CommentPage {
        thread_id: expected,
        parent_post_id: pid,
        title: thread.text(3)?,
        forum: forum(f.bytes(6).unwrap_or_default())?,
        page: p,
        comments,
        parent_post: Some(parent_post),
        selected_comment_id: selected,
        selected_comment_found,
    })
}

/// Preserve protocol content for optional expansion without exposing account
/// fields from the server's large User/DataRes messages. Unknown content-fragment
/// fields are user content, while unrecognized server envelope fields are dropped.
pub(crate) fn redacted_source(raw: &[u8], comments: bool) -> Result<Vec<u8>> {
    use crate::wire::{self, Value};
    fn message(raw: &[u8], kind: u8) -> Result<Vec<u8>> {
        let fields = Fields::parse(raw)?;
        let mut out = Vec::new();
        for (field, value) in fields.fields() {
            let child = match (kind, field) {
                (0, 2) | (1, 6) => Some(2),            // DataRes forum
                (0, 3) | (1, 1) => Some(3),            // DataRes page
                (0, 6) | (1, 3) => Some(4),            // DataRes posts
                (0, 8) | (1, 5) => Some(5),            // DataRes thread
                (0, 13) => Some(6),                    // DataRes user list
                (1, 4) => Some(7),                     // floor subposts
                (4, 15) => Some(8),                    // post subpost wrapper
                (4, 23) | (5, 18) | (7, 7) => Some(6), // User fields
                (8, 2) => Some(7),
                _ => None,
            };
            if let Some(child) = child {
                if let Value::Bytes(bytes) = value {
                    wire::bytes(field, &message(bytes, child)?, &mut out);
                }
                continue;
            }
            let keep = match kind {
                2 => matches!(field, 1 | 2),
                3 => matches!(field, 1 | 3 | 4 | 5 | 6 | 7),
                4 => matches!(field, 1 | 3 | 4 | 5 | 13 | 19),
                5 => matches!(field, 1 | 3 | 4 | 56 | 142),
                6 => matches!(field, 2 | 3 | 4 | 5),
                7 => matches!(field, 1 | 2 | 3 | 4 | 5 | 6),
                _ => false,
            };
            if keep {
                wire::value(field, value, &mut out);
            }
        }
        Ok(out)
    }
    let envelope = Fields::parse(raw)?;
    let body = envelope
        .bytes(2)
        .ok_or(Error::Schema("missing source body"))?;
    let mut result = Vec::new();
    wire::bytes(
        2,
        &message(body, if comments { 1 } else { 0 })?,
        &mut result,
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire;
    fn nested(field: u32, bytes: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        wire::bytes(field, bytes, &mut b);
        b
    }
    #[test]
    fn challenge_and_error_responses_do_not_become_empty_threads() {
        assert!(matches!(
            parse_thread(b"<html>challenge</html>", ThreadId(1), 1),
            Err(Error::Challenge)
        ));
        let mut error = Vec::new();
        wire::number(1, 123, &mut error);
        assert!(matches!(
            parse_thread(&nested(1, &error), ThreadId(1), 1),
            Err(Error::Platform(123))
        ));
        assert!(parse_thread(&[], ThreadId(1), 1).is_err());
        for size in [56, 119] {
            let mut error = Vec::new();
            wire::number(1, 123, &mut error);
            wire::bytes(2, &vec![b'x'; size], &mut error);
            assert!(matches!(
                parse_thread(&nested(1, &error), ThreadId(1), 1),
                Err(Error::Platform(123))
            ));
        }
    }
    #[test]
    fn comment_page_requires_identity_but_allows_server_clamping() {
        fn fixture(current_page: u64) -> Vec<u8> {
            let mut pagination = Vec::new();
            wire::number(1, 30, &mut pagination);
            wire::number(3, current_page, &mut pagination);
            wire::number(4, 1, &mut pagination);
            wire::number(5, 1, &mut pagination);
            let mut thread = Vec::new();
            wire::number(1, 7, &mut thread);
            let mut parent = Vec::new();
            wire::number(1, 70, &mut parent);
            let mut reply = Vec::new();
            wire::number(1, 71, &mut reply);
            wire::number(3, 123, &mut reply);
            let mut data = nested(1, &pagination);
            wire::bytes(3, &parent, &mut data);
            wire::bytes(4, &reply, &mut data);
            wire::bytes(5, &thread, &mut data);
            nested(2, &data)
        }
        assert!(matches!(
            parse_comments(&fixture(0), ThreadId(7), 70, 2),
            Err(Error::Schema("missing comment page identity"))
        ));
        // A request beyond the last page may legitimately return that last page.
        let result = parse_comments(&fixture(1), ThreadId(7), 70, 2).unwrap();
        assert_eq!(result.page.requested_page, 2);
        assert_eq!(result.page.current_page, 1);
        assert_eq!(result.comments.len(), 1);
        assert_eq!(result.comments[0].id, 71);
        assert_eq!(result.parent_post.as_ref().unwrap().id, 70);
        let selected =
            parse_comment_context(&fixture(1), ThreadId(7), Some(70), Some(71), 1).unwrap();
        assert_eq!(selected.selected_comment_found, Some(true));
        let missing =
            parse_comment_context(&fixture(1), ThreadId(7), Some(70), Some(99), 1).unwrap();
        assert_eq!(missing.selected_comment_found, Some(false));
        assert_eq!(missing.parent_post.as_ref().unwrap().id, 70);
        assert!(parse_comment_context(&fixture(1), ThreadId(7), Some(700), Some(71), 1).is_err());
    }
    #[test]
    fn a_verified_empty_parent_retains_body_without_inventing_a_comment_page() {
        let mut parent = Vec::new();
        wire::number(1, 70, &mut parent);
        let mut text = Vec::new();
        wire::number(1, 0, &mut text);
        wire::bytes(2, b"parent with no replies", &mut text);
        wire::bytes(5, &text, &mut parent);
        let mut thread = Vec::new();
        wire::number(1, 7, &mut thread);
        let mut pagination = Vec::new();
        wire::number(1, 30, &mut pagination);
        let mut data = nested(1, &pagination);
        wire::bytes(3, &parent, &mut data);
        wire::bytes(5, &thread, &mut data);
        let raw = nested(2, &data);
        let result = parse_comments(&raw, ThreadId(7), 70, 1).unwrap();
        assert_eq!(result.page.current_page, 0);
        assert!(result.comments.is_empty());
        assert!(
            matches!(&result.parent_post.unwrap().fragments[0],Fragment::Text{text}if text=="parent with no replies")
        );
        assert_eq!(
            parse_comment_context(&raw, ThreadId(7), Some(70), Some(71), 1)
                .unwrap()
                .selected_comment_found,
            Some(false)
        );
        assert!(parse_comments(&raw, ThreadId(7), 71, 1).is_err());
    }
    #[test]
    fn mixed_fragments_keep_images_unknowns_and_escaped_text() {
        let mut list = Vec::new();
        for (kind, text, url) in [
            (0, "<script>hello", ""),
            (3, "图", "https://imgsrc.baidu.com/forum/a.jpg"),
            (99, "new type", ""),
        ] {
            let mut f = Vec::new();
            wire::number(1, kind, &mut f);
            wire::bytes(2, text.as_bytes(), &mut f);
            wire::bytes(25, url.as_bytes(), &mut f);
            wire::bytes(5, &f, &mut list);
        }
        let fragments = fragments(&Fields::parse(&list).unwrap(), 5).unwrap();
        assert_eq!(fragments.len(), 3);
        assert!(matches!(&fragments[0],Fragment::Text{text}if text.contains("<script>")));
        assert!(
            matches!(&fragments[1],Fragment::Media{resource,..}if resource.kind==MediaKind::Image)
        );
        assert!(matches!(
            &fragments[2],
            Fragment::Unsupported { kind: 99, .. }
        ));
        assert!(
            !serde_json::to_string(&fragments)
                .unwrap()
                .contains("imgsrc.baidu.com")
        );
    }
    #[test]
    fn source_excludes_user_credentials_and_unknown_server_fields() {
        let mut user = Vec::new();
        wire::number(2, 1, &mut user);
        wire::bytes(3, b"reader", &mut user);
        wire::bytes(29, b"secret-bduss", &mut user);
        wire::bytes(999, b"other-secret", &mut user);
        let mut data = nested(13, &user);
        wire::bytes(999, b"server-token", &mut data);
        let raw = nested(2, &data);
        let clean = redacted_source(&raw, false).unwrap();
        assert!(!clean.windows(6).any(|v| v == b"secret"));
        assert!(clean.windows(6).any(|v| v == b"reader"));
    }
}
