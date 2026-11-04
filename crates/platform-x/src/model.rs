// SPDX-License-Identifier: MIT
use crate::{Error, Result, id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub screen_name: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub followers_count: Option<u64>,
    pub following_count: Option<u64>,
    pub posts_count: Option<u64>,
    pub protected: Option<bool>,
    pub verified: Option<bool>,
    pub profile_image_url: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TweetCounts {
    pub reply: Option<u64>,
    pub retweet: Option<u64>,
    pub like: Option<u64>,
    pub quote: Option<u64>,
    pub bookmark: Option<u64>,
    pub view: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
    AnimatedGif,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoVariant {
    pub url: String,
    pub content_type: String,
    pub bitrate: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Media {
    pub kind: MediaKind,
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt_text: Option<String>,
    pub variants: Vec<VideoVariant>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tweet {
    pub id: String,
    pub author: User,
    pub text: String,
    pub created_at: Option<String>,
    pub lang: Option<String>,
    pub conversation_id: Option<String>,
    pub in_reply_to_tweet_id: Option<String>,
    pub quoted_tweet_id: Option<String>,
    pub retweeted_tweet_id: Option<String>,
    pub counts: TweetCounts,
    pub media: Vec<Media>,
    pub unexpanded: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelinePage {
    #[serde(default)]
    pub timeline_kind: String,
    pub items: Vec<Tweet>,
    pub pinned: Vec<Tweet>,
    pub next_cursor: Option<String>,
    pub server_next_cursor: Option<String>,
    pub has_more: Option<bool>,
    pub truncated: bool,
    pub pagination_note: Option<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn check_errors(v: &Value) -> Result<()> {
    if let Some(errors) = v.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            for e in errors {
                match number(&e["code"]) {
                    Some(32 | 89 | 215 | 239) => return Err(Error::LoginRequired),
                    Some(326 | 353) => return Err(Error::VerificationRequired),
                    Some(88) => return Err(Error::RateLimited { reset_at: None }),
                    Some(63 | 64 | 179) => return Err(Error::PermissionDenied),
                    Some(34 | 144) => return Err(Error::NotFound),
                    _ => (),
                }
            }
            return Err(Error::ProtocolChanged);
        }
    }
    Ok(())
}
fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}
fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}
fn pick<'a>(v: &'a Value, paths: &[&str]) -> Option<&'a Value> {
    paths
        .iter()
        .find_map(|p| v.pointer(p).filter(|x| !x.is_null()))
}
fn s(v: &Value, paths: &[&str]) -> Option<String> {
    pick(v, paths).and_then(text)
}
fn n(v: &Value, paths: &[&str]) -> Option<u64> {
    pick(v, paths).and_then(number)
}
fn b(v: &Value, paths: &[&str]) -> Option<bool> {
    pick(v, paths).and_then(Value::as_bool)
}
pub(crate) fn redirect_screen_name(v: &Value) -> Result<String> {
    s(
        v,
        &[
            "/data/user_result_by_rest_id/result/core/screen_name",
            "/data/user_result_by_rest_id/result/user/core/screen_name",
        ],
    )
    .ok_or(Error::ProtocolChanged)
}
pub(crate) fn user_response(v: &Value) -> Result<User> {
    check_errors(v)?;
    let r = pick(
        v,
        &[
            "/data/user/result",
            "/data/user_result_by_screen_name/result",
            "/data/user_result_by_rest_id/result",
        ],
    )
    .ok_or(Error::ProtocolChanged)?;
    user(r)
}
fn user(v: &Value) -> Result<User> {
    if v["__typename"] == "UserUnavailable" {
        return Err(Error::PermissionDenied);
    }
    let v = v.get("user").unwrap_or(v);
    let identity = s(v, &["/rest_id", "/id_str"]).ok_or(Error::ProtocolChanged)?;
    id(&identity).map_err(|_| Error::ProtocolChanged)?;
    let profile = v.get("profile").unwrap_or(v);
    let screen_name = s(v, &["/core/screen_name", "/legacy/screen_name"])
        .or_else(|| s(profile, &["/core/screen_name"]))
        .ok_or(Error::ProtocolChanged)?;
    crate::screen_name(&screen_name).map_err(|_| Error::ProtocolChanged)?;
    let name = s(profile, &["/core/name", "/legacy/name"])
        .or_else(|| s(v, &["/core/name", "/legacy/name"]))
        .ok_or(Error::ProtocolChanged)?;
    Ok(User {
        id: identity,
        screen_name,
        name,
        description: s(
            profile,
            &["/profile_bio/description", "/legacy/description"],
        ),
        created_at: s(profile, &["/core/created_at", "/legacy/created_at"])
            .or_else(|| n(profile, &["/core/created_at_ms"]).map(|n| format!("unix_ms:{n}"))),
        followers_count: n(
            profile,
            &["/relationship_counts/followers", "/legacy/followers_count"],
        ),
        following_count: n(
            profile,
            &["/relationship_counts/following", "/legacy/friends_count"],
        ),
        posts_count: n(profile, &["/tweet_counts/tweets", "/legacy/statuses_count"]),
        protected: b(profile, &["/privacy/protected", "/legacy/protected"]),
        verified: b(
            profile,
            &[
                "/verification/is_blue_verified",
                "/is_blue_verified",
                "/legacy/verified",
            ],
        ),
        profile_image_url: s(
            profile,
            &["/avatar/image_url", "/legacy/profile_image_url_https"],
        )
        .filter(|u| media_url(u)),
    })
}
pub(crate) fn media_url(url: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("https")
        && matches!(uri.host(), Some("pbs.twimg.com" | "video.twimg.com"))
        && uri.port_u16().is_none_or(|p| p == 443)
        && !url.contains(['@', '\\', '#'])
        && !url.bytes().any(|b| b.is_ascii_control())
        && url.len() <= 8192
}

