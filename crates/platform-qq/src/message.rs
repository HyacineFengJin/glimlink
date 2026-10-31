// SPDX-License-Identifier: AGPL-3.0-only
// Adapted from the Agent Runtime QQ Rust experiment; protocol references in NOTICE.
//! Plain-text MessageSvc.PbSendMsg encoding and acknowledgement metadata.
//!
//! This module never opens a connection, retries a send, or logs message text.
//! Wire reference: pinned LagrangeGo f7f8abfaa7f1, client/message.go.
use crate::{
    Result,
    wire::{self, Fields, Value},
};
use serde::{Deserialize, Serialize};

pub const SEND_COMMAND: &str = "MessageSvc.PbSendMsg";
/// An adapter memory bound, not a claim about QQ's server-side text limit.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// NTQQ UID, not the numeric QQ account number.
    Private(&'a str),
    /// Numeric group code used by RoutingHead.Grp.GroupCode.
    Group(u32),
}

/// `client_sequence` is the current session sequence modulo 0x8000, before
/// allocating the subsequent SSO request sequence. The caller must allocate and
/// persist one random value per send attempt and must not retry unknown results.
pub fn encode_text(
    target: Target<'_>,
    text: &str,
    client_sequence: u32,
    random: u32,
    unix_seconds: u32,
) -> Result<Vec<u8>> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err("text is empty or exceeds the adapter byte limit".into());
    }
    let mut destination = Vec::new();
    let mut route = Vec::new();
    match target {
        Target::Private(uid) => {
            if uid.is_empty()
                || uid.len() > 128
                || !uid.is_ascii()
                || uid
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            {
                return Err("private target must be a nonempty NTQQ UID".into());
            }
            wire::bytes(2, uid.as_bytes(), &mut destination);
            wire::bytes(1, &destination, &mut route);
        }
        Target::Group(code) => {
            if code == 0 {
                return Err("group code must be nonzero".into());
            }
            wire::number(1, u64::from(code), &mut destination);
            wire::bytes(2, &destination, &mut route);
        }
    }

    // Type=1; optional SubType and DivSeq must be present even when zero.
    let mut content = Vec::new();
    wire::number(1, 1, &mut content);
    wire::number(2, 0, &mut content);
    wire::number(3, 0, &mut content);

    // Message.Body.RichText.Elems[].Text.Str. No mention/card interpretation.
    let mut plain_text = Vec::new();
    wire::bytes(1, text.as_bytes(), &mut plain_text);
    let mut element = Vec::new();
    wire::bytes(1, &plain_text, &mut element);
    let mut rich_text = Vec::new();
    wire::bytes(2, &element, &mut rich_text);
    let mut body = Vec::new();
    wire::bytes(1, &rich_text, &mut body);

    let mut request = Vec::new();
    wire::bytes(1, &route, &mut request);
    wire::bytes(2, &content, &mut request);
    wire::bytes(3, &body, &mut request);
    wire::number(4, u64::from(client_sequence), &mut request);
    wire::number(5, u64::from(random), &mut request);
    if matches!(target, Target::Group(_)) {
        let mut control = Vec::new();
        // MsgFlag is int32, not uint32 or zigzag sint32. The Go reference
        // omits its default zero but preserves the optional Ctrl message.
        if unix_seconds != 0 {
            wire::number(1, (unix_seconds as i32) as i64 as u64, &mut control);
        }
        wire::bytes(12, &control, &mut request);
    }
    Ok(request)
}

