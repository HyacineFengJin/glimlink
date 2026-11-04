// SPDX-License-Identifier: MIT
//! Small Tieba HTTP client with explicit human reply controls. No browser, JavaScript,
//! desktop app, account discovery, or automatic write retries.
mod actions;
mod auth;
mod discovery;
mod following;
mod home;
mod model;
mod notifications;
mod posting;
mod publications;
mod transport;
mod wire;
pub use actions::{ActionReceipt, ActionTarget, PreparedAction, UploadedImage};
pub use auth::{
    AccountInfo, AuthClient, AuthError, AuthResult, LoginAttempt, LoginStatus, QrReceipt, Session,
};
pub use discovery::{ForumThreads, SearchHit, SearchPage, ThreadSummary};
pub use following::FollowedForums;
pub use home::{RecommendationPage, RecommendedThread};
pub use model::*;
pub use notifications::{
    Notification, NotificationActor, NotificationKind, NotificationPage, NotificationTarget,
};
pub use posting::{PreparedReply, ReplyReceipt, ReplyTarget, ReplyVerification, VerifiedReply};
pub use publications::{Publication, PublicationKind, PublicationOwner, PublicationPage};
use std::{fmt, io::Write, path::Path};
pub use transport::{Config, DownloadReceipt, validate_media_url};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidId,
    InvalidConfig,
    InvalidPage,
    UrlPolicy,
    HttpStatus(u16),
    Network,
    Limit,
    InvalidProto,
    Schema(&'static str),
    Platform(i64),
    Challenge,
    LoginRequired,
    Io,
    /// The sole write request may have reached the service. Do not resend.
    WriteResultUnknown,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Platform(code) => write!(f, "Tieba rejected request ({code})"),
            Self::HttpStatus(code) => write!(f, "Tieba HTTP status {code}"),
            Self::Schema(name) => write!(f, "unexpected Tieba response: {name}"),
            _ => write!(f, "Tieba adapter: {self:?}"),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThreadId(pub u64);
impl ThreadId {
    pub fn validate(&self) -> Result<()> {
        if self.0 == 0 || self.0 > i64::MAX as u64 {
            Err(Error::InvalidId)
        } else {
            Ok(())
        }
    }
}
impl std::str::FromStr for ThreadId {
    type Err = Error;
    fn from_str(input: &str) -> Result<Self> {
        let number = if input.bytes().all(|b| b.is_ascii_digit()) {
            input
        } else {
            let uri: ureq::http::Uri = input.parse().map_err(|_| Error::InvalidId)?;
            if uri.scheme_str() != Some("https")
                || !matches!(uri.host(), Some("tieba.baidu.com" | "www.tieba.baidu.com"))
                || uri.authority().is_none_or(|a| a.as_str().contains('@'))
                || !matches!(uri.port_u16(), None | Some(443))
            {
                return Err(Error::InvalidId);
            }
            let path = uri.path().strip_prefix("/p/").ok_or(Error::InvalidId)?;
            if path.is_empty() || !path.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::InvalidId);
            }
            let id = ThreadId(path.parse().map_err(|_| Error::InvalidId)?);
            id.validate()?;
            return Ok(id);
        };
        if number.is_empty() || number.len() > 19 {
            return Err(Error::InvalidId);
        }
        let id = ThreadId(number.parse().map_err(|_| Error::InvalidId)?);
        id.validate()?;
        Ok(id)
    }
}

