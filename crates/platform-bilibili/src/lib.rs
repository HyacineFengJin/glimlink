// SPDX-License-Identifier: MIT
//! Bilibili native HTTP reads and account-bound single-attempt social writes.
mod danmaku;
pub use danmaku::DanmakuItem;
mod features;
pub use features::{FeatureCommand, PreparedDynamicImage, PreparedFeature};
mod auth;
mod comment_images;
mod comment_read;
mod comments;
mod context;
mod discovery;
mod dynamics;
mod engagement;
mod http;
mod id;
mod model;
mod notification_context;
mod notifications;
mod publications;
pub mod wbi;

pub use auth::{AccountInfo, AuthClient, FavoriteFolder, LoginAttempt, LoginStatus, Session};
pub use comment_images::{ImageUploadReceipt, PreparedImageUpload, UploadedCommentImage};
pub use comment_read::{
    CommentMediaKind, CommentOrder, CommentRead, CommentReadAuthor, CommentReadMedia,
    CommentReadPage, CommentReadReplies, CommentSource,
};
pub use comments::{
    CommentEmote, CommentEmoteFlags, CommentEmoteMeta, CommentEmotePackage, CommentPicture,
    CommentReceipt, CommentReplyPage, CommentTarget, PreparedComment, VideoComment,
};
pub use context::{UploaderInfo, UploaderOfficial, VideoTag, VideoTagCounts};
pub use discovery::{PopularVideoPage, SearchDuration, SearchOrder, SearchVideo, VideoSearchPage};
pub use dynamics::{
    DynamicAuthor, DynamicFeedPage, DynamicItem, DynamicMajor, DynamicMedia, DynamicMediaKind,
    DynamicOriginalReference, DynamicRichNode, DynamicStats,
};
pub use engagement::{
    EngagementAction, EngagementReceipt, EngagementState, EngagementTarget, EngagementVerification,
    PreparedEngagement,
};
pub use http::{Config, DownloadReceipt, validate_resource_url};
pub use id::VideoId;
pub use model::{
    MediaResource, Owner, Page, Playback, SelectedMedia, StreamTrack, SubtitleIndex, SubtitleLine,
    SubtitleTrack, VideoInfo, VideoStats,
};
pub use notification_context::{
    NotificationCommentLocator, NotificationContext, NotificationContextStatus,
    NotificationLocation, NotificationLocationStatus, notification_location,
};
pub use notifications::{
    Notification, NotificationActor, NotificationCursor, NotificationKind, NotificationPage,
    NotificationTarget, NotificationUnread,
};
pub use publications::{PublicationPage, PublishedVideo, UploaderVideoPage};
use serde_json::Value;
use std::{fmt, io::Write, path::Path};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidId,
    InvalidConfig,
    UrlPolicy,
    HttpStatus(u16),
    Network,
    JsonLimit,
    InvalidJson,
    Schema(&'static str),
    Platform(i64),
    RiskControl(i64),
    LoginRequired,
    NoCompatibleMedia,
    InvalidPage,
    DownloadLimit,
    DownloadIncomplete,
    Io,
    InvalidSignature,
    LoginState,
    PollTooSoon,
    InvalidComment,
    NotOwnVideo,
    CommentPreparationChanged,
    WriteOutcomeUnknown(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never forward transport/server messages: they can contain URLs/tokens.
        match self {
            Self::Platform(c) => write!(f, "Bilibili API rejected request ({c})"),
            Self::RiskControl(c) => write!(f, "Bilibili risk control ({c}); stop and retry later"),
            Self::HttpStatus(c) => write!(f, "Bilibili HTTP status {c}"),
            Self::Schema(s) => write!(f, "unexpected Bilibili response: {s}"),
            Self::WriteOutcomeUnknown(reason) => write!(
                f,
                "Bilibili comment outcome unknown ({reason}); do not resend"
            ),
            _ => write!(f, "Bilibili adapter error: {self:?}"),
        }
    }
}
impl std::error::Error for Error {}

