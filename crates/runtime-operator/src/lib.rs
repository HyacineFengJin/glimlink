// SPDX-License-Identifier: MIT
//! A small human client. No platform protocol, model engine or media dependency.
pub mod console;
pub mod browser_login;
mod browser_proxy;
pub mod interrupt;
pub mod login;
pub mod presentation;

use runtime_control::{
    Action, Frame, LoginMethod, MAX_SECRET, Platform, Request, Result, Secret, VERSION, read_json,
    write_json,
};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::fd::AsRawFd,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Options {
    pub ssh: Option<String>,
    pub ssh_batch: bool,
    pub runtime: Option<PathBuf>,
    pub state: PathBuf,
    pub account: String,
    pub platform: Platform,
    pub action: Action,
    pub secret_file: Option<PathBuf>,
    pub profile: Option<String>,
    pub challenge_id: Option<String>,
    pub open_browser: bool,
    pub interactive: bool,
    pub method: LoginMethod,
    pub timeout_secs: u64,
    pub ui: presentation::UiMode,
}
impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>> {
        let mut args = args.into_iter();
        let mut result = Self {
            ssh: None,
            ssh_batch: false,
            runtime: None,
            state: std::env::var_os("AGENT_RUNTIME_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".runtime/state")),
            account: "public".into(),
            platform: Platform::Runtime,
            action: Action::Status,
            secret_file: None,
            profile: None,
            challenge_id: None,
            open_browser: true,
            interactive: true,
            method: LoginMethod::Auto,
            timeout_secs: 900,
            ui: presentation::UiMode::Auto,
        };
        let mut positional = Vec::new();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--ssh" => result.ssh = Some(args.next().ok_or("missing SSH destination")?),
                "--ssh-batch" => result.ssh_batch = true,
                "--runtime" => {
                    result.runtime = Some(PathBuf::from(
                        args.next().ok_or("missing Runtime executable")?,
                    ))
                }
                "--state" => {
                    result.state = PathBuf::from(args.next().ok_or("missing Runtime state path")?)
                }
                "--account" => result.account = args.next().ok_or("missing account alias")?,
                "--secret-file" => {
                    result.secret_file = Some(PathBuf::from(
                        args.next().ok_or("missing private input file")?,
                    ))
                }
                "--profile" => {
                    result.profile = Some(args.next().ok_or("missing QQ application profile")?)
                }
                "--challenge-id" => {
                    result.challenge_id = Some(args.next().ok_or("missing password challenge ID")?)
                }
                "--method" => result.method = args.next().ok_or("missing login method")?.parse()?,
                "--ui" => result.ui = args.next().ok_or("missing client UI mode")?.parse()?,
                "--timeout-secs" => {
                    result.timeout_secs = args
                        .next()
                        .ok_or("missing login timeout")?
                        .parse()
                        .map_err(|_| "invalid login timeout")?;
                    if !(1..=3600).contains(&result.timeout_secs) {
                        return Err("login timeout must be 1..3600 seconds".into());
                    }
                }
                "--no-open" => result.open_browser = false,
                "--no-ui" => {
                    result.interactive = false;
                    result.open_browser = false;
                }
                arg if arg.starts_with('-') => return Err("unknown agentctl option".into()),
                _ => positional.push(arg),
            }
        }
        if positional.len() != 2 {
            return Err("usage: agentctl [OPTIONS] PLATFORM ACTION".into());
        }
        result.platform = positional[0].parse()?;
        result.action = positional[1].parse()?;
        // Explicit private files keep their original token-import behavior.
        // Ordinary GitHub login uses the project's browser authorization.
        if result.platform == Platform::Github
            && result.secret_file.is_some()
            && result.method == LoginMethod::Auto
            && result.action == Action::Login
        {
            result.method = LoginMethod::Token;
        }
        if result
            .ssh
            .as_ref()
            .is_some_and(|host| !valid_ssh_destination(host))
        {
            return Err("invalid SSH destination; use a configured SSH host or user@host".into());
        }
        if result.state.as_os_str().is_empty() {
            return Err("Runtime state path is empty".into());
        }
        if result.profile.is_some()
            && !(result.platform == Platform::Qq
                && matches!(
                    result.action,
                    Action::Qr | Action::Login | Action::PasswordLogin
                ))
        {
            return Err("--profile is only supported for QQ login".into());
        }
        if !result.interactive && result.action == Action::Qr {
            return Err("QR login needs the local page; use --no-open to open it manually".into());
        }
        if result.challenge_id.is_some() && result.action != Action::PasswordResume {
            return Err("--challenge-id is only supported for password-resume".into());
        }
        Ok(Some(result))
    }
}
pub const HELP: &str = "agentctl [--ssh user@host] [--runtime PATH] [--state PATH] [--account ALIAS] PLATFORM login [--method auto|qr|password|device|browser|token]\n\nPlatforms: bilibili (bili), tieba, qq, github, x (twitter), pixiv, reddit, discord, codex, antigravity (agy), weixin (paused)\nManagement: status, capabilities, login, import-login, reuse-login\nLogin reuses the saved identity first. Only an explicit authentication-needed result starts a new supported flow.\nPrivate input: --secret-file FILE (regular private0600) or a hidden /dev/tty prompt; never command arguments or RPC stdin.\nWeb login: Pixiv uses the ordinary system browser plus the Glimlink Session Bridge extension; first use shows its local installation directory. It reuses the signed-in account without a debugging browser. Bilibili, Tieba, X, Reddit and Discord open an installed browser on the operator and hand off the selected platform session automatically. Runtime verifies identity before saving. Explicit --method browser refreshes the session; Bilibili/Tieba retain --method qr. --secret-file remains available.\nQQ: --profile PROFILE; verification continues in the same control session without another password file.\nLocal UI: --ui auto|browser|webview (auto uses optional adjacent WebKitGTK helper for local pages).\nOAuth HTTPS always uses the system browser. Explicit webview never silently falls back.\nHuman UI: --no-open prints the client-local page/authorization URL; --no-ui disables pages and hidden prompts except X cookie input via hidden /dev/tty.\nDeadline: --timeout-secs 1..3600 (default 900); closing the local page or Ctrl-C cancels the active flow.\n\nLocal starts the adjacent agent-runtime. Remote uses system OpenSSH with normal host-key policy.\nNo VPS HTTP port, browser engine, platform protocol or model is embedded in agentctl or the core; optional client WebView uses system libraries.";

