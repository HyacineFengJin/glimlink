//! Bounded comment image uploads. Preparation reads identity; submit is one POST.
//! Field reference: inorilzy/bilibili-api dynamic.upload_image and comment.send_comment.
use crate::comments::{csrf_from_cookie, picture_url};
use crate::{Client, CommentPicture, CommentTarget, Error, Result, VideoId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;

/// Private payload and CSRF capability; consumed by one upload call.
pub struct PreparedImageUpload {
    target: CommentTarget,
    body: Vec<u8>,
    content_type: String,
    cookie_binding: String,
    bytes: u64,
    width: u32,
    height: u32,
    mime: &'static str,
}
impl fmt::Debug for PreparedImageUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedImageUpload")
            .field("target", &self.target)
            .field("bytes", &self.bytes)
            .field("dimensions", &(self.width, self.height))
            .field("private_payload", &"[redacted]")
            .finish()
    }
}
impl PreparedImageUpload {
    pub fn target(&self) -> &CommentTarget {
        &self.target
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn mime(&self) -> &str {
        self.mime
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ImageUploadReceipt {
    pub image_url: String,
    pub image_width: u32,
    pub image_height: u32,
    /// Kilobytes, as returned by the service.
    pub img_size: f64,
    pub actor_mid: u64,
}
impl fmt::Debug for ImageUploadReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageUploadReceipt")
            .field("image_url", &"[redacted]")
            .field("image_width", &self.image_width)
            .field("image_height", &self.image_height)
            .field("img_size", &self.img_size)
            .field("actor_mid", &self.actor_mid)
            .finish()
    }
}
impl ImageUploadReceipt {
    pub fn into_picture(self) -> Result<UploadedCommentImage> {
        if self.actor_mid == 0 || self.actor_mid > i64::MAX as u64 {
            return Err(Error::InvalidComment);
        }
        let picture = CommentPicture {
            img_src: picture_url(&self.image_url)?,
            img_width: self.image_width,
            img_height: self.image_height,
            img_size: self.img_size,
        };
        picture.validate()?;
        Ok(UploadedCommentImage {
            picture,
            actor_mid: self.actor_mid,
        })
    }
}

/// Persist only in the private account journal; tool callers pass resource IDs.
#[derive(Clone, Serialize, Deserialize)]
pub struct UploadedCommentImage {
    picture: CommentPicture,
    actor_mid: u64,
}
impl fmt::Debug for UploadedCommentImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadedCommentImage")
            .field("actor_mid", &self.actor_mid)
            .field("picture", &"[private locator]")
            .finish()
    }
}
impl UploadedCommentImage {
    pub fn picture(&self) -> &CommentPicture {
        &self.picture
    }
    pub fn actor_mid(&self) -> u64 {
        self.actor_mid
    }
}

pub(crate) fn image_info(bytes: &[u8]) -> Result<(&'static str, u32, u32)> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err(Error::InvalidComment);
    }
    let (mime, width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        if bytes.len() < 33 || bytes[8..12] != 13u32.to_be_bytes() || &bytes[12..16] != b"IHDR" {
            return Err(Error::InvalidComment);
        }
        (
            "image/png",
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
        )
    } else if bytes.starts_with(b"\xff\xd8\xff") && bytes.ends_with(b"\xff\xd9") {
        let mut cursor = 2usize;
        let mut dimensions = None;
        while cursor + 4 <= bytes.len() {
            if bytes[cursor] != 0xff {
                return Err(Error::InvalidComment);
            }
            while bytes.get(cursor) == Some(&0xff) {
                cursor += 1;
            }
            let marker = *bytes.get(cursor).ok_or(Error::InvalidComment)?;
            cursor += 1;
            if marker == 0xda || marker == 0xd9 {
                break;
            }
            if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
                continue;
            }
            let raw = bytes.get(cursor..cursor + 2).ok_or(Error::InvalidComment)?;
            let length = u16::from_be_bytes(raw.try_into().unwrap()) as usize;
            if length < 2 || cursor + length > bytes.len() {
                return Err(Error::InvalidComment);
            }
            if matches!(marker,0xc0..=0xc3|0xc5..=0xc7|0xc9..=0xcb|0xcd..=0xcf) {
                if length < 8 {
                    return Err(Error::InvalidComment);
                }
                let height =
                    u16::from_be_bytes(bytes[cursor + 3..cursor + 5].try_into().unwrap()) as u32;
                let width =
                    u16::from_be_bytes(bytes[cursor + 5..cursor + 7].try_into().unwrap()) as u32;
                dimensions = Some((width, height));
                break;
            }
            cursor += length;
        }
        let (width, height) = dimensions.ok_or(Error::InvalidComment)?;
        ("image/jpeg", width, height)
    } else {
        return Err(Error::InvalidComment);
    };
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || u64::from(width) * u64::from(height) > 8_000_000
    {
        return Err(Error::InvalidComment);
    }
    Ok((mime, width, height))
}

