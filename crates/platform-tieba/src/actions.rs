// SPDX-License-Identifier: MIT
//! Typed forum actions. Preparation is read-only, submit consumes one capability.
//! Requests follow published Tieba client protocols; no challenge bypass/fallback.
use crate::{
    AuthClient, Client, Config, Error, Forum, Fragment, Result, Session, ThreadId,
    wire::{self, Fields},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionTarget {
    NewThread {
        forum_id: u64,
        forum_name: String,
    },
    FloorReply {
        thread_id: u64,
        forum_id: u64,
        forum_name: String,
        parent_post_id: u64,
        subpost_id: Option<u64>,
        reply_uid: u64,
    },
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadedImage {
    pub forum_id: u64,
    pub pic_id: String,
    pub width: u32,
    pub height: u32,
    pub url: String,
}
impl std::fmt::Debug for UploadedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadedImage")
            .field("pic_id", &self.pic_id)
            .field("forum_id", &self.forum_id)
            .finish_non_exhaustive()
    }
}
pub struct PreparedAction {
    target: ActionTarget,
    title: String,
    message: String,
    identity: String,
    actor_uid: u64,
    body: Vec<u8>,
    created_at: u64,
    images: Vec<UploadedImage>,
    actor_display: String,
}
impl PreparedAction {
    pub fn target(&self) -> &ActionTarget {
        &self.target
    }
    pub fn actor_uid(&self) -> u64 {
        self.actor_uid
    }
    pub fn prepared_at(&self) -> u64 {
        self.created_at
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub target: ActionTarget,
    pub thread_id: u64,
    pub post_id: Option<u64>,
    pub actor_uid: u64,
    pub title: String,
    pub message: String,
    pub images: Vec<UploadedImage>,
    pub attempted_at: u64,
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| Error::InvalidConfig)
}
fn positive(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|id| *id > 0 && *id <= i64::MAX as u64)
}
fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty()
        || text.len() > 4096
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        Err(Error::InvalidConfig)
    } else {
        Ok(())
    }
}
fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\n', "<br>")
}
fn image_valid(image: &UploadedImage, forum: u64) -> Result<()> {
    if image.forum_id != forum
        || image.pic_id.is_empty()
        || image.pic_id.len() > 128
        || !image
            .pic_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        || image.width == 0
        || image.height == 0
        || image.width > 16384
        || image.height > 16384
    {
        return Err(Error::InvalidConfig);
    }
    crate::validate_media_url(&image.url)?;
    Ok(())
}
pub(crate) fn thread_form(
    forum: &Forum,
    title: &str,
    message: &str,
    tbs: &str,
    images: &[UploadedImage],
) -> Result<Vec<u8>> {
    crate::discovery::text_input(title, 300)?;
    validate_text(message)?;
    if forum.id == 0 || images.len() > 4 {
        return Err(Error::InvalidConfig);
    }
    let mut content = html(message);
    for image in images {
        image_valid(image, forum.id)?;
        content.push_str(&format!(
            "<br><img class=\"BDE_Image\" src=\"{}\" width=\"{}\" height=\"{}\">",
            html(&image.url),
            image.width,
            image.height
        ));
    }
    Ok(crate::transport::form_bytes(&[
        ("ie", "utf-8".into()),
        ("kw", forum.name.clone()),
        ("fid", forum.id.to_string()),
        ("tbs", tbs.into()),
        ("title", title.into()),
        ("content", content),
        ("rich_text", "1".into()),
        ("is_login", "1".into()),
        ("__type__", "thread".into()),
    ]))
}
pub(crate) fn floor_request(
    bduss: &str,
    tbs: &str,
    target: &ActionTarget,
    message: &str,
    display: &str,
    images: &[UploadedImage],
) -> Result<Vec<u8>> {
    validate_text(message)?;
    let ActionTarget::FloorReply {
        thread_id,
        forum_id,
        forum_name,
        parent_post_id,
        subpost_id,
        reply_uid,
    } = target
    else {
        return Err(Error::InvalidConfig);
    };
    if [*thread_id, *forum_id, *parent_post_id, *reply_uid]
        .iter()
        .any(|id| *id == 0 || *id > i64::MAX as u64)
        || subpost_id.is_some_and(|id| id == 0 || id > i64::MAX as u64)
    {
        return Err(Error::InvalidId);
    }
    if images.len() > 4 {
        return Err(Error::Limit);
    }
    let mut content = message.to_owned();
    for image in images {
        image_valid(image, *forum_id)?;
        content.push_str(&format!(
            "#(pic,{},{},{})",
            image.pic_id, image.width, image.height
        ));
    }
    let mut common = Vec::new();
    wire::number(1, 2, &mut common);
    wire::bytes(2, b"22.12.1.0", &mut common);
    wire::bytes(10, bduss.as_bytes(), &mut common);
    wire::bytes(11, tbs.as_bytes(), &mut common);
    let mut data = Vec::new();
    wire::bytes(1, &common, &mut data);
    for (field, text) in [
        (4, tbs.to_owned()),
        (6, "1".into()),
        (16, "12".into()),
        (18, "1".into()),
        (19, content),
        (20, reply_uid.to_string()),
        (26, forum_id.to_string()),
        (30, forum_name.clone()),
        (45, thread_id.to_string()),
        (46, parent_post_id.to_string()),
        (48, "0".into()),
        (49, parent_post_id.to_string()),
        (50, subpost_id.unwrap_or(0).to_string()),
        (58, display.into()),
    ] {
        wire::bytes(field, text.as_bytes(), &mut data);
    }
    let mut out = Vec::new();
    wire::bytes(1, &data, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image() -> UploadedImage {
        UploadedImage {
            forum_id: 42,
            pic_id: "abc123".into(),
            width: 64,
            height: 32,
            url: "https://imgsrc.baidu.com/forum/pic/item/abc123.jpg".into(),
        }
    }
    fn target() -> ActionTarget {
        ActionTarget::FloorReply {
            thread_id: 123,
            forum_id: 42,
            forum_name: "Rust".into(),
            parent_post_id: 456,
            subpost_id: Some(789),
            reply_uid: 77,
        }
    }
    fn prepared(target: ActionTarget) -> PreparedAction {
        PreparedAction {
            target,
            title: "title".into(),
            message: "message".into(),
            identity: "fixture".into(),
            actor_uid: 9,
            body: vec![],
            created_at: 100,
            images: vec![],
            actor_display: "operator".into(),
        }
    }
    #[test]
    fn floor_wire_keeps_parent_child_recipient_and_picture_coordinates() {
        let raw = floor_request(
            "private",
            "token",
            &target(),
            "测试",
            "operator",
            &[image()],
        )
        .unwrap();
        let envelope = Fields::parse(&raw).unwrap();
        let data = Fields::parse(envelope.bytes(1).unwrap()).unwrap();
        assert_eq!(data.text(45).unwrap(), "123");
        assert_eq!(data.text(46).unwrap(), "456");
        assert_eq!(data.text(49).unwrap(), "456");
        assert_eq!(data.text(50).unwrap(), "789");
        assert_eq!(data.text(20).unwrap(), "77");
        assert_eq!(data.text(19).unwrap(), "测试#(pic,abc123,64,32)");
        let common = Fields::parse(data.bytes(1).unwrap()).unwrap();
        assert_eq!(common.text(10).unwrap(), "private");
        assert_eq!(common.text(11).unwrap(), "token");
        let mut foreign = image();
        foreign.forum_id = 43;
        assert!(
            floor_request(
                "private",
                "token",
                &target(),
                "text",
                "operator",
                &[foreign]
            )
            .is_err()
        );
    }
    #[test]
    fn new_thread_escapes_content_and_only_generates_typed_images() {
        let form = thread_form(
            &Forum {
                id: 42,
                name: "Rust".into(),
            },
            "title",
            "<script>&\"\n正文",
            "token",
            &[image()],
        )
        .unwrap();
        let encoded = String::from_utf8(form).unwrap();
        assert!(encoded.contains("%26lt%3Bscript%26gt%3B%26amp%3B%26quot%3B%3Cbr%3E"));
        assert!(encoded.contains("BDE_Image"));
        assert!(encoded.contains("fid=42"));
        assert!(!encoded.contains("<script>"));
        let mut invalid = image();
        invalid.url = "https://example.com/a.jpg".into();
        assert!(
            thread_form(
                &Forum {
                    id: 42,
                    name: "Rust".into()
                },
                "title",
                "text",
                "token",
                &[invalid]
            )
            .is_err()
        );
    }
    #[test]
    fn upload_response_validates_dimensions_id_and_domain() {
        let img = parse_uploaded(br#"{"error_code":0,"info":{"pic_id":"abc123","fullpic_width":"64","fullpic_height":"32"}}"#, 42).unwrap();
        assert_eq!((img.width, img.height), (64, 32));
        assert_eq!(
            img.url,
            "https://imgsrc.baidu.com/forum/pic/item/abc123.jpg"
        );
        assert!(parse_uploaded(br#"{"error_code":0,"info":{"pic_id":"x,1,1)","fullpic_width":64,"fullpic_height":32}}"#,42).is_err());
        assert!(parse_uploaded(br#"{"error_code":0,"info":{"pic_id":"abc","fullpic_width":0,"fullpic_height":32}}"#,42).is_err());
        assert!(parse_uploaded(br#"{"error_code":0,"info":{"pic_id":"abc","fullpic_width":64,"fullpic_height":32,"url":"https://example.com/a"}}"#,42).is_err());
    }
    #[test]
    fn submission_requires_consistent_ack_and_does_not_invent_receipt_ids() {
        let action = prepared(ActionTarget::NewThread {
            forum_id: 42,
            forum_name: "Rust".into(),
        });
        let receipt =
            parse_action(br#"{"no":0,"data":{"tid":"123","pid":"456"}}"#, &action).unwrap();
        assert_eq!((receipt.thread_id, receipt.post_id), (123, Some(456)));
        assert!(matches!(
            parse_action(br#"{"no":0,"err_code":1,"data":{"tid":123}}"#, &action),
            Err(Error::WriteResultUnknown)
        ));
        assert!(matches!(
            parse_action(br#"{"no":0,"data":{}}"#, &action),
            Err(Error::WriteResultUnknown)
        ));
        assert!(matches!(
            parse_action(br#"{"no":9}"#, &action),
            Err(Error::Platform(9))
        ));
        assert!(matches!(
            parse_action(br#"{"no":0,"data":{"tid":123,"need_vcode":1}}"#, &action),
            Err(Error::Challenge)
        ));
        let mut data = vec![];
        wire::bytes(3, b"999", &mut data);
        let mut raw = vec![];
        wire::bytes(2, &data, &mut raw);
        let receipt = parse_action(&raw, &prepared(target())).unwrap();
        assert_eq!((receipt.thread_id, receipt.post_id), (123, Some(999)));
        assert!(matches!(
            parse_action(b"<html>lost</html>", &prepared(target())),
            Err(Error::WriteResultUnknown)
        ));
    }
}
fn json_result(raw: &[u8]) -> Result<Value> {
    let v: Value = serde_json::from_slice(raw).map_err(|_| Error::WriteResultUnknown)?;
    let mut codes = Vec::new();
    for key in ["no", "error_code", "err_code"] {
        if let Some(c) = v.get(key) {
            codes.push(
                c.as_i64()
                    .or_else(|| c.as_str()?.parse().ok())
                    .ok_or(Error::WriteResultUnknown)?,
            );
        }
    }
    if codes.is_empty() {
        return Err(Error::WriteResultUnknown);
    }
    if codes.iter().all(|n| *n != 0) {
        return Err(Error::Platform(codes[0]));
    }
    if codes.iter().any(|n| *n != 0) {
        return Err(Error::WriteResultUnknown);
    }
    for path in [
        "/need_vcode",
        "/data/need_vcode",
        "/data/need_verify",
        "/info/need_vcode",
    ] {
        if v.pointer(path)
            .is_some_and(|x| x == true || positive(x).is_some())
        {
            return Err(Error::Challenge);
        }
    }
    Ok(v)
}
pub(crate) fn parse_uploaded(raw: &[u8], forum_id: u64) -> Result<UploadedImage> {
    let v = json_result(raw)?;
    let info = &v["info"];
    let pic_id = info["pic_id"]
        .as_str()
        .ok_or(Error::WriteResultUnknown)?
        .to_owned();
    let width = u32::try_from(positive(&info["fullpic_width"]).ok_or(Error::WriteResultUnknown)?)
        .map_err(|_| Error::WriteResultUnknown)?;
    let height = u32::try_from(positive(&info["fullpic_height"]).ok_or(Error::WriteResultUnknown)?)
        .map_err(|_| Error::WriteResultUnknown)?;
    let url = if let Some(url) = info["fullpic_url"]
        .as_str()
        .or_else(|| info["url"].as_str())
    {
        crate::transport::normalize_media_url(url)?
    } else {
        format!("https://imgsrc.baidu.com/forum/pic/item/{pic_id}.jpg")
    };
    let image = UploadedImage {
        forum_id,
        pic_id,
        width,
        height,
        url,
    };
    image_valid(&image, forum_id)?;
    Ok(image)
}
pub(crate) fn parse_action(raw: &[u8], prepared: &PreparedAction) -> Result<ActionReceipt> {
    let (thread_id, post_id) = match &prepared.target {
        ActionTarget::NewThread { .. } => {
            let v = json_result(raw)?;
            (
                positive(&v["data"]["tid"])
                    .or_else(|| positive(&v["tid"]))
                    .ok_or(Error::WriteResultUnknown)?,
                positive(&v["data"]["pid"]).or_else(|| positive(&v["pid"])),
            )
        }
        ActionTarget::FloorReply { thread_id, .. } => {
            let envelope = Fields::parse(raw).map_err(|_| Error::WriteResultUnknown)?;
            if let Some(e) = envelope.bytes(1) {
                let error = Fields::parse(e).map_err(|_| Error::WriteResultUnknown)?;
                let code = error.num(1) as i64;
                if code != 0 {
                    return Err(Error::Platform(code));
                }
            }
            let data = Fields::parse(envelope.bytes(2).ok_or(Error::WriteResultUnknown)?)
                .map_err(|_| Error::WriteResultUnknown)?;
            if let Some(info) = data.bytes(14) {
                let info = Fields::parse(info).map_err(|_| Error::WriteResultUnknown)?;
                if info.text(3).map_err(|_| Error::WriteResultUnknown)? == "1" {
                    return Err(Error::Challenge);
                }
            }
            let id = data
                .text(3)
                .map_err(|_| Error::WriteResultUnknown)?
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0);
            (*thread_id, id)
        }
    };
    Ok(ActionReceipt {
        target: prepared.target.clone(),
        thread_id,
        post_id,
        actor_uid: prepared.actor_uid,
        title: prepared.title.clone(),
        message: prepared.message.clone(),
        images: prepared.images.clone(),
        attempted_at: prepared.created_at,
    })
}
impl AuthClient {
    pub fn resolve_forum(&self, session: &mut Session, name: &str) -> Result<Forum> {
        let api = Client::new(Config {
            bduss: Some(session.bduss().into()),
            ..Default::default()
        })?;
        Ok(api.forum_threads(name, 1, 1, 6, false)?.forum)
    }
    pub fn upload_image(
        &self,
        session: &mut Session,
        forum_id: u64,
        bytes: &[u8],
        mime: &str,
    ) -> Result<UploadedImage> {
        if forum_id == 0
            || bytes.is_empty()
            || bytes.len() > 2 * 1024 * 1024
            || !match mime {
                "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                "image/jpeg" => bytes.starts_with(&[255, 216, 255]),
                _ => false,
            }
        {
            return Err(Error::InvalidConfig);
        }
        parse_uploaded(
            &self.upload_feature_image(session, forum_id, bytes, mime)?,
            forum_id,
        )
    }
    pub fn prepare_thread(
        &self,
        session: &mut Session,
        forum: &str,
        title: &str,
        message: &str,
        images: Vec<UploadedImage>,
    ) -> Result<PreparedAction> {
        crate::discovery::text_input(title, 300)?;
        validate_text(message)?;
        let actor = self.self_identity(session)?;
        let forum = self.resolve_forum(session, forum)?;
        let tbs = self.reply_tbs(session)?;
        let body = thread_form(&forum, title, message, &tbs, &images)?;
        Ok(PreparedAction {
            target: ActionTarget::NewThread {
                forum_id: forum.id,
                forum_name: forum.name,
            },
            title: title.into(),
            message: message.into(),
            identity: session.stable_identity(),
            actor_uid: actor.user_id,
            body,
            created_at: now()?,
            images,
            actor_display: actor.name.unwrap_or_default(),
        })
    }
    pub fn prepare_floor_reply(
        &self,
        session: &mut Session,
        thread: ThreadId,
        parent: u64,
        subpost: Option<u64>,
        message: &str,
    ) -> Result<PreparedAction> {
        validate_text(message)?;
        thread.validate()?;
        if parent == 0 || subpost == Some(0) {
            return Err(Error::InvalidId);
        }
        let actor = self.self_identity(session)?;
        let api = Client::new(Config {
            bduss: Some(session.bduss().into()),
            ..Default::default()
        })?;
        let page = if let Some(selected) = subpost {
            api.comment_context(&thread, selected, Some(parent))?
        } else {
            api.comments(&thread, parent, 1)?
        }
        .data;
        if page.parent_post_id != parent || page.forum.id == 0 {
            return Err(Error::Schema("floor target mismatch"));
        }
        let reply_uid = if let Some(selected) = subpost {
            page.comments
                .iter()
                .find(|p| p.id == selected)
                .ok_or(Error::Schema("selected subreply not returned"))?
                .author
                .id
        } else {
            page.parent_post
                .as_ref()
                .ok_or(Error::Schema("selected floor not returned"))?
                .author
                .id
        };
        let target = ActionTarget::FloorReply {
            thread_id: thread.0,
            forum_id: page.forum.id,
            forum_name: page.forum.name,
            parent_post_id: parent,
            subpost_id: subpost,
            reply_uid,
        };
        let tbs = self.reply_tbs(session)?;
        let body = floor_request(
            session.bduss(),
            &tbs,
            &target,
            message,
            actor.name.as_deref().unwrap_or(""),
            &[],
        )?;
        Ok(PreparedAction {
            target,
            title: String::new(),
            message: message.into(),
            identity: session.stable_identity(),
            actor_uid: actor.user_id,
            body,
            created_at: now()?,
            images: Vec::new(),
            actor_display: actor.name.unwrap_or_default(),
        })
    }
    /// Attach forum-bound uploaded images to an already resolved exact floor.
    /// This only refreshes the CSRF token; it never changes the target or submits.
    pub fn attach_floor_images(
        &self,
        session: &mut Session,
        mut prepared: PreparedAction,
        images: Vec<UploadedImage>,
    ) -> Result<PreparedAction> {
        if prepared.identity != session.stable_identity()
            || now()?.saturating_sub(prepared.created_at) > 300
            || !matches!(prepared.target, ActionTarget::FloorReply { .. })
        {
            return Err(Error::InvalidConfig);
        }
        if !images.is_empty() {
            let tbs = self.reply_tbs(session)?;
            prepared.body = floor_request(
                session.bduss(),
                &tbs,
                &prepared.target,
                &prepared.message,
                &prepared.actor_display,
                &images,
            )?;
            prepared.images = images;
        }
        Ok(prepared)
    }
    pub fn send_action(
        &self,
        session: &mut Session,
        prepared: PreparedAction,
    ) -> Result<ActionReceipt> {
        if prepared.identity != session.stable_identity()
            || now()?.saturating_sub(prepared.created_at) > 300
        {
            return Err(Error::InvalidConfig);
        }
        let is_floor = matches!(prepared.target, ActionTarget::FloorReply { .. });
        let raw = self.submit_feature(
            session,
            if is_floor {
                "/c/c/post/add?cmd=309731"
            } else {
                "/f/commit/thread/add"
            },
            &prepared.body,
            is_floor,
        )?;
        parse_action(&raw, &prepared)
    }
    pub fn verify_action(&self, session: &mut Session, receipt: &ActionReceipt) -> Result<bool> {
        let actor = self.self_identity(session)?;
        if actor.user_id != receipt.actor_uid {
            return Err(Error::Schema("action verification account mismatch"));
        }
        let api = Client::new(Config {
            bduss: Some(session.bduss().into()),
            ..Default::default()
        })?;
        let text = |fragments: &[Fragment]| {
            fragments
                .iter()
                .filter_map(|f| match f {
                    Fragment::Text { text }
                    | Fragment::Mention { text, .. }
                    | Fragment::Link { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        };
        match receipt.target {
            ActionTarget::NewThread { forum_id, .. } => {
                let page = api.thread_page(&ThreadId(receipt.thread_id), 1)?.data;
                let Some(post) = page.posts.iter().find(|p| p.floor == 1) else {
                    return Ok(false);
                };
                Ok(page.forum.id==forum_id&&page.title==receipt.title&&post.author.id==receipt.actor_uid&&post.created_at>=receipt.attempted_at.saturating_sub(30)&&text(&post.fragments)==receipt.message&&receipt.images.iter().all(|image|post.fragments.iter().any(|f|matches!(f,Fragment::Media{resource,..}if resource.url.contains(&image.pic_id)))))
            }
            ActionTarget::FloorReply {
                parent_post_id,
                reply_uid,
                ..
            } => {
                let Some(id) = receipt.post_id else {
                    return Ok(false);
                };
                let page = api
                    .comment_context(&ThreadId(receipt.thread_id), id, Some(parent_post_id))?
                    .data;
                Ok(page.parent_post_id == parent_post_id
                    && page.comments.iter().any(|p| {
                        p.id == id
                            && p.author.id == receipt.actor_uid
                            && p.created_at >= receipt.attempted_at.saturating_sub(30)
                            && text(&p.fragments) == receipt.message
                            && receipt.images.iter().all(|image| p.fragments.iter().any(|f| matches!(f,Fragment::Media{resource,..} if resource.url.contains(&image.pic_id))))
                            && p.reply_to_user_id.is_none_or(|uid| uid == reply_uid)
                    }))
            }
        }
    }
}