pub struct Client {
    transport: http::Transport,
    wbi: Option<(wbi::WbiKey, std::time::Instant)>,
}
impl Client {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            transport: http::Transport::new(config)?,
            wbi: None,
        })
    }
    /// Verify the current cookie using the authenticated navigation endpoint.
    pub fn current_user(&self) -> Result<AccountInfo> {
        auth::account_info(api_data(
            self.transport
                .json("https://api.bilibili.com/x/web-interface/nav")?,
            false,
        )?)
    }
    /// Read folders created by an explicit UID. Cookies stay on api.bilibili.com.
    pub fn favorite_folders(&self, mid: u64) -> Result<Vec<FavoriteFolder>> {
        if mid == 0 || mid > i64::MAX as u64 {
            return Err(Error::InvalidId);
        }
        auth::favorite_folders(api_data(
            self.transport.json(&format!(
                "https://api.bilibili.com/x/v3/fav/folder/created/list-all?up_mid={mid}"
            ))?,
            false,
        )?)
    }
    /// Public video metadata and all parts. Input may be BV, av, or a video URL.
    pub fn video_info(&self, id: &VideoId) -> Result<VideoInfo> {
        id.validate()?;
        let url = format!(
            "https://api.bilibili.com/x/web-interface/view?{}",
            id.query()
        );
        model::video_info(api_data(self.transport.json(&url)?, false)?)
    }
    pub fn refresh_wbi_key(&mut self) -> Result<()> {
        // Unauthenticated nav deliberately returns -101 while still carrying keys.
        let data = api_data(
            self.transport
                .json("https://api.bilibili.com/x/web-interface/nav")?,
            true,
        )?;
        self.wbi = Some((wbi::WbiKey::from_nav(&data)?, std::time::Instant::now()));
        Ok(())
    }
    fn signed_get(&mut self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        if self
            .wbi
            .as_ref()
            .is_none_or(|(_, fetched)| fetched.elapsed().as_secs() >= 300)
        {
            self.refresh_wbi_key()?;
        }
        let key = &self.wbi.as_ref().ok_or(Error::InvalidSignature)?.0;
        let query = key.sign(params, wbi::unix_seconds()?)?;
        api_data(
            self.transport
                .json(&format!("https://api.bilibili.com{path}?{query}"))?,
            false,
        )
    }
    /// Select lowest available AVC/H.264 video plus AAC audio. Returns no muxed
    /// fiction: DASH tracks remain separate; progressive fragments remain ordered.
    /// Page is one-based, like the website's `?p=` parameter.
    pub fn playback(&mut self, video: &VideoInfo, page: u32) -> Result<Playback> {
        let part = video
            .pages
            .iter()
            .find(|p| p.page == page)
            .ok_or(Error::InvalidPage)?;
        let mut params = [
            ("bvid", video.bvid.clone()),
            ("cid", part.cid.to_string()),
            ("qn", "16".into()),
            ("fnval", "16".into()),
            ("fnver", "0".into()),
            ("fourk", "0".into()),
            ("gaia_source", "view-card".into()),
        ];
        let data = self.signed_get("/x/player/wbi/playurl", &params)?;
        let mut playback = match model::playback(data, part.cid) {
            Ok(playback) => playback,
            Err(Error::NoCompatibleMedia) => {
                // A codec/format mismatch gets one MP4 request, never a retry of
                // a platform rejection or challenge.
                params[3].1 = "1".into();
                let data = self.signed_get("/x/player/wbi/playurl", &params)?;
                model::playback(data, part.cid)?
            }
            Err(error) => return Err(error),
        };
        // Subtitle failure is explicit and cannot silently turn a failed media
        // lookup into successful metadata. Call `subtitles` separately to retry.
        match self.subtitles(video, part.cid) {
            Ok(index) => playback.subtitles = Some(index),
            Err(error) => playback.subtitle_error = Some(error.to_string()),
        }
        Ok(playback)
    }
    pub fn subtitles(&mut self, video: &VideoInfo, cid: u64) -> Result<SubtitleIndex> {
        if !video.pages.iter().any(|p| p.cid == cid) {
            return Err(Error::InvalidPage);
        }
        let data = self.signed_get(
            "/x/player/wbi/v2",
            &[("aid", video.aid.to_string()), ("cid", cid.to_string())],
        )?;
        model::subtitle_index(data)
    }
    pub fn subtitle_lines(&self, track: &SubtitleTrack) -> Result<Vec<SubtitleLine>> {
        let json = self.transport.json_resource(&track.resource.url)?;
        model::subtitle_lines(json)
    }
    /// Stream to caller-owned storage. May write a prefix before a later error;
    /// callers must not mark that prefix as a completed resource.
    pub fn download_to_writer<W: Write + ?Sized>(
        &self,
        resource: &MediaResource,
        output: &mut W,
        max_bytes: u64,
    ) -> Result<DownloadReceipt> {
        self.transport.download(resource, output, max_bytes)
    }
    /// Creates a new file only. A failed or over-limit download is removed.
    /// Existing files are never overwritten.
    pub fn download_to_path(
        &self,
        resource: &MediaResource,
        path: &Path,
        max_bytes: u64,
    ) -> Result<DownloadReceipt> {
        use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| Error::Io)?;
        let result = self
            .download_to_writer(resource, &mut file, max_bytes)
            .and_then(|receipt| {
                file.sync_all().map_err(|_| Error::Io)?;
                Ok(receipt)
            });
        if result.is_err() {
            drop(file);
            let _ = std::fs::remove_file(path);
        }
        result
    }
}
fn api_data(mut value: Value, nav: bool) -> Result<Value> {
    let code = value
        .get("code")
        .and_then(Value::as_i64)
        .ok_or(Error::Schema("missing code"))?;
    if code != 0 && !(nav && code == -101) {
        return Err(match code {
            -352 | -412 | -401 => Error::RiskControl(code),
            -101 => Error::LoginRequired,
            _ => Error::Platform(code),
        });
    }
    let data = value
        .get("data")
        .filter(|v| !v.is_null())
        .ok_or(Error::Schema("missing data"))?;
    if data.get("v_voucher").is_some() {
        return Err(Error::RiskControl(code));
    }
    // Move the response body rather than duplicating its complete JSON tree.
    Ok(value
        .get_mut("data")
        .ok_or(Error::Schema("missing data"))?
        .take())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn risk_and_login_failures_are_never_fabricated_as_empty_success() {
        assert_eq!(
            api_data(
                serde_json::json!({"code":-352,"message":"private-url"}),
                false
            ),
            Err(Error::RiskControl(-352))
        );
        assert_eq!(
            api_data(
                serde_json::json!({"code":0,"data":{"v_voucher":"secret"}}),
                false
            ),
            Err(Error::RiskControl(0))
        );
        assert_eq!(
            api_data(serde_json::json!({"code":-101,"data":{}}), false),
            Err(Error::LoginRequired)
        );
        assert!(api_data(serde_json::json!({"code":-101,"data":{}}), true).is_ok());
    }
}
