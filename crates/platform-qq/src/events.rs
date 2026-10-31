// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded push projection, adapted from the QQ lab; see NOTICE. Field numbers:
//! LagrangeGo f7f8abfaa7f1 client/packets/pb/message/{message,routing}.pb.go.
use crate::{
    Result,
    wire::{Fields, MAX_FRAME, Value},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_VIEW: usize = 32768;
pub(crate) const PUSH: &str = "trpc.msg.olpush.OlPushService.MsgPush";

/// Raw protobuf is deliberately not automatically serializable or debuggable.
pub struct IncomingEvent {
    pub key: String,
    pub command: String,
    pub sso_sequence: u32,
    pub view: Option<View>,
    pub raw: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct View {
    pub kind: u64,
    pub chat: bool,
    pub sequence: Option<u64>,
    pub timestamp: Option<u64>,
    pub message_id: Option<u64>,
    pub new_id: Option<u64>,
    pub from_uin: Option<u64>,
    pub to_uin: Option<u64>,
    pub group_uin: Option<u64>,
    pub from_uid: Option<String>,
    pub to_uid: Option<String>,
    pub text: String,
    /// Escaped, bounded HTML. Never executable source supplied by a sender.
    pub content: String,
    pub text_bytes: usize,
    pub truncated: bool,
    pub unexpanded: bool,
    #[serde(default)]
    pub segments: Vec<crate::rich::Segment>,
}

impl IncomingEvent {
    pub(crate) fn from_push(sequence: u32, raw: Vec<u8>) -> Result<Self> {
        let view = project_push(&raw)?;
        Ok(Self {
            key: format!("qq-push-{:x}", Sha256::digest(&raw)),
            command: PUSH.into(),
            sso_sequence: sequence,
            view,
            raw,
        })
    }
}

fn number(fields: &Fields<'_>, field: u32, max: u64) -> Result<Option<u64>> {
    match fields.0.iter().rev().find(|(f, _)| *f == field) {
        None => Ok(None),
        Some((_, Value::Number(value))) if *value <= max => Ok(Some(*value)),
        _ => Err("invalid push metadata integer".into()),
    }
}
fn uid(fields: &Fields<'_>, field: u32) -> Result<Option<String>> {
    match fields.0.iter().rev().find(|(f, _)| *f == field) {
        None => Ok(None),
        Some((_, Value::Bytes(value))) if value.is_empty() => Ok(None),
        Some((_, Value::Bytes(value)))
            if value.len() <= 128
                && value.is_ascii()
                && !value
                    .iter()
                    .any(|b| b.is_ascii_control() || b.is_ascii_whitespace()) =>
        {
            String::from_utf8(value.to_vec())
                .map(Some)
                .map_err(|_| "invalid push UID".into())
        }
        _ => Err("invalid push UID".into()),
    }
}
fn append(out: &mut String, value: &str) -> bool {
    if value.len() <= MAX_VIEW.saturating_sub(out.len()) {
        out.push_str(value);
        false
    } else {
        true
    }
}
fn append_text(out: &mut String, value: &[u8], escape: bool) -> bool {
    for c in String::from_utf8_lossy(value).chars() {
        let mut encoded = [0; 4];
        let text = if escape {
            match c {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&#39;",
                _ => c.encode_utf8(&mut encoded),
            }
        } else {
            c.encode_utf8(&mut encoded)
        };
        if out.len() + text.len() > MAX_VIEW {
            return true;
        }
        out.push_str(text);
    }
    false
}

/// Decode only documented chat headers and bounded text. Unknown fields remain
/// available in IncomingEvent.raw; missing identities are never inferred.
pub fn project_push(payload: &[u8]) -> Result<Option<View>> {
    if payload.len() > MAX_FRAME {
        return Err("push payload exceeds frame limit".into());
    }
    let push = Fields::parse(payload)?;
    let Some(message) = push.data(1) else {
        return Ok(None);
    };
    let message = Fields::parse(message)?;
    let route = Fields::parse(message.data(1).unwrap_or_default())?;
    let content = Fields::parse(message.data(2).unwrap_or_default())?;
    let group = Fields::parse(route.data(8).unwrap_or_default())?;
    let kind = number(&content, 1, u32::MAX.into())?.unwrap_or(0);
    let chat = matches!(kind, 82 | 141 | 166 | 208 | 529);
    let mut view = View {
        kind,
        chat,
        sequence: number(&content, 5, u32::MAX.into())?,
        timestamp: number(&content, 6, u32::MAX.into())?,
        message_id: number(&content, 4, u32::MAX.into())?,
        new_id: number(&content, 12, u64::MAX)?,
        from_uin: number(&route, 1, u32::MAX.into())?.filter(|id| *id != 0),
        to_uin: number(&route, 5, u32::MAX.into())?.filter(|id| *id != 0),
        group_uin: number(&group, 1, u32::MAX.into())?.filter(|id| *id != 0),
        from_uid: uid(&route, 2)?,
        to_uid: uid(&route, 6)?,
        text: String::new(),
        content: String::new(),
        text_bytes: 0,
        truncated: false,
        unexpanded: false,
        segments: Vec::new(),
    };
    if !chat {
        view.content = "[QQ 事件，原始内容可按需展开]".into();
        view.unexpanded = true;
        return Ok(Some(view));
    }
    let body = Fields::parse(message.data(3).unwrap_or_default())?;
    let rich = Fields::parse(body.data(1).unwrap_or_default())?;
    view.segments = match crate::rich::parse_elements(
        body.data(1).unwrap_or_default(),
        view.group_uin.and_then(|n| u32::try_from(n).ok()),
    ) {
        Ok(segments) => segments,
        Err(_) => {
            view.unexpanded = true;
            vec![crate::rich::Segment::Unknown { field: 0 }]
        }
    };
    if let Ok(Some(file)) = crate::rich::parse_body_file(
        message.data(3).unwrap_or_default(),
        view.group_uin.and_then(|v| u32::try_from(v).ok()),
    ) {
        view.segments.push(file)
    }
    let mut used: usize = 0;
    view.segments.retain(|segment| {
        let bytes = serde_json::to_vec(segment).map_or(usize::MAX, |s| s.len());
        used = used.saturating_add(bytes);
        let keep = used <= 24 * 1024;
        if !keep {
            view.truncated = true;
            view.unexpanded = true
        }
        keep
    });
    for (field, value) in &rich.0 {
        if *field != 2 {
            continue;
        }
        let Value::Bytes(data) = value else {
            return Err("invalid rich-text element".into());
        };
        let element = Fields::parse(data)?;
        if let Some(text) = element.data(1) {
            let text = Fields::parse(text)?;
            if let Some(value) = text.data(1) {
                view.text_bytes = view.text_bytes.saturating_add(value.len());
                view.truncated |= append_text(&mut view.text, value, false);
                view.truncated |= append_text(&mut view.content, value, true);
            }
            if text.0.iter().any(|(field, _)| *field != 1) {
                view.unexpanded = true;
                view.truncated |=
                    append(&mut view.content, "<span>[文本附加信息可按需展开]</span>");
            }
        }
        for (field, _) in &element.0 {
            if *field != 1 {
                view.unexpanded = true;
                view.truncated |= append(
                    &mut view.content,
                    &format!("<span data-kind=\"{field}\">[非文本消息，原始内容可按需展开]</span>"),
                );
            }
        }
    }
    if rich.0.iter().any(|(field, _)| *field != 1 && *field != 2)
        || body.0.iter().any(|(field, _)| *field != 1)
        || view.content.is_empty()
    {
        view.unexpanded = true;
        view.truncated |= append(&mut view.content, "<span>[另有未展开的消息内容]</span>");
    }
    Ok(Some(view))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire;
    fn push(text: &str, identities: bool) -> Vec<u8> {
        let mut route = Vec::new();
        wire::number(1, 12345, &mut route);
        if identities {
            wire::bytes(2, b"u_fixture_only", &mut route);
            wire::bytes(6, b"u_target_only", &mut route);
        }
        let mut content = Vec::new();
        for (field, value) in [(1, 166), (4, 800), (5, 9), (6, 1_700_000_000), (12, 900)] {
            wire::number(field, value, &mut content);
        }
        let mut plain = Vec::new();
        wire::bytes(1, text.as_bytes(), &mut plain);
        let mut element = Vec::new();
        wire::bytes(1, &plain, &mut element);
        wire::bytes(53, &[8, 1], &mut element);
        let mut rich = Vec::new();
        wire::bytes(2, &element, &mut rich);
        let mut body = Vec::new();
        wire::bytes(1, &rich, &mut body);
        let mut msg = Vec::new();
        wire::bytes(1, &route, &mut msg);
        wire::bytes(2, &content, &mut msg);
        wire::bytes(3, &body, &mut msg);
        let mut out = Vec::new();
        wire::bytes(1, &msg, &mut out);
        out
    }
    #[test]
    fn uid_is_only_from_documented_fields_and_ids_time_survive() {
        let raw = push("hello", false);
        let view = project_push(&raw).unwrap().unwrap();
        assert_eq!(view.from_uin, Some(12345));
        assert!(view.from_uid.is_none());
        assert_eq!(
            (view.sequence, view.timestamp, view.message_id, view.new_id),
            (Some(9), Some(1_700_000_000), Some(800), Some(900))
        );
        let view = project_push(&push("hello", true)).unwrap().unwrap();
        assert_eq!(view.from_uid.as_deref(), Some("u_fixture_only"));
        assert_eq!(view.to_uid.as_deref(), Some("u_target_only"));
    }
    #[test]
    fn stable_event_keys_do_not_depend_on_transport_sequence() {
        let raw = push("same", true);
        let a = IncomingEvent::from_push(1, raw.clone()).unwrap();
        let b = IncomingEvent::from_push(2, raw).unwrap();
        assert_eq!(a.key, b.key);
        assert_eq!(a.raw, b.raw);
    }
    #[test]
    fn mixed_media_and_bounded_html_unicode_are_preserved() {
        let view = project_push(&push(&"喵<script>&".repeat(MAX_VIEW), true))
            .unwrap()
            .unwrap();
        assert!(view.truncated && view.unexpanded);
        assert!(view.content.len() <= MAX_VIEW && view.text.len() <= MAX_VIEW);
        assert!(!view.content.contains("<script>"));
        assert!(view.content.starts_with("喵&lt;script&gt;&amp;"));
    }
    #[test]
    fn unknown_events_are_not_parsed_as_chat_text() {
        let mut c = Vec::new();
        wire::number(1, 732, &mut c);
        let mut m = Vec::new();
        wire::bytes(2, &c, &mut m);
        wire::bytes(3, &[255, 255], &mut m);
        let mut p = Vec::new();
        wire::bytes(1, &m, &mut p);
        let v = project_push(&p).unwrap().unwrap();
        assert!(!v.chat && v.unexpanded);
        assert!(v.sequence.is_none());
    }
}