fn valid_ssh_destination(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"@._-:[]%".contains(&c))
}
fn shell_word(value: &str) -> Result<String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err("invalid remote executable or state argument".into());
    }
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
}
/// All remote tokens are data, including a caller-chosen executable/state path.
/// SSH host verification and authentication use the normal system configuration.
pub fn launch_command(options: &Options) -> Result<Command> {
    launch_endpoint(options, "serve")
}
pub fn launch_management(options: &Options) -> Result<Command> {
    launch_endpoint(options, "manage")
}
pub fn launch_worker(options: &Options, start: bool) -> Result<Command> {
    launch_endpoint(options, if start { "worker-start" } else { "worker-stop" })
}
pub fn launch_worker_status(options: &Options) -> Result<Command> {
    launch_endpoint(options, "worker-status")
}
pub fn launch_worker_memory(options: &Options) -> Result<Command> {
    launch_endpoint(options, "worker-memory")
}
fn launch_endpoint(options: &Options, endpoint: &'static str) -> Result<Command> {
    let executable = options.runtime.clone().unwrap_or_else(|| {
        if options.ssh.is_some() {
            return PathBuf::from("agent-runtime");
        }
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|parent| parent.join("agent-runtime")))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("agent-runtime"))
    });
    let mut command = if let Some(host) = &options.ssh {
        if !valid_ssh_destination(host) {
            return Err("invalid SSH destination".into());
        }
        let words = [
            executable
                .to_str()
                .ok_or("remote executable must be UTF-8")?,
            "--state",
            options
                .state
                .to_str()
                .ok_or("remote state path must be UTF-8")?,
            "operator",
            endpoint,
        ];
        let remote = format!(
            "exec {}",
            words
                .iter()
                .map(|word| shell_word(word))
                .collect::<Result<Vec<_>>>()?
                .join(" ")
        );
        let mut command = Command::new("ssh");
        command.arg("-T");
        if options.ssh_batch {
            command
                .arg("-oBatchMode=yes")
                .arg("-oStrictHostKeyChecking=yes");
        }
        command.arg("--").arg(host).arg(remote);
        command
    } else {
        let mut command = Command::new(executable);
        command
            .arg("--state")
            .arg(&options.state)
            .arg("operator")
            .arg(endpoint);
        command
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    Ok(command)
}
pub fn private_input(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path).map_err(|_| "cannot inspect private client input")?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 || meta.len() > MAX_SECRET as u64 {
        return Err("client input must be a bounded regular private0600 file".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "cannot open private client input")?;
    let meta = file
        .metadata()
        .map_err(|_| "cannot inspect opened client input")?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 || meta.len() > MAX_SECRET as u64 {
        return Err("private client input changed or exceeds limit".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SECRET as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read private client input")?;
    if bytes.is_empty() || bytes.len() > MAX_SECRET {
        return Err("private client input size rejected".into());
    }
    Ok(bytes)
}

/// Performs one request, consuming bounded notifications before its response.
pub fn exchange(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    request: &Request,
    mut event: impl FnMut(&str, &Value) -> Result<()>,
) -> Result<Frame> {
    request.validate()?;
    write_json(writer, request)?;
    for _ in 0..512 {
        let frame =
            read_json::<Frame>(reader)?.ok_or("operator server closed before its response")?;
        if frame.version() != VERSION || frame.id() != request.id {
            return Err("operator response version or request ID differs".into());
        }
        match frame {
            Frame::Event {
                event: kind, data, ..
            } => event(&kind, &data)?,
            frame @ Frame::Response { .. } => return Ok(frame),
        }
    }
    Err("too many operator notifications".into())
}
pub(crate) struct Connection {
    child: Child,
    pub(crate) last_request_id: u64,
    frames: Receiver<Result<Frame>>,
    reader: Option<JoinHandle<()>>,
    writer: Option<ChildStdin>,
}
impl Connection {
    pub(crate) fn open(options: &Options) -> Result<Self> {
        let mut child = launch_command(options)?
            .spawn()
            .map_err(|_| "cannot start local Runtime or system SSH")?;
        let stdout = child.stdout.take().ok_or("operator output unavailable")?;
        let writer = child.stdin.take().ok_or("operator input unavailable")?;
        let flags = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cannot configure bounded operator input pipe".into());
        }
        let (tx, frames) = mpsc::sync_channel(8);
        let reader = thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            loop {
                match read_json::<Frame>(&mut input) {
                    Ok(Some(frame)) => {
                        if tx.send(Ok(frame)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(Err("operator server closed before its response".into()));
                        break;
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            last_request_id: 0,
            frames,
            reader: Some(reader),
            writer: Some(writer),
        })
    }
    fn request(
        &mut self,
        request: &Request,
        event: impl FnMut(&str, &Value) -> Result<()>,
    ) -> Result<Frame> {
        self.request_until(
            request,
            Instant::now() + Duration::from_secs(180),
            true,
            event,
        )
    }
    pub(crate) fn request_until(
        &mut self,
        request: &Request,
        deadline: Instant,
        interruptible: bool,
        event: impl FnMut(&str, &Value) -> Result<()>,
    ) -> Result<Frame> {
        self.request_until_checked(request, deadline, interruptible, event, || Ok(()))
    }
    pub(crate) fn request_until_checked(
        &mut self,
        request: &Request,
        deadline: Instant,
        interruptible: bool,
        mut event: impl FnMut(&str, &Value) -> Result<()>,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Frame> {
        check()?;
        request.validate()?;
        self.last_request_id = request.id;
        let mut bytes = Vec::new();
        write_json(&mut bytes, request)?;
        let send = (|| -> Result<()> {
            let writer = self.writer.as_mut().ok_or("operator pipe closed")?;
            let mut written = 0;
            while written < bytes.len() {
                check()?;
                if interruptible && interrupt::is_interrupted() {
                    return Err("operator_cancelled".into());
                }
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or("operator_timeout")?;
                let mut ready = libc::pollfd {
                    fd: writer.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut ready, 1, remaining.as_millis().clamp(1, 100) as i32) }
                    <= 0
                {
                    continue;
                }
                match writer.write(&bytes[written..]) {
                    Ok(0) => return Err("operator input pipe closed".into()),
                    Ok(count) => written += count,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err("operator input write failed".into()),
                }
            }
            Ok(())
        })();
        if send.is_err() {
            // A partially written JSONL frame cannot be followed by another request.
            // EOF cancels the server's connection-owned flow without ambiguity.
            self.writer.take();
        }
        send?;
        let mut count = 0;
        loop {
            check()?;
            if interruptible && interrupt::is_interrupted() {
                return Err("operator_cancelled".into());
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("operator_timeout")?;
            let frame = match self
                .frames
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(frame) => frame?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err("operator response pipe closed".into()),
            };
            if frame.version() != VERSION || frame.id() != request.id {
                return Err("operator response version or request ID differs".into());
            }
            match frame {
                Frame::Event {
                    event: kind, data, ..
                } => {
                    count += 1;
                    if count > 512 {
                        return Err("too many operator notifications".into());
                    }
                    event(&kind, &data)?;
                }
                frame @ Frame::Response { .. } => return Ok(frame),
            }
        }
    }
    pub(crate) fn finish(mut self) -> Result<()> {
        drop(self.writer.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|_| "cannot wait for operator server")?
            {
                return if status.success() {
                    Ok(())
                } else {
                    Err("operator server exited unsuccessfully".into())
                };
            }
            if Instant::now() >= deadline {
                return Err("operator server did not exit after stdin closed".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        drop(self.writer.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            // A broken remote SSH may retain inherited descriptors: do not block shutdown waiting for that reader.
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

/// Bounded, nonprinting GUI/API management request. Interactive login flows
/// must use login::run_with_ui so their opaque IDs keep one control connection.
/// SSH uses only configured keys/agent and verified known_hosts, never a prompt.
pub fn request_once(options: &Options, request: &Request, deadline: Instant) -> Result<Frame> {
    if !matches!(
        request.action,
        Action::Status | Action::Capabilities | Action::ReuseLogin | Action::ImportLogin
    ) {
        return Err(
            "interactive login requires run_with_ui and one persistent control connection".into(),
        );
    }
    let mut options = options.clone();
    options.ssh_batch = true;
    let mut connection = Connection::open(&options)?;
    let frame = connection.request_until(request, deadline, false, |_, _| Ok(()))?;
    connection.finish()?;
    Ok(frame)
}

fn browser(url: &str, open: bool) {
    if !open {
        eprintln!("Human verification: {url}");
        return;
    }
    eprintln!("Opening the system browser for human verification.");
    // The URL is always a newly created client loopback page, never shell code.
    let spawned = Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(url)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn();
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
fn display_safe(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for field in [
                "authorization",
                "image_base64",
                "qr_url",
                "verify_url",
                "url",
                "user_code",
                "authorization_code",
                "access_token",
                "refresh_token",
                "id_token",
                "token",
                "secret",
                "password",
                "password_md5",
                "cookies",
                "cookie",
                "auth_token",
                "ct0",
                "twid",
                "ticket",
                "randstr",
                "session_id",
            ] {
                object.remove(field);
            }
            for value in object.values_mut() {
                display_safe(value);
            }
        }
        Value::Array(array) => {
            for value in array {
                display_safe(value);
            }
        }
        _ => {}
    }
}
pub fn run(options: Options) -> Result<bool> {
    run_with_exit_code(options).map(|code| code == 0)
}
pub fn run_with_exit_code(mut options: Options) -> Result<i32> {
    if matches!(
        options.action,
        Action::Login | Action::Qr | Action::PasswordLogin
    ) {
        if options.action == Action::Qr {
            options.method = LoginMethod::Qr;
        }
        if options.action == Action::PasswordLogin {
            options.method = LoginMethod::Password;
        }
        options.action = Action::Login;
        return login::run(options);
    }
    if options.action == Action::PasswordResume {
        return Err("QQ verification now continues automatically in agentctl qq login; standalone password-resume cannot attach to that control session and does not require another password file".into());
    }
    run_legacy(options).map(|ok| if ok { 0 } else { 1 })
}
fn run_legacy(options: Options) -> Result<bool> {
    let mut request = Request::new(1, options.platform, options.account.clone(), options.action);
    request.secret = options
        .secret_file
        .as_deref()
        .map(private_input)
        .transpose()?
        .map(Secret::new)
        .transpose()?;
    request.validate()?;
    let mut connection = Connection::open(&options)?;
    let frame = connection.request(&request, |_, _| Ok(()))?;
    let success = matches!(&frame,Frame::Response{ok:true,result,..}if !matches!(result["status"].as_str(),Some("rejected"|"expired"|"canceled"|"cancelled"|"failed")));
    let mut output = serde_json::to_value(&frame).map_err(|_| "cannot encode operator result")?;
    display_safe(&mut output);
    println!(
        "{}",
        serde_json::to_string(&output).map_err(|_| "cannot encode operator output")?
    );
    connection.finish()?;
    Ok(success)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;
    fn options(args: &[&str]) -> Options {
        Options::parse(args.iter().map(|s| s.to_string()))
            .unwrap()
            .unwrap()
    }
    #[test]
    fn local_and_ssh_use_same_fixed_server_and_preserve_host_verification() {
        let local = options(&[
            "--runtime",
            "/tmp/runtime path",
            "--state",
            "/tmp/state path",
            "qq",
            "status",
        ]);
        let command = launch_command(&local).unwrap();
        assert_eq!(command.get_program(), "/tmp/runtime path");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--state", "/tmp/state path", "operator", "serve"]
        );
        let remote = options(&[
            "--ssh",
            "person@host-alias",
            "--runtime",
            "/opt/runtime's/bin/runtime",
            "--state",
            "/data/state;touch /tmp/pwn",
            "qq",
            "status",
        ]);
        let command = launch_command(&remote).unwrap();
        assert_eq!(command.get_program(), "ssh");
        let args = command
            .get_args()
            .map(|v| v.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&args[..3], ["-T", "--", "person@host-alias"]);
        assert_eq!(
            args[3],
            "exec '/opt/runtime'\"'\"'s/bin/runtime' '--state' '/data/state;touch /tmp/pwn' 'operator' 'serve'"
        );
        assert!(
            !args
                .iter()
                .any(|s| s.contains("StrictHostKeyChecking") || s.contains("ProxyCommand"))
        );
        assert!(
            Options::parse(["--ssh", "-oProxyCommand=evil", "qq", "status"].map(str::to_string))
                .is_err()
        );
        let batch = options(&["--ssh", "configured-host", "--ssh-batch", "qq", "status"]);
        let command = launch_command(&batch).unwrap();
        let args = command.get_args().collect::<Vec<_>>();
        assert!(args.contains(&std::ffi::OsStr::new("-oBatchMode=yes")));
        assert!(args.contains(&std::ffi::OsStr::new("-oStrictHostKeyChecking=yes")));
    }
    #[test]
    fn events_are_correlated_and_secrets_never_enter_command_arguments() {
        let mut request = Request::new(8, Platform::Qq, "synthetic".into(), Action::PasswordLogin);
        request.secret = Some(Secret::new(b"synthetic-password".to_vec()).unwrap());
        let mut fixture = Vec::new();
        write_json(
            &mut fixture,
            &Frame::event(8, "qr_status", json!({"status":"waiting_for_scan"})),
        )
        .unwrap();
        write_json(&mut fixture, &Frame::success(8, json!({"status":"online"}))).unwrap();
        let mut sent = Vec::new();
        let mut count = 0;
        let frame = exchange(&mut Cursor::new(fixture), &mut sent, &request, |kind, _| {
            assert_eq!(kind, "qr_status");
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        assert!(matches!(frame, Frame::Response { ok: true, .. }));
        let decoded: Request = read_json(&mut Cursor::new(sent)).unwrap().unwrap();
        assert_eq!(decoded.secret.unwrap().bytes(), b"synthetic-password");
        let mut wrong = Vec::new();
        write_json(&mut wrong, &Frame::success(9, json!({}))).unwrap();
        assert!(
            exchange(
                &mut Cursor::new(wrong),
                &mut Vec::new(),
                &request,
                |_, _| Ok(())
            )
            .is_err()
        );
        let command = launch_command(&options(&[
            "--ssh",
            "host",
            "--secret-file",
            "/private/input",
            "--account",
            "main",
            "qq",
            "password-login",
        ]))
        .unwrap();
        assert!(!format!("{command:?}").contains("/private/input"));
    }
    #[test]
    fn private_file_and_display_boundaries() {
        let dir = std::env::temp_dir().join(format!("agentctl-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("input");
        fs::write(&file, b"synthetic").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(private_input(&file).unwrap(), b"synthetic");
        let link = dir.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(private_input(&link).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_input(&file).is_err());
        let mut output = json!({"result":{"image_base64":"synthetic-image","challenge":{"verify_url":"synthetic-private-url","kind":"captcha"}}});
        display_safe(&mut output);
        assert_eq!(output, json!({"result":{"challenge":{"kind":"captcha"}}}));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn human_authorization_and_credentials_never_enter_stdout_projection() {
        let mut value = serde_json::json!({"status":"pending","authorization":{"url":"https://accounts.google.com/synthetic","user_code":"synthetic-user-code","message":"synthetic authorization URL"},"qr":{"qr_url":"https://example.invalid/synthetic","image_base64":"synthetic-image"},"access_token":"synthetic-access","refresh_token":"synthetic-refresh","id_token":"synthetic-id","authorization_code":"synthetic-code","captcha":{"ticket":"synthetic-ticket","randstr":"synthetic-rand","session_id":"synthetic-session"},"auth_token":"synthetic-auth-cookie","ct0":"synthetic-csrf-cookie","twid":"synthetic-user-cookie","error":{"code":"risk_control","message":"Safe public message"}});
        display_safe(&mut value);
        let output = serde_json::to_string(&value).unwrap();
        assert!(!output.contains("synthetic"));
        assert_eq!(value["error"]["code"], "risk_control");
        assert_eq!(value["status"], "pending");
    }
    #[test]
    fn stalled_stdio_has_a_bounded_deadline() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("agentctl-stalled-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("fixture");
        fs::write(&fixture, b"#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = options(&["bili", "status"]);
        config.runtime = Some(fixture);
        let mut connection = Connection::open(&config).unwrap();
        let request = Request::new(1, Platform::Bilibili, "synthetic".into(), Action::Status);
        assert_eq!(
            connection
                .request_until(
                    &request,
                    Instant::now() + Duration::from_millis(40),
                    true,
                    |_, _| Ok(())
                )
                .unwrap_err(),
            "operator_timeout"
        );
        drop(connection);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn gui_status_cancels_a_stalled_rpc_without_installing_global_signal() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("gui-stalled-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("fixture");
        fs::write(&fixture, b"#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = options(&["bili", "status"]);
        options.runtime = Some(fixture);
        let mut connection = Connection::open(&options).unwrap();
        let request = Request::new(1, Platform::Bilibili, "synthetic".into(), Action::Status);
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            signal.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        assert_eq!(
            connection
                .request_until_checked(
                    &request,
                    started + Duration::from_secs(3),
                    false,
                    |_, _| Ok(()),
                    || if cancelled.load(Ordering::Relaxed) {
                        Err("operator_cancelled".into())
                    } else {
                        Ok(())
                    }
                )
                .unwrap_err(),
            "operator_cancelled"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        thread.join().unwrap();
        drop(connection);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn large_secret_write_to_stalled_child_obeys_same_deadline() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("agentctl-stalled-write-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("fixture");
        fs::write(&fixture, b"#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = options(&["--account", "synthetic", "qq", "import-login"]);
        config.runtime = Some(fixture);
        let mut connection = Connection::open(&config).unwrap();
        let mut request = Request::new(1, Platform::Qq, "synthetic".into(), Action::ImportLogin);
        request.secret = Some(Secret::new(vec![b'x'; MAX_SECRET]).unwrap());
        assert_eq!(
            connection
                .request_until(
                    &request,
                    Instant::now() + Duration::from_millis(40),
                    true,
                    |_, _| Ok(())
                )
                .unwrap_err(),
            "operator_timeout"
        );
        assert!(connection.writer.is_none());
        drop(connection);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn local_spawn_uses_real_stdio_and_clean_eof() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("agentctl-spawn-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("synthetic runtime's fixture");
        // This is a synthetic local transport endpoint, never an SSH connection
        // or a platform login. No input is echoed to logs or process arguments.
        fs::write(&fixture,b"#!/bin/sh\nwhile IFS= read -r request; do\n  printf '%s\\n' '{\"type\":\"response\",\"version\":1,\"id\":1,\"ok\":true,\"result\":{\"status\":\"signed_out\"}}'\ndone\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = options(&["--no-ui", "bili", "status"]);
        config.runtime = Some(fixture);
        config.state = dir.join("state with spaces");
        assert!(run(config.clone()).unwrap());
        let request = Request::new(1, Platform::Bilibili, "synthetic".into(), Action::Status);
        assert!(matches!(
            request_once(&config, &request, Instant::now() + Duration::from_secs(1)).unwrap(),
            Frame::Response { ok: true, .. }
        ));
        let interactive = Request::new(1, Platform::Bilibili, "synthetic".into(), Action::Login);
        assert!(
            request_once(
                &config,
                &interactive,
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn ssh_remote_shell_preserves_arguments_and_jsonl_without_network() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("agentctl-ssh-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("runtime's fixture");
        let recorded = dir.join("argv");
        let injected = dir.join("injected");
        fs::write(&fixture,b"#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AGENTCTL_FIXTURE_ARGS\"\nwhile IFS= read -r request; do\n  printf '%s\\n' '{\"type\":\"response\",\"version\":1,\"id\":1,\"ok\":true,\"result\":{\"status\":\"signed_out\"}}'\ndone\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = options(&["--ssh", "synthetic-host-never-contacted", "qq", "status"]);
        config.runtime = Some(fixture);
        let state = format!("state ' ; touch {} #", injected.display());
        config.state = PathBuf::from(&state);
        let command = launch_command(&config).unwrap();
        let remote = command.get_args().last().unwrap();
        // OpenSSH hands this exact string to the remote login shell. Execute it
        // locally against a fixture to verify its shell semantics, without SSH.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(remote)
            .env("AGENTCTL_FIXTURE_ARGS", &recorded)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let request = Request::new(1, Platform::Qq, "synthetic".into(), Action::Status);
        assert!(matches!(
            exchange(&mut reader, &mut stdin, &request, |_, _| Ok(())).unwrap(),
            Frame::Response { ok: true, .. }
        ));
        drop(stdin);
        assert!(child.wait().unwrap().success());
        let argv = fs::read_to_string(recorded).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            ["--state", &state, "operator", "serve"]
        );
        assert!(!injected.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
