// SPDX-License-Identifier: MIT
// Adapted from Agent Runtime QQ experiment; pinned LagrangeGo attribution in NOTICE.
//! One fixed HTTPS endpoint for QQ's rotating QR-login public key.
//! TLS validates both the certificate chain and keyrotate.qq.com. HTTP support
//! is deliberately limited to a 200 response with Content-Length or chunked
//! framing; redirects, compression, and ambiguous framing are rejected.
use crate::{Result, qr};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

const HOST: &str = "keyrotate.qq.com";
const MAX_RESPONSE: usize = 32 * 1024;
const MAX_JSON: usize = 16 * 1024;
const MAX_HEADERS: usize = 8 * 1024;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

struct DeadlineSocket<'a> {
    socket: TcpStream,
    deadline: Instant,
    stop: &'a AtomicBool,
}
impl DeadlineSocket<'_> {
    fn timeout(&self) -> io::Result<Duration> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "QR login canceled",
            ));
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.min(IO_TIMEOUT))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "key rotation deadline exceeded")
            })
    }
}
impl Read for DeadlineSocket<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.timeout()?))?;
        self.socket.read(buffer)
    }
}
impl Write for DeadlineSocket<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.timeout()?))?;
        self.socket.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.set_write_timeout(Some(self.timeout()?))?;
        self.socket.flush()
    }
}

pub fn fetch_server_key() -> Result<(u16, Vec<u8>)> {
    fetch_server_key_until(Instant::now() + TOTAL_TIMEOUT, &AtomicBool::new(false))
}

pub(crate) fn fetch_server_key_until(
    overall: Instant,
    stop: &AtomicBool,
) -> Result<(u16, Vec<u8>)> {
    check(overall, stop)?;
    let deadline = overall.min(Instant::now() + TOTAL_TIMEOUT);
    let path = qr::KEY_ROTATION_URL
        .strip_prefix("https://keyrotate.qq.com")
        .filter(|path| path.starts_with('/') && !path.contains(['\r', '\n']))
        .ok_or("invalid fixed key rotation URL")?;
    let mut socket = None;
    // Only public DNS data enter this helper. A system resolver cannot be
    // interrupted, so bound the caller's wait instead of blocking QR cancellation.
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("qq-key-dns".into())
        .spawn(move || {
            let addresses = (HOST, 443)
                .to_socket_addrs()
                .map(|a| a.take(8).collect::<Vec<_>>());
            let _ = tx.send(addresses);
        })
        .map_err(|_| "cannot start key rotation resolver")?;
    let addresses = loop {
        check(deadline, stop)?;
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        match rx.recv_timeout(wait) {
            Ok(Ok(addresses)) => break addresses,
            Ok(Err(_)) => return Err("key rotation DNS failed".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("key rotation resolver stopped".into());
            }
        }
    };
    for address in addresses {
        check(deadline, stop)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or("key rotation connection deadline exceeded")?;
        if let Ok(stream) =
            TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(4)))
        {
            socket = Some(stream);
            break;
        }
    }
    let socket = socket.ok_or("cannot connect to keyrotate.qq.com")?;
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("key rotation TLS configuration: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let server_name =
        ServerName::try_from(HOST).map_err(|_| "invalid key rotation TLS hostname")?;
    let connection = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| format!("key rotation TLS setup: {e}"))?;
    let mut tls = StreamOwned::new(
        connection,
        DeadlineSocket {
            socket,
            deadline,
            stop,
        },
    );
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {HOST}\r\nAccept: application/json\r\nAccept-Encoding: identity\r\nConnection: close\r\nUser-Agent: agent-runtime/0.1\r\n\r\n"
    );
    tls.write_all(request.as_bytes())
        .and_then(|_| tls.flush())
        .map_err(|e| format!("key rotation HTTPS request: {e}"))?;
    let mut response = Vec::with_capacity(2048);
    let mut buffer = [0; 2048];
    loop {
        if let Some(json) = decode_http(&response)? {
            return qr::parse_server_key_json(&json);
        }
        let room = MAX_RESPONSE
            .checked_sub(response.len())
            .filter(|n| *n != 0)
            .ok_or("key rotation HTTP response exceeds 32 KiB")?;
        check(deadline, stop)?;
        let n = tls
            .read(&mut buffer[..room.min(2048)])
            .map_err(|e| format!("key rotation HTTPS response: {e}"))?;
        if n == 0 {
            return Err("truncated key rotation HTTPS response".into());
        }
        response.extend_from_slice(&buffer[..n]);
    }
}

