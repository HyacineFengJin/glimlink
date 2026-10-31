// SPDX-License-Identifier: MIT
//! Native segmented danmaku protobuf projection; no protobuf VM or browser.
use crate::{Client, Error, Result, VideoId};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DanmakuItem {
    pub id: String,
    pub progress_ms: u64,
    pub mode: u32,
    pub font_size: u32,
    pub color: u32,
    pub content: String,
    pub sent_at: u64,
}
fn varint(raw: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for i in 0..10 {
        let c = *raw.get(*pos).ok_or(Error::Schema("danmaku varint"))?;
        *pos += 1;
        if i == 9 && c > 1 {
            return Err(Error::Schema("danmaku varint overflow"));
        }
        v |= u64::from(c & 127) << (i * 7);
        if c < 128 {
            return Ok(v);
        }
    }
    Err(Error::Schema("danmaku varint length"))
}
fn field<'a>(raw: &'a [u8], pos: &mut usize) -> Result<(u64, u64, Option<&'a [u8]>, u64)> {
    let tag = varint(raw, pos)?;
    let number = tag >> 3;
    if number == 0 {
        return Err(Error::Schema("danmaku field"));
    }
    let wire = tag & 7;
    match wire {
        0 => Ok((number, wire, None, varint(raw, pos)?)),
        1 | 5 => {
            let n = if wire == 1 { 8 } else { 4 };
            *pos = pos
                .checked_add(n)
                .filter(|n| *n <= raw.len())
                .ok_or(Error::Schema("danmaku fixed field"))?;
            Ok((number, wire, None, 0))
        }
        2 => {
            let n = usize::try_from(varint(raw, pos)?).map_err(|_| Error::JsonLimit)?;
            let end = pos
                .checked_add(n)
                .filter(|n| *n <= raw.len())
                .ok_or(Error::Schema("danmaku bytes"))?;
            let b = &raw[*pos..end];
            *pos = end;
            Ok((number, wire, Some(b), 0))
        }
        _ => Err(Error::Schema("danmaku wire type")),
    }
}
fn element(raw: &[u8]) -> Result<DanmakuItem> {
    let (mut pos, mut id, mut progress, mut mode, mut font, mut color, mut content, mut ctime) =
        (0, 0, 0, 1, 25, 0xffffff, String::new(), 0);
    let mut id_string = None;
    while pos < raw.len() {
        let (n, w, b, v) = field(raw, &mut pos)?;
        match (n, w) {
            (1, 0) => id = v,
            (2, 0) => progress = v,
            (3, 0) => mode = u32::try_from(v).map_err(|_| Error::JsonLimit)?,
            (4, 0) => font = u32::try_from(v).map_err(|_| Error::JsonLimit)?,
            (5, 0) => color = u32::try_from(v).map_err(|_| Error::JsonLimit)?,
            (7, 2) => {
                let b = b.unwrap();
                if b.len() > 16384 {
                    return Err(Error::JsonLimit);
                }
                content = std::str::from_utf8(b)
                    .map_err(|_| Error::Schema("danmaku UTF8"))?
                    .into()
            }
            (8, 0) => ctime = v,
            (12, 2) => {
                let b = b.unwrap();
                if b.len() > 32 {
                    return Err(Error::JsonLimit);
                }
                id_string = Some(
                    std::str::from_utf8(b)
                        .map_err(|_| Error::Schema("danmaku ID"))?
                        .to_owned(),
                )
            }
            _ => {}
        }
    }
    let id = id_string
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| id.to_string());
    if id == "0" || !id.bytes().all(|c| c.is_ascii_digit()) || content.is_empty() {
        return Err(Error::Schema("danmaku required fields"));
    }
    Ok(DanmakuItem {
        id,
        progress_ms: progress,
        mode,
        font_size: font,
        color,
        content,
        sent_at: ctime,
    })
}
fn parse(raw: &[u8]) -> Result<Vec<DanmakuItem>> {
    if raw.len() > 2 * 1024 * 1024 {
        return Err(Error::JsonLimit);
    }
    let mut pos = 0;
    let mut items = Vec::new();
    while pos < raw.len() {
        let (n, w, b, _) = field(raw, &mut pos)?;
        if (n, w) == (1, 2) {
            if items.len() >= 6000 {
                return Err(Error::JsonLimit);
            }
            items.push(element(b.unwrap())?)
        }
    }
    Ok(items)
}
impl Client {
    pub fn danmaku_segment(
        &self,
        video: &VideoId,
        page: u32,
        segment: u32,
    ) -> Result<Vec<DanmakuItem>> {
        if segment == 0 || segment > 10000 {
            return Err(Error::InvalidPage);
        }
        let info = self.video_info(video)?;
        let part = info
            .pages
            .iter()
            .find(|p| p.page == page)
            .ok_or(Error::InvalidPage)?;
        if u64::from(segment - 1) * 360 > part.duration_seconds {
            return Err(Error::InvalidPage);
        }
        let raw = self.transport.api_bytes(&format!(
            "https://api.bilibili.com/x/v2/dm/web/seg.so?type=1&oid={}&segment_index={segment}",
            part.cid
        ))?;
        parse(&raw)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projects_native_payload_without_author_hash() {
        let raw = [
            10, 16, 8, 1, 16, 150, 1, 24, 1, 58, 3, b'm', b'e', b'w', 64, 100, 96, 1,
        ];
        let r = parse(&raw).unwrap();
        assert_eq!(r[0].id, "1");
        assert_eq!(r[0].progress_ms, 150);
        assert_eq!(r[0].content, "mew");
    }
    #[test]
    fn malformed_or_unbounded_data_is_rejected() {
        for raw in [vec![10, 255], vec![10, 2, 8, 255], vec![0], vec![15]] {
            assert!(parse(&raw).is_err());
        }
    }
}