pub(crate) fn multipart(bytes: &[u8], mime: &str, csrf: &str) -> Result<(String, Vec<u8>)> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::InvalidConfig)?
        .as_nanos();
    let boundary = format!("agent-runtime-{}-{nonce}", std::process::id());
    if bytes
        .windows(boundary.len())
        .any(|v| v == boundary.as_bytes())
    {
        return Err(Error::InvalidComment);
    }
    let mut body = Vec::with_capacity(bytes.len() + 1024);
    for (key, value) in [
        ("biz", "new_dyn"),
        ("category", "daily"),
        ("csrf", csrf),
        ("csrf_token", csrf),
    ] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    let extension = if mime == "image/png" { "png" } else { "jpg" };
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file_up\"; filename=\"agent-runtime-test.{extension}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), body))
}

fn parse_receipt(value: Value, prepared: &PreparedImageUpload) -> Result<ImageUploadReceipt> {
    parse_upload_result(
        value,
        prepared.width,
        prepared.height,
        prepared.target.actor_mid(),
    )
}

pub(crate) fn parse_upload_result(
    value: Value,
    width: u32,
    height: u32,
    actor: u64,
) -> Result<ImageUploadReceipt> {
    let code = value
        .get("code")
        .and_then(Value::as_i64)
        .ok_or(Error::WriteOutcomeUnknown("image_result_code"))?;
    if code != 0 {
        return Err(match code {
            -101 => Error::LoginRequired,
            -352 | -412 | -401 | -509 | 12015 => Error::RiskControl(code),
            _ => Error::Platform(code),
        });
    }
    let data = value
        .get("data")
        .ok_or(Error::WriteOutcomeUnknown("image_receipt"))?;
    let dimension = |name: &str| -> Result<u32> {
        data.get(name)
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(Error::WriteOutcomeUnknown("image_dimensions"))
    };
    let image_width = dimension("image_width")?;
    let image_height = dimension("image_height")?;
    let image_url = picture_url(
        data.get("image_url")
            .and_then(Value::as_str)
            .ok_or(Error::WriteOutcomeUnknown("image_url"))?,
    )
    .map_err(|_| Error::WriteOutcomeUnknown("image_url_scope"))?;
    let img_size = data
        .get("img_size")
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
        .ok_or(Error::WriteOutcomeUnknown("image_size"))?;
    if image_width != width
        || image_height != height
        || !img_size.is_finite()
        || !(0.0..=16384.0).contains(&img_size)
    {
        return Err(Error::WriteOutcomeUnknown("image_metadata_mismatch"));
    }
    Ok(ImageUploadReceipt {
        image_url,
        image_width,
        image_height,
        img_size,
        actor_mid: actor,
    })
}

