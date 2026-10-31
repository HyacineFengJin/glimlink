// SPDX-License-Identifier: MIT
//! Optional client-local presentation. HTTPS provider authorization always uses
//! the system browser; the optional helper accepts only our nonce-scoped pages.
use crate::interrupt;
use runtime_control::{
    Result, VERSION,
    human_ui::UiStatus,
    login_window::{
        MAX_WINDOW_EVENT, MAX_WINDOW_FRAME, WindowErrorCode, WindowEvent, WindowRequest,
    },
};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiMode {
    #[default]
    Auto,
    Browser,
    Webview,
}
impl std::str::FromStr for UiMode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "browser" => Ok(Self::Browser),
            "webview" => Ok(Self::Webview),
            _ => Err("UI must be auto, browser or webview".into()),
        }
    }
}
pub(crate) enum LocalAction {
    Window,
    Browser { open: bool },
}
pub(crate) struct Presenter {
    mode: UiMode,
    helper: Option<PathBuf>,
    gui: bool,
    window: Option<Window>,
}
impl Presenter {
    pub(crate) fn new(mode: UiMode) -> Self {
        let helper = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.parent()
                    .map(|parent| parent.join("agent-login-window"))
            })
            .filter(|path| path.is_file());
        let gui = ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
        Self {
            mode,
            helper,
            gui,
            window: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn fixture(mode: UiMode, helper: PathBuf) -> Self {
        Self {
            mode,
            helper: Some(helper),
            gui: true,
            window: None,
        }
    }
    pub(crate) fn local(
        &mut self,
        url: &str,
        open: bool,
        timeout_secs: u64,
    ) -> Result<LocalAction> {
        let mut request = WindowRequest::new(url);
        request.timeout_secs = timeout_secs.clamp(1, 3600);
        request.validate()?;
        self.close();
        if !open {
            return Ok(LocalAction::Browser { open: false });
        }
        if self.mode == UiMode::Browser {
            return Ok(LocalAction::Browser { open: true });
        }
        let unavailable = if !self.gui {
            Some("no local graphical display")
        } else if self.helper.is_none() {
            Some("adjacent agent-login-window helper is unavailable")
        } else {
            None
        };
        if let Some(reason) = unavailable {
            return self.fallback(reason);
        }
        match Window::start(self.helper.as_ref().unwrap(), &request) {
            Ok(window) => {
                self.window = Some(window);
                eprintln!("Opened local verification in the optional WebView window.");
                Ok(LocalAction::Window)
            }
            Err(StartError::Cancelled) => Err("operator_cancelled".into()),
            Err(StartError::Unavailable(reason)) => self.fallback(reason),
        }
    }
    fn fallback(&self, reason: &str) -> Result<LocalAction> {
        if self.mode == UiMode::Webview {
            eprintln!(
                "Requested WebView could not start ({reason}); choose --ui browser to use the system browser explicitly."
            );
            return Err(format!(
                "Requested WebView could not start: {reason}; use --ui browser to choose the system browser explicitly"
            ));
        }
        eprintln!(
            "WebView unavailable ({reason}); using the system browser for local verification."
        );
        Ok(LocalAction::Browser { open: true })
    }
    pub(crate) fn status(&self) -> UiStatus {
        self.window
            .as_ref()
            .map_or(UiStatus::Active, Window::status)
    }
    pub(crate) fn close(&mut self) {
        self.window.take();
    }
}

