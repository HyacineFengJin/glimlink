//! Notification links are evidence, never arbitrary request URLs.
//!
//! Reply business 1 coordinates: bilibili-plugins/bilibili-api-collect,
//! docs/message/msg.md (subject/root/source/target). Like reply item_id:
//! https://www.cnblogs.com/NeonCoding/p/18187805 (author's implementation).
//! Root-video @ notifications: the author's output/at_raw.json in
//! https://github.com/naaammme/bilibili-tools; its native_uri explicitly binds
//! source_id to comment_root_id for business=1, root_id=target_id=0.
//! Web anchors: https://greasyfork.org/scripts/446823/code (author's code).
//! seek_rpid/seek_root_reply: docs/comment/list.md and the author of
//! https://greasyfork.org/scripts/403348/code, jump(). We use the current WBI
//! endpoint, not that script's old unsigned endpoint or deprecated reply/info.
use crate::{
    Client, CommentRead, CommentReadReplies, CommentSource, Error, Notification, NotificationKind,
    Result, VideoId, VideoInfo, comment_read::parse_comment,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLocationStatus {
    Video,
    /// Coordinates have source evidence but have not been read back yet.
    CommentCandidate,
    CommentVerified,
    Unresolved,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationCommentLocator {
    pub oid: Option<u64>,
    pub rpid: u64,
    /// The actual root rpid, including when the selected comment is the root.
    pub root: Option<u64>,
    /// Zero means a root comment; None means the relationship is unknown.
    pub parent: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationLocation {
    pub status: NotificationLocationStatus,
    pub video: Option<VideoId>,
    pub comment: Option<NotificationCommentLocator>,
    pub evidence: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationContextStatus {
    VideoOnly,
    Found,
    Unresolved,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationContext {
    pub status: NotificationContextStatus,
    pub location: NotificationLocation,
    pub video: Option<VideoInfo>,
    pub root_comment: Option<CommentRead>,
    /// Only the exact selected rpid. Never substituted with its root.
    pub selected_comment: Option<CommentRead>,
    pub replies: Vec<CommentRead>,
    pub next_page: Option<u32>,
    pub has_more: Option<bool>,
    pub truncated: bool,
    pub reason: Option<String>,
    pub media_downloaded: bool,
}

fn positive(id: u64) -> bool {
    id > 0 && id <= i64::MAX as u64
}
fn decimal(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 19 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|id| positive(*id))
}

#[derive(Default)]
struct Anchors {
    selected: Option<u64>,
    root: Option<u64>,
}

fn video_link(raw: &str) -> Result<(VideoId, Anchors)> {
    if raw.len() > 4096 || raw.bytes().any(|b| b <= 32 || b == 127) {
        return Err(Error::UrlPolicy);
    }
    let (base, fragment) = raw
        .split_once('#')
        .map_or((raw, None), |(a, b)| (a, Some(b)));
    let uri = crate::http::parse_url(base, crate::http::UrlRole::Video)?;
    let video: VideoId = base.parse()?;
    let mut root = None;
    let mut secondary = None;
    let mut seen = BTreeSet::new();
    for pair in uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if matches!(key, "comment_root_id" | "comment_secondary_id") {
            if !seen.insert(key) {
                return Err(Error::Schema("ambiguous notification anchor"));
            }
            let id = decimal(value).ok_or(Error::Schema("notification anchor id"))?;
            match key {
                "comment_root_id" => root = Some(id),
                _ => secondary = Some(id),
            }
        }
    }
    if secondary.is_some() && root.is_none() {
        return Err(Error::Schema("notification secondary anchor without root"));
    }
    let hash = match fragment.filter(|f| !f.is_empty()) {
        None => None,
        Some(f) => Some(
            f.strip_prefix("reply")
                .and_then(decimal)
                .ok_or(Error::Schema("unrecognized notification fragment"))?,
        ),
    };
    let query_selected = secondary.or(root);
    if hash.zip(query_selected).is_some_and(|(a, b)| a != b) {
        return Err(Error::Schema("conflicting notification anchors"));
    }
    Ok((
        video,
        Anchors {
            selected: hash.or(query_selected),
            root,
        },
    ))
}

fn unresolved(mut location: NotificationLocation, reason: &'static str) -> NotificationLocation {
    location.status = NotificationLocationStatus::Unresolved;
    location.comment = None;
    location.reason = Some(reason.into());
    location
}

/// Pure projection of source-supported semantics. No HTTP or media fetches.
/// Business IDs are interpreted within each notification kind, not globally.
pub fn notification_location(notification: &Notification) -> NotificationLocation {
    let mut result = NotificationLocation {
        status: NotificationLocationStatus::Unsupported,
        video: None,
        comment: None,
        evidence: Vec::new(),
        reason: None,
    };
    let target = &notification.target;
    let Some(link) = target.uri.as_deref() else {
        return unresolved(result, "Notification has no supported video link.");
    };
    let (video, anchors) = match video_link(link) {
        Ok(value) => value,
        Err(_) => {
            result.reason = Some("Only strict HTTPS Bilibili video links with unambiguous comment anchors are supported.".into());
            return result;
        }
    };
    result.video = Some(video);
    result.evidence.push("strict_video_uri".into());
    let content_type = target.content_type.as_deref();
    let mut locator = match notification.kind {
        NotificationKind::Reply
            if target.business_id == Some(1) && matches!(content_type, Some("reply" | "video")) =>
        {
            let Some(rpid) = target.source_id.filter(|id| positive(*id)) else {
                return unresolved(
                    result,
                    "Video reply notification lacks its source comment ID.",
                );
            };
            let Some(oid) = target.subject_id.filter(|id| positive(*id)) else {
                return unresolved(
                    result,
                    "Video reply notification lacks its video comment subject.",
                );
            };
            let (root, parent) = match target.root_id {
                Some(0) if target.target_id.is_none_or(|id| id == 0) => (Some(rpid), Some(0)),
                Some(id) if positive(id) => (Some(id), target.target_id.filter(|id| positive(*id))),
                _ => {
                    return unresolved(
                        result,
                        "Video reply relationship is missing or contradictory.",
                    );
                }
            };
            result
                .evidence
                .push("reply_business_1_subject_root_source_target".into());
            NotificationCommentLocator {
                oid: Some(oid),
                rpid,
                root,
                parent,
            }
        }
        NotificationKind::Like if content_type == Some("reply") => {
            let Some(rpid) = target.item_id.filter(|id| positive(*id)) else {
                return unresolved(result, "Reply-like notification lacks its comment item ID.");
            };
            result.evidence.push("like_reply_item_id".into());
            NotificationCommentLocator {
                oid: None,
                rpid,
                root: None,
                parent: None,
            }
        }
        NotificationKind::At
            if target.business_id == Some(1)
                && content_type == Some("reply")
                && target.root_id == Some(0)
                && target.target_id == Some(0) =>
        {
            let Some(rpid) = target.source_id.filter(|id| positive(*id)) else {
                return unresolved(result, "Root mention lacks its source comment ID.");
            };
            let Some(oid) = target.subject_id.filter(|id| positive(*id)) else {
                return unresolved(result, "Root mention lacks its video comment subject.");
            };
            result
                .evidence
                .push("at_business_1_root_native_uri_source_mapping".into());
            NotificationCommentLocator {
                oid: Some(oid),
                rpid,
                root: Some(rpid),
                parent: Some(0),
            }
        }
        NotificationKind::At
            if target.business_id == Some(3)
                && content_type == Some("video")
                && anchors.selected.is_none() =>
        {
            result.status = NotificationLocationStatus::Video;
            return result;
        }
        NotificationKind::At
            if anchors.selected.is_some()
                && matches!(
                    (target.business_id, content_type),
                    (Some(1), Some("reply"))
                        | (Some(3), Some("video"))
                        | (None, Some("reply" | "video"))
                ) =>
        {
            NotificationCommentLocator {
                oid: None,
                rpid: anchors.selected.unwrap(),
                root: anchors.root,
                parent: None,
            }
        }
        _ => {
            return unresolved(
                result,
                "Video is known, but this notification business has no verified comment coordinate mapping.",
            );
        }
    };
    if anchors.selected.is_some_and(|id| id != locator.rpid)
        || anchors.root.zip(locator.root).is_some_and(|(a, b)| a != b)
    {
        return unresolved(
            result,
            "Notification fields and explicit comment anchors disagree.",
        );
    }
    if anchors.selected.is_some() {
        result.evidence.push("explicit_web_comment_anchor".into());
    }
    locator.root = locator.root.or(anchors.root);
    result.status = NotificationLocationStatus::CommentCandidate;
    result.comment = Some(locator);
    result
}

impl NotificationContext {
    fn new(location: NotificationLocation) -> Self {
        Self {
            status: match location.status {
                NotificationLocationStatus::Video => NotificationContextStatus::VideoOnly,
                NotificationLocationStatus::Unsupported => NotificationContextStatus::Unsupported,
                _ => NotificationContextStatus::Unresolved,
            },
            reason: location.reason.clone(),
            location,
            video: None,
            root_comment: None,
            selected_comment: None,
            replies: Vec::new(),
            next_page: None,
            has_more: None,
            truncated: false,
            media_downloaded: false,
        }
    }
    fn unavailable(&mut self) {
        self.status = NotificationContextStatus::Unavailable;
        self.selected_comment = None;
        self.reason = Some("Comment is unavailable (missing, deleted or inaccessible); no substitute comment was selected.".into());
    }
    fn accept_selected(&mut self, selected: CommentRead) -> Result<()> {
        let expected = self
            .location
            .comment
            .as_mut()
            .ok_or(Error::Schema("notification comment locator"))?;
        let root = self
            .root_comment
            .as_ref()
            .ok_or(Error::Schema("notification root context"))?;
        check_selected(&selected, root, expected)?;
        expected.oid = Some(selected.oid);
        expected.root = Some(root.rpid);
        expected.parent = selected.parent;
        self.location.status = NotificationLocationStatus::CommentVerified;
        self.location.evidence.push("exact_comment_readback".into());
        self.status = NotificationContextStatus::Found;
        self.reason = None;
        self.selected_comment = Some(selected);
        Ok(())
    }
}

fn check_root(root: &CommentRead, aid: u64, expected: &NotificationCommentLocator) -> Result<()> {
    if root.oid != aid
        || root.root != Some(0)
        || root.parent != Some(0)
        || expected.root.is_some_and(|id| id != root.rpid)
    {
        return Err(Error::Schema("notification root mismatch"));
    }
    Ok(())
}

fn check_selected(
    selected: &CommentRead,
    root: &CommentRead,
    expected: &NotificationCommentLocator,
) -> Result<()> {
    if selected.rpid != expected.rpid
        || selected.oid != root.oid
        || expected.oid.is_some_and(|id| id != selected.oid)
        || expected.root.is_some_and(|id| id != root.rpid)
        || expected
            .parent
            .is_some_and(|id| Some(id) != selected.parent)
    {
        return Err(Error::Schema("notification selected comment mismatch"));
    }
    if selected.rpid == root.rpid {
        if selected.root != Some(0) || selected.parent != Some(0) {
            return Err(Error::Schema("notification selected root relationship"));
        }
    } else if selected.root != Some(root.rpid) || selected.parent.is_none_or(|id| !positive(id)) {
        return Err(Error::Schema("notification selected child relationship"));
    }
    Ok(())
}

fn apply_reply_page(context: &mut NotificationContext, page: CommentReadReplies) -> Result<()> {
    let expected = context
        .location
        .comment
        .as_ref()
        .ok_or(Error::Schema("notification locator"))?;
    check_root(&page.root_comment, page.aid, expected)?;
    let selected = if page.root_comment.rpid == expected.rpid {
        Some(page.root_comment.clone())
    } else {
        page.items
            .iter()
            .find(|item| item.rpid == expected.rpid)
            .cloned()
    };
    context.root_comment = Some(page.root_comment);
    context.replies = page.items;
    context.next_page = page.next_page;
    context.has_more = page.has_more;
    context.truncated = page.truncated;
    if let Some(selected) = selected {
        context.accept_selected(selected)?;
    }
    Ok(())
}

/// Apply only the exact sought root and child. An unrelated normal page is not
/// evidence that the requested comment exists or that it has been deleted.
fn apply_seek(context: &mut NotificationContext, data: Value, aid: u64, limit: u32) -> Result<()> {
    if !data.is_object() {
        return Err(Error::Schema("notification seek data"));
    }
    for key in ["need_captcha", "need_verify"] {
        match data.get(key) {
            Some(Value::Bool(true)) => return Err(Error::RiskControl(0)),
            Some(v) if v.as_u64() == Some(1) => return Err(Error::RiskControl(0)),
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            Some(v) if v.as_u64() == Some(0) => {}
            _ => return Err(Error::Schema("notification seek challenge flag")),
        }
    }
    if data.get("v_voucher").is_some() {
        return Err(Error::RiskControl(0));
    }
    let expected = context
        .location
        .comment
        .as_ref()
        .ok_or(Error::Schema("notification locator"))?
        .clone();
    let raw = data.get("seek_root_reply").filter(|v| !v.is_null());
    let Some(raw) = raw else {
        // seek_root_reply can be absent when a root is present in regular rows.
        let rows = data
            .get("replies")
            .ok_or(Error::Schema("notification seek reply shape"))?;
        if !rows.is_null() && !rows.is_array() {
            return Err(Error::Schema("notification seek replies"));
        }
        let wanted_root = expected.root.unwrap_or(expected.rpid);
        let found = rows
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| number(row, "rpid").ok() == Some(wanted_root));
        if let Some(found) = found {
            return apply_seek(context, json!({"seek_root_reply":found}), aid, limit);
        }
        return Ok(());
    };
    if raw.get("type").is_some_and(|value| !value.is_null()) && number(raw, "type")? != 1 {
        return Err(Error::Schema("notification seek comment type"));
    }
    let root = parse_comment(raw, aid, CommentSource::Regular, false)?;
    check_root(&root, aid, &expected)?;
    let mut selected = (root.rpid == expected.rpid).then(|| root.clone());
    let children = match raw.get("replies") {
        None | Some(Value::Null) => &[][..],
        Some(value) => value
            .as_array()
            .ok_or(Error::Schema("notification seek children"))?
            .as_slice(),
    };
    if children.len() > 100 {
        return Err(Error::JsonLimit);
    }
    let mut replies = Vec::new();
    let mut seen = BTreeSet::new();
    for child in children {
        // Select by exact rpid before truncating surrounding context.
        let id = number(child, "rpid")?;
        if replies.len() >= limit as usize && id != expected.rpid {
            continue;
        }
        if child.get("type").is_some_and(|value| !value.is_null()) && number(child, "type")? != 1 {
            return Err(Error::Schema("notification seek child type"));
        }
        let parsed = parse_comment(child, aid, CommentSource::ReplyPreview, false)?;
        if parsed.root != Some(root.rpid) || parsed.parent.is_none_or(|id| !positive(id)) {
            return Err(Error::Schema("notification seek child relationship"));
        }
        if parsed.rpid == expected.rpid {
            if selected.is_some() {
                return Err(Error::Schema("duplicate notification focus"));
            }
            selected = Some(parsed.clone());
        }
        if replies.len() < limit as usize && seen.insert(parsed.rpid) {
            replies.push(parsed);
        }
    }
    let previous_page = context.root_comment.is_some();
    context.root_comment = Some(root);
    if !previous_page {
        context.replies = replies;
        context.truncated = children.len() > limit as usize;
        // Preview rows have no safe page-number continuation.
        context.has_more = None;
        context.next_page = None;
    }
    if let Some(selected) = selected {
        context.accept_selected(selected)?;
    }
    Ok(())
}

fn number(raw: &Value, key: &str) -> Result<u64> {
    let value = raw
        .get(key)
        .ok_or(Error::Schema("notification seek missing id"))?;
    value
        .as_u64()
        .filter(|id| positive(*id))
        .or_else(|| value.as_str().and_then(decimal))
        .ok_or(Error::Schema("notification seek id"))
}

impl Client {
    /// At most one video read, one reply page and one WBI seek (plus a WBI key
    /// refresh if required). Never scans the whole comment area or fetches media.
    pub fn notification_context(
        &mut self,
        notification: &Notification,
        limit: u32,
    ) -> Result<NotificationContext> {
        if !(1..=20).contains(&limit) {
            return Err(Error::InvalidPage);
        }
        let mut context = NotificationContext::new(notification_location(notification));
        let Some(id) = context.location.video.clone() else {
            return Ok(context);
        };
        let video = self.video_info(&id)?;
        let aid = video.aid;
        context.video = Some(video);
        let Some(mut locator) = context.location.comment.clone() else {
            return Ok(context);
        };
        if locator.oid.is_some_and(|oid| oid != aid) {
            return Err(Error::Schema("notification video subject mismatch"));
        }
        locator.oid = Some(aid);
        context.location.comment = Some(locator.clone());
        if let Some(root) = locator.root {
            match self.comment_replies(aid, root, 1, limit) {
                Ok(page) => apply_reply_page(&mut context, page)?,
                Err(Error::Platform(-404 | 12006)) => {
                    context.unavailable();
                    return Ok(context);
                }
                Err(error) => return Err(error),
            }
            if context.selected_comment.is_some() {
                return Ok(context);
            }
        } else {
            // A like records the selected rpid without saying whether it is a
            // root. A bounded exact-root read can establish that fact; a failed
            // probe does not prove deletion and falls through to native seek.
            match self.comment_replies(aid, locator.rpid, 1, limit) {
                Ok(page) => apply_reply_page(&mut context, page)?,
                Err(Error::Platform(-404 | 12006)) | Err(Error::Schema(_)) => {}
                Err(error) => return Err(error),
            }
            if context.selected_comment.is_some() {
                return Ok(context);
            }
        }
        let params = [
            ("type", "1".into()),
            ("oid", aid.to_string()),
            ("mode", "3".into()),
            ("pagination_str", json!({"offset":""}).to_string()),
            ("plat", "1".into()),
            ("seek_rpid", locator.rpid.to_string()),
            ("web_location", "1315875".into()),
        ];
        match self.signed_get("/x/v2/reply/wbi/main", &params) {
            Ok(data) => apply_seek(&mut context, data, aid, limit)?,
            Err(Error::Platform(-404 | 12006)) => {
                context.unavailable();
                return Ok(context);
            }
            Err(error) => return Err(error),
        }
        if context.selected_comment.is_none() {
            context.reason = Some("The exact comment was not returned in the bounded page/seek result. Its root, if present, is context only; absence is not proof of deletion.".into());
        }
        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn notification(kind: NotificationKind, target: Value) -> Notification {
        let mut complete = json!({"item_id":null,"subject_id":null,"root_id":null,"source_id":null,"target_id":null,"business_id":null,"content_type":null,"business":null,"title":null,"description":null,"detail_title":null,"root_reply_content":null,"source_content":null,"target_reply_content":null,"message":null,"image":null,"uri":"https://www.bilibili.com/video/BV17x411w7KC"});
        for (key, value) in target.as_object().unwrap() {
            complete[key] = value.clone();
        }
        Notification {
            id: 1,
            kind,
            section: "items".into(),
            created_at: 1,
            actors: vec![],
            actors_unavailable: true,
            count: None,
            target: serde_json::from_value(complete).unwrap(),
        }
    }
    fn comment(rpid: u64, root: u64, parent: u64) -> Value {
        json!({"rpid":rpid,"oid":170001,"root":root,"parent":parent,"mid":4,"member":{"mid":"4","uname":"fixture"},"content":{"message":format!("comment {rpid}")},"replies":[],"state":0,"like":2})
    }
    #[test]
    fn unknown_root_is_verified_only_when_the_native_root_is_the_exact_selected_comment() {
        let n = notification(
            NotificationKind::Like,
            json!({"content_type":"reply","item_id":25}),
        );
        let mut ctx = NotificationContext::new(notification_location(&n));
        let page = |rpid| CommentReadReplies {
            aid: 170001,
            root: rpid,
            root_comment: parse_comment(
                &comment(rpid, 0, 0),
                170001,
                CommentSource::Regular,
                false,
            )
            .unwrap(),
            items: vec![],
            page: 1,
            page_size: 20,
            total: Some(0),
            has_more: Some(false),
            next_page: None,
            truncated: false,
            pagination_note: None,
            media_downloaded: false,
        };
        apply_reply_page(&mut ctx, page(26)).unwrap();
        assert!(ctx.selected_comment.is_none());
        assert_eq!(ctx.status, NotificationContextStatus::Unresolved);
        apply_reply_page(&mut ctx, page(25)).unwrap();
        assert_eq!(ctx.status, NotificationContextStatus::Found);
        let located = ctx.location.comment.unwrap();
        assert_eq!(
            (located.rpid, located.root, located.parent),
            (25, Some(25), Some(0))
        );
    }
    #[test]
    fn documented_reply_coordinates_distinguish_root_child_and_target() {
        let root = notification(
            NotificationKind::Reply,
            json!({"business_id":1,"content_type":"video","subject_id":170001,"root_id":0,"source_id":25,"target_id":0}),
        );
        let root = notification_location(&root).comment.unwrap();
        assert_eq!((root.rpid, root.root, root.parent), (25, Some(25), Some(0)));
        let child = notification(
            NotificationKind::Reply,
            json!({"business_id":1,"content_type":"reply","subject_id":170001,"root_id":11,"source_id":25,"target_id":19}),
        );
        let child = notification_location(&child).comment.unwrap();
        assert_eq!(
            (child.rpid, child.root, child.parent),
            (25, Some(11), Some(19))
        );
    }
    #[test]
    fn business_names_and_unknown_at_coordinates_are_not_guessed() {
        for kind in [NotificationKind::Reply, NotificationKind::At] {
            let n = notification(
                kind,
                json!({"business_id":17,"content_type":"reply","subject_id":170001,"root_id":11,"source_id":25,"target_id":19,"business":"视频"}),
            );
            assert_eq!(
                notification_location(&n).status,
                NotificationLocationStatus::Unresolved
            );
            let mut anchored = n.clone();
            anchored.target.uri =
                Some("https://www.bilibili.com/video/BV17x411w7KC#reply25".into());
            assert!(notification_location(&anchored).comment.is_none());
        }
        let n = notification(
            NotificationKind::At,
            json!({"business_id":1,"content_type":"reply","source_id":25,"root_id":0}),
        );
        assert!(notification_location(&n).comment.is_none());
        let n = notification(
            NotificationKind::At,
            json!({"business_id":1,"content_type":"reply","subject_id":170001,"source_id":25,"root_id":0,"target_id":0}),
        );
        let locator = notification_location(&n).comment.unwrap();
        assert_eq!(
            (locator.rpid, locator.root, locator.parent),
            (25, Some(25), Some(0))
        );
        let n = notification(
            NotificationKind::At,
            json!({"business_id":1,"content_type":"reply","subject_id":170001,"source_id":25,"root_id":11,"target_id":19}),
        );
        assert!(notification_location(&n).comment.is_none());
        let n = notification(
            NotificationKind::At,
            json!({"business_id":3,"content_type":"video"}),
        );
        assert_eq!(
            notification_location(&n).status,
            NotificationLocationStatus::Video
        );
    }
    #[test]
    fn uri_anchors_are_strict_and_never_broaden_hosts() {
        let n = notification(
            NotificationKind::At,
            json!({"content_type":"reply","uri":"https://www.bilibili.com/video/BV17x411w7KC/?comment_root_id=11&comment_secondary_id=25#reply25"}),
        );
        let located = notification_location(&n).comment.unwrap();
        assert_eq!((located.rpid, located.root), (25, Some(11)));
        for uri in [
            "https://www.bilibili.com.evil.test/video/BV17x411w7KC#reply25",
            "https://www.bilibili.com@evil.test/video/BV17x411w7KC#reply25",
            "http://www.bilibili.com/video/BV17x411w7KC#reply25",
            "https://www.bilibili.com:444/video/BV17x411w7KC#reply25",
            "https://www.bilibili.com/video/BV17x411w7KC?comment_root_id=11&comment_root_id=12#reply11",
            "https://www.bilibili.com/video/BV17x411w7KC?comment_root_id=11#reply25",
            "https://www.bilibili.com/video/BV17x411w7KC#reply25#reply11",
        ] {
            let n = notification(
                NotificationKind::At,
                json!({"content_type":"reply","uri":uri}),
            );
            assert!(notification_location(&n).comment.is_none(), "{uri}");
        }
    }
    #[test]
    fn exact_child_after_context_limit_keeps_gif_variants_and_never_selects_root() {
        let n = notification(
            NotificationKind::Like,
            json!({"content_type":"reply","item_id":25}),
        );
        let mut ctx = NotificationContext::new(notification_location(&n));
        let mut root = comment(11, 0, 0);
        let mut child = comment(25, 11, 19);
        child["content"]["emote"] = json!({"[test]":{"id":1,"package_id":2,"type":3,"url":"https://i0.hdslb.com/static.png","gif_url":"https://i0.hdslb.com/animated.gif","webp_url":"https://i0.hdslb.com/animated.webp"}});
        root["replies"] = json!([comment(12, 11, 11), child]);
        apply_seek(&mut ctx, json!({"seek_root_reply":root}), 170001, 1).unwrap();
        assert_eq!(ctx.status, NotificationContextStatus::Found);
        assert_eq!(ctx.root_comment.as_ref().unwrap().rpid, 11);
        let focus = ctx.selected_comment.as_ref().unwrap();
        assert_eq!(focus.rpid, 25);
        assert_eq!(focus.media.len(), 3);
        assert!(focus.media.iter().all(|m| m.animated.is_none()));
        assert_eq!(ctx.replies.len(), 1);
        assert!(ctx.truncated);
    }
    #[test]
    fn missing_focus_does_not_substitute_root_or_assert_deleted() {
        let n = notification(
            NotificationKind::Like,
            json!({"content_type":"reply","item_id":25}),
        );
        let mut ctx = NotificationContext::new(notification_location(&n));
        apply_seek(
            &mut ctx,
            json!({"seek_root_reply":comment(11,0,0)}),
            170001,
            20,
        )
        .unwrap();
        assert!(ctx.selected_comment.is_none());
        assert_eq!(ctx.status, NotificationContextStatus::Unresolved);
        ctx.unavailable();
        assert_eq!(ctx.status, NotificationContextStatus::Unavailable);
        assert!(ctx.selected_comment.is_none());
        let mut ctx = NotificationContext::new(notification_location(&n));
        apply_seek(
            &mut ctx,
            json!({"seek_root_reply":null,"replies":[]}),
            170001,
            20,
        )
        .unwrap();
        assert!(ctx.root_comment.is_none());
    }
    #[test]
    fn oid_root_parent_mismatches_and_challenges_fail_explicitly() {
        let n = notification(
            NotificationKind::Reply,
            json!({"business_id":1,"content_type":"reply","subject_id":170001,"source_id":25,"root_id":11,"target_id":19}),
        );
        for (root_id, parent, oid) in [(10, 19, 170001), (11, 18, 170001), (11, 19, 170002)] {
            let mut ctx = NotificationContext::new(notification_location(&n));
            let mut root = comment(root_id, 0, 0);
            let mut child = comment(25, root_id, parent);
            child["oid"] = json!(oid);
            root["replies"] = json!([child]);
            assert!(apply_seek(&mut ctx, json!({"seek_root_reply":root}), 170001, 20).is_err());
        }
        let mut ctx = NotificationContext::new(notification_location(&n));
        assert!(matches!(
            apply_seek(
                &mut ctx,
                json!({"need_captcha":true,"replies":[]}),
                170001,
                20
            ),
            Err(Error::RiskControl(0))
        ));
    }
}