pub fn encode_rich(
    target: Target<'_>,
    content: &crate::rich::Message,
    client_sequence: u32,
    random: u32,
    unix_seconds: u32,
) -> Result<Vec<u8>> {
    if (!content.mentions.is_empty()
        || content
            .parts
            .iter()
            .any(|p| matches!(p, crate::rich::Part::Mention { .. })))
        && !matches!(target, Target::Group(_))
    {
        return Err("QQ native mentions require a group destination".into());
    }
    if content
        .images
        .iter()
        .any(|image| image.group != matches!(target, Target::Group(_)))
        || content
            .media
            .iter()
            .any(|media| media.group != matches!(target, Target::Group(_)))
    {
        return Err("QQ image metadata belongs to another destination kind".into());
    }
    let plain = encode_text(target, "x", client_sequence, random, unix_seconds)?;
    let rich = content.elements()?;
    let mut body = Vec::new();
    wire::bytes(1, &rich, &mut body);
    if let Some(file) = &content.file {
        if !matches!(target, Target::Private(_)) {
            return Err("QQ file message requires a private destination".into());
        }
        wire::bytes(2, &file.content(unix_seconds)?, &mut body);
    }
    let mut out = Vec::new();
    for (tag, value) in Fields::parse(&plain)?.0 {
        if tag == 1 && content.file.is_some() {
            let Target::Private(uid) = target else {
                unreachable!()
            };
            let mut trans = Vec::new();
            wire::number(2, 4, &mut trans);
            wire::bytes(8, uid.as_bytes(), &mut trans);
            let mut route = Vec::new();
            wire::bytes(15, &trans, &mut route);
            wire::bytes(1, &route, &mut out);
            continue;
        }
        if tag == 3 {
            wire::bytes(3, &body, &mut out);
            continue;
        }
        match value {
            Value::Number(n) => wire::number(tag, n, &mut out),
            Value::Bytes(b) => wire::bytes(tag, b, &mut out),
            Value::Fixed | Value::Fixed32(_) => return Err("unexpected fixed send field".into()),
        }
    }
    Ok(out)
}

/// Server acknowledgement only. It contains no outgoing text or server error
/// string, which could otherwise enter public logs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendResponse {
    pub result: i32,
    pub private_sequence: Option<u32>,
    pub group_sequence: Option<u32>,
    pub timestamp: Option<u32>,
    pub timestamp2: Option<u32>,
}

impl SendResponse {
    /// A zero Result by itself does not establish delivery. Require the
    /// route-specific acknowledgement sequence before marking it confirmed.
    pub fn confirmed_sequence(&self, target: Target<'_>) -> Result<u32> {
        if self.result != 0 {
            return Err(format!("QQ rejected send (result {})", self.result));
        }
        let sequence = match target {
            Target::Private(_) => self.private_sequence,
            Target::Group(_) => self.group_sequence,
        };
        sequence.filter(|n| *n != 0).ok_or_else(||
            "QQ send acknowledgement has no route-specific sequence; delivery unknown, do not retry automatically".into())
    }
}

pub fn decode_response(data: &[u8]) -> Result<SendResponse> {
    let fields = Fields::parse(data)?;
    let mut response = SendResponse {
        result: 0,
        private_sequence: None,
        group_sequence: None,
        timestamp: None,
        timestamp2: None,
    };
    let mut recognized = false;
    for (field, value) in fields.0 {
        if !matches!(field, 1 | 3 | 11 | 12 | 14) {
            continue;
        }
        let Value::Number(raw) = value else {
            return Err("wrong wire type in send acknowledgement".into());
        };
        recognized = true;
        if field == 1 {
            // protobuf int32 negative values use ten-byte sign extension.
            let result = raw as u32 as i32;
            if raw > u64::from(u32::MAX) && raw != result as i64 as u64 {
                return Err("send result outside int32 range".into());
            }
            response.result = result;
            continue;
        }
        let number = u32::try_from(raw).map_err(|_| "send acknowledgement integer overflow")?;
        // Protobuf singular fields use the last occurrence.
        match field {
            3 => response.timestamp = Some(number),
            11 => response.group_sequence = Some(number),
            12 => response.timestamp2 = Some(number),
            14 => response.private_sequence = Some(number),
            _ => unreachable!(),
        }
    }
    if !recognized {
        return Err("missing send acknowledgement metadata".into());
    }
    Ok(response)
}
