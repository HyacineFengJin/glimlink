//! Authorized one-shot reply posting. Host tools must journal `attempting`
//! before calling send. No write retries live in this adapter.
//! Web form fields: shfshanyue/tieba_post/tieba.py (author source). The older
//! aiotieba Android add_post requires device fingerprint fields and is not used.
use crate::{
    AuthClient, Client, Config, Error, Fragment, Post, Result, Session, ThreadId, ThreadPage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyTarget {
    pub thread_id: u64,
    pub title: String,
    pub forum_id: u64,
    pub forum_name: String,
    pub owner_uid: u64,
}
/// Not Clone or Serialize: contains a private CSRF token and a consumed send capability.
pub struct PreparedReply {
    target: ReplyTarget,
    message: String,
    body: Vec<u8>,
    identity: String,
    before_post_ids: Vec<u64>,
    prepared_at: u64,
}
impl fmt::Debug for PreparedReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedReply")
            .field("target", &self.target)
            .field("message", &self.message)
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl PreparedReply {
    pub fn target(&self) -> &ReplyTarget {
        &self.target
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn before_post_ids(&self) -> &[u64] {
        &self.before_post_ids
    }
    pub fn prepared_at(&self) -> u64 {
        self.prepared_at
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyReceipt {
    pub thread_id: u64,
    pub post_id: Option<u64>,
    /// Expected author, verified during preparation; visibility needs readback.
    pub owner_uid: u64,
    pub message: String,
    pub before_post_ids: Vec<u64>,
    pub accepted_at: u64,
    pub echo_content: Option<String>,
}
/// A read-only lookup specification. This does not assert that a write succeeded
/// and can reconcile an attempting/unknown host journal without a fake receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyVerification {
    pub thread_id: u64,
    pub post_id: Option<u64>,
    pub owner_uid: u64,
    pub message: String,
    pub before_post_ids: Vec<u64>,
    pub attempted_at: u64,
}
impl From<&ReplyReceipt> for ReplyVerification {
    fn from(receipt: &ReplyReceipt) -> Self {
        Self {
            thread_id: receipt.thread_id,
            post_id: receipt.post_id,
            owner_uid: receipt.owner_uid,
            message: receipt.message.clone(),
            before_post_ids: receipt.before_post_ids.clone(),
            attempted_at: receipt.accepted_at,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedReply {
    pub thread_id: u64,
    pub post_id: u64,
    pub author_uid: u64,
    pub content: String,
    pub created_at: u64,
    pub floor: u64,
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| Error::InvalidConfig)
}
fn client(session: &Session) -> Result<Client> {
    Client::new(Config {
        bduss: Some(session.bduss().to_owned()),
        ..Default::default()
    })
}
fn validate_message(message: &str) -> Result<()> {
    if message.trim().is_empty() || message.len() > 4096 || message.chars().any(char::is_control) {
        return Err(Error::InvalidConfig);
    }
    Ok(())
}
fn target(page: &ThreadPage, uid: u64) -> Result<ReplyTarget> {
    if uid == 0
        || page.thread_id.0 == 0
        || page.forum.id == 0
        || page.forum.name.is_empty()
        || page.forum.name.len() > 1024
        || page.forum.name.chars().any(char::is_control)
        || (page.author.id != 0 && page.author.id != uid)
    {
        return Err(Error::Schema("reply target identity or forum"));
    }
    let first = page.posts.iter().find(|p| p.floor == 1);
    if first.is_some_and(|p| p.author.id != 0 && p.author.id != uid)
        || (page.author.id != uid && !first.is_some_and(|p| p.author.id == uid))
    {
        return Err(Error::Schema(
            "reply target is not owned by current account",
        ));
    }
    Ok(ReplyTarget {
        thread_id: page.thread_id.0,
        title: page.title.clone(),
        forum_id: page.forum.id,
        forum_name: page.forum.name.clone(),
        owner_uid: uid,
    })
}
fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn form(target: &ReplyTarget, message: &str, tbs: &str) -> Result<Vec<u8>> {
    validate_message(message)?;
    if tbs.is_empty()
        || tbs.len() > 128
        || !tbs
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::Schema("invalid reply CSRF token"));
    }
    // rich_text=1 is the documented web route. Escape plain text rather than
    // accepting caller HTML; the authorized test contains no markup characters.
    let content = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let fields = [
        ("ie", "utf-8".to_owned()),
        ("kw", target.forum_name.clone()),
        ("fid", target.forum_id.to_string()),
        ("tid", target.thread_id.to_string()),
        ("content", content),
        ("is_login", "1".into()),
        ("rich_text", "1".into()),
        ("tbs", tbs.into()),
        ("__type__", "reply".into()),
    ];
    Ok(fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes())
}
fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}
pub(crate) fn parse_tbs(raw: &[u8]) -> Result<String> {
    let value: Value = serde_json::from_slice(raw).map_err(|_| Error::Challenge)?;
    let logged_in = value
        .get("is_login")
        .is_some_and(|v| v.as_bool() == Some(true) || integer(v) == Some(1));
    if !logged_in {
        return Err(Error::LoginRequired);
    }
    let tbs = value
        .get("tbs")
        .and_then(Value::as_str)
        .ok_or(Error::Schema("missing reply CSRF token"))?;
    if tbs.is_empty()
        || tbs.len() > 128
        || !tbs
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::Schema("invalid reply CSRF token"));
    }
    Ok(tbs.into())
}
fn positive(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|id| *id > 0 && *id <= i64::MAX as u64)
}
fn parse_receipt(raw: &[u8], prepared: &PreparedReply) -> Result<ReplyReceipt> {
    let value: Value = serde_json::from_slice(raw).map_err(|_| Error::WriteResultUnknown)?;
    let mut codes = Vec::new();
    for name in ["no", "err_code", "error_code"] {
        if let Some(v) = value.get(name) {
            codes.push(integer(v).ok_or(Error::WriteResultUnknown)?);
        }
    }
    if codes.is_empty() {
        return Err(Error::WriteResultUnknown);
    }
    // The web envelope and API-specific error namespaces need not use the same
    // numeric value. Every explicit nonzero code is a rejection; a mixed zero
    // and nonzero envelope is ambiguous and must never become success.
    if codes.iter().all(|code| *code != 0) {
        return Err(Error::Platform(codes[0]));
    }
    if codes.iter().any(|code| *code != 0) {
        return Err(Error::WriteResultUnknown);
    }
    if [
        "/need_vcode",
        "/data/need_vcode",
        "/data/need_verify",
        "/data/vcode",
    ]
    .iter()
    .any(|path| {
        value
            .pointer(path)
            .is_some_and(|v| v.as_bool() == Some(true) || integer(v).is_some_and(|n| n != 0))
    }) {
        return Err(Error::Challenge);
    }
    let mut ids = Vec::new();
    for path in ["/data/pid", "/data/post_id", "/pid", "/post_id"] {
        if let Some(v) = value.pointer(path) {
            if !v.is_null() && v.as_str() != Some("") && integer(v) != Some(0) {
                ids.push(positive(v).ok_or(Error::WriteResultUnknown)?);
            }
        }
    }
    if ids.iter().any(|id| *id != ids[0]) {
        return Err(Error::WriteResultUnknown);
    }
    let echo_content = value
        .pointer("/data/content")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 4096)
        .map(str::to_owned);
    Ok(ReplyReceipt {
        thread_id: prepared.target.thread_id,
        post_id: ids.first().copied(),
        owner_uid: prepared.target.owner_uid,
        message: prepared.message.clone(),
        before_post_ids: prepared.before_post_ids.clone(),
        accepted_at: now()?,
        echo_content,
    })
}
fn verify_candidate(post: &Post, lookup: &ReplyVerification) -> Option<VerifiedReply> {
    if post.id == 0
        || lookup.before_post_ids.contains(&post.id)
        || post.author.id != lookup.owner_uid
        || lookup.post_id.is_some_and(|pid| pid != post.id)
        || post.created_at == 0
        || post.created_at < lookup.attempted_at.saturating_sub(30)
    {
        return None;
    }
    let text = post
        .fragments
        .iter()
        .map(|fragment| match fragment {
            Fragment::Text { text }
            | Fragment::Link { text, .. }
            | Fragment::Mention { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?
        .join("");
    if text != lookup.message {
        return None;
    }
    Some(VerifiedReply {
        thread_id: lookup.thread_id,
        post_id: post.id,
        author_uid: post.author.id,
        content: text,
        created_at: post.created_at,
        floor: post.floor,
    })
}
impl AuthClient {
    pub fn prepare_own_reply(
        &self,
        session: &mut Session,
        thread: ThreadId,
        message: &str,
    ) -> Result<PreparedReply> {
        validate_message(message)?;
        thread.validate()?;
        let owner = self.self_identity(session)?;
        let api = client(session)?;
        let head = api.thread_page(&thread, 1)?.data;
        let target = target(&head, owner.user_id)?;
        let mut before_post_ids: Vec<_> = head.posts.iter().map(|p| p.id).collect();
        if head.page.total_pages > 1 {
            let page = u32::try_from(head.page.total_pages).map_err(|_| Error::InvalidPage)?;
            before_post_ids.extend(
                api.thread_page(&thread, page)?
                    .data
                    .posts
                    .iter()
                    .map(|p| p.id),
            );
        }
        before_post_ids.sort_unstable();
        before_post_ids.dedup();
        let tbs = self.reply_tbs(session)?;
        let body = form(&target, message, &tbs)?;
        Ok(PreparedReply {
            target,
            message: message.into(),
            body,
            identity: session.stable_identity(),
            before_post_ids,
            prepared_at: now()?,
        })
    }
    /// Exactly one write HTTP request. The caller must journal its attempt first.
    pub fn send_prepared_reply(
        &self,
        session: &mut Session,
        prepared: PreparedReply,
    ) -> Result<ReplyReceipt> {
        if session.stable_identity() != prepared.identity
            || now()?.saturating_sub(prepared.prepared_at) > 300
        {
            return Err(Error::InvalidConfig);
        }
        let raw = self.submit_reply_form(session, prepared.target.thread_id, &prepared.body)?;
        parse_receipt(&raw, &prepared)
    }
    /// Read head/tail only, never sends or repeats the write. A successful POST
    /// without a visible exact author/body match remains unverified.
    pub fn verify_post(
        &self,
        session: &mut Session,
        receipt: &ReplyReceipt,
    ) -> Result<Option<VerifiedReply>> {
        self.verify_reply(session, &ReplyVerification::from(receipt))
    }
    /// Reconcile a journaled attempt using reads only. No acceptance receipt is
    /// implied, no write occurs, and old/undated matching messages are rejected.
    pub fn verify_reply(
        &self,
        session: &mut Session,
        lookup: &ReplyVerification,
    ) -> Result<Option<VerifiedReply>> {
        validate_message(&lookup.message)?;
        if lookup.attempted_at == 0 {
            return Err(Error::InvalidConfig);
        }
        let owner = self.self_identity(session)?;
        if owner.user_id != lookup.owner_uid {
            return Err(Error::Schema("reply verification account mismatch"));
        }
        let thread = ThreadId(lookup.thread_id);
        thread.validate()?;
        let api = client(session)?;
        let head = api.thread_page(&thread, 1)?.data;
        target(&head, lookup.owner_uid)?;
        if let Some(found) = head
            .posts
            .iter()
            .find_map(|post| verify_candidate(post, lookup))
        {
            return Ok(Some(found));
        }
        if head.page.total_pages > 1 {
            let page = u32::try_from(head.page.total_pages).map_err(|_| Error::InvalidPage)?;
            let tail = api.thread_page(&thread, page)?.data;
            return Ok(tail
                .posts
                .iter()
                .find_map(|post| verify_candidate(post, lookup)));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Author, Forum, Page};
    use serde_json::json;
    fn prepared() -> PreparedReply {
        let target = ReplyTarget {
            thread_id: 70,
            title: "own".into(),
            forum_id: 7,
            forum_name: "自己的吧".into(),
            owner_uid: 42,
        };
        PreparedReply {
            body: form(&target, "晚安喵（test）", "syntheticPrivateTbs").unwrap(),
            target,
            message: "晚安喵（test）".into(),
            identity: "portrait:test".into(),
            before_post_ids: vec![1],
            prepared_at: now().unwrap(),
        }
    }
    #[test]
    fn web_reply_form_preserves_exact_unicode_and_never_adds_device_fields() {
        let p = prepared();
        let body = String::from_utf8(p.body.clone()).unwrap();
        assert!(body.contains("content=%E6%99%9A%E5%AE%89%E5%96%B5%EF%BC%88test%EF%BC%89"));
        assert!(
            body.contains("tid=70") && body.contains("fid=7") && body.contains("__type__=reply")
        );
        assert!(!body.contains("mouse_pwd") && !body.contains("imei") && !body.contains("BDUSS"));
        assert!(!format!("{p:?}").contains("syntheticPrivateTbs"));
        assert!(validate_message("").is_err());
        assert!(validate_message("x\0y").is_err());
    }
    #[test]
    fn reply_receipts_require_an_explicit_result_and_never_invent_a_post_id() {
        let p = prepared();
        let receipt =
            parse_receipt(br#"{"no":0,"data":{"pid":"99","private":"hidden"}}"#, &p).unwrap();
        assert_eq!(receipt.post_id, Some(99));
        assert_eq!(receipt.owner_uid, 42);
        assert_eq!(
            parse_receipt(br#"{"no":0,"data":{}}"#, &p).unwrap().post_id,
            None
        );
        assert!(matches!(
            parse_receipt(br#"{"no":4}"#, &p),
            Err(Error::Platform(4))
        ));
        assert!(matches!(
            parse_receipt(br#"{"no":0,"data":{"need_vcode":1}}"#, &p),
            Err(Error::Challenge)
        ));
        for raw in [
            br#"{"data":{"pid":"99"}}"#.as_slice(),
            b"<html>unknown",
            br#"{"no":0,"err_code":4}"#,
        ] {
            assert!(matches!(
                parse_receipt(raw, &p),
                Err(Error::WriteResultUnknown)
            ));
        }
        assert!(parse_tbs(br#"{"is_login":0,"tbs":"not-a-session"}"#).is_err());
        assert_eq!(
            parse_tbs(br#"{"is_login":1,"tbs":"syntheticToken_1"}"#).unwrap(),
            "syntheticToken_1"
        );
    }
    #[test]
    fn target_must_be_owned_and_uses_real_forum_metadata() {
        let mut page = ThreadPage {
            thread_id: ThreadId(70),
            title: "own".into(),
            forum: Forum {
                id: 7,
                name: "actual".into(),
            },
            author: Author {
                id: 42,
                ..Default::default()
            },
            reply_count: 0,
            page: Page::default(),
            posts: vec![],
            comments_included: false,
            notes: vec![],
        };
        assert_eq!(target(&page, 42).unwrap().forum_id, 7);
        assert!(target(&page, 43).is_err());
        page.forum.id = 0;
        assert!(target(&page, 42).is_err());
    }
    #[test]
    fn distinct_nonzero_error_namespaces_reject_but_mixed_or_malformed_codes_are_unknown() {
        let p = prepared();
        assert!(matches!(
            parse_receipt(
                br#"{"no":308,"err_code":230308,"error":"not allowed","data":{}}"#,
                &p
            ),
            Err(Error::Platform(308))
        ));
        assert!(matches!(
            parse_receipt(br#"{"no":"308","err_code":"230308","error_code":9}"#, &p),
            Err(Error::Platform(308))
        ));
        assert!(
            parse_receipt(
                br#"{"no":0,"err_code":"0","error_code":0,"data":{"pid":99}}"#,
                &p
            )
            .is_ok()
        );
        for raw in [
            br#"{"no":0,"err_code":230308}"#.as_slice(),
            br#"{"no":308,"err_code":0}"#,
            br#"{"no":308,"err_code":"not-an-integer"}"#,
            br#"{"no":0,"err_code":0.5}"#,
        ] {
            assert!(matches!(
                parse_receipt(raw, &p),
                Err(Error::WriteResultUnknown)
            ));
        }
    }
    #[test]
    fn readback_needs_new_id_exact_author_and_exact_message() {
        let receipt = parse_receipt(
            &serde_json::to_vec(&json!({"no":0,"data":{"pid":"99"}})).unwrap(),
            &prepared(),
        )
        .unwrap();
        let lookup = ReplyVerification::from(&receipt);
        assert_eq!(lookup.attempted_at, receipt.accepted_at);
        let mut post = Post {
            id: 99,
            floor: 2,
            created_at: receipt.accepted_at,
            author: Author {
                id: 42,
                ..Default::default()
            },
            reply_count: 0,
            fragments: vec![Fragment::Text {
                text: "晚安喵（test）".into(),
            }],
            comments: vec![],
        };
        assert!(verify_candidate(&post, &lookup).is_some());
        post.author.id = 43;
        assert!(verify_candidate(&post, &lookup).is_none());
        post.author.id = 42;
        post.fragments = vec![Fragment::Text {
            text: "晚安喵(test)".into(),
        }];
        assert!(verify_candidate(&post, &lookup).is_none());
        post.fragments = vec![Fragment::Text {
            text: "晚安喵（test）".into(),
        }];
        post.id = 1;
        assert!(verify_candidate(&post, &lookup).is_none());
        post.id = 99;
        post.created_at = lookup.attempted_at.saturating_sub(31);
        assert!(verify_candidate(&post, &lookup).is_none());
        post.created_at = 0;
        assert!(verify_candidate(&post, &lookup).is_none());
        post.created_at = lookup.attempted_at.saturating_sub(30);
        assert!(verify_candidate(&post, &lookup).is_some());
        let without_pid = ReplyVerification {
            post_id: None,
            ..lookup
        };
        post.created_at = without_pid.attempted_at.saturating_sub(31);
        assert!(verify_candidate(&post, &without_pid).is_none());
    }
}
