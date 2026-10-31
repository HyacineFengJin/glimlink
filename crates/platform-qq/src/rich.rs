// SPDX-License-Identifier: AGPL-3.0-only
//! Typed QQ content and native mentions/replies/faces. Based on pinned message/*.proto.
use crate::{
    Result,
    directory::{num, text},
    wire::{self, Fields, Value},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

pub fn valid_uid(uid: &str) -> bool {
    !uid.is_empty()
        && uid.len() <= 128
        && uid.is_ascii()
        && !uid
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mention {
    pub uid: String,
    pub display: String,
    #[serde(default)]
    pub all: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quote {
    pub sequence: u32,
    pub sender_uin: u32,
    pub sender_uid: String,
    pub timestamp: u32,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    Mention {
        uid: String,
        display: String,
        all: bool,
    },
    Face {
        id: u32,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    #[serde(default)]
    pub faces: Vec<u32>,
    #[serde(default)]
    pub quote: Option<Quote>,
    #[serde(default)]
    pub images: Vec<crate::upload::NativeImage>,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub media: Vec<crate::attachments::NativeMedia>,
    #[serde(default)]
    pub file: Option<crate::attachments::NativeFile>,
}
impl Message {
    pub fn plain(s: &str) -> Self {
        Self {
            text: s.into(),
            ..Default::default()
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.text.len() > 16384
            || self.text.contains('\0')
            || self.mentions.len() > 16
            || self.faces.len() > 32
            || self.faces.iter().any(|id| *id > 65535)
            || self.images.len() > 4
            || self.parts.len() > 64
            || self.media.len() > 1
        {
            return Err("QQ rich message exceeds bounds".into());
        }
        if self.text.is_empty()
            && self.mentions.is_empty()
            && self.faces.is_empty()
            && self.images.is_empty()
            && self.parts.is_empty()
            && self.media.is_empty()
            && self.file.is_none()
        {
            return Err("QQ rich message is empty".into());
        }
        for m in &self.mentions {
            if m.display.is_empty()
                || m.display.len() > 512
                || m.display.contains('\0')
                || (!m.all && !valid_uid(&m.uid))
                || (m.all && !m.uid.is_empty())
            {
                return Err("QQ mention requires a resolved UID and bounded display".into());
            }
        }
        let mut bytes = self.text.len();
        for part in &self.parts {
            let message = match part {
                Part::Text { text } => {
                    bytes += text.len();
                    Message::plain(text)
                }
                Part::Mention { uid, display, all } => {
                    bytes += display.len();
                    Message {
                        mentions: vec![Mention {
                            uid: uid.clone(),
                            display: display.clone(),
                            all: *all,
                        }],
                        ..Default::default()
                    }
                }
                Part::Face { id } => Message {
                    faces: vec![*id],
                    ..Default::default()
                },
            };
            message.validate()?;
        }
        if bytes > 16384 {
            return Err("QQ rich message total text exceeds bound".into());
        }
        if let Some(q) = &self.quote {
            if q.sequence == 0
                || q.sender_uin == 0
                || q.timestamp == 0
                || !valid_uid(&q.sender_uid)
                || q.text.len() > 2048
                || q.text.contains('\0')
            {
                return Err("QQ quote lacks exact source identity".into());
            }
        }
        Ok(())
    }
    pub(crate) fn elements(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut rich = Vec::new();
        if let Some(q) = &self.quote {
            let mut src = Vec::new();
            wire::number(1, q.sequence.into(), &mut src);
            wire::number(2, q.sender_uin.into(), &mut src);
            wire::number(3, (q.timestamp as i32) as i64 as u64, &mut src);
            let mut txt = Vec::new();
            wire::bytes(1, q.text.as_bytes(), &mut txt);
            let mut e = Vec::new();
            wire::bytes(1, &txt, &mut e);
            wire::bytes(5, &e, &mut src);
            let mut reserve = Vec::new();
            wire::number(3, q.sequence.into(), &mut reserve);
            wire::bytes(7, q.sender_uid.as_bytes(), &mut reserve);
            wire::bytes(8, &reserve, &mut src);
            wire::number(10, 0, &mut src);
            let mut e = Vec::new();
            wire::bytes(45, &src, &mut e);
            wire::bytes(2, &e, &mut rich)
        }
        for m in &self.mentions {
            let mut reserve = Vec::new();
            wire::number(3, if m.all { 1 } else { 2 }, &mut reserve);
            wire::number(4, 0, &mut reserve);
            wire::number(5, 0, &mut reserve);
            wire::bytes(9, m.uid.as_bytes(), &mut reserve);
            let mut txt = Vec::new();
            wire::bytes(1, m.display.as_bytes(), &mut txt);
            wire::bytes(12, &reserve, &mut txt);
            let mut e = Vec::new();
            wire::bytes(1, &txt, &mut e);
            wire::bytes(2, &e, &mut rich)
        }
        if !self.text.is_empty() {
            let mut txt = Vec::new();
            wire::bytes(1, self.text.as_bytes(), &mut txt);
            let mut e = Vec::new();
            wire::bytes(1, &txt, &mut e);
            wire::bytes(2, &e, &mut rich)
        }
        for id in &self.faces {
            let mut face = Vec::new();
            wire::number(1, (*id).into(), &mut face);
            let mut e = Vec::new();
            wire::bytes(2, &face, &mut e);
            wire::bytes(2, &e, &mut rich)
        }
        for part in &self.parts {
            let message = match part {
                Part::Text { text } => Message::plain(text),
                Part::Mention { uid, display, all } => Message {
                    mentions: vec![Mention {
                        uid: uid.clone(),
                        display: display.clone(),
                        all: *all,
                    }],
                    ..Default::default()
                },
                Part::Face { id } => Message {
                    faces: vec![*id],
                    ..Default::default()
                },
            };
            rich.extend_from_slice(&message.elements()?);
        }
        for image in &self.images {
            let msg = decode64(&image.msg_info)?;
            let compat = decode64(&image.compat)?;
            Fields::parse(&msg)?;
            Fields::parse(&compat)?;
            let mut old = Vec::new();
            wire::bytes(if image.group { 8 } else { 4 }, &compat, &mut old);
            wire::bytes(2, &old, &mut rich);
            let mut common = Vec::new();
            wire::number(1, 48, &mut common);
            wire::bytes(2, &msg, &mut common);
            wire::number(3, if image.group { 20 } else { 10 }, &mut common);
            let mut e = Vec::new();
            wire::bytes(53, &common, &mut e);
            wire::bytes(2, &e, &mut rich);
        }
        for media in &self.media {
            let msg = decode64(&media.msg_info)?;
            Fields::parse(&msg)?;
            let mut common = Vec::new();
            wire::number(1, 48, &mut common);
            wire::bytes(2, &msg, &mut common);
            wire::number(
                3,
                match media.kind {
                    MediaKind::Voice => 22,
                    MediaKind::Video => {
                        if media.group {
                            21
                        } else {
                            11
                        }
                    }
                    _ => return Err("QQ native media kind invalid".into()),
                },
                &mut common,
            );
            let mut e = Vec::new();
            wire::bytes(53, &common, &mut e);
            wire::bytes(2, &e, &mut rich)
        }
        Ok(rich)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Voice,
    Video,
    File,
}
/// Private resolver input; host must replace this with an opaque media_ref in public output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaLocator {
    pub kind: MediaKind,
    pub node: Option<String>,
    pub url: Option<String>,
    pub file_uuid: Option<String>,
    pub file_hash: Option<String>,
    pub group: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Segment {
    Text {
        text: String,
    },
    Mention {
        uid: String,
        display: String,
        all: bool,
    },
    Face {
        id: u32,
    },
    Reply {
        sequence: u32,
        sender_uin: u32,
        timestamp: u32,
        text: String,
    },
    Media {
        kind: MediaKind,
        name: String,
        size: Option<u64>,
        width: Option<u32>,
        height: Option<u32>,
        duration_ms: Option<u64>,
        locator: MediaLocator,
    },
    Unknown {
        field: u32,
    },
}
fn encode64(v: &[u8]) -> Result<String> {
    if v.len() > 16384 {
        return Err("QQ media node too large".into());
    }
    Ok(STANDARD.encode(v))
}
pub(crate) fn decode64(v: &str) -> Result<Vec<u8>> {
    if v.len() > 24576 {
        return Err("QQ media node too large".into());
    }
    STANDARD
        .decode(v)
        .map_err(|_| "invalid QQ media node".into())
}
fn txt(bytes: Option<&[u8]>, tag: u32, max: usize) -> Result<String> {
    text(&Fields::parse(bytes.unwrap_or_default())?, tag, max)
}
pub fn parse_elements(rich: &[u8], group: Option<u32>) -> Result<Vec<Segment>> {
    let f = Fields::parse(rich)?;
    let mut out = Vec::new();
    for (tag, value) in f.0 {
        if matches!(tag, 3 | 4) {
            let Value::Bytes(raw) = value else {
                return Err("QQ body attachment malformed".into());
            };
            let data = Fields::parse(raw)?;
            if tag == 3 {
                if let Some(media) = file_segment(&data, group)? {
                    out.push(media)
                }
            } else {
                let uuid = if group.is_some() {
                    text(&data, 18, 2048)?
                } else {
                    text(&data, 3, 2048)?
                };
                if !uuid.is_empty() {
                    let mut node = Vec::new();
                    wire::bytes(2, uuid.as_bytes(), &mut node);
                    out.push(Segment::Media {
                        kind: MediaKind::Voice,
                        name: text(&data, 5, 512)?,
                        size: Some(u64::from(num(&data, 6)?)),
                        width: None,
                        height: None,
                        duration_ms: Some(u64::from(num(&data, 19)?) * 1000),
                        locator: MediaLocator {
                            kind: MediaKind::Voice,
                            node: Some(encode64(&node)?),
                            url: None,
                            file_uuid: None,
                            file_hash: None,
                            group,
                        },
                    })
                }
            }
            continue;
        }
        if tag != 2 {
            continue;
        }
        if out.len() >= 64 {
            return Err("QQ rich element count exceeds64".into());
        }
        let Value::Bytes(raw) = value else {
            return Err("QQ rich element malformed".into());
        };
        let e = Fields::parse(raw)?;
        for (tag, v) in e.0 {
            if out.len() >= 64 {
                return Err("QQ rich segment count exceeds64".into());
            }
            let Value::Bytes(b) = v else { continue };
            let data = Fields::parse(b)?;
            match tag {
                1 => {
                    let value = data.data(1).unwrap_or_default();
                    let display = String::from_utf8_lossy(value)
                        .chars()
                        .take(2048)
                        .collect::<String>();
                    if let Some(r) = data.data(12) {
                        let reserve = Fields::parse(r)?;
                        let typ = num(&reserve, 3)?;
                        if matches!(typ, 1 | 2) {
                            out.push(Segment::Mention {
                                uid: text(&reserve, 9, 128)?,
                                display,
                                all: typ == 1,
                            });
                            continue;
                        }
                    }
                    out.push(Segment::Text { text: display })
                }
                2 => out.push(Segment::Face { id: num(&data, 1)? }),
                45 => {
                    let mut quoted = String::new();
                    for (n, v) in &data.0 {
                        if *n == 5 {
                            if let Value::Bytes(v) = v {
                                let e = Fields::parse(v)?;
                                quoted.push_str(&txt(e.data(1), 1, 2048)?);
                                if quoted.len() > 2048 {
                                    return Err("QQ quoted text exceedsbound".into());
                                }
                            }
                        }
                    }
                    let sequence = match data.0.iter().find(|(f, _)| *f == 1).map(|(_, v)| v) {
                        Some(Value::Number(n)) => {
                            u32::try_from(*n).map_err(|_| "QQ quote sequence overflow")?
                        }
                        Some(Value::Bytes(packed)) => {
                            let mut value = 0u32;
                            let mut shift = 0;
                            let mut ended = false;
                            for b in packed.iter().take(5) {
                                if shift == 28 && *b > 15 {
                                    return Err("QQ quote sequence overflow".into());
                                }
                                value |= u32::from(b & 127) << shift;
                                if b & 128 == 0 {
                                    ended = true;
                                    break;
                                }
                                shift += 7
                            }
                            if !ended {
                                return Err("QQ quote sequence malformed".into());
                            }
                            value
                        }
                        _ => 0,
                    };
                    out.push(Segment::Reply {
                        sequence,
                        sender_uin: num(&data, 2)?,
                        timestamp: num(&data, 3)?,
                        text: quoted,
                    })
                }
                5 if num(&data, 1)? == 24 && group.is_some() => {
                    let payload = data.data(2).ok_or("QQ group file payload missing")?;
                    if payload.len() < 3 {
                        return Err("QQ group file header truncated".into());
                    }
                    let length = u16::from_be_bytes(payload[1..3].try_into().unwrap()) as usize;
                    let extra = Fields::parse(
                        payload
                            .get(3..3 + length)
                            .ok_or("QQ group file payload truncated")?,
                    )?;
                    let inner =
                        Fields::parse(extra.data(7).ok_or("QQ group file metadata missing")?)?;
                    let info = Fields::parse(inner.data(2).ok_or("QQ group file info missing")?)?;
                    let uuid = text(&info, 2, 2048)?;
                    if uuid.is_empty() {
                        return Err("QQ group file ID missing".into());
                    }
                    out.push(Segment::Media {
                        kind: MediaKind::File,
                        name: text(&info, 4, 512)?,
                        size: info.num(3),
                        width: None,
                        height: None,
                        duration_ms: None,
                        locator: MediaLocator {
                            kind: MediaKind::File,
                            node: None,
                            url: None,
                            file_uuid: Some(uuid),
                            file_hash: None,
                            group,
                        },
                    })
                }
                19 => {
                    let mut info = Vec::new();
                    wire::number(1, num(&data, 6)?.into(), &mut info);
                    wire::bytes(
                        2,
                        data.data(2)
                            .unwrap_or_default()
                            .iter()
                            .map(|v| format!("{v:02x}"))
                            .collect::<String>()
                            .as_bytes(),
                        &mut info,
                    );
                    wire::bytes(4, text(&data, 3, 512)?.as_bytes(), &mut info);
                    let mut node = Vec::new();
                    wire::bytes(1, &info, &mut node);
                    wire::bytes(2, text(&data, 1, 2048)?.as_bytes(), &mut node);
                    out.push(Segment::Media {
                        kind: MediaKind::Video,
                        name: text(&data, 3, 512)?,
                        size: Some(u64::from(num(&data, 6)?)),
                        width: Some(num(&data, 16)?),
                        height: Some(num(&data, 17)?),
                        duration_ms: Some(u64::from(num(&data, 5)?) * 1000),
                        locator: MediaLocator {
                            kind: MediaKind::Video,
                            node: Some(encode64(&node)?),
                            url: None,
                            file_uuid: None,
                            file_hash: None,
                            group,
                        },
                    })
                }
                4 | 8 => {
                    let (size, width, height, name, url) = if tag == 8 {
                        (
                            num(&data, 25)?,
                            num(&data, 22)?,
                            num(&data, 23)?,
                            text(&data, 2, 512)?,
                            text(&data, 16, 4096)?,
                        )
                    } else {
                        (
                            num(&data, 2)?,
                            num(&data, 9)?,
                            num(&data, 8)?,
                            text(&data, 1, 512)?,
                            text(&data, 15, 4096)?,
                        )
                    };
                    let md5 = data.data(if tag == 8 { 13 } else { 7 });
                    let url = if !url.is_empty() {
                        Some(if url.starts_with('/') {
                            format!(
                                "https://{}{url}",
                                if url.contains("rkey") {
                                    "multimedia.nt.qq.com.cn"
                                } else {
                                    "gchat.qpic.cn"
                                }
                            )
                        } else {
                            url
                        })
                    } else {
                        md5.filter(|b| b.len() == 16).map(|b| {
                            format!(
                                "https://gchat.qpic.cn/gchatpic_new/0/0-0-{}/0",
                                b.iter().map(|v| format!("{v:02X}")).collect::<String>()
                            )
                        })
                    };
                    out.push(Segment::Media {
                        kind: MediaKind::Image,
                        name,
                        size: Some(size.into()),
                        width: Some(width),
                        height: Some(height),
                        duration_ms: None,
                        locator: MediaLocator {
                            kind: MediaKind::Image,
                            node: None,
                            url,
                            file_uuid: None,
                            file_hash: md5
                                .filter(|v| v.len() == 16)
                                .map(|v| v.iter().map(|b| format!("{b:02x}")).collect()),
                            group,
                        },
                    })
                }
                53 if num(&data, 1)? == 33 => {
                    let face = Fields::parse(data.data(2).ok_or("QQ face metadata missing")?)?;
                    out.push(Segment::Face { id: num(&face, 1)? })
                }
                53 if num(&data, 1)? == 48 => {
                    let business = num(&data, 3)?;
                    let kind = match business {
                        10 | 20 => MediaKind::Image,
                        12 | 22 => MediaKind::Voice,
                        11 | 21 => MediaKind::Video,
                        _ => {
                            out.push(Segment::Unknown { field: tag });
                            continue;
                        }
                    };
                    let info = Fields::parse(data.data(2).unwrap_or_default())?;
                    let body = Fields::parse(info.data(1).unwrap_or_default())?;
                    let node = body.data(1).ok_or("QQ NT media lacks index")?;
                    let index = Fields::parse(node)?;
                    let fi = Fields::parse(index.data(1).unwrap_or_default())?;
                    out.push(Segment::Media {
                        kind: kind.clone(),
                        name: text(&fi, 4, 512)?,
                        size: Some(num(&fi, 1)?.into()),
                        width: Some(num(&fi, 6)?),
                        height: Some(num(&fi, 7)?),
                        duration_ms: Some(u64::from(num(&fi, 8)?) * 1000),
                        locator: MediaLocator {
                            kind,
                            node: Some(encode64(node)?),
                            url: None,
                            file_uuid: None,
                            file_hash: Some(text(&fi, 2, 64)?).filter(|s| !s.is_empty()),
                            group,
                        },
                    })
                }
                16 | 37 => {}
                _ => out.push(Segment::Unknown { field: tag }),
            }
        }
    }
    // NT messages commonly include an old-client image element followed by its
    // NT element. Expose one lazy reference when the actual file hashes agree.
    let native: std::collections::BTreeSet<String> = out
        .iter()
        .filter_map(|segment| match segment {
            Segment::Media {
                kind: MediaKind::Image,
                locator,
                ..
            } if locator.node.is_some() => {
                locator.file_hash.as_ref().map(|h| h.to_ascii_lowercase())
            }
            _ => None,
        })
        .collect();
    out.retain(|segment|!matches!(segment,Segment::Media{kind:MediaKind::Image,locator,..} if locator.node.is_none() && locator.file_hash.as_ref().is_some_and(|h|native.contains(&h.to_ascii_lowercase()))));
    Ok(out)
}
fn file_segment(data: &Fields<'_>, group: Option<u32>) -> Result<Option<Segment>> {
    let uuid = text(data, 3, 2048)?;
    let hash = text(data, 57, 2048)?;
    if uuid.is_empty() || hash.is_empty() {
        return Ok(None);
    }
    Ok(Some(Segment::Media {
        kind: MediaKind::File,
        name: text(data, 5, 512)?,
        size: data.num(6),
        width: None,
        height: None,
        duration_ms: None,
        locator: MediaLocator {
            kind: MediaKind::File,
            node: None,
            url: None,
            file_uuid: Some(uuid),
            file_hash: Some(hash),
            group,
        },
    }))
}
pub fn parse_body_file(body: &[u8], group: Option<u32>) -> Result<Option<Segment>> {
    let f = Fields::parse(body)?;
    let Some(extra) = f.data(2) else {
        return Ok(None);
    };
    let extra = Fields::parse(extra)?;
    let Some(file) = extra.data(1) else {
        return Ok(None);
    };
    file_segment(&Fields::parse(file)?, group)
}
