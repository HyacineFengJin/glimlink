// SPDX-License-Identifier: MIT
//! Per-execution MCP transport. The proxy never opens Runtime or account state;
//! the parent dispatches all business calls serially through its existing host.
use crate::mcp::Session;
use runtime_core::Result;
use runtime_engine::{ToolCall, ToolResult};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    net::Shutdown,
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(600);
const TICK: Duration = Duration::from_millis(25);

struct Request {
    bytes: Vec<u8>,
    reply: SyncSender<Option<Value>>,
}

pub struct Bridge {
    path: PathBuf,
    inode: (u64, u64),
    requests: Option<Receiver<Request>>,
    stop: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    active: Arc<Mutex<Option<UnixStream>>>,
    worker: Option<JoinHandle<()>>,
}

fn private_parent(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > 100 {
        return Err("engine bridge needs a short absolute socket path".into());
    }
    let parent = path.parent().ok_or("engine bridge parent missing")?;
    let meta = fs::symlink_metadata(parent).map_err(|_| "engine bridge parent unavailable")?;
    if !meta.is_dir()
        || meta.permissions().mode() & 0o777 != 0o700
        || meta.uid() != unsafe { libc::geteuid() }
    {
        return Err("engine bridge parent must be an owned private 0700 directory".into());
    }
    Ok(())
}

impl Bridge {
    /// The private parent must already exist. Never replace an existing path.
    pub fn bind(path: &Path) -> Result<Self> {
        private_parent(path)?;
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err("engine bridge path already exists or is inaccessible".into()),
        }
        let listener = UnixListener::bind(path).map_err(|_| "cannot bind private engine bridge")?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "cannot protect engine bridge")?;
        let meta = fs::symlink_metadata(path).map_err(|_| "cannot inspect engine bridge")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "cannot configure engine bridge")?;
        let (tx, rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(None));
        let (worker_stop, worker_connected, worker_closed, worker_failed, worker_active) = (
            stop.clone(),
            connected.clone(),
            closed.clone(),
            failed.clone(),
            active.clone(),
        );
        let worker = thread::spawn(move || {
            if serve_connection(
                listener,
                tx,
                &worker_stop,
                &worker_connected,
                &worker_active,
            )
            .is_err()
                && !worker_stop.load(Ordering::Relaxed)
            {
                worker_failed.store(true, Ordering::Relaxed);
            }
            if let Ok(mut active) = worker_active.lock()
                && let Some(stream) = active.take()
            {
                let _ = stream.shutdown(Shutdown::Both);
            }
            worker_closed.store(true, Ordering::Release);
        });
        Ok(Self {
            path: path.into(),
            inode: (meta.dev(), meta.ino()),
            requests: Some(rx),
            stop,
            connected,
            closed,
            failed,
            active,
            worker: Some(worker),
        })
    }

    /// Wait-free queue inspection, at most one frame. The business callback can
    /// take its ordinary bounded platform I/O time; no Runtime runs on a worker.
    pub fn poll(
        &mut self,
        session: &mut Session,
        dispatch: &mut impl FnMut(&ToolCall) -> ToolResult,
    ) -> Result<bool> {
        if self.failed.load(Ordering::Relaxed) {
            return Err("engine bridge stream failed or exceeded its limit".into());
        }
        let request = match self
            .requests
            .as_ref()
            .ok_or("engine bridge closed")?
            .try_recv()
        {
            Ok(request) => request,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(false),
        };
        let response = match serde_json::from_slice(&request.bytes) {
            Ok(value) => session.message(value, dispatch),
            Err(_) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
            ),
        };
        request
            .reply
            .try_send(response)
            .map_err(|_| "engine bridge peer closed")?;
        Ok(true)
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.requests.take();
        if let Ok(active) = self.active.lock()
            && let Some(stream) = active.as_ref()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Do not unlink an operator's replacement or some other execution.
        if fs::symlink_metadata(&self.path).is_ok_and(|meta| {
            meta.file_type().is_socket() && (meta.dev(), meta.ino()) == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn read_frame(
    input: &mut impl BufRead,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err("engine bridge deadline exceeded".into());
        }
        let available = match input.fill_buf() {
            Ok(bytes) => bytes,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(_) => return Err("engine bridge read failed".into()),
        };
        if available.is_empty() {
            return Ok(if frame.is_empty() { None } else { Some(frame) });
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |index| index + 1);
        if frame.len().saturating_add(count) > MAX_FRAME_BYTES {
            return Err("engine bridge frame exceeds limit".into());
        }
        frame.extend_from_slice(&available[..count]);
        input.consume(count);
        if end.is_some() {
            return Ok(Some(frame));
        }
    }
}

