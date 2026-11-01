// SPDX-License-Identifier: MIT
use crate::{Error, Result, VideoId, http::normalize_resource_url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Owner {
    pub id: u64,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub page: u32,
    pub cid: u64,
    pub title: String,
    pub duration_seconds: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoInfo {
    pub aid: u64,
    pub bvid: String,
    pub title: String,
    pub description: String,
    pub owner: Owner,
    pub duration_seconds: u64,
    pub published_at: i64,
    pub pages: Vec<Page>,
    #[serde(default)]
    pub stat: VideoStats,
}

/// Snapshot counters only. Missing or explicitly masked fields stay unknown;
/// these are neither unique users nor a list of people who liked the video.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VideoStats {
    pub view: Option<u64>,
    pub like: Option<u64>,
    pub coin: Option<u64>,
    pub favorite: Option<u64>,
    pub share: Option<u64>,
    pub reply: Option<u64>,
    pub danmaku: Option<u64>,
    pub dislike: Option<u64>,
}

pub(crate) fn counter(value: &Value, field: &str) -> Result<Option<u64>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) if raw == "-" => Ok(None),
        Some(raw) => raw
            .as_u64()
            .or_else(|| {
                raw.as_str().and_then(|s| {
                    (!s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| s.parse().ok())
                        .flatten()
                })
            })
            .map(Some)
            .ok_or(Error::Schema("video counter")),
    }
}

