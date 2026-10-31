// SPDX-License-Identifier: MIT
//! Video tags and public uploader cards. Fixed read-only endpoints, no media fetch.
//! Sources: pskdje/bilibili-API-collect docs/video/tags.md and docs/user/info.md.
use crate::model::counter;
use crate::{Client, Error, MediaResource, Result, VideoId, api_data};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VideoTagCounts {
    pub view: Option<u64>,
    pub use_count: Option<u64>,
    pub followers: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoTag {
    /// Modern BGM entries have tag_id=0 and are identified by music_id instead.
    pub tag_id: Option<u64>,
    pub name: String,
    pub tag_type: Option<String>,
    pub music_id: Option<String>,
    /// Modern tag responses may omit descriptions and aggregate counters.
    pub description: Option<String>,
    pub short_description: Option<String>,
    pub count: VideoTagCounts,
    pub cover: Option<MediaResource>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploaderOfficial {
    /// Service type: -1 unverified, 0 personal, 1 organization. Missing is unknown.
    pub kind: Option<i64>,
    pub role: Option<u64>,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploaderInfo {
    pub mid: u64,
    pub name: String,
    pub sign: Option<String>,
    pub level: Option<u32>,
    pub official: Option<UploaderOfficial>,
    pub follower_count: Option<u64>,
    /// The uploader's following total, not whether the viewing account follows them.
    pub following_count: Option<u64>,
    pub archive_count: Option<u64>,
    pub like_count: Option<u64>,
    pub avatar: Option<MediaResource>,
}

impl Client {
    /// Returns tag metadata only. In particular, this neither subscribes to tags
    /// nor accesses the mutable tag-like endpoints.
    pub fn video_tags(&self, video: &VideoId) -> Result<Vec<VideoTag>> {
        video.validate()?;
        let url = format!(
            "https://api.bilibili.com/x/web-interface/view/detail/tag?{}",
            video.query()
        );
        parse_tags(api_data(self.transport.json(&url)?, false)?)
    }

    /// A public card GET. No WBI fingerprint, alternate profile or actor relation
    /// request is made if this fixed endpoint rejects the request.
    pub fn uploader_info(&self, mid: u64) -> Result<UploaderInfo> {
        validate_mid(mid)?;
        let url = format!("https://api.bilibili.com/x/web-interface/card?mid={mid}&photo=false");
        parse_uploader(api_data(self.transport.json(&url)?, false)?, mid)
    }
}

fn validate_mid(mid: u64) -> Result<()> {
    if mid == 0 || mid > i64::MAX as u64 {
        Err(Error::InvalidId)
    } else {
        Ok(())
    }
}
fn text(value: &Value, name: &str, limit: usize) -> Result<Option<String>> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= limit && !value.contains('\0') => {
            Ok(Some(value.clone()))
        }
        Some(Value::String(_)) => Err(Error::JsonLimit),
        _ => Err(Error::Schema("context text")),
    }
}
fn required_text(value: &Value, name: &str, limit: usize) -> Result<String> {
    text(value, name, limit)?
        .filter(|s| !s.is_empty())
        .ok_or(Error::Schema("missing context text"))
}
fn object<'a>(value: &'a Value, name: &str) -> Result<Option<&'a Value>> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) if v.is_object() => Ok(Some(v)),
        _ => Err(Error::Schema("context object")),
    }
}
fn signed_number(value: &Value, name: &str) -> Result<Option<i64>> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .or_else(|| {
                v.as_str()
                    .filter(|s| s.len() <= 20)
                    .and_then(|s| s.parse().ok())
            })
            .map(Some)
            .ok_or(Error::Schema("context signed number")),
    }
}
fn image_reference(value: Option<String>) -> Result<Option<MediaResource>> {
    let Some(value) = value.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let url = if let Some(tail) = value.strip_prefix("http://") {
        format!("https://{tail}")
    } else if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value
    };
    crate::validate_resource_url(&url)?;
    Ok(Some(MediaResource {
        url,
        backup_urls: vec![],
        expected_bytes: None,
        mime_type: None,
    }))
}
fn check_challenge(value: &Value) -> Result<()> {
    let risk = match value.get("is_risk") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(v)) => *v,
        Some(v) if v.as_u64() == Some(0) => false,
        Some(v) if v.as_u64() == Some(1) => true,
        _ => return Err(Error::Schema("context risk flag")),
    };
    if value.get("v_voucher").is_some()
        || risk
        || counter(value, "gaia_res_type")?.is_some_and(|n| n != 0)
    {
        return Err(Error::RiskControl(0));
    }
    Ok(())
}
fn parse_tags(data: Value) -> Result<Vec<VideoTag>> {
    let rows = data.as_array().ok_or(Error::Schema("video tag list"))?;
    if rows.len() > 256 {
        return Err(Error::JsonLimit);
    }
    let mut tags = Vec::with_capacity(rows.len());
    for raw in rows {
        if !raw.is_object() {
            return Err(Error::Schema("video tag object"));
        }
        let tag_type = text(raw, "tag_type", 128)?;
        let tag_id = counter(raw, "tag_id")?.filter(|id| *id != 0);
        let music_id = text(raw, "music_id", 128)?.filter(|s| !s.is_empty());
        if tag_id.is_none()
            && !(tag_type.as_deref() == Some("bgm")
                && music_id.as_ref().is_some_and(|s| {
                    s.starts_with("MA") && s.len() > 2 && s[2..].bytes().all(|b| b.is_ascii_digit())
                }))
        {
            return Err(Error::Schema("video tag identity"));
        }
        if tag_id.is_some_and(|id| id > i64::MAX as u64) {
            return Err(Error::InvalidId);
        }
        if tags.iter().any(|tag: &VideoTag| {
            if let Some(id) = tag_id {
                tag.tag_id == Some(id)
            } else {
                tag.tag_id.is_none() && tag.music_id == music_id
            }
        }) {
            return Err(Error::Schema("duplicate video tag"));
        }
        let count = if let Some(raw) = object(raw, "count")? {
            VideoTagCounts {
                view: counter(raw, "view")?,
                use_count: counter(raw, "use")?,
                followers: counter(raw, "atten")?,
            }
        } else {
            VideoTagCounts::default()
        };
        tags.push(VideoTag {
            tag_id,
            name: required_text(raw, "tag_name", 4096)?,
            tag_type,
            music_id,
            description: text(raw, "content", 65536)?,
            short_description: text(raw, "short_content", 8192)?,
            count,
            cover: image_reference(text(raw, "cover", 16384)?)?,
        });
    }
    Ok(tags)
}
fn official(card: &Value) -> Result<Option<UploaderOfficial>> {
    let raw = object(card, "Official")?.or(object(card, "official_verify")?);
    let Some(raw) = raw else { return Ok(None) };
    Ok(Some(UploaderOfficial {
        kind: signed_number(raw, "type")?,
        role: counter(raw, "role")?,
        title: text(raw, "title", 4096)?,
        description: text(raw, "desc", 8192)?,
    }))
}
fn parse_uploader(data: Value, mid: u64) -> Result<UploaderInfo> {
    check_challenge(&data)?;
    let card = object(&data, "card")?.ok_or(Error::Schema("uploader card"))?;
    check_challenge(card)?;
    let actual = counter(card, "mid")?.ok_or(Error::Schema("uploader identity"))?;
    if actual != mid {
        return Err(Error::Schema("uploader identity mismatch"));
    }
    let level = object(card, "level_info")?
        .map(|v| counter(v, "current_level"))
        .transpose()?
        .flatten()
        .map(|n| u32::try_from(n).map_err(|_| Error::Schema("uploader level")))
        .transpose()?;
    let follower_count = counter(&data, "follower")?.or(counter(card, "fans")?);
    Ok(UploaderInfo {
        mid,
        name: required_text(card, "name", 1024)?,
        sign: text(card, "sign", 8192)?,
        level,
        official: official(card)?,
        follower_count,
        following_count: counter(card, "attention")?,
        archive_count: counter(&data, "archive_count")?,
        like_count: counter(&data, "like_num")?,
        avatar: image_reference(text(card, "face", 16384)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn modern_tag_identity_keeps_bgm_and_unknown_counts_without_follow_actions() {
        let result = parse_tags(json!([
            {"tag_id":7,"tag_name":"猫","tag_type":"old_channel","music_id":"","private":"hidden"},
            {"tag_id":0,"tag_name":"发现音乐","tag_type":"bgm","music_id":"MA1234"}
        ]))
        .unwrap();
        assert_eq!(result[0].tag_id, Some(7));
        assert_eq!(result[0].count, VideoTagCounts::default());
        assert_eq!(result[0].description, None);
        assert_eq!(result[1].tag_id, None);
        assert_eq!(result[1].music_id.as_deref(), Some("MA1234"));
        assert!(!serde_json::to_string(&result).unwrap().contains("hidden"));
        assert!(
            parse_tags(json!([{"tag_id":0,"tag_name":"missing ID","tag_type":"topic"}])).is_err()
        );
        assert!(parse_tags(json!({"unexpected":[]})).is_err());
        assert!(parse_tags(json!([])).unwrap().is_empty());
    }
    #[test]
    fn optional_tag_details_and_images_are_metadata_only() {
        let result=parse_tags(json!([{"tag_id":7,"tag_name":"猫","tag_type":"topic",
            "content":"tag description","short_content":"summary","count":{"view":0,"use":"12","atten":3},
            "cover":"http://i0.hdslb.com/bfs/tag/example.png","is_atten":1}])).unwrap();
        assert_eq!(result[0].count.use_count, Some(12));
        assert_eq!(result[0].count.view, Some(0));
        assert_eq!(result[0].description.as_deref(), Some("tag description"));
        assert!(
            result[0]
                .cover
                .as_ref()
                .unwrap()
                .url
                .starts_with("https://")
        );
        assert!(!serde_json::to_string(&result).unwrap().contains("is_atten"));
    }
    #[test]
    fn uploader_card_preserves_identity_sign_official_and_true_count_semantics() {
        let data = json!({"card":{"mid":"42","name":"uploader","sign":"public signature",
            "attention":123,"fans":400,"level_info":{"current_level":6},
            "face":"http://i0.hdslb.com/bfs/face/example.png",
            "Official":{"type":0,"role":2,"title":"verified","desc":"official description"},
            "coins":999,"tel_status":1},"following":true,"follower":401,"like_num":17,"archive_count":4});
        let profile = parse_uploader(data.clone(), 42).unwrap();
        assert_eq!(profile.following_count, Some(123));
        assert_eq!(profile.follower_count, Some(401));
        assert_eq!(profile.sign.as_deref(), Some("public signature"));
        assert_eq!(profile.official.as_ref().unwrap().kind, Some(0));
        assert_eq!(profile.level, Some(6));
        let serialized = serde_json::to_string(&profile).unwrap();
        assert!(!serialized.contains("tel_status") && !serialized.contains("coins"));
        assert!(parse_uploader(data, 43).is_err());
    }
    #[test]
    fn absent_profile_metadata_does_not_invent_zero_or_false_relations() {
        let profile = parse_uploader(
            json!({"card":{"mid":42,"name":"uploader",
            "official_verify":{"type":-1,"desc":""}},"following":false}),
            42,
        )
        .unwrap();
        assert_eq!(profile.following_count, None);
        assert_eq!(profile.follower_count, None);
        assert_eq!(profile.official.unwrap().kind, Some(-1));
        assert!(
            parse_uploader(
                json!({"is_risk":true,"card":{"mid":42,"name":"uploader"}}),
                42
            )
            .is_err()
        );
        assert!(
            parse_uploader(json!({"is_risk":1,"card":{"mid":42,"name":"uploader"}}), 42).is_err()
        );
        assert!(validate_mid(0).is_err());
        assert!(validate_mid(u64::MAX).is_err());
        assert!(
            Client::new(crate::Config::default())
                .unwrap()
                .uploader_info(0)
                .is_err()
        );
    }
}