fn check(deadline: Instant, stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Relaxed) {
        return Err("QR login canceled".into());
    }
    if Instant::now() >= deadline {
        return Err("key rotation deadline exceeded".into());
    }
    Ok(())
}

fn find(bytes: &[u8], delimiter: &[u8]) -> Option<usize> {
    bytes
        .windows(delimiter.len())
        .position(|window| window == delimiter)
}
fn field(line: &str) -> Result<(&str, &str)> {
    let (name, value) = line
        .split_once(':')
        .ok_or("invalid key rotation HTTP header")?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return Err("invalid key rotation HTTP header name".into());
    }
    if !value
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
    {
        return Err("invalid key rotation HTTP header value".into());
    }
    Ok((name, value.trim_matches([' ', '\t'])))
}

fn decode_http(response: &[u8]) -> Result<Option<Vec<u8>>> {
    if response.len() > MAX_RESPONSE {
        return Err("key rotation HTTP response exceeds 32 KiB".into());
    }
    let Some(end) = find(response, b"\r\n\r\n") else {
        if response.len() > MAX_HEADERS {
            return Err("key rotation HTTP headers exceed limit".into());
        }
        return Ok(None);
    };
    let start = end + 4;
    if start > MAX_HEADERS {
        return Err("key rotation HTTP headers exceed limit".into());
    }
    let headers =
        std::str::from_utf8(&response[..end]).map_err(|_| "invalid key rotation HTTP headers")?;
    let mut lines = headers.split("\r\n");
    let status = lines.next().ok_or("missing key rotation HTTP status")?;
    if !status.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err("invalid key rotation HTTP status".into());
    }
    let mut parts = status.splitn(3, ' ');
    if !matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) || parts.next() != Some("200") {
        return Err("key rotation endpoint did not return HTTP 200".into());
    }
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = field(line)?;
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("ambiguous key rotation Content-Length".into());
            }
            let n = value
                .parse::<usize>()
                .map_err(|_| "invalid key rotation Content-Length")?;
            if n > MAX_JSON {
                return Err("key rotation JSON exceeds 16 KiB".into());
            }
            length = Some(n);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err("unsupported key rotation Transfer-Encoding".into());
            }
            chunked = true;
        } else if name.eq_ignore_ascii_case("content-encoding")
            && !value.eq_ignore_ascii_case("identity")
        {
            return Err("unsupported key rotation Content-Encoding".into());
        }
    }
    if length.is_some() && chunked {
        return Err("ambiguous key rotation HTTP framing".into());
    }
    if let Some(length) = length {
        let body = &response[start..];
        if body.len() < length {
            return Ok(None);
        }
        if body.len() > length {
            return Err("unexpected bytes after key rotation HTTP body".into());
        }
        return Ok(Some(body.to_vec()));
    }
    if !chunked {
        return Err("key rotation HTTP response lacks supported framing".into());
    }
    decode_chunked(response, start)
}