const STARTING: u8 = 0;
const READY: u8 = 1;
const CLOSED: u8 = 2;
const FAILED: u8 = 3;
const EXPIRED: u8 = 4;
enum StartError {
    Cancelled,
    Unavailable(&'static str),
}
struct Window {
    child: Child,
    input: Option<ChildStdin>,
    state: Arc<AtomicU8>,
    closing: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}
fn error_name(code: WindowErrorCode) -> &'static str {
    match code {
        WindowErrorCode::MissingWebkit => "system WebKitGTK is unavailable",
        WindowErrorCode::NoDisplay => "no local graphical display",
        WindowErrorCode::InitFailed => "native window initialization failed",
        WindowErrorCode::NavigationFailed => "local verification page could not load",
        WindowErrorCode::InvalidInput => "helper rejected the local page request",
        WindowErrorCode::Expired => "window deadline elapsed",
    }
}
fn read_event(reader: &mut impl BufRead) -> Result<Option<WindowEvent>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| "window event read failed")?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err("incomplete window event".into())
            };
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |at| at + 1);
        if bytes.len() + count > MAX_WINDOW_EVENT {
            return Err("window event exceeds limit".into());
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            break;
        }
    }
    let event: WindowEvent = serde_json::from_slice(&bytes).map_err(|_| "invalid window event")?;
    if event.version() != VERSION {
        return Err("window protocol version differs".into());
    }
    Ok(Some(event))
}
impl Window {
    fn start(
        executable: &PathBuf,
        request: &WindowRequest,
    ) -> std::result::Result<Self, StartError> {
        let mut bytes = serde_json::to_vec(request)
            .map_err(|_| StartError::Unavailable("window request encoding failed"))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_WINDOW_FRAME {
            return Err(StartError::Unavailable("window request exceeds limit"));
        }
        let mut child = Command::new(executable)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| StartError::Unavailable("helper process could not start"))?;
        let input = child.stdin.take();
        let output = child.stdout.take();
        let state = Arc::new(AtomicU8::new(STARTING));
        let closing = Arc::new(AtomicBool::new(false));
        let mut window = Self {
            child,
            input,
            state: state.clone(),
            closing: closing.clone(),
            reader: None,
        };
        let Some(output) = output else {
            return Err(StartError::Unavailable("helper output is unavailable"));
        };
        let (sender, receiver) = mpsc::sync_channel(4);
        window.reader = Some(thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                match read_event(&mut reader) {
                    Ok(Some(event)) => {
                        let terminal = match &event {
                            WindowEvent::Ready { .. } => {
                                state.store(READY, Ordering::Relaxed);
                                false
                            }
                            WindowEvent::Closed { .. } => {
                                state.store(CLOSED, Ordering::Relaxed);
                                true
                            }
                            WindowEvent::Error { code, .. } => {
                                state.store(
                                    if *code == WindowErrorCode::Expired {
                                        EXPIRED
                                    } else {
                                        FAILED
                                    },
                                    Ordering::Relaxed,
                                );
                                true
                            }
                        };
                        let _ = sender.try_send(Ok(event));
                        if terminal {
                            break;
                        }
                    }
                    _ => {
                        if !closing.load(Ordering::Relaxed) {
                            state.store(FAILED, Ordering::Relaxed);
                            let _ = sender
                                .try_send(Err("helper exited or violated its private protocol"));
                        }
                        break;
                    }
                }
            }
        }));
        window
            .input
            .as_mut()
            .ok_or(StartError::Unavailable("helper input is unavailable"))?
            .write_all(&bytes)
            .map_err(|_| StartError::Unavailable("helper rejected its startup request"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if interrupt::is_interrupted() {
                return Err(StartError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(StartError::Unavailable("helper readiness deadline elapsed"));
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(WindowEvent::Ready { .. })) => return Ok(window),
                Ok(Ok(WindowEvent::Closed { .. })) => return Err(StartError::Cancelled),
                Ok(Ok(WindowEvent::Error { code, .. })) => {
                    return Err(StartError::Unavailable(error_name(code)));
                }
                Ok(Err(reason)) => return Err(StartError::Unavailable(reason)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err(StartError::Unavailable("helper exited before it was ready")),
            }
        }
    }
    fn status(&self) -> UiStatus {
        match self.state.load(Ordering::Relaxed) {
            CLOSED => UiStatus::Cancelled,
            FAILED => UiStatus::Failed,
            EXPIRED => UiStatus::TimedOut,
            _ => UiStatus::Active,
        }
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Relaxed);
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                _ => {
                    // The helper owns its own process group, including WebKit children.
                    unsafe {
                        libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
                    }
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        // Any surviving WebKit descendants belong to the helper's dedicated
        // group, even when its main process exited before they finished.
        unsafe {
            libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
        }
        if let Some(reader) = self.reader.take() {
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Cursor, os::unix::fs::PermissionsExt};
    fn url() -> String {
        format!("http://127.0.0.1:19799/{}", "a".repeat(48))
    }
    fn fixture(name: &str, script: &str) -> PathBuf {
        let path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("window-fixture-{}-{name}", std::process::id()));
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn presenter(mode: UiMode, helper: Option<PathBuf>, gui: bool) -> Presenter {
        Presenter {
            mode,
            helper,
            gui,
            window: None,
        }
    }
    #[test]
    fn mode_policy_falls_back_only_for_auto_and_never_sends_oauth_to_helper() {
        let mut auto = presenter(UiMode::Auto, None, true);
        assert!(matches!(
            auto.local(&url(), true, 120).unwrap(),
            LocalAction::Browser { open: true }
        ));
        let mut explicit = presenter(UiMode::Webview, None, true);
        assert!(explicit.local(&url(), true, 120).is_err());
        let mut no_display = presenter(UiMode::Auto, Some("/not/spawned".into()), false);
        assert!(matches!(
            no_display.local(&url(), true, 120).unwrap(),
            LocalAction::Browser { open: true }
        ));
        assert!(
            explicit
                .local("https://accounts.google.com/o/oauth2/auth", true, 120)
                .is_err()
        );
        assert!(matches!(
            explicit.local(&url(), false, 120).unwrap(),
            LocalAction::Browser { open: false }
        ));
        assert_eq!("auto".parse::<UiMode>().unwrap(), UiMode::Auto);
        assert!("chromium".parse::<UiMode>().is_err());
    }
    #[test]
    fn ready_closed_and_owner_eof_use_only_lifecycle_events() {
        let helper = fixture(
            "ready",
            "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"version\":1,\"event\":\"ready\"}'\nwhile IFS= read -r ignored; do :; done\n",
        );
        let mut view = presenter(UiMode::Webview, Some(helper.clone()), true);
        assert!(matches!(
            view.local(&url(), true, 120).unwrap(),
            LocalAction::Window
        ));
        assert_eq!(view.status(), UiStatus::Active);
        let argv = fs::read(format!(
            "/proc/{}/cmdline",
            view.window.as_ref().unwrap().child.id()
        ))
        .unwrap();
        assert!(!String::from_utf8_lossy(&argv).contains("http://"));
        view.close();
        assert_eq!(view.status(), UiStatus::Active);
        fs::remove_file(helper).unwrap();
        let helper = fixture(
            "closed",
            "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"version\":1,\"event\":\"ready\"}' '{\"version\":1,\"event\":\"closed\"}'\n",
        );
        let mut view = presenter(UiMode::Webview, Some(helper.clone()), true);
        view.local(&url(), true, 120).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while view.status() == UiStatus::Active && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(view.status(), UiStatus::Cancelled);
        view.close();
        fs::remove_file(helper).unwrap();
    }
    #[test]
    fn startup_error_is_explicit_and_helper_protocol_cannot_leak_urls() {
        let helper = fixture(
            "missing",
            "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"version\":1,\"event\":\"error\",\"code\":\"missing_webkit\"}'\n",
        );
        let mut auto = presenter(UiMode::Auto, Some(helper.clone()), true);
        assert!(matches!(
            auto.local(&url(), true, 120).unwrap(),
            LocalAction::Browser { open: true }
        ));
        let mut explicit = presenter(UiMode::Webview, Some(helper.clone()), true);
        assert!(explicit.local(&url(), true, 120).is_err());
        fs::remove_file(helper).unwrap();
        assert!(
            read_event(&mut Cursor::new(
                b"{\"version\":1,\"event\":\"error\",\"code\":\"https://secret.invalid\"}\n"
            ))
            .is_err()
        );
        assert!(read_event(&mut Cursor::new(vec![b'x'; MAX_WINDOW_EVENT + 1])).is_err());
        let helper = fixture(
            "closed-before-ready",
            "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"version\":1,\"event\":\"closed\"}'\n",
        );
        let mut auto = presenter(UiMode::Auto, Some(helper.clone()), true);
        assert!(matches!(auto.local(&url(),true,120),Err(error)if error=="operator_cancelled"));
        fs::remove_file(helper).unwrap();
    }
}