fn unwrap_tweet(mut v: &Value) -> Result<&Value> {
    for _ in 0..4 {
        match v["__typename"].as_str() {
            Some("TweetTombstone" | "TweetUnavailable") => return Err(Error::NotFound),
            Some("TweetWithVisibilityResults") => {
                v = v.get("tweet").ok_or(Error::ProtocolChanged)?
            }
            _ => return Ok(v),
        }
    }
    Err(Error::ProtocolChanged)
}
fn tweet(v: &Value) -> Result<Tweet> {
    let v = unwrap_tweet(v)?;
    let identity = s(v, &["/rest_id", "/legacy/id_str"]).ok_or(Error::ProtocolChanged)?;
    id(&identity).map_err(|_| Error::ProtocolChanged)?;
    let author_raw = pick(
        v,
        &[
            "/core/user_results/result",
            "/author_results/result",
            "/author/result",
        ],
    )
    .ok_or(Error::ProtocolChanged)?;
    let author = user(author_raw)?;
    let text = s(
        v,
        &[
            "/note_tweet/note_tweet_results/result/text",
            "/note_tweet_results/result/text",
            "/details/full_text",
            "/legacy/full_text",
            "/text/full_text",
            "/text/text",
        ],
    )
    .ok_or(Error::ProtocolChanged)?;
    if text.len() > 128 * 1024 {
        return Err(Error::Limit);
    }
    let mut media = Vec::new();
    let mut unexpanded = Vec::new();
    let list = pick(
        v,
        &[
            "/legacy/extended_entities/media",
            "/media_entities",
            "/media_entities2",
        ],
    )
    .and_then(Value::as_array);
    if let Some(list) = list {
        if list.len() > 32 {
            return Err(Error::Limit);
        }
        for m in list {
            let typ = s(m, &["/type", "/media_key/type"]).unwrap_or_default();
            let kind = match typ.as_str() {
                "photo" | "Photo" => MediaKind::Photo,
                "video" | "Video" => MediaKind::Video,
                "animated_gif" | "AnimatedGif" => MediaKind::AnimatedGif,
                _ => {
                    unexpanded.push("unknown_media".into());
                    continue;
                }
            };
            let Some(url) =
                s(m, &["/media_url_https", "/image/url", "/image_url"]).filter(|u| media_url(u))
            else {
                unexpanded.push("media_locator_unavailable".into());
                continue;
            };
            let mut variants = Vec::new();
            if let Some(vs) = pick(m, &["/video_info/variants", "/video/video_info/variants"])
                .and_then(Value::as_array)
            {
                for variant in vs.iter().take(16) {
                    if let (Some(url), Some(content_type)) = (
                        s(variant, &["/url"]).filter(|u| media_url(u)),
                        s(variant, &["/content_type"]),
                    ) {
                        if matches!(
                            content_type.as_str(),
                            "video/mp4" | "application/x-mpegURL" | "application/vnd.apple.mpegurl"
                        ) {
                            variants.push(VideoVariant {
                                url,
                                content_type,
                                bitrate: n(variant, &["/bitrate"]),
                            });
                        }
                    }
                }
            }
            media.push(Media {
                kind,
                url,
                width: n(m, &["/original_info/width", "/sizes/large/w", "/width"])
                    .and_then(|n| n.try_into().ok()),
                height: n(m, &["/original_info/height", "/sizes/large/h", "/height"])
                    .and_then(|n| n.try_into().ok()),
                alt_text: s(m, &["/ext_alt_text", "/alt_text"]),
                variants,
            });
        }
    }
    for field in [
        "card",
        "article",
        "quoted_status_result",
        "retweeted_status_result",
        "retweeted_tweet_results",
    ] {
        if v.get(field).is_some_and(|x| !x.is_null()) {
            unexpanded.push(field.to_owned());
        }
    }
    Ok(Tweet {
        id: identity,
        author,
        text,
        created_at: s(v, &["/legacy/created_at", "/core/created_at"])
            .or_else(|| n(v, &["/details/created_at_ms"]).map(|v| format!("unix_ms:{v}"))),
        lang: s(v, &["/legacy/lang", "/language/lang", "/details/lang"]),
        conversation_id: s(
            v,
            &[
                "/legacy/conversation_id_str",
                "/conversation_id_str",
                "/details/conversation_id",
            ],
        ),
        in_reply_to_tweet_id: s(
            v,
            &[
                "/legacy/in_reply_to_status_id_str",
                "/in_reply_to_status_id_str",
                "/details/in_reply_to_status_id",
            ],
        ),
        quoted_tweet_id: s(
            v,
            &[
                "/legacy/quoted_status_id_str",
                "/quoted_status_result/result/rest_id",
            ],
        ),
        retweeted_tweet_id: s(
            v,
            &[
                "/legacy/retweeted_status_result/result/rest_id",
                "/retweeted_tweet_results/rest_id",
                "/retweeted_status_result/result/rest_id",
            ],
        ),
        counts: TweetCounts {
            reply: n(v, &["/legacy/reply_count", "/counts/reply_count"]),
            retweet: n(v, &["/legacy/retweet_count", "/counts/retweet_count"]),
            like: n(v, &["/legacy/favorite_count", "/counts/favorite_count"]),
            quote: n(v, &["/legacy/quote_count", "/counts/quote_count"]),
            bookmark: n(v, &["/legacy/bookmark_count", "/counts/bookmark_count"]),
            view: n(v, &["/views/count", "/view_count_info/count"]),
        },
        media,
        unexpanded,
    })
}
fn candidates<'a>(
    v: &'a Value,
    out: &mut Vec<&'a Value>,
    depth: usize,
    visited: &mut usize,
) -> Result<()> {
    *visited += 1;
    if depth > 48 || *visited > 40_000 {
        return Err(Error::Limit);
    }
    let wrapper = v["__typename"] == "TweetWithVisibilityResults" && v.get("tweet").is_some();
    let authored = v.get("rest_id").is_some()
        && [
            "/core/user_results",
            "/author_results/result",
            "/author/result",
        ]
        .iter()
        .any(|p| v.pointer(p).is_some());
    if wrapper || authored {
        out.push(v);
        return Ok(());
    }
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if !matches!(
                    k.as_str(),
                    "quoted_status_result" | "retweeted_status_result" | "retweeted_tweet_results"
                ) {
                    candidates(x, out, depth + 1, visited)?;
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                candidates(x, out, depth + 1, visited)?;
            }
        }
        _ => (),
    }
    Ok(())
}
pub(crate) fn tweet_response(v: &Value, expected: &str) -> Result<Tweet> {
    check_errors(v)?;
    let mut found = Vec::new();
    candidates(v, &mut found, 0, &mut 0)?;
    for raw in found {
        if raw["rest_id"].as_str() == Some(expected)
            || raw.pointer("/tweet/rest_id").and_then(Value::as_str) == Some(expected)
        {
            return tweet(raw);
        }
    }
    if v.pointer("/data/tweet_result_by_rest_id/result")
        .is_some_and(Value::is_null)
    {
        return Err(Error::NotFound);
    }
    Err(Error::ProtocolChanged)
}
fn instructions(v: &Value) -> Option<&Vec<Value>> {
    for path in [
        "/data/user/result/timeline_v2/timeline/instructions",
        "/data/user/result/timeline/timeline/instructions",
        "/data/user_result_by_screen_name/result/profile_user_originals_timeline/timeline/instructions",
        "/data/user_result_by_screen_name/result/user/profile_user_originals_timeline/timeline/instructions",
    ] {
        if let Some(list) = v.pointer(path).and_then(Value::as_array) {
            return Some(list);
        }
    }
    None
}
pub(crate) fn timeline_response(
    v: &Value,
    _user_id: &str,
    previous: Option<&str>,
    limit: usize,
) -> Result<TimelinePage> {
    check_errors(v)?;
    let ins = instructions(v).ok_or(Error::ProtocolChanged)?;
    timeline_instructions(
        ins,
        previous,
        limit,
        if v.pointer("/data/user_result_by_screen_name").is_some() {
            "original_posts"
        } else {
            "posts"
        },
    )
}
pub(crate) fn home_response(
    v: &Value,
    recommended: bool,
    previous: Option<&str>,
    limit: usize,
) -> Result<TimelinePage> {
    check_errors(v)?;
    let modern = if recommended {
        "/data/home_timeline/home_timeline_urt/instructions"
    } else {
        "/data/home_latest_timeline/home_timeline_urt/instructions"
    };
    let ins = [modern, "/data/home/home_timeline_urt/instructions"]
        .iter()
        .find_map(|path| v.pointer(path).and_then(Value::as_array))
        .ok_or(Error::ProtocolChanged)?;
    timeline_instructions(
        ins,
        previous,
        limit,
        if recommended { "for_you" } else { "following" },
    )
}
fn timeline_instructions(
    ins: &[Value],
    previous: Option<&str>,
    limit: usize,
    kind: &str,
) -> Result<TimelinePage> {
    if ins.len() > 128 || !(1..=100).contains(&limit) {
        return Err(Error::Limit);
    }
    let mut page = TimelinePage {
        timeline_kind: kind.into(),
        items: Vec::new(),
        pinned: Vec::new(),
        next_cursor: None,
        server_next_cursor: None,
        has_more: None,
        truncated: false,
        pagination_note: None,
        warnings: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut ended = false;
    for instruction in ins {
        let typ = instruction
            .get("type")
            .or_else(|| instruction.get("__typename"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if typ == "TimelineTerminateTimeline" && instruction["direction"] == "Bottom" {
            ended = true;
        }
        let is_pin = typ == "TimelinePinEntry";
        let entries: Vec<&Value> = if let Some(a) = instruction["entries"].as_array() {
            a.iter().collect()
        } else if let Some(a) = instruction.get("module_items").and_then(Value::as_array) {
            a.iter().collect()
        } else {
            instruction.get("entry").into_iter().collect()
        };
        if !matches!(
            typ,
            "TimelineAddEntries"
                | "TimelinePinEntry"
                | "TimelineReplaceEntry"
                | "TimelineAddToModule"
                | "TimelineTerminateTimeline"
                | "TimelineClearCache"
                | "TimelineMarkEntriesUnread"
        ) {
            page.warnings.push("unexpanded_instruction".into());
        }
        if entries.len() > 256 {
            return Err(Error::Limit);
        }
        for entry in entries {
            if pick(entry, &["/content/cursorType", "/content/cursor_type"]).and_then(Value::as_str)
                == Some("Bottom")
            {
                page.server_next_cursor = s(entry, &["/content/value"]).filter(|s| s.len() <= 4096);
                continue;
            }
            if [
                "/content/itemContent/promotedMetadata",
                "/content/content/promotedMetadata",
                "/item/itemContent/promotedMetadata",
                "/item/content/promotedMetadata",
            ]
            .iter()
            .any(|path| entry.pointer(path).is_some_and(|v| !v.is_null()))
            {
                continue;
            }
            let mut list = Vec::new();
            candidates(entry, &mut list, 0, &mut 0)?;
            for raw in list {
                match tweet(raw) {
                    Ok(t) if seen.insert(t.id.clone()) => {
                        if is_pin {
                            page.pinned.push(t);
                        } else {
                            page.items.push(t);
                        }
                    }
                    Ok(_) => (),
                    Err(Error::NotFound | Error::PermissionDenied) => {
                        page.warnings.push("unavailable_item".into())
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
    if page.items.len() > limit {
        page.items.truncate(limit);
        page.truncated = true;
        page.pagination_note = Some("server_page_exceeds_limit_no_safe_continuation".into());
    }
    page.has_more = if ended {
        Some(false)
    } else if page.server_next_cursor.is_some() {
        Some(true)
    } else {
        None
    };
    if !page.truncated && !ended && !page.items.is_empty() {
        page.next_cursor = page.server_next_cursor.clone();
    }
    if previous.is_some() && previous == page.next_cursor.as_deref() {
        page.pagination_note =
            Some("cursor_reused_deduplicate_ids_stop_on_empty_or_no_new_items".into());
    }
    if page.items.is_empty() {
        page.next_cursor = None;
    }
    Ok(page)
}