fn decode_chunked(response: &[u8], mut at: usize) -> Result<Option<Vec<u8>>> {
    let mut body = Vec::new();
    loop {
        let Some(line_length) = find(&response[at..], b"\r\n") else {
            if response.len() - at > 128 {
                return Err("key rotation chunk size line exceeds limit".into());
            }
            return Ok(None);
        };
        if line_length > 128 {
            return Err("key rotation chunk size line exceeds limit".into());
        }
        let line = std::str::from_utf8(&response[at..at + line_length])
            .map_err(|_| "invalid key rotation chunk size")?;
        let (size, extension) = line
            .split_once(';')
            .map_or((line, None), |(size, extension)| (size, Some(extension)));
        if size.is_empty()
            || !size.bytes().all(|b| b.is_ascii_hexdigit())
            || extension.is_some_and(|e| !e.bytes().all(|b| (0x20..=0x7e).contains(&b)))
        {
            return Err("invalid key rotation chunk size".into());
        }
        let size =
            usize::from_str_radix(size, 16).map_err(|_| "key rotation chunk size overflow")?;
        at += line_length + 2;
        if size == 0 {
            let trailers_start = at;
            loop {
                let Some(length) = find(&response[at..], b"\r\n") else {
                    if response.len() - trailers_start > MAX_HEADERS {
                        return Err("key rotation HTTP trailers exceed limit".into());
                    }
                    return Ok(None);
                };
                if at + length + 2 - trailers_start > MAX_HEADERS {
                    return Err("key rotation HTTP trailers exceed limit".into());
                }
                if length == 0 {
                    if at + 2 != response.len() {
                        return Err("unexpected bytes after key rotation HTTP chunks".into());
                    }
                    return Ok(Some(body));
                }
                let line = std::str::from_utf8(&response[at..at + length])
                    .map_err(|_| "invalid key rotation HTTP trailer")?;
                let (name, _) = field(line)?;
                if [
                    "content-length",
                    "transfer-encoding",
                    "content-encoding",
                    "host",
                ]
                .iter()
                .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
                {
                    return Err("forbidden key rotation HTTP trailer".into());
                }
                at += length + 2;
            }
        }
        if size > MAX_JSON - body.len() {
            return Err("key rotation JSON exceeds 16 KiB".into());
        }
        let end = at
            .checked_add(size)
            .ok_or("key rotation chunk size overflow")?;
        let next = end
            .checked_add(2)
            .ok_or("key rotation chunk size overflow")?;
        if next > response.len() {
            return Ok(None);
        }
        if &response[end..next] != b"\r\n" {
            return Err("invalid key rotation chunk terminator".into());
        }
        body.extend_from_slice(&response[at..end]);
        at = next;
    }
}

/// Parse an entire bounded HTTP response without performing a network request.
/// Useful for fixtures and a local fake server; the result is the JSON body.
pub fn parse_http(response: &[u8]) -> Result<Vec<u8>> {
    decode_http(response)?.ok_or_else(|| "truncated key rotation HTTP response".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canceled_or_expired_key_fetch_stops_before_dns_or_tls() {
        assert!(
            fetch_server_key_until(
                Instant::now() + Duration::from_secs(20),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(fetch_server_key_until(Instant::now(), &AtomicBool::new(false)).is_err());
    }

    fn cl(body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[test]
    fn bounded_content_length_and_incremental_frames() {
        let body = br#"{"PubKeyMeta":{"KeyVer":1}}"#;
        let response = cl(body);
        assert_eq!(parse_http(&response).unwrap(), body);
        for n in 0..response.len() {
            assert!(decode_http(&response[..n]).unwrap().is_none(), "prefix {n}");
        }
        assert_eq!(
            parse_http(&cl(&vec![b'x'; MAX_JSON])).unwrap().len(),
            MAX_JSON
        );
        assert!(parse_http(&cl(&vec![b'x'; MAX_JSON + 1])).is_err());
        let mut extra = response;
        extra.push(b'x');
        assert!(parse_http(&extra).is_err());
    }

    #[test]
    fn bounded_chunked_with_extensions_and_trailers() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n4;source=fixture\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\nX-Fixture: safe\r\n\r\n";
        assert_eq!(parse_http(response).unwrap(), br#"{"a":1}"#);
        for n in 0..response.len() {
            assert!(decode_http(&response[..n]).unwrap().is_none(), "prefix {n}");
        }
        let empty = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
        assert!(parse_http(empty).unwrap().is_empty());
        let mut maximum = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4000\r\n".to_vec();
        maximum.extend_from_slice(&vec![b'x'; MAX_JSON]);
        maximum.extend_from_slice(b"\r\n0\r\n\r\n");
        assert_eq!(parse_http(&maximum).unwrap().len(), MAX_JSON);
        let oversized = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4001\r\n";
        assert!(parse_http(oversized).is_err());
    }

    #[test]
    fn ambiguous_unbounded_and_malformed_responses_are_rejected() {
        let cases: &[&[u8]] = &[
            b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n\r\n{}",
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\n{}",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2,2\r\n\r\n{}",
            b"HTTP/1.1 200 OK\r\nContent-Length: 184467440737095516160\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n Content-Length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\n{}",
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\x00\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nx\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx!!",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nContent-Length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nx",
        ];
        for (i, response) in cases.iter().enumerate() {
            assert!(parse_http(response).is_err(), "malformed case {i}");
        }
        assert!(parse_http(&vec![b'x'; MAX_RESPONSE + 1]).is_err());
        let mut headers = b"HTTP/1.1 200 OK\r\nX-Long: ".to_vec();
        headers.extend(vec![b'x'; MAX_HEADERS]);
        assert!(parse_http(&headers).is_err());
    }
}
