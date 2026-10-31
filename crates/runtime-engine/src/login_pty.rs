// SPDX-License-Identifier: MIT
//! Bounded human-login transport for the pinned native Antigravity CLI.
//! Native code owns OAuth/PKCE and token exchange. This adapter neither renders
//! a browser nor implements OAuth. Only its official URL leaves the PTY.
use crate::{EngineError, ErrorKind, Result, antigravity};
use std::{
    fs::File,
    io::{Read, Write},
    os::{fd::FromRawFd, unix::process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const MAX_OUTPUT: usize = 1024 * 1024;
const KEEP_OUTPUT: usize = 128 * 1024;

pub enum LoginPtyProgress {
    Pending,
    Authorization { url: String },
    Completed,
    Failed,
}
pub struct AntigravityLogin {
    child: Child,
    terminal: File,
    home_lock: Option<crate::credential_home::CredentialHomeLock>,
    output: Vec<u8>,
    total: usize,
    selected: bool,
    authorization_sent: bool,
    code_sent: bool,
    closed: bool,
    deadline: Instant,
    watchdog: Option<crate::process_deadline::ProcessDeadline>,
}
fn fail(kind: ErrorKind, detail: &str) -> EngineError {
    EngineError::new(kind, detail)
}
impl AntigravityLogin {
    pub fn spawn(binary: &Path, home: &Path, workspace: &Path, timeout: Duration) -> Result<Self> {
        let binary = antigravity::native(binary)?;
        let home = antigravity::private_directory(home)?;
        let workspace = antigravity::private_directory(workspace)?;
        if home.starts_with(&workspace) || workspace.starts_with(&home) {
            return Err(fail(
                ErrorKind::InvalidInput,
                "login workspace and home must be separate",
            ));
        }
        let home_lock = crate::credential_home::lock(&home)?;
        let gemini = antigravity::private_directory(&home.join(".gemini"))?;
        let cache = antigravity::private_directory(&home.join("cache"))?;
        let app_data = antigravity::private_directory(&home.join("app-data"))?;
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if master < 0 {
            return Err(fail(ErrorKind::Io, "cannot allocate native login terminal"));
        }
        let terminal = unsafe { File::from_raw_fd(master) };
        let mut slave_name = [0i8; 256];
        if unsafe { libc::grantpt(master) } != 0
            || unsafe { libc::unlockpt(master) } != 0
            || unsafe { libc::ptsname_r(master, slave_name.as_mut_ptr(), slave_name.len()) } != 0
        {
            return Err(fail(ErrorKind::Io, "cannot prepare native login terminal"));
        }
        let slave = unsafe {
            libc::open(
                slave_name.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        if slave < 0 {
            return Err(fail(ErrorKind::Io, "cannot open native login terminal"));
        }
        let slave = unsafe { File::from_raw_fd(slave) };
        let size = libc::winsize {
            ws_row: 80,
            ws_col: 160,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &size) } < 0 {
            return Err(fail(ErrorKind::Io, "cannot size native login terminal"));
        }
        let stdin = slave
            .try_clone()
            .map_err(|_| fail(ErrorKind::Io, "cannot connect native login input"))?;
        let stdout = slave
            .try_clone()
            .map_err(|_| fail(ErrorKind::Io, "cannot connect native login output"))?;
        let parent = unsafe { libc::getpid() };
        let mut command = Command::new(binary);
        command.args(["--disable-slash-commands","--log-file","/dev/stderr","--settings",
            r#"{"toolPermission":"strict","permissions":{"deny":["command(*)","read_url(*)","execute_url(*)","write_file(*)"],"allow":[],"ask":[]}}"#])
            .current_dir(workspace).env_clear()
            .env("HOME",home).env("XDG_CONFIG_HOME",gemini).env("XDG_DATA_HOME",app_data).env("XDG_CACHE_HOME",cache)
            .env("PATH","/usr/local/bin:/usr/bin:/bin").env("LANG","C.UTF-8").env("TERM","xterm-256color")
            .env("BROWSER","/usr/bin/false").env("SSH_CONNECTION","runtime 0 vps 0")
            .env("AGY_CLI_DISABLE_AUTO_UPDATE","true")
            .stdin(Stdio::from(stdin)).stdout(Stdio::from(stdout)).stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0
                    || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0
                    || libc::prctl(
                        libc::PR_SET_PDEATHSIG,
                        libc::SIGKILL as libc::c_ulong,
                        0,
                        0,
                        0,
                    ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|_| fail(ErrorKind::Io, "cannot start native login"))?;
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail(
                ErrorKind::Io,
                "cannot configure native login terminal",
            ));
        }
        let watchdog = match crate::process_deadline::ProcessDeadline::new(child.id(), timeout) {
            Ok(guard) => guard,
            Err(error) => {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            terminal,
            home_lock: Some(home_lock),
            output: Vec::new(),
            total: 0,
            selected: false,
            authorization_sent: false,
            code_sent: false,
            closed: false,
            deadline: Instant::now() + timeout,
            watchdog: Some(watchdog),
        })
    }
    pub fn poll(&mut self) -> Result<LoginPtyProgress> {
        if self.closed {
            return Err(fail(ErrorKind::Closed, "native login is closed"));
        }
        if Instant::now() >= self.deadline {
            self.shutdown();
            return Err(fail(ErrorKind::Timeout, "native login expired"));
        }
        let mut chunk = [0u8; 8192];
        for _ in 0..32 {
            match self.terminal.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    self.total = self.total.saturating_add(n);
                    if self.total > MAX_OUTPUT {
                        self.shutdown();
                        return Err(fail(ErrorKind::Limit, "native login output limit"));
                    }
                    self.output.extend_from_slice(&chunk[..n]);
                    if self.output.len() > KEEP_OUTPUT {
                        self.output.drain(..self.output.len() - KEEP_OUTPUT);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(_) => {
                    self.shutdown();
                    return Err(fail(ErrorKind::Io, "native login terminal failed"));
                }
            }
        }
        if !self.selected
            && contains(&self.output, b"Select login method:")
            && contains(&self.output, b"1. Google OAuth")
        {
            self.write(b"\r")?;
            self.selected = true;
        }
        if !self.authorization_sent {
            if let Some(url) = official_google_url(&self.output) {
                self.authorization_sent = true;
                return Ok(LoginPtyProgress::Authorization { url });
            }
        }
        if self.code_sent {
            if [
                b"OAuth: token exchange failed".as_slice(),
                b"OAuth: failed to fetch user info:".as_slice(),
            ]
            .iter()
            .any(|marker| contains(&self.output, marker))
            {
                self.shutdown();
                return Ok(LoginPtyProgress::Failed);
            }
            if [
                b"OAuth: authenticated successfully as ".as_slice(),
                b"OAuth: authentication completed successfully".as_slice(),
                b"Successfully authenticated as ".as_slice(),
            ]
            .iter()
            .any(|marker| contains(&self.output, marker))
            {
                self.shutdown();
                return Ok(LoginPtyProgress::Completed);
            }
        }
        // Peek without reaping. The watchdog must stop while this PID is still
        // allocated, before shutdown waits for the child.
        let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut status,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(fail(ErrorKind::Io, "cannot inspect native login"));
        }
        if unsafe { status.si_pid() } != 0 {
            self.shutdown();
            return Ok(LoginPtyProgress::Failed);
        }
        Ok(LoginPtyProgress::Pending)
    }
    pub fn submit_code(&mut self, code: &str) -> Result<()> {
        if !self.authorization_sent
            || self.code_sent
            || self.closed
            || Instant::now() >= self.deadline
        {
            return Err(fail(
                ErrorKind::State,
                "native login is not awaiting an authorization code",
            ));
        }
        let code = code.trim();
        if code.is_empty() || code.len() > 8192 || !code.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(fail(
                ErrorKind::InvalidInput,
                "invalid private authorization code",
            ));
        }
        let mut input = code.as_bytes().to_vec();
        input.push(b'\r');
        self.write(&input)?;
        self.code_sent = true;
        self.output.clear();
        Ok(())
    }
    fn write(&mut self, mut input: &[u8]) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !input.is_empty() {
            match self.terminal.write(input) {
                Ok(0) => return Err(fail(ErrorKind::Io, "native login input closed")),
                Ok(n) => input = &input[n..],
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => return Err(fail(ErrorKind::Io, "native login input failed")),
            }
        }
        Ok(())
    }
    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.watchdog.take();
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.home_lock.take();
        self.output.clear();
    }
}
impl Drop for AntigravityLogin {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn contains(bytes: &[u8], part: &[u8]) -> bool {
    bytes.windows(part.len()).any(|p| p == part)
}
fn official_google_url(bytes: &[u8]) -> Option<String> {
    for prefix in [
        b"https://accounts.google.com/o/oauth2/auth?".as_slice(),
        b"https://accounts.google.com/o/oauth2/v2/auth?".as_slice(),
    ] {
        for (i, _) in bytes
            .windows(prefix.len())
            .enumerate()
            .filter(|(_, p)| *p == prefix)
        {
            let tail = &bytes[i..];
            let Some(end) = tail
                .iter()
                .position(|b| !b.is_ascii_graphic() || b"\"<>".contains(b))
            else {
                continue;
            };
            if end > 16384 {
                continue;
            }
            let Ok(url) = std::str::from_utf8(&tail[..end]) else {
                continue;
            };
            if valid_google_url(url) {
                return Some(url.into());
            }
        }
    }
    None
}
fn valid_google_url(url: &str) -> bool {
    let Some((endpoint, query)) = url.split_once('?') else {
        return false;
    };
    if !matches!(
        endpoint,
        "https://accounts.google.com/o/oauth2/auth"
            | "https://accounts.google.com/o/oauth2/v2/auth"
    ) || url.len() > 16384
        || url.contains('#')
    {
        return false;
    }
    let mut fields = std::collections::BTreeMap::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            return false;
        };
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || fields.insert(key, value).is_some()
        {
            return false;
        }
    }
    // Validate exact native output fields; never construct an OAuth exchange or
    // forward a native URI with ambiguous duplicate/encoded field names.
    fields.get("redirect_uri") == Some(&"https%3A%2F%2Fantigravity.google%2Foauth-callback")
        && fields.get("response_type") == Some(&"code")
        && fields.get("code_challenge_method") == Some(&"S256")
        && ["state", "code_challenge", "client_id"]
            .iter()
            .all(|key| fields.get(key).is_some_and(|v| !v.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_complete_official_native_url_is_projected_from_terminal_markup() {
        let url = "https://accounts.google.com/o/oauth2/auth?redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&response_type=code&code_challenge_method=S256&state=synthetic&client_id=synthetic&code_challenge=synthetic";
        assert!(official_google_url(url.as_bytes()).is_none());
        let osc = format!("\x1b]8;id=synthetic;{url}\x07Visible link\x1b]8;;\x07");
        assert_eq!(official_google_url(osc.as_bytes()).as_deref(), Some(url));
        assert!(
            official_google_url(
                osc.replace("accounts.google.com/", "accounts.google.com.evil/")
                    .as_bytes()
            )
            .is_none()
        );
        assert!(
            official_google_url(
                osc.replace("antigravity.google", "untrusted.example")
                    .as_bytes()
            )
            .is_none()
        );
        for suffix in [
            "&redirect_uri=untrusted",
            "&response_type=token",
            "&state=duplicate",
            "#fragment",
        ] {
            assert!(!valid_google_url(&format!("{url}{suffix}")));
        }
        assert!(!valid_google_url(
            &url.replace("redirect_uri=", "other=redirect_uri=")
        ));
        assert!(!valid_google_url(&url.replace("/auth?", "/auth/extra?")));
        assert!(!valid_google_url(&url.replace("state=synthetic", "state=")));
        for end in 1..url.len() {
            assert!(official_google_url(&url.as_bytes()[..end]).is_none());
        }
        let mut split = url.as_bytes().to_vec();
        split.push(7);
        assert_eq!(official_google_url(&split).as_deref(), Some(url));
    }
}