pub struct Client {
    transport: transport::Transport,
}
impl Client {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            transport: transport::Transport::new(config)?,
        })
    }
    fn common(&self, version: &str) -> Vec<u8> {
        let mut common = Vec::new();
        wire::number(1, 2, &mut common);
        wire::bytes(2, version.as_bytes(), &mut common);
        if let Some(bduss) = self.transport.bduss() {
            wire::bytes(10, bduss.as_bytes(), &mut common);
        }
        common
    }
    pub(crate) fn publications_for_owner(
        &self,
        owner: PublicationOwner,
        kind: PublicationKind,
        page: u32,
        limit: usize,
    ) -> Result<PublicationPage> {
        check_page(page)?;
        if !(1..=50).contains(&limit) {
            return Err(Error::Limit);
        }
        if self.transport.bduss().is_none() {
            return Err(Error::LoginRequired);
        }
        let request =
            publications::request(&self.common("22.12.1.0"), owner.user_id, kind, page, limit)?;
        let raw = self
            .transport
            .api("/c/u/feed/userpost?cmd=303002", &request)?;
        publications::parse(&raw, owner, kind, page, limit)
    }
    /// One read of the authenticated account's native notification feed.
    /// The endpoint has no explicit mark-read call; server-side read effects are unknown.
    pub fn notifications(
        &self,
        kind: NotificationKind,
        page: u32,
        limit: usize,
    ) -> Result<NotificationPage> {
        check_page(page)?;
        if !(1..=100).contains(&limit) {
            return Err(Error::Limit);
        }
        if self.transport.bduss().is_none() {
            return Err(Error::LoginRequired);
        }
        let raw = self.transport.notification_form(kind.path(), page)?;
        notifications::parse(&raw, kind, page, limit)
    }
    pub fn thread_page(&self, thread: &ThreadId, page: u32) -> Result<Fetched<ThreadPage>> {
        thread.validate()?;
        check_page(page)?;
        let common = self.common("12.64.1.1");
        let mut data = Vec::new();
        wire::bytes(25, &common, &mut data);
        wire::number(4, thread.0, &mut data);
        wire::number(13, 30, &mut data);
        wire::number(18, page.into(), &mut data);
        let mut request = Vec::new();
        wire::bytes(1, &data, &mut request);
        let raw = self.transport.api("/c/f/pb/page?cmd=302001", &request)?;
        let parsed = model::parse_thread(&raw, *thread, page)?;
        let response_bytes = raw.len();
        let raw_source = model::redacted_source(&raw, false)?;
        Ok(Fetched {
            data: parsed,
            raw_source,
            source_mime: "application/x-protobuf",
            source_redacted: true,
            response_bytes,
        })
    }
    pub fn comments(
        &self,
        thread: &ThreadId,
        post_id: u64,
        page: u32,
    ) -> Result<Fetched<CommentPage>> {
        self.fetch_comment_context(thread, Some(post_id), None, page)
    }
    /// Locate a notification's selected subpost directly. The native `spid`
    /// field is distinct from parent `pid`; validate the returned parent and
    /// selection rather than treating the first reply as the selected one.
    pub fn comment_context(
        &self,
        thread: &ThreadId,
        comment_id: u64,
        expected_parent: Option<u64>,
    ) -> Result<Fetched<CommentPage>> {
        if comment_id == 0 || comment_id > i64::MAX as u64 {
            return Err(Error::InvalidId);
        }
        self.fetch_comment_context(thread, expected_parent, Some(comment_id), 1)
    }
    fn fetch_comment_context(
        &self,
        thread: &ThreadId,
        parent: Option<u64>,
        selected: Option<u64>,
        page: u32,
    ) -> Result<Fetched<CommentPage>> {
        thread.validate()?;
        check_page(page)?;
        if parent.is_some_and(|post_id| post_id == 0 || post_id > i64::MAX as u64)
            || (parent.is_none() && selected.is_none())
        {
            return Err(Error::InvalidId);
        }
        let common = self.common("22.12.1.0");
        let mut data = Vec::new();
        wire::bytes(9, &common, &mut data);
        wire::number(1, thread.0, &mut data);
        if let Some(comment_id) = selected {
            wire::number(3, comment_id, &mut data);
        } else if let Some(post_id) = parent {
            wire::number(2, post_id, &mut data);
        }
        wire::number(4, page.into(), &mut data);
        let mut request = Vec::new();
        wire::bytes(1, &data, &mut request);
        let raw = self.transport.api("/c/f/pb/floor?cmd=302002", &request)?;
        let parsed = if selected.is_none() {
            model::parse_comments(&raw, *thread, parent.ok_or(Error::InvalidId)?, page)?
        } else {
            model::parse_comment_context(&raw, *thread, parent, selected, page)?
        };
        let response_bytes = raw.len();
        let raw_source = model::redacted_source(&raw, true)?;
        Ok(Fetched {
            data: parsed,
            raw_source,
            source_mime: "application/x-protobuf",
            source_redacted: true,
            response_bytes,
        })
    }
    pub fn download_to_writer<W: Write + ?Sized>(
        &self,
        resource: &MediaResource,
        writer: &mut W,
        max_bytes: u64,
    ) -> Result<DownloadReceipt> {
        self.transport.download(resource, writer, max_bytes)
    }
    pub fn download_to_path(
        &self,
        resource: &MediaResource,
        path: &Path,
        max_bytes: u64,
    ) -> Result<DownloadReceipt> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
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
fn check_page(page: u32) -> Result<()> {
    if page == 0 || page > 100000 {
        Err(Error::InvalidPage)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thread_ids_are_strict() {
        assert_eq!(
            "https://tieba.baidu.com/p/8537603600?pn=2"
                .parse::<ThreadId>()
                .unwrap(),
            ThreadId(8537603600)
        );
        for id in [
            "0",
            "-1",
            "18446744073709551615",
            "https://evil.invalid/p/1",
            "https://tieba.baidu.com@evil.invalid/p/1",
            "https://tieba.baidu.com/p/1/2",
            "http://tieba.baidu.com/p/1",
            "1&pn=2",
        ] {
            assert!(id.parse::<ThreadId>().is_err(), "{id}");
        }
    }
}
