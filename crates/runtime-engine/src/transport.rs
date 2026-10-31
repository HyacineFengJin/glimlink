// SPDX-License-Identifier: MIT
use crate::{EngineError, ErrorKind, Result};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

enum Incoming {
    Line(Vec<u8>),
    Failure(ErrorKind),
    Eof,
}
struct Outgoing {
    bytes: Vec<u8>,
    result: SyncSender<bool>,
}

pub(crate) struct Transport {
    child: Child,
    executable: Option<File>,
    input: Option<SyncSender<Outgoing>>,
    output: Option<Receiver<Incoming>>,
    workers: Vec<JoinHandle<()>>,
    line_limit: usize,
    closed: bool,
    lifetime: Option<crate::process_deadline::ProcessDeadline>,
}

fn read_line_bounded(reader: &mut impl BufRead, limit: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "incomplete JSONL",
                ))
            };
        }
        let count = chunk
            .iter()
            .position(|c| *c == b'\n')
            .map_or(chunk.len(), |p| p + 1);
        if line.len().saturating_add(count) > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "JSONL limit",
            ));
        }
        let ended = chunk[count - 1] == b'\n';
        line.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if ended {
            return Ok(Some(line));
        }
    }
}

impl Transport {
    pub fn spawn(mut command: Command, line_limit: usize, stderr_limit: usize) -> Result<Self> {
        // Keep the actual executable inode available for a best-effort cache
        // hint after exit. Serial provider handoffs otherwise retain both large
        // native CLIs' file pages inside a small cgroup.
        let executable = File::open(command.get_program()).ok();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // Capture in the parent, before fork. Installing PDEATHSIG alone
            // would miss a parent that died between fork and prctl. The signal
            // covers this direct app-server; Linux clears it in fork children.
            // Normal shutdown additionally kills its process group and waits
            // for the direct child. Other descendants are not adopted here.
            let parent = unsafe { libc::getpid() };
            unsafe {
                command.pre_exec(move || {
                    // Post-fork, pre-exec: no allocation, formatting, locks or
                    // application callbacks. prctl/getppid are Linux syscalls;
                    // these io::Error constructors retain only an inline errno.
                    if libc::prctl(
                        libc::PR_SET_PDEATHSIG,
                        libc::SIGKILL as libc::c_ulong,
                        0 as libc::c_ulong,
                        0 as libc::c_ulong,
                        0 as libc::c_ulong,
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
        }
        // Other Unix targets retain process-group cleanup on normal Drop;
        // no Linux-style parent-death guarantee is implied on those targets.
        let mut child = command
            .spawn()
            .map_err(|_| EngineError::new(ErrorKind::Io, "cannot spawn native app-server"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdin: ChildStdin = child.stdin.take().expect("piped stdin");
        let (output_tx, output) = mpsc::sync_channel(16);
        let errors = output_tx.clone();
        let out_reader = thread::spawn(move || {
            let mut reader = BufReader::with_capacity(8192, stdout);
            loop {
                let message = match read_line_bounded(&mut reader, line_limit) {
                    Ok(Some(line)) => Incoming::Line(line),
                    Ok(None) => {
                        let _ = output_tx.send(Incoming::Eof);
                        break;
                    }
                    Err(error) => {
                        let kind = if error.kind() == std::io::ErrorKind::FileTooLarge {
                            ErrorKind::Limit
                        } else {
                            ErrorKind::Protocol
                        };
                        let _ = output_tx.send(Incoming::Failure(kind));
                        break;
                    }
                };
                if output_tx.send(message).is_err() {
                    break;
                }
            }
        });
        let err_reader = thread::spawn(move || {
            // stderr can contain credentials or command output. Never retain or echo it.
            let mut stderr = stderr;
            let mut count = 0_usize;
            let mut chunk = [0_u8; 8192];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        count = count.saturating_add(n);
                        if count > stderr_limit {
                            let _ = errors.send(Incoming::Failure(ErrorKind::Limit));
                            break;
                        }
                    }
                }
            }
        });
        let (input, requests) = mpsc::sync_channel::<Outgoing>(1);
        let writer = thread::spawn(move || {
            let mut stdin = stdin;
            while let Ok(request) = requests.recv() {
                let ok = stdin
                    .write_all(&request.bytes)
                    .and_then(|_| stdin.flush())
                    .is_ok();
                let _ = request.result.send(ok);
                if !ok {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            executable,
            input: Some(input),
            output: Some(output),
            workers: vec![out_reader, err_reader, writer],
            line_limit,
            closed: false,
            lifetime: None,
        })
    }

    pub fn write(&mut self, value: &Value, deadline: Instant) -> Result<()> {
        if self.closed {
            return Err(EngineError::new(ErrorKind::Closed, "app-server is closed"));
        }
        let mut bytes = serde_json::to_vec(value)
            .map_err(|_| EngineError::new(ErrorKind::Protocol, "cannot encode RPC"))?;
        bytes.push(b'\n');
        if bytes.len() > self.line_limit {
            return Err(EngineError::new(
                ErrorKind::Limit,
                "outgoing JSONL exceeds limit",
            ));
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.input
            .as_ref()
            .ok_or_else(|| EngineError::new(ErrorKind::Closed, "app-server is closed"))?
            .try_send(Outgoing { bytes, result: tx })
            .map_err(|_| EngineError::new(ErrorKind::Closed, "app-server writer is unavailable"))?;
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(true) => Ok(()),
            Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(EngineError::new(
                ErrorKind::Closed,
                "app-server pipe closed",
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(EngineError::new(
                ErrorKind::Timeout,
                "app-server write timed out",
            )),
        }
    }
    pub fn read(&mut self, deadline: Instant) -> Result<(Value, usize)> {
        self.poll(deadline)?
            .ok_or_else(|| EngineError::new(ErrorKind::Timeout, "app-server response timed out"))
    }
    /// A short receive interval lets another bounded local transport be served
    /// while an engine request is outstanding. None only means no message yet.
    pub fn poll(&mut self, deadline: Instant) -> Result<Option<(Value, usize)>> {
        let message = self
            .output
            .as_ref()
            .ok_or_else(|| EngineError::new(ErrorKind::Closed, "app-server is closed"))?
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        match message {
            Ok(Incoming::Line(line)) => {
                let size = line.len();
                let value: Value = serde_json::from_slice(&line).map_err(|_| {
                    EngineError::new(ErrorKind::Protocol, "invalid app-server JSONL")
                })?;
                if !value.is_object() {
                    return Err(EngineError::new(
                        ErrorKind::Protocol,
                        "RPC message is not an object",
                    ));
                }
                Ok(Some((value, size)))
            }
            Ok(Incoming::Failure(kind)) => Err(EngineError::new(
                kind,
                "app-server stream exceeded a limit or was malformed",
            )),
            Ok(Incoming::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(EngineError::new(
                ErrorKind::Closed,
                "app-server exited or closed stdout",
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        }
    }
    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        // Stop the timer before wait/reap, while this child's PID cannot be reused.
        self.lifetime.take();
        self.input.take();
        self.output.take();
        #[cfg(unix)]
        unsafe {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // The app-server and its inherited children have a dedicated process group.
            let _ = kill(-(self.child.id() as i32), 9);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(target_os = "linux")]
        if let Some(file) = self.executable.take() {
            use std::os::fd::AsRawFd;
            // Read-only advisory on this inode, after the engine has exited.
            // Mapped pages still used by other processes remain protected by
            // the kernel. Unsupported hints must not change task outcomes.
            unsafe {
                let _ = libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        for worker in self.workers.drain(..) {
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
    pub(crate) fn set_lifetime(&mut self, timeout: Duration) -> Result<()> {
        self.lifetime.take();
        self.lifetime = Some(crate::process_deadline::ProcessDeadline::new(
            self.child.id(),
            timeout,
        )?);
        Ok(())
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.shutdown();
    }
}