impl Client {
    pub fn prepare_own_video_image_upload(
        &self,
        id: &VideoId,
        bytes: &[u8],
    ) -> Result<PreparedImageUpload> {
        self.prepare_video_image_upload_inner(id, bytes, true)
    }
    pub fn prepare_video_image_upload(
        &self,
        id: &VideoId,
        bytes: &[u8],
    ) -> Result<PreparedImageUpload> {
        self.prepare_video_image_upload_inner(id, bytes, false)
    }
    fn prepare_video_image_upload_inner(
        &self,
        id: &VideoId,
        bytes: &[u8],
        own_only: bool,
    ) -> Result<PreparedImageUpload> {
        let (mime, width, height) = image_info(bytes)?;
        let (target, csrf, cookie_binding) = if own_only {
            self.prepare_comment_target(id)?
        } else {
            self.prepare_public_comment_target(id)?
        };
        let (content_type, body) = multipart(bytes, mime, &csrf)?;
        Ok(PreparedImageUpload {
            target,
            body,
            content_type,
            cookie_binding,
            bytes: bytes.len() as u64,
            width,
            height,
            mime,
        })
    }
    pub fn submit_prepared_image_upload(
        &self,
        prepared: PreparedImageUpload,
    ) -> Result<ImageUploadReceipt> {
        if self.transport.cookie_header() != Some(prepared.cookie_binding.as_str()) {
            return Err(Error::CommentPreparationChanged);
        }
        // Preparation fixed content type, dimensions, account and bytes. No reads or retries here.
        csrf_from_cookie(&prepared.cookie_binding)?;
        let value = self.transport.post_comment_image(
            &prepared.body,
            &prepared.target.bvid,
            &prepared.content_type,
        )?;
        parse_receipt(value, &prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn png() -> Vec<u8> {
        let mut bytes = vec![0; 33];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[8..12].copy_from_slice(&13u32.to_be_bytes());
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&160u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&160u32.to_be_bytes());
        bytes
    }
    fn prepared() -> PreparedImageUpload {
        let (content_type, body) =
            multipart(&png(), "image/png", "0123456789abcdef0123456789abcdef").unwrap();
        PreparedImageUpload {
            target: CommentTarget {
                aid: 42,
                bvid: "BV17x411w7KC".into(),
                title: "owned".into(),
                owner_mid: 7,
                actor_mid: 7,
            },
            body,
            content_type,
            cookie_binding: "private-cookie".into(),
            bytes: 33,
            width: 160,
            height: 160,
            mime: "image/png",
        }
    }
    #[test]
    fn image_headers_and_multipart_are_bounded_without_a_decoder_or_browser() {
        assert_eq!(image_info(&png()).unwrap(), ("image/png", 160, 160));
        assert!(image_info(b"not an image").is_err());
        let mut oversized = png();
        oversized[16..20].copy_from_slice(&4097u32.to_be_bytes());
        assert!(image_info(&oversized).is_err());
        assert!(image_info(&vec![0; MAX_IMAGE_BYTES + 1]).is_err());
        let p = prepared();
        let payload = String::from_utf8_lossy(&p.body);
        assert!(
            payload.contains("name=\"file_up\"") && payload.contains("name=\"biz\"\r\n\r\nnew_dyn")
        );
        assert!(!format!("{p:?}").contains("private-cookie"));
    }
    #[test]
    fn image_receipts_require_accepted_scoped_dimensions_and_never_leak_debug_urls() {
        let p = prepared();
        let receipt = parse_receipt(
            json!({"code":0,"data":{"image_url":"http://i0.hdslb.com/bfs/new_dyn/example.png",
            "image_width":160,"image_height":160,"img_size":0.5,"private":"not-public"}}),
            &p,
        )
        .unwrap();
        assert!(receipt.image_url.starts_with("https://"));
        assert!(!format!("{receipt:?}").contains("example.png"));
        assert_eq!(receipt.into_picture().unwrap().actor_mid(), 7);
        assert!(parse_receipt(json!({"code":0,"data":{"image_url":"https://evil.example/x.png","image_width":160,"image_height":160,"img_size":1}}),&p).is_err());
        assert!(matches!(
            parse_receipt(json!({"code":-509}), &p),
            Err(Error::RiskControl(-509))
        ));
        assert!(matches!(
            Client::new(crate::Config::default())
                .unwrap()
                .submit_prepared_image_upload(prepared()),
            Err(Error::CommentPreparationChanged)
        ));
    }
}
