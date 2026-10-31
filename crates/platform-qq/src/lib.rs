// SPDX-License-Identifier: MIT
//! Synchronous, bounded NTQQ chat transport. No CLI, background worker or outbox.
pub mod app_info;
pub mod attachments;
pub mod auth;
pub mod password;
pub use app_info::{AppInfo, AppProfile};
pub mod directory;
mod history;
pub use history::HistoryPage;
mod events;
pub mod key_rotation;
mod login;
pub mod media;
pub mod message;
mod protocol;
pub mod qr;
mod qr_login;
pub mod rich;
pub mod session;
pub mod signer;
pub mod tea;
pub mod upload;
pub mod wire;

pub use events::{IncomingEvent, View, project_push};
pub use login::{LoginMode, LoginOutcome, LoginSource};
pub use message::SendResponse;
pub use protocol::{Client, ClientConfig, PreparedText, SendTarget};
pub use qr::QrState;
pub use qr_login::login_qr;
pub use session::{Device, Session};

pub type Result<T> = std::result::Result<T, String>;

pub fn unix_seconds() -> Result<u32> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "invalid system clock")?
        .as_secs();
    u32::try_from(seconds).map_err(|_| "system clock exceeds protocol range".into())
}
