// SPDX-License-Identifier: MIT
//! A shared lock for native login and model clients using one credential home.
use crate::{EngineError, ErrorKind, Result};
#[cfg(target_os = "linux")]
use std::os::{
    fd::AsRawFd,
    unix::fs::{OpenOptionsExt, PermissionsExt},
};
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

pub(crate) struct CredentialHomeLock(File);
impl Drop for CredentialHomeLock {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
pub(crate) fn lock(home: &Path) -> Result<CredentialHomeLock> {
    #[cfg(target_os = "linux")]
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home.join(".agent-runtime-engine.lock"))
            .map_err(|_| {
                EngineError::new(ErrorKind::State, "cannot lock engine credential home")
            })?;
        let metadata = file.metadata().map_err(|_| {
            EngineError::new(ErrorKind::State, "cannot inspect engine credential lock")
        })?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(EngineError::new(
                ErrorKind::State,
                "engine credential lock must be a private regular file",
            ));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(EngineError::new(
                ErrorKind::State,
                "engine credential home is busy",
            ));
        }
        Ok(CredentialHomeLock(file))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = home;
        Err(EngineError::new(
            ErrorKind::Unsupported,
            "native credential locking requires Linux",
        ))
    }
}