fn serve_connection(
    listener: UnixListener,
    tx: SyncSender<Request>,
    stop: &AtomicBool,
    connected: &AtomicBool,
    active: &Mutex<Option<UnixStream>>,
) -> Result<()> {
    let deadline = Instant::now() + LIFETIME;
    let mut stream = loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("engine bridge connection deadline exceeded".into());
        }
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(TICK),
            Err(_) => return Err("engine bridge accept failed".into()),
        }
    };
    drop(listener); // One native MCP process belongs to this execution.
    stream
        .set_read_timeout(Some(TICK))
        .map_err(|_| "engine bridge read timeout unavailable")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "engine bridge write timeout unavailable")?;
    *active
        .lock()
        .map_err(|_| "engine bridge cancellation unavailable")? = Some(
        stream
            .try_clone()
            .map_err(|_| "engine bridge cancellation unavailable")?,
    );
    connected.store(true, Ordering::Release);
    let mut input = BufReader::with_capacity(
        8192,
        stream
            .try_clone()
            .map_err(|_| "engine bridge reader unavailable")?,
    );
    while let Some(bytes) = read_frame(&mut input, stop, deadline)? {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let (reply, rx) = mpsc::sync_channel(1);
        tx.send(Request { bytes, reply })
            .map_err(|_| "engine bridge host closed")?;
        let response = loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("engine bridge response deadline exceeded".into());
            }
            match rx.recv_timeout(TICK) {
                Ok(response) => break response,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("engine bridge host response unavailable".into()),
            }
        };
        if let Some(value) = response {
            let mut bytes =
                serde_json::to_vec(&value).map_err(|_| "engine bridge response unavailable")?;
            bytes.push(b'\n');
            if bytes.len() > MAX_FRAME_BYTES {
                return Err("engine bridge response exceeds limit".into());
            }
            stream
                .write_all(&bytes)
                .and_then(|_| stream.flush())
                .map_err(|_| "engine bridge response write failed")?;
        }
    }
    Ok(())
}

struct NonblockingFd {
    fd: RawFd,
    previous: i32,
}
impl NonblockingFd {
    fn new(fd: RawFd) -> Result<Self> {
        let previous = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if previous == -1
            || unsafe { libc::fcntl(fd, libc::F_SETFL, previous | libc::O_NONBLOCK) } == -1
        {
            return Err("cannot configure engine proxy stream".into());
        }
        Ok(Self { fd, previous })
    }
}
impl Drop for NonblockingFd {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(self.fd, libc::F_SETFL, self.previous);
        }
    }
}

#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    sent: usize,
    frame_bytes: usize,
}
impl Buffer {
    fn pending(&self) -> usize {
        self.bytes.len() - self.sent
    }
    fn read(&mut self, fd: RawFd) -> Result<bool> {
        let mut chunk = [0u8; 8192];
        let capacity = chunk.len().min(MAX_FRAME_BYTES - self.pending());
        let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), capacity) };
        if count < 0 {
            let error = io::Error::last_os_error();
            return if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                Ok(true)
            } else {
                Err("engine proxy read failed".into())
            };
        }
        if count == 0 {
            return Ok(false);
        }
        let chunk = &chunk[..count as usize];
        for byte in chunk {
            self.frame_bytes += 1;
            if self.frame_bytes > MAX_FRAME_BYTES {
                return Err("engine proxy frame exceeds limit".into());
            }
            if *byte == b'\n' {
                self.frame_bytes = 0;
            }
        }
        if self.sent > 0 {
            self.bytes.drain(..self.sent);
            self.sent = 0;
        }
        self.bytes.extend_from_slice(chunk);
        Ok(true)
    }
    fn write(&mut self, fd: RawFd) -> Result<()> {
        let bytes = &self.bytes[self.sent..];
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count < 0 {
            let error = io::Error::last_os_error();
            return if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                Ok(())
            } else {
                Err("engine proxy write failed".into())
            };
        }
        self.sent += count as usize;
        if self.sent == self.bytes.len() {
            self.bytes.clear();
            self.sent = 0;
        }
        Ok(())
    }
}