pub(crate) fn video_stats(value: Option<&Value>, aid: u64) -> Result<VideoStats> {
    let value = match value {
        None | Some(Value::Null) => return Ok(VideoStats::default()),
        Some(value) if value.is_object() => value,
        _ => return Err(Error::Schema("video stat object")),
    };
    if counter(value, "aid")?.is_some_and(|id| id != aid) {
        return Err(Error::Schema("video stat identity"));
    }
    Ok(VideoStats {
        view: counter(value, "view")?,
        like: counter(value, "like")?,
        coin: counter(value, "coin")?,
        favorite: counter(value, "favorite")?,
        share: counter(value, "share")?,
        reply: counter(value, "reply")?,
        danmaku: counter(value, "danmaku")?,
        dislike: counter(value, "dislike")?,
    })
}
/// URL serialization is for private runtime storage/IR mapping; Debug redacts it.
#[derive(Clone, Serialize, Deserialize)]
pub struct MediaResource {
    pub url: String,
    pub backup_urls: Vec<String>,
    pub expected_bytes: Option<u64>,
    pub mime_type: Option<String>,
}
impl fmt::Debug for MediaResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaResource")
            .field("url", &"[redacted]")
            .field("backups", &self.backup_urls.len())
            .field("expected_bytes", &self.expected_bytes)
            .field("mime_type", &self.mime_type)
            .finish()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamTrack {
    pub resource: MediaResource,
    pub codec: String,
    pub bandwidth: u64,
    pub quality: u64,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectedMedia {
    Dash {
        video: StreamTrack,
        audio: StreamTrack,
    },
    Progressive {
        segments: Vec<MediaResource>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playback {
    pub cid: u64,
    pub duration_millis: u64,
    pub media: SelectedMedia,
    pub subtitles: Option<SubtitleIndex>,
    pub subtitle_error: Option<String>,
    pub preview_only: bool,
    pub requested_quality: VideoQuality,
    pub available_video: Vec<VideoVariant>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoQuality {
    #[default]
    #[serde(rename = "preview")]
    Preview,
    #[serde(rename = "480p")]
    P480,
    #[serde(rename = "720p")]
    P720,
    #[serde(rename = "1080p")]
    P1080,
}
impl VideoQuality {
    pub fn qn(self) -> u32 {
        match self {
            Self::Preview => 16,
            Self::P480 => 32,
            Self::P720 => 64,
            Self::P1080 => 80,
        }
    }
    pub fn height(self) -> u32 {
        match self {
            Self::Preview => 360,
            Self::P480 => 480,
            Self::P720 => 720,
            Self::P1080 => 1080,
        }
    }
    pub fn default_edge(self) -> u32 {
        match self {
            Self::Preview => 480,
            Self::P480 => 854,
            Self::P720 => 1280,
            Self::P1080 => 1920,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoVariant {
    pub quality: u64,
    pub width: u32,
    pub height: u32,
    pub codec: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub id: String,
    pub language: String,
    pub label: String,
    pub resource: MediaResource,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubtitleIndex {
    pub requires_login: bool,
    pub tracks: Vec<SubtitleTrack>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubtitleLine {
    pub from: f64,
    pub to: f64,
    pub content: String,
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Error::Schema("missing text field"))
}
fn number(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(Error::Schema("missing numeric field"))
}
pub(crate) fn video_info(value: Value) -> Result<VideoInfo> {
    let aid = number(&value, "aid")?;
    let bvid = text(&value, "bvid")?.to_owned();
    VideoId::Bv(bvid.clone()).validate()?;
    if aid == 0 {
        return Err(Error::Schema("zero video ID"));
    }
    let raw_pages = value
        .get("pages")
        .and_then(Value::as_array)
        .ok_or(Error::Schema("missing pages"))?;
    if raw_pages.is_empty() || raw_pages.len() > 2048 {
        return Err(Error::Schema("invalid page count"));
    }
    let mut pages = Vec::with_capacity(raw_pages.len());
    for raw in raw_pages {
        let cid = number(raw, "cid")?;
        let page =
            u32::try_from(number(raw, "page")?).map_err(|_| Error::Schema("invalid page"))?;
        if cid == 0 || page == 0 || pages.iter().any(|p: &Page| p.page == page) {
            return Err(Error::Schema("invalid page identity"));
        }
        pages.push(Page {
            page,
            cid,
            title: text(raw, "part")?.to_owned(),
            duration_seconds: number(raw, "duration")?,
        });
    }
    let owner = value.get("owner").ok_or(Error::Schema("missing owner"))?;
    Ok(VideoInfo {
        aid,
        bvid,
        title: text(&value, "title")?.to_owned(),
        description: value
            .get("desc")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        owner: Owner {
            id: number(owner, "mid")?,
            name: text(owner, "name")?.to_owned(),
        },
        duration_seconds: number(&value, "duration")?,
        published_at: value
            .get("pubdate")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        pages,
        stat: video_stats(value.get("stat"), aid)?,
    })
}
fn resource(raw: &Value) -> Result<MediaResource> {
    let primary = raw
        .get("baseUrl")
        .or_else(|| raw.get("base_url"))
        .or_else(|| raw.get("url"))
        .and_then(Value::as_str);
    let backups = raw
        .get("backupUrl")
        .or_else(|| raw.get("backup_url"))
        .and_then(Value::as_array);
    let mut urls = Vec::new();
    for source in primary
        .into_iter()
        .chain(backups.into_iter().flatten().filter_map(Value::as_str))
        .take(8)
    {
        // Platform sometimes advertises HTTP or nonstandard-port P2P mirrors.
        // Select only already-valid HTTPS CDN alternatives, without upgrading HTTP.
        if let Ok(url) = normalize_resource_url(source) {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    if urls.is_empty() {
        return Err(Error::UrlPolicy);
    }
    let url = urls.remove(0);
    Ok(MediaResource {
        url,
        backup_urls: urls,
        expected_bytes: raw.get("size").and_then(Value::as_u64),
        mime_type: raw
            .get("mimeType")
            .or_else(|| raw.get("mime_type"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
fn track(raw: &Value) -> Result<StreamTrack> {
    let resource = resource(raw)?;
    Ok(StreamTrack {
        resource,
        codec: text(raw, "codecs")?.to_owned(),
        bandwidth: raw
            .get("bandwidth")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX),
        quality: raw.get("id").and_then(Value::as_u64).unwrap_or_default(),
        width: raw
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|v| v.try_into().ok())
            .unwrap_or_default(),
        height: raw
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|v| v.try_into().ok())
            .unwrap_or_default(),
    })
}
#[cfg(test)]
pub(crate) fn playback(value: Value, cid: u64) -> Result<Playback> {
    playback_quality(value, cid, VideoQuality::Preview)
}
pub(crate) fn playback_quality(value: Value, cid: u64, quality: VideoQuality) -> Result<Playback> {
    let mut selected = None;
    let mut available_video = Vec::new();
    if let Some(dash) = value.get("dash") {
        let list = |field| -> Result<&[Value]> {
            let values = dash
                .get(field)
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if values.len() > 256 {
                return Err(Error::Schema("too many streams"));
            }
            Ok(values)
        };
        let videos = list("video")?;
        let audios = list("audio")?;
        let compatible = videos
            .iter()
            .filter(|v| {
                v.get("codecs")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.starts_with("avc1") || c.starts_with("avc3"))
            })
            .filter_map(|v| track(v).ok())
            .collect::<Vec<_>>();
        available_video = compatible
            .iter()
            .map(|v| VideoVariant {
                quality: v.quality,
                width: v.width,
                height: v.height,
                codec: v.codec.clone(),
            })
            .collect();
        let video = if quality == VideoQuality::Preview {
            compatible
                .into_iter()
                .min_by_key(|v| (if v.height == 0 { u32::MAX } else { v.height }, v.bandwidth))
        } else {
            compatible
                .into_iter()
                .filter(|v| {
                    v.width > 0 && v.height > 0 && v.width.min(v.height) <= quality.height()
                })
                .min_by_key(|v| (quality.height() - v.width.min(v.height), v.bandwidth))
        };
        let audio = audios
            .iter()
            .filter(|v| {
                v.get("codecs")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.starts_with("mp4a.40."))
            })
            .filter_map(|v| track(v).ok())
            .min_by_key(|v| v.bandwidth);
        if let (Some(video), Some(audio)) = (video, audio) {
            selected = Some(SelectedMedia::Dash { video, audio });
        }
    }
    if selected.is_none() {
        if let Some(durl) = value
            .get("durl")
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty() && v.len() <= 256)
        {
            // Progressive FLV is not silently represented as MP4/H.264+AAC.
            let format = value
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if format.contains("mp4") {
                let segments = durl.iter().map(resource).collect::<Result<Vec<_>>>()?;
                selected = Some(SelectedMedia::Progressive { segments });
            }
        }
    }
    Ok(Playback {
        cid,
        duration_millis: value
            .get("timelength")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        media: selected.ok_or(Error::NoCompatibleMedia)?,
        subtitles: None,
        subtitle_error: None,
        preview_only: value.get("is_preview").is_some_and(|v| {
            v.as_bool()
                .unwrap_or_else(|| v.as_u64().is_some_and(|n| n != 0))
        }),
        requested_quality: quality,
        available_video,
    })
}
pub(crate) fn subtitle_index(value: Value) -> Result<SubtitleIndex> {
    let mut tracks = Vec::new();
    if let Some(list) = value
        .pointer("/subtitle/subtitles")
        .and_then(Value::as_array)
    {
        if list.len() > 256 {
            return Err(Error::Schema("too many subtitles"));
        }
        for track in list {
            let raw_url = text(track, "subtitle_url")?;
            let resource = MediaResource {
                url: normalize_resource_url(raw_url)?,
                backup_urls: Vec::new(),
                expected_bytes: None,
                mime_type: Some("application/json".into()),
            };
            let id = track
                .get("id_str")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    track
                        .get("id")
                        .and_then(Value::as_u64)
                        .map(|v| v.to_string())
                })
                .unwrap_or_default();
            tracks.push(SubtitleTrack {
                id,
                language: text(track, "lan")?.to_owned(),
                label: text(track, "lan_doc")?.to_owned(),
                resource,
            });
        }
    }
    Ok(SubtitleIndex {
        requires_login: value
            .get("need_login_subtitle")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tracks,
    })
}
pub(crate) fn subtitle_lines(value: Value) -> Result<Vec<SubtitleLine>> {
    let body = value
        .get("body")
        .and_then(Value::as_array)
        .ok_or(Error::Schema("missing subtitle body"))?;
    if body.len() > 20_000 {
        return Err(Error::Schema("too many subtitle lines"));
    }
    let mut lines = Vec::with_capacity(body.len());
    for raw in body {
        let from = raw
            .get("from")
            .and_then(Value::as_f64)
            .ok_or(Error::Schema("subtitle start"))?;
        let to = raw
            .get("to")
            .and_then(Value::as_f64)
            .ok_or(Error::Schema("subtitle end"))?;
        if !from.is_finite() || !to.is_finite() || from < 0.0 || to < from {
            return Err(Error::Schema("invalid subtitle interval"));
        }
        lines.push(SubtitleLine {
            from,
            to,
            content: text(raw, "content")?.to_owned(),
        });
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    #[test]
    fn requested_quality_selects_source_not_an_upscaled_low_track() {
        use super::*;
        let track = |w, h, codec: &str| serde_json::json!({"baseUrl":"https://x.bilivideo.com/v.mp4","codecs":codec,"width":w,"height":h,"bandwidth":w*h,"id":80});
        let raw = serde_json::json!({"dash":{"video":[track(640,360,"avc1"),track(1280,720,"avc1"),track(1920,1080,"avc1"),track(3840,2160,"hev1")],"audio":[track(0,0,"mp4a.40.2")]}});
        for (quality, height) in [
            (VideoQuality::Preview, 360),
            (VideoQuality::P480, 360),
            (VideoQuality::P720, 720),
            (VideoQuality::P1080, 1080),
        ] {
            let p = playback_quality(raw.clone(), 1, quality).unwrap();
            let SelectedMedia::Dash { video, .. } = p.media else {
                panic!()
            };
            assert_eq!(video.height, height);
            assert_eq!(p.available_video.len(), 3);
        }
        let portrait = serde_json::json!({"dash":{"video":[track(1080,1920,"avc1")],"audio":[track(0,0,"mp4a.40.2")]}});
        assert!(playback_quality(portrait, 1, VideoQuality::P1080).is_ok());
    }
    use super::*;
    use serde_json::json;
    #[test]
    fn video_snapshot_stats_keep_unknown_counters_and_load_old_cache() {
        let raw = json!({"aid":170001,"bvid":"BV17x411w7KC","title":"test","desc":"example",
            "owner":{"mid":42,"name":"test"},"duration":15,
            "pages":[{"cid":7,"page":1,"part":"first","duration":15}],
            "stat":{"aid":170001,"view":"12","like":0,"reply":"-","coin":null,"private":"hidden"}});
        let parsed = video_info(raw.clone()).unwrap();
        assert_eq!(parsed.stat.view, Some(12));
        assert_eq!(parsed.stat.like, Some(0));
        assert_eq!(parsed.stat.reply, None);
        assert_eq!(parsed.stat.coin, None);
        assert!(!serde_json::to_string(&parsed).unwrap().contains("hidden"));
        let mut old = serde_json::to_value(&parsed).unwrap();
        old.as_object_mut().unwrap().remove("stat");
        assert_eq!(
            serde_json::from_value::<VideoInfo>(old).unwrap().stat,
            VideoStats::default()
        );
        let mut bad = raw.clone();
        bad["stat"]["aid"] = json!(99);
        assert!(video_info(bad).is_err());
        let mut bad = raw;
        bad["stat"]["like"] = json!("about 5k");
        assert!(video_info(bad).is_err());
    }
    #[test]
    fn stream_selection_keeps_only_lowest_avc_and_aac() {
        let base = "https://upos-sz-mirrorali.bilivideo.com/test";
        let video = |codec, height, bw| json!({"codecs":codec,"height":height,"bandwidth":bw,"id":16,"baseUrl":base});
        let raw = json!({"dash":{"video":[video("hev1",144,1),video("avc1.64001e",720,300),video("avc1.64001e",360,100)],"audio":[video("ec-3",0,1),video("mp4a.40.2",0,30),video("mp4a.40.2",0,10)]}});
        match playback(raw, 123).unwrap().media {
            SelectedMedia::Dash { video, audio } => {
                assert_eq!(video.height, 360);
                assert_eq!(audio.bandwidth, 10);
            }
            _ => panic!("expected separate DASH tracks"),
        }
        assert!(playback(json!({"dash":{"video":[],"audio":[]}}), 123).is_err());
    }
    #[test]
    fn progressive_retains_all_segments_and_unsafe_urls_are_rejected() {
        let raw = json!({"format":"mp4","durl":[{"url":"https://x.bilivideo.com/one","size":12},{"url":"https://x.bilivideo.com/two","size":13}]});
        match playback(raw, 1).unwrap().media {
            SelectedMedia::Progressive { segments } => assert_eq!(segments.len(), 2),
            _ => panic!("expected progressive"),
        }
        assert!(
            playback(
                json!({"format":"flv","durl":[{"url":"https://x.bilivideo.com/one"}]}),
                1
            )
            .is_err()
        );
        assert!(resource(&json!({"url":"http://localhost/media"})).is_err());
        let resource = resource(
            &json!({"url":"http://x.bilivideo.com/a","backup_url":["https://x.bilivideo.com/a"]}),
        )
        .unwrap();
        assert!(resource.url.starts_with("https://"));
        assert!(!format!("{resource:?}").contains("https://"));
    }
    #[test]
    fn subtitle_login_requirement_and_intervals_are_explicit() {
        let index = subtitle_index(json!({"need_login_subtitle":true,"subtitle":{"subtitles":[]}}))
            .unwrap();
        assert!(index.requires_login);
        assert!(index.tracks.is_empty());
        assert!(subtitle_lines(json!({"body":[{"from":2,"to":1,"content":"bad"}]})).is_err());
        assert_eq!(
            subtitle_lines(json!({"body":[{"from":0,"to":1.5,"content":"valid"}]}))
                .unwrap()
                .len(),
            1
        );
    }
}