/// A standalone stdio relay. No Runtime, credentials, account or business lock
/// is opened. Both directions have bounded buffering, framing and lifetime.
pub fn run_stdio_proxy(path: &Path) -> Result<()> {
    private_parent(path)?;
    let meta = fs::symlink_metadata(path).map_err(|_| "engine proxy socket unavailable")?;
    if !meta.file_type().is_socket()
        || meta.permissions().mode() & 0o777 != 0o600
        || meta.uid() != unsafe { libc::geteuid() }
    {
        return Err("engine proxy requires an owned private socket".into());
    }
    let stream = UnixStream::connect(path).map_err(|_| "cannot connect engine proxy")?;
    stream
        .set_nonblocking(true)
        .map_err(|_| "cannot configure engine proxy socket")?;
    let socket = stream.as_raw_fd();
    let _input_mode = NonblockingFd::new(0)?;
    let _output_mode = NonblockingFd::new(1)?;
    let deadline = Instant::now() + LIFETIME;
    let mut to_socket = Buffer::default();
    let mut to_stdout = Buffer::default();
    let mut stdin_open = true;
    let mut socket_open = true;
    let mut write_shutdown = false;
    loop {
        if Instant::now() >= deadline {
            return Err("engine proxy deadline exceeded".into());
        }
        if !socket_open && to_stdout.pending() == 0 {
            return Ok(());
        }
        if !stdin_open && to_socket.pending() == 0 && !write_shutdown {
            stream
                .shutdown(Shutdown::Write)
                .map_err(|_| "engine proxy shutdown failed")?;
            write_shutdown = true;
        }
        let mut fds = [
            libc::pollfd {
                fd: 0,
                events: if stdin_open && to_socket.pending() < MAX_FRAME_BYTES {
                    libc::POLLIN
                } else {
                    0
                },
                revents: 0,
            },
            libc::pollfd {
                fd: socket,
                events: (if socket_open && to_stdout.pending() < MAX_FRAME_BYTES {
                    libc::POLLIN
                } else {
                    0
                }) | (if to_socket.pending() > 0 {
                    libc::POLLOUT
                } else {
                    0
                }),
                revents: 0,
            },
            libc::pollfd {
                fd: 1,
                events: if to_stdout.pending() > 0 {
                    libc::POLLOUT
                } else {
                    0
                },
                revents: 0,
            },
        ];
        // Disabled descriptors must be ignored entirely: poll reports HUP even
        // with events=0, which would otherwise spin after stdin reaches EOF.
        for fd in &mut fds {
            if fd.events == 0 {
                fd.fd = -1;
            }
        }
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 25) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err("engine proxy polling failed".into());
        }
        if stdin_open
            && to_socket.pending() < MAX_FRAME_BYTES
            && fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0
        {
            stdin_open = to_socket.read(0)?;
        }
        if socket_open
            && to_stdout.pending() < MAX_FRAME_BYTES
            && fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0
        {
            socket_open = to_stdout.read(socket)?;
        }
        if to_socket.pending() > 0 && fds[1].revents & libc::POLLOUT != 0 {
            to_socket.write(socket)?;
        }
        if to_stdout.pending() > 0 && fds[2].revents & libc::POLLOUT != 0 {
            to_stdout.write(1)?;
        }
        if fds
            .iter()
            .any(|fd| fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0)
        {
            return Err("engine proxy pipe closed".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, sync::atomic::AtomicU64};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Lab(PathBuf);
    impl Lab {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "engine-bridge-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn socket(&self) -> PathBuf {
            self.0.join("mcp.sock")
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn spec() -> runtime_engine::DynamicToolSpec {
        runtime_engine::DynamicToolSpec {
            name: "selected_tool".into(),
            description: "synthetic".into(),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        }
    }
    fn request(id: u64, method: &str, mut params: Value) -> Value {
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}});
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
    }
    fn rpc(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, request: &Value) -> Value {
        let mut bytes = serde_json::to_vec(request).unwrap();
        bytes.push(b'\n');
        stream.write_all(&bytes).unwrap();
        let response = read_frame(
            reader,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap()
        .unwrap();
        serde_json::from_slice(&response).unwrap()
    }

    #[test]
    fn bridge_dispatches_only_selected_valid_calls_on_parent_thread_and_preserves_id_reuse() {
        let lab = Lab::new();
        let socket = lab.socket();
        let mut bridge = Bridge::bind(&socket).unwrap();
        let peer = thread::spawn(move || {
            let mut stream = UnixStream::connect(&socket).unwrap();
            stream.set_read_timeout(Some(TICK)).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            assert_eq!(
                rpc(
                    &mut stream,
                    &mut reader,
                    &request(1, "tools/list", json!({}))
                )["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            let unknown = rpc(
                &mut stream,
                &mut reader,
                &request(
                    2,
                    "tools/call",
                    json!({"name":"not_selected","arguments":{}}),
                ),
            );
            assert_eq!(unknown["error"]["code"], -32602);
            let invalid = rpc(
                &mut stream,
                &mut reader,
                &request(
                    3,
                    "tools/call",
                    json!({"name":"selected_tool","arguments":{"injected":true}}),
                ),
            );
            assert_eq!(invalid["result"]["isError"], true);
            for _ in 0..2 {
                let value = rpc(
                    &mut stream,
                    &mut reader,
                    &request(
                        4,
                        "tools/call",
                        json!({"name":"selected_tool","arguments":{}}),
                    ),
                );
                assert_eq!(value["result"]["structuredContent"]["accepted"], true);
            }
            stream.write_all(b"invalid-json\n").unwrap();
            let error = read_frame(
                &mut reader,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&error).unwrap()["error"]["code"],
                -32700
            );
            stream
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/unknown\"}\n")
                .unwrap();
            assert_eq!(
                rpc(
                    &mut stream,
                    &mut reader,
                    &request(5, "server/discover", json!({}))
                )["id"],
                5
            );
            stream.shutdown(Shutdown::Both).unwrap();
        });
        let mut session = Session::new_with_tools(vec![spec()]).unwrap();
        let caller = thread::current().id();
        let mut calls = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !peer.is_finished() {
            assert!(Instant::now() < deadline);
            bridge
                .poll(&mut session, &mut |call| {
                    assert_eq!(thread::current().id(), caller);
                    calls.push(call.clone());
                    ToolResult::text(json!({"accepted":true}).to_string(), true)
                })
                .unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        peer.join().unwrap();
        assert!(bridge.connected());
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].turn_id, calls[1].turn_id);
        assert_eq!(calls[0].call_id, calls[1].call_id);
    }

    #[test]
    fn bridge_requires_private_parent_never_overwrites_and_preserves_a_replacement() {
        let lab = Lab::new();
        fs::set_permissions(&lab.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Bridge::bind(&lab.socket()).is_err());
        fs::set_permissions(&lab.0, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = lab.socket();
        let bridge = Bridge::bind(&socket).unwrap();
        assert_eq!(
            fs::symlink_metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(Bridge::bind(&socket).is_err());
        fs::remove_file(&socket).unwrap();
        fs::write(&socket, b"operator replacement").unwrap();
        drop(bridge);
        assert_eq!(fs::read(&socket).unwrap(), b"operator replacement");
        assert!(run_stdio_proxy(&socket).is_err());
    }

    #[test]
    fn drop_cancels_a_partial_request_and_removes_only_its_socket() {
        let lab = Lab::new();
        let socket = lab.socket();
        let bridge = Bridge::bind(&socket).unwrap();
        let mut peer = UnixStream::connect(&socket).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        peer.write_all(b"{\"partial\":").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !bridge.connected() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let start = Instant::now();
        drop(bridge);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!socket.exists());
        assert_eq!(peer.read(&mut [0u8; 1]).unwrap(), 0);
    }

    #[test]
    fn oversized_native_frame_closes_without_a_business_dispatch() {
        let lab = Lab::new();
        let socket = lab.socket();
        let mut bridge = Bridge::bind(&socket).unwrap();
        let peer = thread::spawn(move || {
            let mut stream = UnixStream::connect(&socket).unwrap();
            let _ = stream.write_all(&vec![b'x'; MAX_FRAME_BYTES + 1]);
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while !bridge.is_closed() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        peer.join().unwrap();
        assert!(
            bridge
                .poll(
                    &mut Session::new_with_tools(vec![spec()]).unwrap(),
                    &mut |_| panic!("oversized input dispatched")
                )
                .is_err()
        );
    }
}
