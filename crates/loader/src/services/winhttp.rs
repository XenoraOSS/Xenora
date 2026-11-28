// SPDX-License-Identifier: GPL-3.0-or-later

//! A bounded synchronous subset of the publicly documented WinHTTP API.
//!
//! The object layer models sessions, connections and requests. The transport
//! layer performs one cleartext HTTP/1.1 request/response exchange per request
//! with `Connection: close`. TLS, asynchronous callbacks,
//! cancellation, redirection, authentication, proxying, chunked upload and
//! connection reuse remain separate unsupported slices.

use super::{ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ServicesState};
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::GuestMemory;
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

mod request_headers;
mod url;

/// The caller-owned opaque value Windows stores on every WinHTTP handle
/// (Microsoft Learn, WINHTTP_OPTION_CONTEXT_VALUE in "Option Flags").
const WINHTTP_OPTION_CONTEXT_VALUE: u32 = 78;

const MODULE: &str = "winhttp.dll";
const MAX_HANDLES: usize = 256;
const MAX_WIDE_UNITS: usize = 512;
const MAX_ACCEPT_TYPES: usize = 32;
const ERROR_OUTOFMEMORY: u32 = 14;
const ERROR_WINHTTP_INVALID_URL: u32 = 12005;
const ERROR_WINHTTP_INVALID_OPTION: u32 = 12009;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_WINHTTP_TIMEOUT: u32 = 12002;
const ERROR_WINHTTP_NAME_NOT_RESOLVED: u32 = 12007;
const ERROR_WINHTTP_INCORRECT_HANDLE_TYPE: u32 = 12018;
const ERROR_WINHTTP_INCORRECT_HANDLE_STATE: u32 = 12019;
const ERROR_WINHTTP_CANNOT_CONNECT: u32 = 12029;
const ERROR_WINHTTP_CONNECTION_ERROR: u32 = 12030;
const ERROR_WINHTTP_INVALID_SERVER_RESPONSE: u32 = 12152;
const ERROR_WINHTTP_HEADER_SIZE_OVERFLOW: u32 = 12182;
const WINHTTP_QUERY_CONTENT_TYPE: u32 = 1;
const WINHTTP_QUERY_CONTENT_LENGTH: u32 = 5;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_STATUS_TEXT: u32 = 20;
const WINHTTP_QUERY_SERVER: u32 = 37;
const WINHTTP_QUERY_DATE: u32 = 9;
const WINHTTP_QUERY_EXPIRES: u32 = 10;
const WINHTTP_QUERY_LAST_MODIFIED: u32 = 11;
const WINHTTP_QUERY_RAW_HEADERS: u32 = 21;
const WINHTTP_QUERY_RAW_HEADERS_CRLF: u32 = 22;
const WINHTTP_QUERY_CUSTOM: u32 = 65535;
const WINHTTP_QUERY_FLAG_SYSTEMTIME: u32 = 0x4000_0000;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
const WINHTTP_OPTION_RESOLVE_TIMEOUT: u32 = 2;
const WINHTTP_OPTION_CONNECT_TIMEOUT: u32 = 3;
const WINHTTP_OPTION_SEND_TIMEOUT: u32 = 5;
const WINHTTP_OPTION_RECEIVE_TIMEOUT: u32 = 6;
const ERROR_WINHTTP_HEADER_NOT_FOUND: u32 = 12150;
const ERROR_WINHTTP_REDIRECT_FAILED: u32 = 12156;
const MAX_ADDITIONAL_HEADER_UNITS: usize = 4096;
const MAX_HEADER_LINE_BYTES: usize = 8192;
const MAX_RESPONSE_HEAD_BYTES: usize = 64 * 1024;
const MAX_READ_CHUNK_BYTES: usize = 64 * 1024;
const MAX_OPTIONAL_BODY_BYTES: u32 = 16 * 1024 * 1024;

const WINHTTP_ACCESS_TYPE_DEFAULT_PROXY: u32 = 0;
const WINHTTP_ACCESS_TYPE_NO_PROXY: u32 = 1;
const WINHTTP_FLAG_ASYNC: u32 = 0x1000_0000;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const DOCUMENTED_REQUEST_FLAGS: u32 = 0x0000_01cc | WINHTTP_FLAG_SECURE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimeoutSettings {
    resolve: u32,
    connect: u32,
    send: u32,
    receive: u32,
}

impl Default for TimeoutSettings {
    fn default() -> Self {
        Self {
            resolve: 0,
            connect: 60_000,
            send: 30_000,
            receive: 30_000,
        }
    }
}

impl TimeoutSettings {
    fn option_mut(&mut self, option: u32) -> Option<&mut u32> {
        match option {
            WINHTTP_OPTION_RESOLVE_TIMEOUT => Some(&mut self.resolve),
            WINHTTP_OPTION_CONNECT_TIMEOUT => Some(&mut self.connect),
            WINHTTP_OPTION_SEND_TIMEOUT => Some(&mut self.send),
            WINHTTP_OPTION_RECEIVE_TIMEOUT => Some(&mut self.receive),
            _ => None,
        }
    }
}

fn timeout_duration(timeout: u32) -> Option<Duration> {
    (timeout != 0 && timeout != u32::MAX).then(|| Duration::from_millis(u64::from(timeout)))
}

enum HandleKind {
    Session {
        agent: Option<String>,
        timeouts: Arc<Mutex<TimeoutSettings>>,
    },
    Connection {
        agent: Option<String>,
        server: String,
        port: u32,
        session_timeouts: Arc<Mutex<TimeoutSettings>>,
    },
    Request {
        server: String,
        port: u32,
        verb: String,
        object_name: String,
        version: String,
        secure: bool,
        timeouts: Arc<Mutex<TimeoutSettings>>,
        transport: Arc<Mutex<TransportState>>,
    },
}

struct RequestSnapshot {
    server: String,
    port: u32,
    verb: String,
    object_name: String,
    version: String,
    secure: bool,
    timeouts: TimeoutSettings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportPhase {
    New,
    Busy,
    Writing,
    Sent,
    Ready,
    Failed,
}

#[derive(Clone, Copy)]
enum BodyKind {
    Empty,
    Fixed(u64),
    UntilClose { eof: bool },
    Chunked,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkedPosition {
    SizeLine,
    Data,
    DataCrLf,
    Trailers,
    Eof,
}

struct ChunkedState {
    position: ChunkedPosition,
    chunk_remaining: u64,
    queued: VecDeque<u8>,
    trailer_bytes: usize,
}

impl ChunkedState {
    fn new() -> Self {
        Self {
            position: ChunkedPosition::SizeLine,
            chunk_remaining: 0,
            queued: VecDeque::new(),
            trailer_bytes: 0,
        }
    }
}

struct ResponseHead {
    status: u32,
    status_text: String,
    headers: Vec<(String, String)>,
    /// Exact wire text of the final response head: the status line and each
    /// header line in received order, every line CRLF-terminated, with no
    /// trailing blank line.
    raw: String,
}

struct TransportState {
    phase: TransportPhase,
    request_headers: Vec<(String, String)>,
    verb: String,
    stream: Option<BufReader<TcpStream>>,
    response: Option<ResponseHead>,
    write_remaining: u64,
    chunked_upload: bool,
    body: BodyKind,
    chunked: ChunkedState,
    /// The current redirect target. Initialized from the request snapshot at
    /// send time and rewritten by each followed redirect.
    server: String,
    port: u32,
    secure: bool,
    object_name: String,
    timeouts: TimeoutSettings,
    /// Content-Length declared at send time; zero means none. A POST-to-GET
    /// redirect rewrite clears it together with the replay body.
    total_length: u32,
    /// All body bytes sent so far, retained so a verb-preserving redirect can
    /// replay them exactly. Bounded; overflow fails a verb-preserving redirect.
    replay_body: Vec<u8>,
    replay_overflow: bool,
    redirects: u32,
}

impl TransportState {
    fn new(agent: Option<String>, referrer: Option<String>, accept_types: Vec<String>) -> Self {
        let mut request_headers = Vec::new();
        if let Some(agent) = agent.filter(|agent| !agent.is_empty()) {
            request_headers.push(("User-Agent".to_owned(), agent));
        }
        if !accept_types.is_empty() {
            request_headers.push(("Accept".to_owned(), accept_types.join(", ")));
        }
        if let Some(referrer) = referrer {
            request_headers.push(("Referer".to_owned(), referrer));
        }
        Self {
            phase: TransportPhase::New,
            request_headers,
            verb: String::new(),
            stream: None,
            response: None,
            write_remaining: 0,
            chunked_upload: false,
            body: BodyKind::Empty,
            chunked: ChunkedState::new(),
            server: String::new(),
            port: 0,
            secure: false,
            object_name: String::new(),
            timeouts: TimeoutSettings::default(),
            total_length: 0,
            replay_body: Vec::new(),
            replay_overflow: false,
            redirects: 0,
        }
    }
}

/// The bounded redirect count Xenora follows inside one receive call. The
/// public text does not document a default maximum; ten is a Xenora choice.
const MAX_REDIRECTS: u32 = 10;

/// Resolves a redirect `Location` against the transport target, rewriting the
/// target fields in place. Absolute URLs crack through the same parser the
/// public cracking service uses; a location the parser rejects is treated as
/// relative, matching the public implementation evidence: a `/`-rooted
/// location replaces the path, anything else resolves against the parent
/// directory of the current path.
fn resolve_redirect(transport: &mut TransportState, location: &str) -> Result<(), u32> {
    let wide: Vec<u16> = location.encode_utf16().collect();
    if let Ok(parsed) = url::parser::parse(&wide, false) {
        if parsed.scheme_id != 1 {
            // HTTPS targets need TLS, which the transport does not implement.
            return Err(ERROR_WINHTTP_REDIRECT_FAILED);
        }
        let text = |range: &std::ops::Range<usize>| -> String {
            wide[range.clone()]
                .iter()
                .map(|&unit| char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'))
                .collect()
        };
        let host = text(&parsed.host);
        if host.is_empty() {
            return Err(ERROR_WINHTTP_REDIRECT_FAILED);
        }
        let mut path = text(&parsed.path);
        path.push_str(&text(&parsed.extra));
        if path.is_empty() {
            path = "/".to_owned();
        }
        transport.server = host;
        transport.port = if parsed.port != 0 {
            u32::from(parsed.port)
        } else {
            80
        };
        transport.object_name = path;
        return Ok(());
    }
    let object = if let Some(rest) = location.strip_prefix('/') {
        let _ = rest;
        location.to_owned()
    } else {
        let current = &transport.object_name;
        let parent = current.rfind('/').map_or(0, |index| index + 1);
        format!("{}{}", &current[..parent], location)
    };
    if !object.starts_with('/')
        || !object.is_ascii()
        || object.bytes().any(|byte| byte <= 0x20 || byte == 0x7f)
    {
        return Err(ERROR_WINHTTP_REDIRECT_FAILED);
    }
    transport.object_name = object;
    Ok(())
}

/// The WinHTTP handle table owns all copied strings and request settings.
/// Later guest-memory writes cannot mutate an existing object.
#[derive(Default)]
pub(super) struct WinHttpHandles {
    next: u64,
    handles: BTreeMap<u64, HandleKind>,
    /// WINHTTP_OPTION_CONTEXT_VALUE of each handle: the caller's own opaque
    /// value, zero until set, exactly as Windows stores one per handle.
    contexts: BTreeMap<u64, u64>,
}

impl WinHttpHandles {
    pub(super) fn open_count(&self) -> usize {
        self.handles.len()
    }

    pub(super) fn close_all(&mut self) {
        self.handles.clear();
        self.contexts.clear();
    }

    fn insert(&mut self, kind: HandleKind) -> Result<u64, u32> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(ERROR_OUTOFMEMORY);
        }
        self.next = self
            .next
            .checked_add(1)
            .filter(|next| *next < (1 << 32))
            .ok_or(ERROR_OUTOFMEMORY)?;
        let handle = 0x5748_0000_0000_0000 | self.next;
        self.handles.insert(handle, kind);
        Ok(handle)
    }

    fn configure_timeouts(
        &self,
        handle: u64,
        update: impl FnOnce(&mut TimeoutSettings) -> Result<(), u32>,
    ) -> Result<(), u32> {
        match self.kind(handle) {
            Some(HandleKind::Session { timeouts, .. }) => {
                update(&mut timeouts.lock().expect("WinHTTP timeouts"))
            }
            Some(HandleKind::Request {
                timeouts,
                transport,
                ..
            }) => {
                let request = transport.lock().expect("WinHTTP transport");
                if request.phase != TransportPhase::New {
                    return Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE);
                }
                update(&mut timeouts.lock().expect("WinHTTP timeouts"))
            }
            Some(_) => Err(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE),
            None => Err(ERROR_INVALID_HANDLE),
        }
    }

    fn kind(&self, handle: u64) -> Option<&HandleKind> {
        self.handles.get(&handle)
    }

    fn close(&mut self, handle: u64) -> bool {
        self.contexts.remove(&handle);
        self.handles.remove(&handle).is_some()
    }

    /// The handle's context value, or its absence when the handle itself is
    /// no handle: reading and writing the context of a closed handle fails
    /// with the invalid-handle error, exactly as every other per-handle
    /// state does.
    fn context(&self, handle: u64) -> Option<u64> {
        self.handles
            .contains_key(&handle)
            .then(|| self.contexts.get(&handle).copied().unwrap_or(0))
    }

    fn set_context(&mut self, handle: u64, value: u64) -> Result<(), u32> {
        if !self.handles.contains_key(&handle) {
            return Err(ERROR_INVALID_HANDLE);
        }
        self.contexts.insert(handle, value);
        Ok(())
    }
}

fn read_wide_string(memory: &GuestMemory, address: u64, limit: usize) -> Result<String, u32> {
    if address == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut units = Vec::new();
    for index in 0..limit {
        let unit_address = address
            .checked_add(2 * index as u64)
            .ok_or(ERROR_INVALID_PARAMETER)?;
        let unit = memory
            .read_u16(unit_address)
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        if unit == 0 {
            return Ok(String::from_utf16_lossy(&units));
        }
        units.push(unit);
    }
    Err(ERROR_INVALID_PARAMETER)
}

fn read_optional_wide_string(memory: &GuestMemory, address: u64) -> Result<Option<String>, u32> {
    if address == 0 {
        Ok(None)
    } else {
        read_wide_string(memory, address, MAX_WIDE_UNITS).map(Some)
    }
}

fn valid_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && byte != b' ' && byte != b'\t')
}

fn valid_server_name(text: &str) -> bool {
    !text.is_empty()
        && text.is_ascii()
        && text.bytes().all(|byte| byte > 0x20 && byte != 0x7f)
        && !text
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'?' | b'#'))
}

fn valid_header_name(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn valid_header_value(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte == b'\t' || (0x20..=0x7e).contains(&byte))
}

fn parse_request_headers(
    text: &str,
    require_final_crlf: bool,
) -> Result<Vec<(String, String)>, u32> {
    let mut headers = Vec::new();
    if text.is_empty() {
        return Ok(headers);
    }
    if require_final_crlf && !text.ends_with("\r\n") {
        return Err(ERROR_INVALID_PARAMETER);
    }
    for line in text.split_terminator("\r\n") {
        if line.is_empty() || line.len() > MAX_HEADER_LINE_BYTES {
            return Err(ERROR_INVALID_PARAMETER);
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ERROR_INVALID_PARAMETER);
        };
        let value = value.trim_matches(|byte| byte == ' ' || byte == '\t');
        if !valid_header_name(name) || !valid_header_value(value) {
            return Err(ERROR_INVALID_PARAMETER);
        }
        let lowered = name.to_ascii_lowercase();
        if matches!(lowered.as_str(), "host" | "content-length" | "connection")
            || (lowered == "transfer-encoding" && !value.eq_ignore_ascii_case("chunked"))
        {
            return Err(ERROR_NOT_SUPPORTED);
        }
        headers.push((name.to_owned(), value.to_owned()));
    }
    Ok(headers)
}

fn read_exact_wide_string(memory: &GuestMemory, address: u64, units: usize) -> Result<String, u32> {
    if address == 0 || units > MAX_ADDITIONAL_HEADER_UNITS {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut wide = Vec::with_capacity(units);
    for index in 0..units {
        let unit_address = address
            .checked_add(2 * index as u64)
            .ok_or(ERROR_INVALID_PARAMETER)?;
        let unit = memory
            .read_u16(unit_address)
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        wide.push(unit);
    }
    Ok(String::from_utf16_lossy(&wide))
}

fn read_request_headers(
    memory: &GuestMemory,
    address: u64,
    units: u32,
    require_final_crlf: bool,
) -> Result<Vec<(String, String)>, u32> {
    if address == 0 {
        return if units == 0 {
            Ok(Vec::new())
        } else {
            Err(ERROR_INVALID_PARAMETER)
        };
    }
    let text = if units == u32::MAX {
        read_wide_string(memory, address, MAX_ADDITIONAL_HEADER_UNITS)?
    } else {
        read_exact_wide_string(memory, address, units as usize)?
    };
    if !text.is_ascii() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    parse_request_headers(&text, require_final_crlf)
}

fn request_path(object_name: &str) -> Result<String, u32> {
    if object_name.is_empty() {
        return Ok("/".to_owned());
    }
    if object_name.starts_with('/')
        && object_name.is_ascii()
        && object_name.bytes().all(|byte| byte > 0x20 && byte != 0x7f)
    {
        Ok(object_name.to_owned())
    } else {
        Err(ERROR_WINHTTP_INVALID_URL)
    }
}

fn effective_port(port: u32, secure: bool) -> u16 {
    if port != 0 {
        port as u16
    } else if secure {
        443
    } else {
        80
    }
}

fn build_request_wire(
    snapshot: &RequestSnapshot,
    configured_headers: &[(String, String)],
    additional_headers: &[(String, String)],
    total_length: u32,
) -> Result<Vec<u8>, u32> {
    let path = request_path(&snapshot.object_name)?;
    let port = effective_port(snapshot.port, snapshot.secure);
    let default_port = if snapshot.secure { 443 } else { 80 };
    let host = if port == default_port {
        snapshot.server.clone()
    } else {
        format!("{}:{}", snapshot.server, port)
    };
    let mut request = format!(
        "{} {} {}\r\nHost: {}\r\n",
        snapshot.verb, path, snapshot.version, host
    );
    for (name, value) in configured_headers.iter().chain(additional_headers) {
        if !valid_header_value(value) {
            return Err(ERROR_INVALID_PARAMETER);
        }
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    if total_length != 0 {
        request.push_str("Content-Length: ");
        request.push_str(&total_length.to_string());
        request.push_str("\r\n");
    }
    request.push_str("Connection: close\r\n\r\n");
    Ok(request.into_bytes())
}

fn request_parts(
    handles: &WinHttpHandles,
    handle: u64,
) -> Result<(RequestSnapshot, Arc<Mutex<TransportState>>), u32> {
    match handles.kind(handle) {
        Some(HandleKind::Request {
            server,
            port,
            verb,
            object_name,
            version,
            secure,
            timeouts,
            transport,
            ..
        }) => Ok((
            RequestSnapshot {
                server: server.clone(),
                port: *port,
                verb: verb.clone(),
                object_name: object_name.clone(),
                version: version.clone(),
                secure: *secure,
                timeouts: *timeouts.lock().expect("WinHTTP timeouts"),
            },
            Arc::clone(transport),
        )),
        Some(_) => Err(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE),
        None => Err(ERROR_INVALID_HANDLE),
    }
}

fn request_transport(
    handles: &WinHttpHandles,
    handle: u64,
) -> Result<Arc<Mutex<TransportState>>, u32> {
    match handles.kind(handle) {
        Some(HandleKind::Request { transport, .. }) => Ok(Arc::clone(transport)),
        Some(_) => Err(ERROR_WINHTTP_INCORRECT_HANDLE_TYPE),
        None => Err(ERROR_INVALID_HANDLE),
    }
}

fn resolve_addresses(server: &str, port: u16, timeout: u32) -> Result<Vec<SocketAddr>, u32> {
    let Some(duration) = timeout_duration(timeout) else {
        return (server, port)
            .to_socket_addrs()
            .map(|addresses| addresses.collect())
            .map_err(|_| ERROR_WINHTTP_NAME_NOT_RESOLVED);
    };

    // Rust's blocking resolver cannot be cancelled once entered. A finite
    // WinHTTP resolution timeout therefore gets the documented helper-thread
    // shape: the caller is bounded, while a late resolver thread is detached
    // and cannot mutate the request after timeout.
    let (sender, receiver) = mpsc::channel();
    let host = server.to_owned();
    if thread::Builder::new()
        .name("xenora-winhttp-resolve".to_owned())
        .spawn(move || {
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.collect::<Vec<_>>());
            let _ = sender.send(result);
        })
        .is_err()
    {
        return Err(ERROR_OUTOFMEMORY);
    }
    match receiver.recv_timeout(duration) {
        Ok(Ok(addresses)) => Ok(addresses),
        Ok(Err(_)) => Err(ERROR_WINHTTP_NAME_NOT_RESOLVED),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(ERROR_WINHTTP_TIMEOUT),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(ERROR_WINHTTP_CONNECTION_ERROR),
    }
}

fn connect_tcp(server: &str, port: u16, timeouts: TimeoutSettings) -> Result<TcpStream, u32> {
    let addresses = resolve_addresses(server, port, timeouts.resolve)?;
    let mut saw_timeout = false;
    let mut saw_address = false;
    for address in addresses {
        saw_address = true;
        let result = match timeout_duration(timeouts.connect) {
            Some(duration) => TcpStream::connect_timeout(&address, duration),
            None => TcpStream::connect(address),
        };
        match result {
            Ok(stream) => {
                stream
                    .set_read_timeout(timeout_duration(timeouts.receive))
                    .map_err(io_winhttp_error)?;
                stream
                    .set_write_timeout(timeout_duration(timeouts.send))
                    .map_err(io_winhttp_error)?;
                return Ok(stream);
            }
            Err(error) => {
                saw_timeout |= matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                );
            }
        }
    }
    if !saw_address {
        Err(ERROR_WINHTTP_NAME_NOT_RESOLVED)
    } else if saw_timeout {
        Err(ERROR_WINHTTP_TIMEOUT)
    } else {
        Err(ERROR_WINHTTP_CANNOT_CONNECT)
    }
}

fn io_winhttp_error(error: std::io::Error) -> u32 {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        ERROR_WINHTTP_TIMEOUT
    } else {
        ERROR_WINHTTP_CONNECTION_ERROR
    }
}

fn fail_transport(transport: &Mutex<TransportState>) {
    transport.lock().expect("WinHTTP transport").phase = TransportPhase::Failed;
}

fn read_crlf_line(reader: &mut BufReader<TcpStream>) -> Result<Vec<u8>, u32> {
    let mut line = Vec::new();
    loop {
        if line.len() >= MAX_HEADER_LINE_BYTES {
            return Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW);
        }
        let mut byte = [0];
        reader.read_exact(&mut byte).map_err(io_winhttp_error)?;
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    if line.pop() != Some(b'\r') {
        return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
    }
    Ok(line)
}

fn parse_one_response_head(reader: &mut BufReader<TcpStream>) -> Result<ResponseHead, u32> {
    let status_line = read_crlf_line(reader)?;
    let mut total = status_line.len() + 2;
    let status_text =
        String::from_utf8(status_line).map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
    let mut raw = format!("{status_text}\r\n");
    let mut parts = status_text.splitn(3, ' ');
    let version = parts.next().ok_or(ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
    let status = parts.next().ok_or(ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
    let status_text = parts.next().unwrap_or("").to_owned();
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || status.len() != 3
        || !status.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
    }
    let status = status
        .parse::<u32>()
        .map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
    let mut headers = Vec::new();
    loop {
        let line = read_crlf_line(reader)?;
        total += line.len() + 2;
        if total > MAX_RESPONSE_HEAD_BYTES {
            return Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW);
        }
        if line.is_empty() {
            break;
        }
        let line = String::from_utf8(line).map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
        raw.push_str(&line);
        raw.push_str("\r\n");
        let Some((name, value)) = line.split_once(':') else {
            return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
        };
        let value = value.trim_matches(|byte| byte == ' ' || byte == '\t');
        if !valid_header_name(name) || !valid_header_value(value) {
            return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
        }
        headers.push((name.to_owned(), value.to_owned()));
    }
    Ok(ResponseHead {
        status,
        status_text,
        headers,
        raw,
    })
}

fn parse_response(
    reader: &mut BufReader<TcpStream>,
    verb: &str,
) -> Result<(ResponseHead, BodyKind), u32> {
    let head = loop {
        let head = parse_one_response_head(reader)?;
        if !(100..200).contains(&head.status) || head.status == 101 {
            break head;
        }
    };
    let no_body = verb.eq_ignore_ascii_case("HEAD")
        || (100..200).contains(&head.status)
        || head.status == 204
        || head.status == 304;
    let body = if no_body {
        BodyKind::Empty
    } else if head
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
    {
        let transfer_encodings = head
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        if transfer_encodings.len() != 1 {
            return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
        }
        if !transfer_encodings[0].eq_ignore_ascii_case("chunked") {
            return Err(ERROR_NOT_SUPPORTED);
        }
        BodyKind::Chunked
    } else {
        let mut length = None;
        for (name, value) in &head.headers {
            if name.eq_ignore_ascii_case("content-length") {
                let parsed = value
                    .parse::<u64>()
                    .map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
                if let Some(previous) = length {
                    if previous != parsed {
                        return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
                    }
                } else {
                    length = Some(parsed);
                }
            }
        }
        if let Some(length) = length {
            BodyKind::Fixed(length)
        } else {
            BodyKind::UntilClose { eof: false }
        }
    };
    Ok((head, body))
}

fn fill_chunked_queue(
    reader: &mut BufReader<TcpStream>,
    state: &mut ChunkedState,
    target: usize,
) -> Result<(), u32> {
    while state.queued.len() < target {
        match state.position {
            ChunkedPosition::Eof => break,
            ChunkedPosition::SizeLine => {
                let line = read_crlf_line(reader)?;
                let text =
                    String::from_utf8(line).map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
                let size_text = text.split(';').next().unwrap_or("").trim();
                if size_text.is_empty()
                    || size_text.len() > 16
                    || !size_text.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
                }
                let size = u64::from_str_radix(size_text, 16)
                    .map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
                state.chunk_remaining = size;
                state.position = if size == 0 {
                    ChunkedPosition::Trailers
                } else {
                    ChunkedPosition::Data
                };
            }
            ChunkedPosition::Data => {
                let wanted = (target - state.queued.len())
                    .min(state.chunk_remaining as usize)
                    .min(MAX_READ_CHUNK_BYTES);
                if wanted == 0 {
                    continue;
                }
                let mut bytes = vec![0; wanted];
                let count = reader.read(&mut bytes).map_err(io_winhttp_error)?;
                if count == 0 {
                    return Err(ERROR_WINHTTP_CONNECTION_ERROR);
                }
                bytes.truncate(count);
                state.queued.extend(bytes);
                state.chunk_remaining -= count as u64;
                if state.chunk_remaining == 0 {
                    state.position = ChunkedPosition::DataCrLf;
                }
                break;
            }
            ChunkedPosition::DataCrLf => {
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf).map_err(io_winhttp_error)?;
                if &crlf != b"\r\n" {
                    return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
                }
                state.position = ChunkedPosition::SizeLine;
            }
            ChunkedPosition::Trailers => loop {
                let line = read_crlf_line(reader)?;
                state.trailer_bytes += line.len() + 2;
                if state.trailer_bytes > MAX_RESPONSE_HEAD_BYTES {
                    return Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW);
                }
                if line.is_empty() {
                    state.position = ChunkedPosition::Eof;
                    break;
                }
                let line =
                    String::from_utf8(line).map_err(|_| ERROR_WINHTTP_INVALID_SERVER_RESPONSE)?;
                let Some((name, value)) = line.split_once(':') else {
                    return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
                };
                let value = value.trim_matches(|byte| byte == ' ' || byte == '\t');
                if !valid_header_name(name) || !valid_header_value(value) {
                    return Err(ERROR_WINHTTP_INVALID_SERVER_RESPONSE);
                }
            },
        }
    }
    Ok(())
}

fn close_result(state: &ServicesState, teb: u64, closed: bool) -> u64 {
    if closed {
        1
    } else {
        state.fail(teb, ERROR_INVALID_HANDLE, 0).unwrap()
    }
}

/// A Windows SYSTEMTIME for the HTTP-date utility functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HttpSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_LONG: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn read_http_system_time(memory: &GuestMemory, address: u64) -> Result<HttpSystemTime, u32> {
    if address == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let field = |index: u64| {
        memory
            .read_u16(address + 2 * index)
            .map_err(|_| ERROR_INVALID_PARAMETER)
    };
    Ok(HttpSystemTime {
        year: field(0)?,
        month: field(1)?,
        day_of_week: field(2)?,
        day: field(3)?,
        hour: field(4)?,
        minute: field(5)?,
        second: field(6)?,
        milliseconds: field(7)?,
    })
}

fn http_system_time_bytes(time: HttpSystemTime) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, field) in [
        time.year,
        time.month,
        time.day_of_week,
        time.day,
        time.hour,
        time.minute,
        time.second,
        time.milliseconds,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[2 * index..2 * index + 2].copy_from_slice(&field.to_le_bytes());
    }
    bytes
}

fn write_http_system_time(
    memory: &mut GuestMemory,
    address: u64,
    time: HttpSystemTime,
) -> Result<(), u32> {
    if address == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    memory
        .write_bytes(address, &http_system_time_bytes(time))
        .map_err(|_| ERROR_INVALID_PARAMETER)
}

fn leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn month_length(year: u16, month: u16) -> Option<u16> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

fn valid_http_system_date(time: HttpSystemTime) -> bool {
    (1..=9999).contains(&time.year)
        && month_length(time.year, time.month).is_some_and(|days| (1..=days).contains(&time.day))
        && time.hour < 24
        && time.minute < 60
        && time.second < 60
        && time.milliseconds < 1000
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar. Howard
/// Hinnant's public `days_from_civil` algorithm; `month` counts from 1.
fn http_days_from_civil(year: u16, month: u16, day: u16) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of `http_days_from_civil`, using Howard Hinnant's public
/// `civil_from_days` algorithm. The returned month counts from 1.
fn http_civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The UTC year carried by a Windows FILETIME. The utility parser needs the
/// current year because RFC 850's two-digit-year rule is intentionally
/// rolling rather than a fixed 1950/2050 pivot.
fn http_filetime_year(filetime: u64) -> Option<u16> {
    const TICKS_PER_DAY: u64 = 10_000 * 86_400_000;
    const DAYS_1601_TO_1970: i64 = 134_774;
    let days_since_1601 = i64::try_from(filetime / TICKS_PER_DAY).ok()?;
    let (year, _, _) = http_civil_from_days(days_since_1601 - DAYS_1601_TO_1970);
    u16::try_from(year).ok()
}

/// Select the nearest century for an RFC 850 two-digit year. This is the
/// bounded year-granularity form of the HTTP rule: a date more than fifty
/// years in the future belongs to the previous century. The service is
/// synchronous and deterministic for a fixed input clock.
fn http_rfc850_year(short_year: u16, current_year: u16) -> Option<u16> {
    let century = current_year / 100 * 100;
    let mut year = century.checked_add(short_year)?;
    if year > current_year.saturating_add(50) {
        year = year.checked_sub(100)?;
    } else if year.saturating_add(50) < current_year {
        year = year.checked_add(100)?;
    }
    Some(year)
}

/// Sunday is zero, as in SYSTEMTIME. 1970-01-01 was a Thursday.
fn http_day_of_week(year: u16, month: u16, day: u16) -> u16 {
    (http_days_from_civil(year, month, day) + 4).rem_euclid(7) as u16
}

fn ascii_digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn month_number(text: &str) -> Option<u16> {
    MONTHS_SHORT
        .iter()
        .position(|month| *month == text)
        .map(|index| index as u16 + 1)
}

fn finish_http_date(
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
) -> Option<HttpSystemTime> {
    let time = HttpSystemTime {
        year,
        month,
        day_of_week: 0,
        day,
        hour,
        minute,
        second,
        milliseconds: 0,
    };
    if !valid_http_system_date(time) {
        return None;
    }
    Some(HttpSystemTime {
        day_of_week: http_day_of_week(year, month, day),
        ..time
    })
}

fn parse_time(text: &str) -> Option<(u16, u16, u16)> {
    let bytes = text.as_bytes();
    if bytes.len() != 8 || bytes[2] != b':' || bytes[5] != b':' {
        return None;
    }
    let hour = ascii_digits(&text[0..2])? as u16;
    let minute = ascii_digits(&text[3..5])? as u16;
    let second = ascii_digits(&text[6..8])? as u16;
    if hour < 24 && minute < 60 && second < 60 {
        Some((hour, minute, second))
    } else {
        None
    }
}

/// Parse the exact three HTTP-date forms of RFC 2616 section 3.3. The
/// weekday token must have a documented name, but its redundant value is not
/// trusted: the output day of week is computed from the calendar date.
fn parse_http_date(text: &str, current_year: u16) -> Option<HttpSystemTime> {
    if !text.is_ascii() {
        return None;
    }
    // rfc1123-date: `Sun, 06 Nov 1994 08:49:37 GMT`.
    if text.len() == 29
        && WEEKDAYS_SHORT.contains(&&text[0..3])
        && text.as_bytes()[3] == b','
        && &text[4..5] == " "
        && &text[7..8] == " "
        && &text[11..12] == " "
        && &text[16..17] == " "
        && &text[25..26] == " "
        && &text[26..29] == "GMT"
    {
        let day = ascii_digits(&text[5..7])? as u16;
        let month = month_number(&text[8..11])?;
        let year = ascii_digits(&text[12..16])? as u16;
        let (hour, minute, second) = parse_time(&text[17..25])?;
        return finish_http_date(year, month, day, hour, minute, second);
    }

    // rfc850-date: `Sunday, 06-Nov-94 08:49:37 GMT`. Its two-digit year
    // uses the rolling fifty-year window required by the HTTP specifications,
    // evaluated against the service clock rather than a frozen pivot.
    if let Some((weekday, rest)) = text.split_once(", ")
        && WEEKDAYS_LONG.contains(&weekday)
        && rest.len() == 22
        && rest.as_bytes()[2] == b'-'
        && rest.as_bytes()[6] == b'-'
        && &rest[9..10] == " "
        && &rest[18..19] == " "
        && &rest[19..22] == "GMT"
    {
        let day = ascii_digits(&rest[0..2])? as u16;
        let month = month_number(&rest[3..6])?;
        let short_year = ascii_digits(&rest[7..9])? as u16;
        let year = http_rfc850_year(short_year, current_year)?;
        let (hour, minute, second) = parse_time(&rest[10..18])?;
        return finish_http_date(year, month, day, hour, minute, second);
    }

    // asctime-date: `Sun Nov  6 08:49:37 1994` or a two-digit day.
    if text.len() == 24
        && WEEKDAYS_SHORT.contains(&&text[0..3])
        && text.as_bytes()[3] == b' '
        && text.as_bytes()[7] == b' '
        && text.as_bytes()[10] == b' '
        && text.as_bytes()[19] == b' '
    {
        let month = month_number(&text[4..7])?;
        let day_text = if text.as_bytes()[8] == b' ' {
            &text[9..10]
        } else {
            &text[8..10]
        };
        let day = ascii_digits(day_text)? as u16;
        let (hour, minute, second) = parse_time(&text[11..19])?;
        let year = ascii_digits(&text[20..24])? as u16;
        return finish_http_date(year, month, day, hour, minute, second);
    }
    None
}

fn write_http_date_string(
    memory: &mut GuestMemory,
    address: u64,
    time: HttpSystemTime,
) -> Result<(), u32> {
    if address == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let text = format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WEEKDAYS_SHORT[usize::from(time.day_of_week)],
        time.day,
        MONTHS_SHORT[usize::from(time.month - 1)],
        time.year,
        time.hour,
        time.minute,
        time.second
    );
    for (index, byte) in text.bytes().chain([0]).enumerate() {
        memory
            .write_u16(address + 2 * index as u64, u16::from(byte))
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
    }
    Ok(())
}

/// Register the implemented handle-lifecycle and utility subset. Every object
/// function remains partial because documented proxy, asynchronous and
/// transport-related flag combinations are intentionally refused rather than
/// silently accepted.
pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    url::register(registry, state);

    let open = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpOpen",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let agent = match read_optional_wide_string(memory, args.rcx) {
                Ok(value) => value,
                Err(error) => return open.fail(cpu.gs_base, error, 0),
            };
            let access_type = args.rdx as u32;
            match access_type {
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY | WINHTTP_ACCESS_TYPE_NO_PROXY => {}
                3 | 4 => return open.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0),
                _ => return open.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0),
            }
            // Proxy strings are inputs to WINHTTP_ACCESS_TYPE_NAMED_PROXY.
            // That access type is refused above; direct and default modes do
            // not consume them, so they are deliberately not dereferenced.
            let flags = args.get(4, memory).map_err(memory_fault)? as u32;
            if flags == WINHTTP_FLAG_ASYNC {
                return open.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            if flags != 0 {
                return open.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let mut handles = open.winhttp.lock().expect("WinHTTP handle table");
            match handles.insert(HandleKind::Session {
                agent,
                timeouts: Arc::new(Mutex::new(TimeoutSettings::default())),
            }) {
                Ok(handle) => Ok(handle),
                Err(error) => open.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let connect = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpConnect",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            if args.r9 != 0 {
                return connect.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let server = match read_wide_string(memory, args.rdx, MAX_WIDE_UNITS) {
                Ok(server) if valid_server_name(&server) => server,
                Ok(_) | Err(_) => {
                    return connect.fail(cpu.gs_base, ERROR_WINHTTP_INVALID_URL, 0);
                }
            };
            // INTERNET_PORT is a WORD. As in the Win64 ABI, only the low
            // sixteen bits of the register carry the argument.
            let port = u32::from(args.r8 as u16);
            let mut handles = connect.winhttp.lock().expect("WinHTTP handle table");
            let (agent, session_timeouts) = match handles.kind(args.rcx) {
                Some(HandleKind::Session { agent, timeouts }) => {
                    (agent.clone(), Arc::clone(timeouts))
                }
                Some(_) => {
                    return connect.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_TYPE, 0);
                }
                None => return connect.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
            };
            match handles.insert(HandleKind::Connection {
                agent,
                server,
                port,
                session_timeouts,
            }) {
                Ok(handle) => Ok(handle),
                Err(error) => connect.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let request = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpOpenRequest",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let verb = match read_optional_wide_string(memory, args.rdx) {
                Ok(Some(verb)) if valid_token(&verb) => verb,
                Ok(Some(_)) | Err(_) => {
                    return request.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                }
                Ok(None) => "GET".to_owned(),
            };
            let object_name = match read_optional_wide_string(memory, args.r8) {
                Ok(Some(object)) => object,
                // Microsoft's examples pass NULL successfully but do not
                // specify an expansion. Retain the absent target honestly;
                // a transport slice must define wire behavior separately.
                Ok(None) => String::new(),
                Err(_) => return request.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0),
            };
            let version = match read_optional_wide_string(memory, args.r9) {
                Ok(Some(version)) if valid_token(&version) => version,
                Ok(Some(_)) | Err(_) => {
                    return request.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                }
                Ok(None) => "HTTP/1.1".to_owned(),
            };
            let referrer_ptr = args.get(4, memory).map_err(memory_fault)?;
            let referrer = match read_optional_wide_string(memory, referrer_ptr) {
                Ok(value) => value,
                Err(error) => return request.fail(cpu.gs_base, error, 0),
            };
            let accept_ptr = args.get(5, memory).map_err(memory_fault)?;
            let mut accept_types = Vec::new();
            if accept_ptr != 0 {
                let mut terminated = false;
                for index in 0..MAX_ACCEPT_TYPES {
                    let pointer = memory
                        .read_u64(accept_ptr + 8 * index as u64)
                        .map_err(|_| ERROR_INVALID_PARAMETER);
                    let pointer = match pointer {
                        Ok(pointer) => pointer,
                        Err(error) => return request.fail(cpu.gs_base, error, 0),
                    };
                    if pointer == 0 {
                        terminated = true;
                        break;
                    }
                    let Ok(text) = read_wide_string(memory, pointer, MAX_WIDE_UNITS) else {
                        return request.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                    };
                    accept_types.push(text);
                }
                if !terminated {
                    return request.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                }
            }
            let flags = args.get(6, memory).map_err(memory_fault)? as u32;
            if flags & !DOCUMENTED_REQUEST_FLAGS != 0 {
                return request.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            if flags & !WINHTTP_FLAG_SECURE != 0 {
                return request.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            let mut handles = request.winhttp.lock().expect("WinHTTP handle table");
            let (agent, server, port, session_timeouts) = match handles.kind(args.rcx) {
                Some(HandleKind::Connection {
                    agent,
                    server,
                    port,
                    session_timeouts,
                }) => (
                    agent.clone(),
                    server.clone(),
                    *port,
                    Arc::clone(session_timeouts),
                ),
                Some(_) => {
                    return request.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_TYPE, 0);
                }
                None => return request.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
            };
            match handles.insert(HandleKind::Request {
                server,
                port,
                verb,
                object_name,
                version,
                secure: flags & WINHTTP_FLAG_SECURE != 0,
                timeouts: Arc::new(Mutex::new(
                    *session_timeouts.lock().expect("WinHTTP timeouts"),
                )),
                transport: Arc::new(Mutex::new(TransportState::new(
                    agent,
                    referrer,
                    accept_types,
                ))),
            }) {
                Ok(handle) => Ok(handle),
                Err(error) => request.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let set_timeouts = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpSetTimeouts",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let values = [
                args.rdx as i32,
                args.r8 as i32,
                args.r9 as i32,
                args.get(4, memory).map_err(memory_fault)? as i32,
            ];
            if values.iter().any(|&timeout| timeout < -1) {
                return set_timeouts.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let [resolve, connect, send, receive] = values.map(|timeout| timeout as u32);
            let handles = set_timeouts.winhttp.lock().expect("WinHTTP handle table");
            match handles.configure_timeouts(args.rcx, |timeouts| {
                *timeouts = TimeoutSettings {
                    resolve,
                    connect,
                    send,
                    receive,
                };
                Ok(())
            }) {
                Ok(()) => Ok(1),
                Err(error) => set_timeouts.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let set_option = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpSetOption",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            // WINHTTP_OPTION_CONTEXT_VALUE carries a DWORD_PTR, eight bytes
            // here; the timeouts are four. The size is part of the option's
            // own contract (Microsoft Learn, "Option Flags").
            if args.rdx as u32 == WINHTTP_OPTION_CONTEXT_VALUE {
                if args.r8 == 0 || args.r9 as u32 != 8 {
                    return set_option.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                }
                let value = match memory.read_u64(args.r8) {
                    Ok(value) => value,
                    Err(_) => return set_option.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0),
                };
                let mut handles = set_option.winhttp.lock().expect("WinHTTP handle table");
                return match handles.set_context(args.rcx, value) {
                    Ok(()) => Ok(1),
                    Err(error) => set_option.fail(cpu.gs_base, error, 0),
                };
            }
            if args.r8 == 0 || args.r9 as u32 != 4 {
                return set_option.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let value = match memory.read_u32(args.r8) {
                Ok(value) => value,
                Err(_) => return set_option.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0),
            };
            let handles = set_option.winhttp.lock().expect("WinHTTP handle table");
            match handles.configure_timeouts(args.rcx, |timeouts| {
                let timeout = timeouts
                    .option_mut(args.rdx as u32)
                    .ok_or(ERROR_WINHTTP_INVALID_OPTION)?;
                *timeout = value;
                Ok(())
            }) {
                Ok(()) => Ok(1),
                Err(error) => set_option.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let add_headers = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpAddRequestHeaders",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let headers = match read_request_headers(memory, args.rdx, args.r8 as u32, false) {
                Ok(headers) if !headers.is_empty() => headers,
                Ok(_) => return add_headers.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0),
                Err(error) => return add_headers.fail(cpu.gs_base, error, 0),
            };
            let handles = add_headers.winhttp.lock().expect("WinHTTP handle table");
            let transport = match request_transport(&handles, args.rcx) {
                Ok(transport) => transport,
                Err(error) => return add_headers.fail(cpu.gs_base, error, 0),
            };
            let mut request = transport.lock().expect("WinHTTP transport");
            if request.phase != TransportPhase::New {
                return add_headers.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
            }
            match request_headers::update(&mut request.request_headers, headers, args.r9 as u32) {
                Ok(()) => Ok(1),
                Err(error) => add_headers.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let send = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpSendRequest",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            {
                let handles = send.winhttp.lock().expect("WinHTTP handle table");
                match handles.kind(args.rcx) {
                    Some(HandleKind::Request { secure: false, .. }) => {}
                    Some(HandleKind::Request { secure: true, .. }) => {
                        return send.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
                    }
                    Some(_) => {
                        return send.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_TYPE, 0);
                    }
                    None => return send.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
                }
            }
            let additional = match read_request_headers(memory, args.rdx, args.r8 as u32, true) {
                Ok(headers) => headers,
                Err(error) => return send.fail(cpu.gs_base, error, 0),
            };
            let optional_length = args.get(4, memory).map_err(memory_fault)? as u32;
            let total_length = args.get(5, memory).map_err(memory_fault)? as u32;
            let _context = args.get(6, memory).map_err(memory_fault)?;
            if total_length < optional_length {
                return send.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            if optional_length > MAX_OPTIONAL_BODY_BYTES {
                return send.fail(cpu.gs_base, ERROR_OUTOFMEMORY, 0);
            }
            let mut body = vec![0; optional_length as usize];
            if optional_length != 0 {
                if args.r9 == 0 {
                    return send.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                }
                if memory.read_bytes(args.r9, &mut body).is_err() {
                    return send.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                }
            }
            let (snapshot, transport, mut wire) = {
                // Hold the table across the timeout snapshot and phase change.
                // Configuration changes either run first or see a started request.
                let handles = send.winhttp.lock().expect("WinHTTP handle table");
                let (snapshot, transport) = match request_parts(&handles, args.rcx) {
                    Ok(parts) => parts,
                    Err(error) => return send.fail(cpu.gs_base, error, 0),
                };
                let wire = {
                    let mut request = transport.lock().expect("WinHTTP transport");
                    if request.phase != TransportPhase::New {
                        return send.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
                    }
                    let chunked_upload = additional
                        .iter()
                        .chain(request.request_headers.iter())
                        .any(|(name, value)| {
                            name.eq_ignore_ascii_case("transfer-encoding")
                                && value.eq_ignore_ascii_case("chunked")
                        });
                    // The public contract requires absent optional data when
                    // the Transfer-Encoding header is present, and the
                    // Content-Length that dwTotalLength specifies cannot
                    // combine with it.
                    if chunked_upload && (optional_length != 0 || total_length != 0) {
                        return send.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                    }
                    let wire = match build_request_wire(
                        &snapshot,
                        &request.request_headers,
                        &additional,
                        total_length,
                    ) {
                        Ok(wire) => wire,
                        Err(error) => return send.fail(cpu.gs_base, error, 0),
                    };
                    request.phase = TransportPhase::Busy;
                    request.chunked_upload = chunked_upload;
                    request.write_remaining = u64::from(total_length - optional_length);
                    request.server = snapshot.server.clone();
                    request.port = u32::from(effective_port(snapshot.port, snapshot.secure));
                    request.secure = snapshot.secure;
                    request.object_name = snapshot.object_name.clone();
                    request.verb = snapshot.verb.clone();
                    request.timeouts = snapshot.timeouts;
                    request.total_length = total_length;
                    request.request_headers.extend(additional.iter().cloned());
                    request.replay_body = body.clone();
                    wire
                };
                (snapshot, transport, wire)
            };
            wire.extend_from_slice(&body);
            let port = effective_port(snapshot.port, snapshot.secure);
            let result =
                connect_tcp(&snapshot.server, port, snapshot.timeouts).and_then(|mut stream| {
                    stream.write_all(&wire).map_err(io_winhttp_error)?;
                    Ok(stream)
                });
            match result {
                Ok(stream) => {
                    let mut transport_guard = transport.lock().expect("WinHTTP transport");
                    transport_guard.phase = if transport_guard.write_remaining == 0
                        && !transport_guard.chunked_upload
                    {
                        TransportPhase::Sent
                    } else {
                        TransportPhase::Writing
                    };
                    transport_guard.verb = snapshot.verb;
                    transport_guard.stream = Some(BufReader::new(stream));
                    Ok(1)
                }
                Err(error) => {
                    fail_transport(&transport);
                    send.fail(cpu.gs_base, error, 0)
                }
            }
        },
    );

    let write_data = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpWriteData",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            if args.r9 == 0 {
                return write_data.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            memory.write_u32(args.r9, 0).map_err(memory_fault)?;
            let wanted = args.r8 as u32;
            if wanted > MAX_OPTIONAL_BODY_BYTES {
                return write_data.fail(cpu.gs_base, ERROR_OUTOFMEMORY, 0);
            }
            if wanted != 0 && args.rdx == 0 {
                return write_data.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let mut bytes = vec![0; wanted as usize];
            if wanted != 0 && memory.read_bytes(args.rdx, &mut bytes).is_err() {
                return write_data.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let transport = {
                let handles = write_data.winhttp.lock().expect("WinHTTP handle table");
                match request_transport(&handles, args.rcx) {
                    Ok(transport) => transport,
                    Err(error) => return write_data.fail(cpu.gs_base, error, 0),
                }
            };
            let mut transport_guard = transport.lock().expect("WinHTTP transport");
            if transport_guard.phase != TransportPhase::Writing {
                return write_data.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
            }
            if !transport_guard.chunked_upload
                && u64::from(wanted) > transport_guard.write_remaining
            {
                return write_data.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let result = match transport_guard.stream.as_mut() {
                Some(reader) => reader.get_mut().write_all(&bytes).map_err(io_winhttp_error),
                None => Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE),
            };
            match result {
                Ok(()) => {
                    if !transport_guard.chunked_upload {
                        transport_guard.write_remaining -= u64::from(wanted);
                        if transport_guard.write_remaining == 0 {
                            transport_guard.phase = TransportPhase::Sent;
                        }
                    }
                    if transport_guard.replay_body.len() + bytes.len()
                        <= MAX_OPTIONAL_BODY_BYTES as usize
                    {
                        transport_guard.replay_body.extend_from_slice(&bytes);
                    } else {
                        transport_guard.replay_overflow = true;
                    }
                    memory.write_u32(args.r9, wanted).map_err(memory_fault)?;
                    Ok(1)
                }
                Err(error) => {
                    transport_guard.phase = TransportPhase::Failed;
                    write_data.fail(cpu.gs_base, error, 0)
                }
            }
        },
    );

    let receive = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpReceiveResponse",
        ServiceStatus::Partial,
        move |args, cpu, _| {
            if args.rdx != 0 {
                return receive.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let transport = {
                let handles = receive.winhttp.lock().expect("WinHTTP handle table");
                match request_transport(&handles, args.rcx) {
                    Ok(transport) => transport,
                    Err(error) => return receive.fail(cpu.gs_base, error, 0),
                }
            };
            let mut transport_guard = transport.lock().expect("WinHTTP transport");
            let acceptable = transport_guard.phase == TransportPhase::Sent
                || (transport_guard.phase == TransportPhase::Writing
                    && transport_guard.chunked_upload);
            if !acceptable {
                return receive.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
            }
            loop {
                let verb = transport_guard.verb.clone();
                let result = match transport_guard.stream.as_mut() {
                    Some(reader) => parse_response(reader, &verb),
                    None => Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE),
                };
                let (head, body) = match result {
                    Ok(parts) => parts,
                    Err(error) => {
                        transport_guard.phase = TransportPhase::Failed;
                        return receive.fail(cpu.gs_base, error, 0);
                    }
                };
                if !matches!(head.status, 301 | 302 | 307 | 308) {
                    transport_guard.response = Some(head);
                    transport_guard.body = body;
                    transport_guard.phase = TransportPhase::Ready;
                    return Ok(1);
                }
                // Redirect: the connection closes anyway, so the old stream is
                // simply dropped and the target rewritten before resending.
                transport_guard.redirects += 1;
                if transport_guard.redirects > MAX_REDIRECTS {
                    transport_guard.phase = TransportPhase::Failed;
                    return receive.fail(cpu.gs_base, ERROR_WINHTTP_REDIRECT_FAILED, 0);
                }
                let Some((_, location)) = head
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                else {
                    transport_guard.phase = TransportPhase::Failed;
                    return receive.fail(cpu.gs_base, ERROR_WINHTTP_HEADER_NOT_FOUND, 0);
                };
                let location = location.clone();
                transport_guard.stream = None;
                if let Err(error) = resolve_redirect(&mut transport_guard, &location) {
                    transport_guard.phase = TransportPhase::Failed;
                    return receive.fail(cpu.gs_base, error, 0);
                }
                // POST rewrites to GET and drops its body for the historical
                // statuses only; 307 and 308 preserve the verb and body.
                if !matches!(head.status, 307 | 308)
                    && transport_guard.verb.eq_ignore_ascii_case("POST")
                {
                    transport_guard.verb = "GET".to_owned();
                    transport_guard.replay_body.clear();
                    transport_guard.replay_overflow = false;
                    transport_guard.total_length = 0;
                }
                if transport_guard.replay_overflow {
                    transport_guard.phase = TransportPhase::Failed;
                    return receive.fail(cpu.gs_base, ERROR_WINHTTP_REDIRECT_FAILED, 0);
                }
                let snapshot = RequestSnapshot {
                    server: transport_guard.server.clone(),
                    port: transport_guard.port,
                    verb: transport_guard.verb.clone(),
                    object_name: transport_guard.object_name.clone(),
                    version: "HTTP/1.1".to_owned(),
                    secure: transport_guard.secure,
                    timeouts: transport_guard.timeouts,
                };
                let wire = match build_request_wire(
                    &snapshot,
                    &[],
                    &transport_guard.request_headers,
                    transport_guard.total_length,
                ) {
                    Ok(wire) => wire,
                    Err(error) => {
                        transport_guard.phase = TransportPhase::Failed;
                        return receive.fail(cpu.gs_base, error, 0);
                    }
                };
                let port = effective_port(snapshot.port, snapshot.secure);
                let result = connect_tcp(&snapshot.server, port, snapshot.timeouts).and_then(
                    |mut stream| {
                        stream.write_all(&wire).map_err(io_winhttp_error)?;
                        stream
                            .write_all(&transport_guard.replay_body)
                            .map_err(io_winhttp_error)?;
                        Ok(stream)
                    },
                );
                match result {
                    Ok(stream) => {
                        transport_guard.stream = Some(BufReader::new(stream));
                    }
                    Err(error) => {
                        transport_guard.phase = TransportPhase::Failed;
                        return receive.fail(cpu.gs_base, error, 0);
                    }
                }
            }
        },
    );

    let query_available = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpQueryDataAvailable",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            if args.rdx == 0 {
                return query_available.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let transport = {
                let handles = query_available
                    .winhttp
                    .lock()
                    .expect("WinHTTP handle table");
                match request_transport(&handles, args.rcx) {
                    Ok(transport) => transport,
                    Err(error) => return query_available.fail(cpu.gs_base, error, 0),
                }
            };
            let mut transport_guard = transport.lock().expect("WinHTTP transport");
            if transport_guard.phase != TransportPhase::Ready {
                return query_available.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
            }
            let body = transport_guard.body;
            let mut mark_eof = false;
            let TransportState {
                stream, chunked, ..
            } = &mut *transport_guard;
            let result = match stream.as_mut() {
                Some(reader) => match body {
                    BodyKind::Empty | BodyKind::Fixed(0) => Ok(0_usize),
                    BodyKind::Fixed(remaining) => match reader.fill_buf() {
                        Ok([]) => Err(ERROR_WINHTTP_CONNECTION_ERROR),
                        Ok(buffer) => Ok(buffer.len().min(remaining as usize)),
                        Err(error) => Err(io_winhttp_error(error)),
                    },
                    BodyKind::UntilClose { eof: true } => Ok(0_usize),
                    BodyKind::UntilClose { eof: false } => match reader.fill_buf() {
                        Ok(buffer) => {
                            if buffer.is_empty() {
                                mark_eof = true;
                            }
                            Ok(buffer.len())
                        }
                        Err(error) => Err(io_winhttp_error(error)),
                    },
                    BodyKind::Chunked => fill_chunked_queue(reader, chunked, MAX_READ_CHUNK_BYTES)
                        .map(|()| chunked.queued.len()),
                },
                None => Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE),
            };
            if mark_eof {
                transport_guard.body = BodyKind::UntilClose { eof: true };
            }
            match result {
                Ok(available) => {
                    memory
                        .write_u32(args.rdx, available as u32)
                        .map_err(memory_fault)?;
                    Ok(1)
                }
                Err(error) => {
                    transport_guard.phase = TransportPhase::Failed;
                    query_available.fail(cpu.gs_base, error, 0)
                }
            }
        },
    );

    let read_data = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpReadData",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            if args.r9 == 0 {
                return read_data.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            memory.write_u32(args.r9, 0).map_err(memory_fault)?;
            let wanted = args.r8 as u32;
            if wanted != 0 {
                memory
                    .check_write(args.rdx, wanted as usize)
                    .map_err(memory_fault)?;
            }
            let transport = {
                let handles = read_data.winhttp.lock().expect("WinHTTP handle table");
                match request_transport(&handles, args.rcx) {
                    Ok(transport) => transport,
                    Err(error) => return read_data.fail(cpu.gs_base, error, 0),
                }
            };
            let mut transport_guard = transport.lock().expect("WinHTTP transport");
            if transport_guard.phase != TransportPhase::Ready {
                return read_data.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
            }
            let body = transport_guard.body;
            let limit = match body {
                BodyKind::Empty | BodyKind::Fixed(0) => 0_usize,
                BodyKind::Fixed(remaining) => (remaining as usize)
                    .min(wanted as usize)
                    .min(MAX_READ_CHUNK_BYTES),
                BodyKind::UntilClose { eof: true } => 0_usize,
                BodyKind::UntilClose { eof: false } => (wanted as usize).min(MAX_READ_CHUNK_BYTES),
                BodyKind::Chunked => (wanted as usize).min(MAX_READ_CHUNK_BYTES),
            };
            let mut bytes = vec![0; limit];
            let result = if limit == 0 {
                Ok(0_usize)
            } else {
                let TransportState {
                    stream, chunked, ..
                } = &mut *transport_guard;
                match stream.as_mut() {
                    Some(reader) if matches!(body, BodyKind::Chunked) => {
                        fill_chunked_queue(reader, chunked, limit).map(|()| {
                            let mut count = 0;
                            while count < limit {
                                let Some(byte) = chunked.queued.pop_front() else {
                                    break;
                                };
                                bytes[count] = byte;
                                count += 1;
                            }
                            count
                        })
                    }
                    Some(reader) => reader.read(&mut bytes).map_err(io_winhttp_error),
                    None => Err(ERROR_WINHTTP_INCORRECT_HANDLE_STATE),
                }
            };
            match result {
                Ok(count) => {
                    match body {
                        BodyKind::Fixed(remaining) => {
                            if count == 0 && remaining != 0 {
                                transport_guard.phase = TransportPhase::Failed;
                                return read_data.fail(
                                    cpu.gs_base,
                                    ERROR_WINHTTP_CONNECTION_ERROR,
                                    0,
                                );
                            }
                            transport_guard.body = BodyKind::Fixed(remaining - count as u64);
                        }
                        BodyKind::UntilClose { eof: false } if count == 0 => {
                            transport_guard.body = BodyKind::UntilClose { eof: true };
                        }
                        _ => {}
                    }
                    bytes.truncate(count);
                    memory.write_bytes(args.rdx, &bytes).map_err(memory_fault)?;
                    memory
                        .write_u32(args.r9, count as u32)
                        .map_err(memory_fault)?;
                    Ok(1)
                }
                Err(error) => {
                    transport_guard.phase = TransportPhase::Failed;
                    read_data.fail(cpu.gs_base, error, 0)
                }
            }
        },
    );

    let query_headers = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpQueryHeaders",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let info_level = args.rdx as u32;
            let length_ptr = args.get(4, memory).map_err(memory_fault)?;
            let index_ptr = args.get(5, memory).map_err(memory_fault)?;
            if length_ptr == 0 || (args.r8 != 0) != (info_level == WINHTTP_QUERY_CUSTOM) {
                return query_headers.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let custom_name = if info_level == WINHTTP_QUERY_CUSTOM {
                match read_wide_string(memory, args.r8, MAX_HEADER_LINE_BYTES) {
                    Ok(name) if !name.is_empty() && name.is_ascii() && valid_header_name(&name) => {
                        Some(name)
                    }
                    _ => return query_headers.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0),
                }
            } else {
                None
            };
            let requested_index = if index_ptr != 0 {
                memory.read_u32(index_ptr).map_err(memory_fault)?
            } else {
                0
            };
            let systemtime_date = info_level & WINHTTP_QUERY_FLAG_SYSTEMTIME != 0;
            let date_header = match info_level & !WINHTTP_QUERY_FLAG_SYSTEMTIME {
                WINHTTP_QUERY_DATE => Some("date"),
                WINHTTP_QUERY_EXPIRES => Some("expires"),
                WINHTTP_QUERY_LAST_MODIFIED => Some("last-modified"),
                _ => None,
            };
            let numeric_status =
                info_level == (WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER);
            let numeric_length =
                info_level == (WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER);
            let textual_status = info_level == WINHTTP_QUERY_STATUS_CODE;
            let textual_status_text = info_level == WINHTTP_QUERY_STATUS_TEXT;
            let textual_content_type = info_level == WINHTTP_QUERY_CONTENT_TYPE;
            let textual_content_length = info_level == WINHTTP_QUERY_CONTENT_LENGTH;
            let textual_server = info_level == WINHTTP_QUERY_SERVER;
            let raw_nul = info_level == WINHTTP_QUERY_RAW_HEADERS;
            let raw_crlf = info_level == WINHTTP_QUERY_RAW_HEADERS_CRLF;
            let custom = info_level == WINHTTP_QUERY_CUSTOM;
            let textual_date = matches!(
                info_level,
                WINHTTP_QUERY_DATE | WINHTTP_QUERY_EXPIRES | WINHTTP_QUERY_LAST_MODIFIED
            );
            let supported = (systemtime_date && date_header.is_some())
                || (textual_date && date_header.is_some())
                || numeric_status
                || numeric_length
                || textual_status
                || textual_status_text
                || textual_content_type
                || textual_content_length
                || textual_server
                || raw_nul
                || raw_crlf
                || custom;
            if !supported {
                return query_headers.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            let transport = {
                let handles = query_headers.winhttp.lock().expect("WinHTTP handle table");
                match request_transport(&handles, args.rcx) {
                    Ok(transport) => transport,
                    Err(error) => return query_headers.fail(cpu.gs_base, error, 0),
                }
            };
            let transport_guard = transport.lock().expect("WinHTTP transport");
            if transport_guard.phase != TransportPhase::Ready {
                return query_headers.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
            }
            let Some(response) = &transport_guard.response else {
                return query_headers.fail(cpu.gs_base, ERROR_WINHTTP_INCORRECT_HANDLE_STATE, 0);
            };
            let named_header = if custom {
                custom_name.as_deref()
            } else if systemtime_date || textual_date {
                date_header
            } else if textual_content_type {
                Some("content-type")
            } else if textual_content_length || numeric_length {
                Some("content-length")
            } else if textual_server {
                Some("server")
            } else {
                None
            };
            // The index parameter selects the zero-based occurrence of a
            // duplicate-named header, as the public implementation evidence
            // counts matches down; an index past the last occurrence is the
            // same not-found error as a missing header.
            let header_value = named_header.map(|wanted| {
                response
                    .headers
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(wanted))
                    .nth(requested_index as usize)
                    .map(|(_, value)| value.as_str())
            });
            if matches!(header_value, Some(None)) {
                return query_headers.fail(cpu.gs_base, ERROR_WINHTTP_HEADER_NOT_FOUND, 0);
            }
            let (contents, success_length) = if systemtime_date {
                let value = header_value.and_then(|value| value).expect("date header");
                let Some(current_year) = http_filetime_year(query_headers.clock.now_filetime())
                else {
                    return query_headers.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                };
                let Some(time) = parse_http_date(value, current_year) else {
                    return query_headers.fail(
                        cpu.gs_base,
                        ERROR_WINHTTP_INVALID_SERVER_RESPONSE,
                        0,
                    );
                };
                (http_system_time_bytes(time).to_vec(), 16_u32)
            } else if numeric_status {
                (response.status.to_le_bytes().to_vec(), 4_u32)
            } else if numeric_length {
                let value = header_value
                    .and_then(|value| value)
                    .expect("content-length");
                let Ok(value) = value.parse::<u32>() else {
                    return query_headers.fail(
                        cpu.gs_base,
                        ERROR_WINHTTP_INVALID_SERVER_RESPONSE,
                        0,
                    );
                };
                (value.to_le_bytes().to_vec(), 4_u32)
            } else {
                let text = if textual_status {
                    response.status.to_string()
                } else if textual_status_text {
                    response.status_text.clone()
                } else if raw_crlf {
                    response.raw.clone()
                } else if raw_nul {
                    // The NUL-separated form is the same head with every line
                    // ending transcribed to a terminator: the capture ends
                    // CRLF-terminated, so the transcription ends with the last
                    // header's terminator and the encoding path appends the
                    // empty string's own, exactly as the public implementation
                    // evidence lays the form out.
                    response.raw.replace("\r\n", "\0")
                } else {
                    header_value
                        .and_then(|value| value)
                        .expect("named header")
                        .to_owned()
                };
                let unit_count = text.encode_utf16().count();
                let mut bytes = Vec::with_capacity((unit_count + 1) * 2);
                for unit in text.encode_utf16().chain([0]) {
                    bytes.extend(unit.to_le_bytes());
                }
                (bytes, (unit_count * 2) as u32)
            };
            let required = contents.len() as u32;
            let capacity = memory.read_u32(length_ptr).map_err(memory_fault)?;
            memory
                .write_u32(length_ptr, required)
                .map_err(memory_fault)?;
            if args.r9 == 0 || capacity < required {
                return query_headers.fail(cpu.gs_base, ERROR_INSUFFICIENT_BUFFER, 0);
            }
            memory
                .write_bytes(args.r9, &contents)
                .map_err(memory_fault)?;
            memory
                .write_u32(length_ptr, success_length)
                .map_err(memory_fault)?;
            if index_ptr != 0 {
                memory
                    .write_u32(index_ptr, requested_index.wrapping_add(1))
                    .map_err(memory_fault)?;
            }
            Ok(1)
        },
    );

    let query_option = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpQueryOption",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            if args.r9 == 0 {
                return query_option.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let length_ptr = args.r9;
            let capacity = memory.read_u32(length_ptr).map_err(memory_fault)?;
            if args.rdx as u32 == WINHTTP_OPTION_CONTEXT_VALUE {
                let value = {
                    let handles = query_option.winhttp.lock().expect("WinHTTP handle table");
                    match handles.context(args.rcx) {
                        Some(value) => value,
                        None => {
                            return query_option.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
                        }
                    }
                };
                memory.write_u32(length_ptr, 8).map_err(memory_fault)?;
                if args.r8 == 0 || capacity < 8 {
                    return query_option.fail(cpu.gs_base, ERROR_INSUFFICIENT_BUFFER, 0);
                }
                memory.write_u64(args.r8, value).map_err(memory_fault)?;
                return Ok(1);
            }
            let mut timeouts = {
                let handles = query_option.winhttp.lock().expect("WinHTTP handle table");
                match handles.kind(args.rcx) {
                    Some(HandleKind::Session { timeouts, .. })
                    | Some(HandleKind::Request { timeouts, .. }) => {
                        *timeouts.lock().expect("WinHTTP timeouts")
                    }
                    Some(_) => {
                        return query_option.fail(
                            cpu.gs_base,
                            ERROR_WINHTTP_INCORRECT_HANDLE_TYPE,
                            0,
                        );
                    }
                    None => return query_option.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
                }
            };
            let Some(&mut value) = timeouts.option_mut(args.rdx as u32) else {
                return query_option.fail(cpu.gs_base, ERROR_WINHTTP_INVALID_OPTION, 0);
            };
            memory.write_u32(length_ptr, 4).map_err(memory_fault)?;
            if args.r8 == 0 || capacity < 4 {
                return query_option.fail(cpu.gs_base, ERROR_INSUFFICIENT_BUFFER, 0);
            }
            memory.write_u32(args.r8, value).map_err(memory_fault)?;
            Ok(1)
        },
    );

    let time_to = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpTimeToSystemTime",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = (|| {
                let text = read_wide_string(memory, args.rcx, MAX_WIDE_UNITS)?;
                let current_year = http_filetime_year(time_to.clock.now_filetime())
                    .ok_or(ERROR_INVALID_PARAMETER)?;
                let time = parse_http_date(&text, current_year).ok_or(ERROR_INVALID_PARAMETER)?;
                write_http_system_time(memory, args.rdx, time)
            })();
            match result {
                Ok(()) => Ok(1),
                Err(error) => time_to.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let time_from = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpTimeFromSystemTime",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = (|| {
                let input = read_http_system_time(memory, args.rcx)?;
                if !valid_http_system_date(input) {
                    return Err(ERROR_INVALID_PARAMETER);
                }
                let time = HttpSystemTime {
                    day_of_week: http_day_of_week(input.year, input.month, input.day),
                    ..input
                };
                write_http_date_string(memory, args.rdx, time)
            })();
            match result {
                Ok(()) => Ok(1),
                Err(error) => time_from.fail(cpu.gs_base, error, 0),
            }
        },
    );

    let close = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WinHttpCloseHandle",
        ServiceStatus::Partial,
        move |args, cpu, _| {
            let mut handles = close.winhttp.lock().expect("WinHTTP handle table");
            Ok(close_result(&close, cpu.gs_base, handles.close(args.rcx)))
        },
    );
}

// The service handlers return `Result<_, ExitReason>`; keeping the conversion
// in one place avoids exposing memory faults as successful handle values.
fn memory_fault(error: crate::memory::MemoryError) -> crate::cpu::ExitReason {
    crate::cpu::ExitReason::MemoryFault(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{CpuState, ExitReason, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::MemoryPermissions;
    use crate::services::MockClock;

    fn write_wide(memory: &mut GuestMemory, address: u64, text: &str) {
        for (index, unit) in text.encode_utf16().chain([0]).enumerate() {
            memory.write_u16(address + 2 * index as u64, unit).unwrap();
        }
    }

    fn read_test_wide(memory: &GuestMemory, address: u64, byte_len: u32) -> String {
        let units = (0..byte_len / 2)
            .map(|index| memory.read_u16(address + 2 * u64::from(index)).unwrap())
            .collect::<Vec<_>>();
        String::from_utf16(&units).unwrap()
    }

    fn write_test_system_time(memory: &mut GuestMemory, address: u64, fields: [u16; 8]) {
        for (index, field) in fields.into_iter().enumerate() {
            memory.write_u16(address + 2 * index as u64, field).unwrap();
        }
    }

    fn read_test_system_time(memory: &GuestMemory, address: u64) -> [u16; 8] {
        let mut fields = [0; 8];
        for (index, field) in fields.iter_mut().enumerate() {
            *field = memory.read_u16(address + 2 * index as u64).unwrap();
        }
        fields
    }

    struct Fixture {
        registry: ImportRegistry,
        state: Arc<ServicesState>,
        memory: GuestMemory,
    }

    impl Fixture {
        fn new() -> Self {
            let mut registry = ImportRegistry::new();
            let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(
                134116992000000000,
            ))));
            register(&mut registry, &state);
            let mut memory = GuestMemory::new();
            memory
                .map_region(0x1000, 0x8000, MemoryPermissions::READ_WRITE)
                .unwrap();
            write_wide(&mut memory, 0x2000, "XenoraTest/1.0");
            write_wide(&mut memory, 0x2100, "example.test");
            write_wide(&mut memory, 0x2200, "GET");
            write_wide(&mut memory, 0x2300, "/fixture");
            write_wide(&mut memory, 0x2400, "HTTP/1.1");
            Self {
                registry,
                state,
                memory,
            }
        }

        fn call(&mut self, symbol: &str, args: &[u64]) -> u64 {
            let mut cpu = CpuState::default();
            for (register, value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
                .into_iter()
                .zip(args.iter().copied())
            {
                cpu.set(register, value);
            }
            cpu.set(Register::Rsp, 0x7000);
            for (index, value) in args.iter().copied().enumerate().skip(4) {
                self.memory
                    .write_u64(0x7000 + 0x28 + (index as u64 - 4) * 8, value)
                    .unwrap();
            }
            let descriptor = self
                .registry
                .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
                .unwrap();
            (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut self.memory).unwrap()
        }

        fn open_session(&mut self) -> u64 {
            self.call("WinHttpOpen", &[0x2000, 1, 0, 0, 0])
        }

        fn open_connection(&mut self) -> (u64, u64) {
            let session = self.open_session();
            let connection = self.call("WinHttpConnect", &[session, 0x2100, 80, 0]);
            (session, connection)
        }
    }

    #[test]
    fn session_connection_request_and_close_have_owner_lifetimes() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        assert_ne!(session, 0);
        assert_ne!(connection, 0);
        assert_ne!(session, connection);
        write_wide(&mut fixture.memory, 0x2500, "text/plain");
        fixture.memory.write_u64(0x2580, 0x2500).unwrap();
        fixture.memory.write_u64(0x2588, 0).unwrap();
        let request = fixture.call(
            "WinHttpOpenRequest",
            &[connection, 0x2200, 0x2300, 0x2400, 0, 0x2580, 0],
        );
        assert_ne!(request, 0);
        assert_eq!(fixture.state.open_handle_count(), 3);
        assert_eq!(fixture.call("WinHttpCloseHandle", &[request]), 1);
        assert_eq!(fixture.call("WinHttpCloseHandle", &[request]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_INVALID_HANDLE);
        assert_eq!(fixture.call("WinHttpCloseHandle", &[connection]), 1);
        assert_eq!(fixture.call("WinHttpCloseHandle", &[session]), 1);
        assert_eq!(fixture.state.open_handle_count(), 0);
    }

    #[test]
    fn defaults_and_secure_flag_are_retained_without_transport() {
        let mut fixture = Fixture::new();
        let (_session, connection) = fixture.open_connection();
        let request = fixture.call(
            "WinHttpOpenRequest",
            &[connection, 0, 0, 0, 0, 0, WINHTTP_FLAG_SECURE as u64],
        );
        assert_ne!(request, 0);
        let handles = fixture.state.winhttp.lock().unwrap();
        assert!(matches!(
            handles.kind(request),
            Some(HandleKind::Request {
                verb,
                object_name,
                version,
                secure: true,
                ..
            }) if verb == "GET" && object_name.is_empty() && version == "HTTP/1.1"
        ));
    }

    #[test]
    fn invalid_parents_flags_strings_and_reserved_values_fail_honestly() {
        let mut fixture = Fixture::new();
        assert_eq!(fixture.call("WinHttpConnect", &[1234, 0x2100, 80, 0]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_INVALID_HANDLE);
        let wrong_parent = fixture.open_session();
        assert_eq!(
            fixture.call("WinHttpOpenRequest", &[wrong_parent, 0, 0, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_TYPE
        );
        assert_eq!(fixture.call("WinHttpCloseHandle", &[wrong_parent]), 1);
        assert_eq!(fixture.call("WinHttpOpen", &[0x2000, 4, 0, 0, 0]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_NOT_SUPPORTED);
        assert_eq!(
            fixture.call("WinHttpOpen", &[0x2000, 1, 0, 0, WINHTTP_FLAG_ASYNC as u64]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_NOT_SUPPORTED);
        let (_session, connection) = fixture.open_connection();
        assert_eq!(
            fixture.call(
                "WinHttpOpenRequest",
                &[connection, 0x2200, 0x2300, 0x2400, 0, 0, 4]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_NOT_SUPPORTED);
        fixture.memory.write_u16(0x2100, 0).unwrap();
        assert_eq!(fixture.call("WinHttpConnect", &[1234, 0x2100, 80, 0]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_INVALID_URL);
    }

    #[test]
    fn process_exit_closes_the_winhttp_handle_table() {
        let mut fixture = Fixture::new();
        let (_session, connection) = fixture.open_connection();
        assert_ne!(connection, 0);
        assert_eq!(fixture.state.open_handle_count(), 2);
        assert_eq!(fixture.state.exit_process(7), 7);
        assert_eq!(fixture.state.open_handle_count(), 0);
    }

    #[test]
    fn internet_port_uses_only_the_low_sixteen_bits() {
        let mut fixture = Fixture::new();
        let session = fixture.open_session();
        let connection = fixture.call("WinHttpConnect", &[session, 0x2100, 0x1_0000_0050, 0]);
        assert_ne!(connection, 0);
        let handles = fixture.state.winhttp.lock().unwrap();
        assert!(matches!(
            handles.kind(connection),
            Some(HandleKind::Connection { port: 80, .. })
        ));
    }

    #[test]
    fn accept_types_are_copied_out_of_guest_memory() {
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2500, "text/plain");
        fixture.memory.write_u64(0x2600, 0x2500).unwrap();
        fixture.memory.write_u64(0x2608, 0).unwrap();
        let (_session, connection) = fixture.open_connection();
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0x2600, 0]);
        assert_ne!(request, 0);
        write_wide(&mut fixture.memory, 0x2500, "text/html");
        let handles = fixture.state.winhttp.lock().unwrap();
        let Some(HandleKind::Request { transport, .. }) = handles.kind(request) else {
            panic!("request handle missing");
        };
        let transport = transport.lock().unwrap();
        assert!(
            transport
                .request_headers
                .iter()
                .any(|(name, value)| { name == "Accept" && value == "text/plain" })
        );
    }

    #[test]
    fn closing_a_parent_does_not_mutate_an_existing_request_snapshot() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
        assert_ne!(request, 0);
        assert_eq!(fixture.call("WinHttpCloseHandle", &[connection]), 1);
        assert_eq!(fixture.call("WinHttpCloseHandle", &[session]), 1);
        let handles = fixture.state.winhttp.lock().unwrap();
        assert!(matches!(
            handles.kind(request),
            Some(HandleKind::Request {
                server,
                port: 80,
                ..
            }) if server == "example.test"
        ));
    }

    #[test]
    fn timeout_defaults_and_request_inheritance_are_explicit() {
        let mut fixture = Fixture::new();
        let session = fixture.open_session();
        assert_ne!(session, 0);
        {
            let handles = fixture.state.winhttp.lock().unwrap();
            let Some(HandleKind::Session { timeouts, .. }) = handles.kind(session) else {
                panic!("session handle missing");
            };
            assert_eq!(*timeouts.lock().unwrap(), TimeoutSettings::default());
        }

        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[session, 0, 1_111, 2_222, 3_333]),
            1
        );
        let connection = fixture.call("WinHttpConnect", &[session, 0x2100, 80, 0]);
        assert_ne!(connection, 0);
        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[session, 0, 4_444, 5_555, 6_666]),
            1
        );
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
        assert_ne!(request, 0);
        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[session, 0, 7_777, 8_888, 9_999]),
            1
        );
        {
            let handles = fixture.state.winhttp.lock().unwrap();
            let Some(HandleKind::Request { timeouts, .. }) = handles.kind(request) else {
                panic!("request handle missing");
            };
            assert_eq!(
                *timeouts.lock().unwrap(),
                TimeoutSettings {
                    resolve: 0,
                    connect: 4_444,
                    send: 5_555,
                    receive: 6_666,
                }
            );
        }

        assert_eq!(
            fixture.call(
                "WinHttpSetTimeouts",
                &[request, u64::from(u32::MAX), 1, 2, 3]
            ),
            1
        );
        let handles = fixture.state.winhttp.lock().unwrap();
        let Some(HandleKind::Request { timeouts, .. }) = handles.kind(request) else {
            panic!("request handle missing");
        };
        assert_eq!(
            *timeouts.lock().unwrap(),
            TimeoutSettings {
                resolve: u32::MAX,
                connect: 1,
                send: 2,
                receive: 3,
            }
        );
    }

    #[test]
    fn timeout_options_can_be_queried_from_sessions_and_requests() {
        let mut fixture = Fixture::new();
        let session = fixture.open_session();
        fixture.memory.write_u32(0x2900, 4).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 3, 0x2910, 0x2900]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2900).unwrap(), 4);
        assert_eq!(fixture.memory.read_u32(0x2910).unwrap(), 60_000);

        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[session, 0, 1_111, 2_222, 3_333]),
            1
        );
        let connection = fixture.call("WinHttpConnect", &[session, 0x2100, 80, 0]);
        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[session, 0, 4_444, 5_555, 6_666]),
            1
        );
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
        assert_ne!(request, 0);
        for (option, expected) in [(2, 0), (3, 4_444), (5, 5_555), (6, 6_666)] {
            fixture.memory.write_u32(0x2900, 4).unwrap();
            assert_eq!(
                fixture.call("WinHttpQueryOption", &[request, option, 0x2910, 0x2900]),
                1
            );
            assert_eq!(fixture.memory.read_u32(0x2910).unwrap(), expected);
        }
        assert_eq!(
            fixture.call(
                "WinHttpSetTimeouts",
                &[request, u64::from(u32::MAX), 1, 2, 3]
            ),
            1
        );
        fixture.memory.write_u32(0x2900, 4).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[request, 2, 0x2910, 0x2900]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2910).unwrap(), u32::MAX);
    }

    #[test]
    fn setting_timeout_options_preserves_dwords_and_request_inheritance() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        for (option, value) in [(2, u32::MAX), (3, 0x8000_0001), (5, 700), (6, 10_000)] {
            fixture.memory.write_u32(0x2910, value).unwrap();
            assert_eq!(
                fixture.call("WinHttpSetOption", &[session, option, 0x2910, 4]),
                1
            );
        }
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
        assert_ne!(request, 0);

        fixture.memory.write_u32(0x2910, 240).unwrap();
        assert_eq!(
            fixture.call("WinHttpSetOption", &[request, 6, 0x2910, 4]),
            1
        );
        for (handle, option, expected) in [
            (request, 2, u32::MAX),
            (request, 3, 0x8000_0001),
            (request, 5, 700),
            (request, 6, 240),
            (session, 6, 10_000),
        ] {
            fixture.memory.write_u32(0x2900, 4).unwrap();
            assert_eq!(
                fixture.call("WinHttpQueryOption", &[handle, option, 0x2920, 0x2900]),
                1
            );
            assert_eq!(fixture.memory.read_u32(0x2920).unwrap(), expected);
        }
    }

    /// WINHTTP_OPTION_CONTEXT_VALUE is the caller's own opaque per-handle
    /// value: zero until set, an eight-byte DWORD_PTR on this bitness, and
    /// gone with the handle (Microsoft Learn, "Option Flags").
    #[test]
    fn context_value_round_trips_per_handle_and_dies_with_it() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        assert_ne!(connection, 0);
        // Unset reads as zero.
        fixture.memory.write_u32(0x2900, 8).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 78, 0x2920, 0x2900]),
            1
        );
        assert_eq!(fixture.memory.read_u64(0x2920).unwrap(), 0);
        // Set then query returns the exact value, here on two handles that
        // do not share one context.
        fixture.memory.write_u64(0x2910, 0xDEAD_BEEF_CAFE).unwrap();
        assert_eq!(
            fixture.call("WinHttpSetOption", &[session, 78, 0x2910, 8]),
            1
        );
        fixture.memory.write_u32(0x2900, 8).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[connection, 78, 0x2920, 0x2900]),
            1
        );
        assert_eq!(fixture.memory.read_u64(0x2920).unwrap(), 0);
        fixture.memory.write_u32(0x2900, 8).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 78, 0x2920, 0x2900]),
            1
        );
        assert_eq!(fixture.memory.read_u64(0x2920).unwrap(), 0xDEAD_BEEF_CAFE);
        // The size is part of the contract: four bytes is not the option.
        assert_eq!(
            fixture.call("WinHttpSetOption", &[session, 78, 0x2910, 4]),
            0
        );
        // Closing the handle ends its context: the same numeric handle is no
        // handle at all, exactly as its other state is gone.
        assert_eq!(fixture.call("WinHttpCloseHandle", &[session]), 1);
        fixture.memory.write_u32(0x2900, 8).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 78, 0x2920, 0x2900]),
            0
        );
    }

    #[test]
    fn invalid_timeout_options_leave_current_values_intact() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        fixture.memory.write_u32(0x2910, 901).unwrap();
        for (args, error) in [
            ([session, 3, 0, 4], ERROR_INVALID_PARAMETER),
            ([session, 3, 0x2910, 3], ERROR_INVALID_PARAMETER),
            ([session, 3, 0x2910, 8], ERROR_INVALID_PARAMETER),
            ([session, 3, 0x9000, 4], ERROR_INVALID_PARAMETER),
            ([session, 4, 0x2910, 4], ERROR_WINHTTP_INVALID_OPTION),
            (
                [connection, 3, 0x2910, 4],
                ERROR_WINHTTP_INCORRECT_HANDLE_TYPE,
            ),
            ([0, 3, 0x2910, 4], ERROR_INVALID_HANDLE),
        ] {
            assert_eq!(fixture.call("WinHttpSetOption", &args), 0);
            assert_eq!(fixture.state.last_error(0), error);
        }
        fixture.memory.write_u32(0x2900, 4).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 3, 0x2920, 0x2900]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2920).unwrap(), 60_000);
    }

    #[test]
    fn timeout_option_queries_report_required_size_and_bad_inputs() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        fixture.memory.write_u32(0x2900, 0).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 3, 0, 0x2900]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
        assert_eq!(fixture.memory.read_u32(0x2900).unwrap(), 4);
        fixture.memory.write_u32(0x2900, 3).unwrap();
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 3, 0x2910, 0x2900]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
        assert_eq!(fixture.memory.read_u32(0x2900).unwrap(), 4);

        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 4, 0x2910, 0x2900]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_INVALID_OPTION);
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[connection, 3, 0x2910, 0x2900]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_TYPE
        );
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[9999, 3, 0x2910, 0x2900]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INVALID_HANDLE);
        assert_eq!(
            fixture.call("WinHttpQueryOption", &[session, 3, 0x2910, 0]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn invalid_timeout_handles_and_values_fail_honestly() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        assert_eq!(
            fixture.call(
                "WinHttpSetTimeouts",
                &[session, u64::from(u32::MAX - 1), 1, 1, 1]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INVALID_PARAMETER);
        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[connection, 0, 1, 1, 1]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_TYPE
        );
        assert_eq!(fixture.call("WinHttpSetTimeouts", &[9999, 0, 1, 1, 1]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_INVALID_HANDLE);
    }

    #[test]
    fn receive_timeout_is_applied_to_a_silent_server() {
        use std::sync::mpsc;

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop_sender, stop_receiver) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            let _ = stop_receiver.recv_timeout(Duration::from_secs(5));
        });
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[request, 5_000, 1_000, 1_000, 0]),
            1
        );
        fixture.memory.write_u32(0x2900, 50).unwrap();
        assert_eq!(
            fixture.call("WinHttpSetOption", &[request, 6, 0x2900, 4]),
            1
        );
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(
            fixture.call("WinHttpSetTimeouts", &[request, 0, 1_000, 1_000, 50]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        assert_eq!(
            fixture.call("WinHttpSetOption", &[request, 6, 0x2900, 4]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_TIMEOUT);
        drop(stop_sender);
        server.join().unwrap();
    }

    type CapturedWires = Arc<Mutex<Vec<Vec<u8>>>>;
    type ChainServer = (CapturedWires, std::thread::JoinHandle<()>);

    /// Serves `responses` on sequential connections of one listener and
    /// captures the exact wire bytes of each request.
    fn serve_chain(
        responses: &'static [&'static [u8]],
    ) -> (u16, CapturedWires, std::thread::JoinHandle<()>) {
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (captured, thread) = serve_chain_on(listener, responses);
        (port, captured, thread)
    }

    /// The connection-driving half of `serve_chain`, for tests that must know
    /// the port before formatting their responses.
    fn serve_chain_on(
        listener: std::net::TcpListener,
        responses: &'static [&'static [u8]],
    ) -> ChainServer {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_thread = Arc::clone(&captured);
        let thread = std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    let complete = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .is_some_and(|header_end| {
                            let head = String::from_utf8_lossy(&request[..header_end]);
                            let body_length = head
                                .split("\r\n")
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            request.len() >= header_end + 4 + body_length
                        });
                    if complete {
                        break;
                    }
                }
                captured_thread.lock().unwrap().push(request);
                stream.write_all(response).unwrap();
            }
        });
        (captured, thread)
    }

    fn serve_once(
        response: &'static [u8],
    ) -> (u16, Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_thread = Arc::clone(&captured);
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                let complete = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .is_some_and(|header_end| {
                        let head = String::from_utf8_lossy(&request[..header_end]);
                        let chunked = head.split("\r\n").any(|line| {
                            line.split_once(':').is_some_and(|(name, value)| {
                                name.trim().eq_ignore_ascii_case("transfer-encoding")
                                    && value.trim().eq_ignore_ascii_case("chunked")
                            })
                        });
                        if chunked {
                            return request[header_end + 4..]
                                .windows(5)
                                .any(|window| window == b"0\r\n\r\n");
                        }
                        let body_length = head
                            .split("\r\n")
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        request.len() >= header_end + 4 + body_length
                    });
                if complete {
                    break;
                }
                assert!(request.len() < 16 * 1024);
            }
            *captured_thread.lock().unwrap() = request;
            stream.write_all(response).unwrap();
        });
        (port, captured, thread)
    }

    fn write_loopback_host(fixture: &mut Fixture) {
        write_wide(&mut fixture.memory, 0x2100, "127.0.0.1");
    }

    fn open_loopback_request(fixture: &mut Fixture, port: u16) -> u64 {
        write_loopback_host(fixture);
        let session = fixture.open_session();
        let connection = fixture.call("WinHttpConnect", &[session, 0x2100, port as u64, 0]);
        assert_ne!(connection, 0);
        write_wide(&mut fixture.memory, 0x2500, "text/plain");
        fixture.memory.write_u64(0x2580, 0x2500).unwrap();
        fixture.memory.write_u64(0x2588, 0).unwrap();
        let request = fixture.call(
            "WinHttpOpenRequest",
            &[connection, 0x2200, 0x2300, 0x2400, 0, 0x2580, 0],
        );
        assert_ne!(request, 0);
        request
    }

    #[test]
    fn configured_headers_are_copied_merged_and_sent_on_the_wire() {
        let (port, captured, server) =
            serve_once(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);

        let mut add = |text: &str, units, modifiers| {
            write_wide(&mut fixture.memory, 0x2700, text);
            fixture.call(
                "WinHttpAddRequestHeaders",
                &[request, 0x2700, units, modifiers],
            )
        };
        let initial = "X-Trace: one\r\nX-Drop: obsolete";
        assert_eq!(
            add(&format!("{initial}ignored"), initial.len() as u64, 0),
            1
        );
        assert_eq!(add("x-TRACE: two", u64::from(u32::MAX), 0x4000_0000), 1);
        assert_eq!(add("X-Drop:", u64::from(u32::MAX), 0x8000_0000), 1);
        assert_eq!(
            add("accept: application/json", u64::from(u32::MAX), 0x8000_0000),
            1
        );
        assert_eq!(
            add(
                "X-Omitted: first\r\nx-trace: duplicate",
                u64::from(u32::MAX),
                0x1000_0000
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), 12155);
        write_wide(&mut fixture.memory, 0x2700, "X-Once: transient\r\n");
        let body = [0, b'A', 0xff];
        fixture.memory.write_bytes(0x2800, &body).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpSendRequest",
                &[request, 0x2700, u64::from(u32::MAX), 0x2800, 3, 3, 0]
            ),
            1
        );
        assert_eq!(
            fixture.call(
                "WinHttpAddRequestHeaders",
                &[request, 0x2700, u64::from(u32::MAX), 0]
            ),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        let captured = captured.lock().unwrap();
        let mut expected = format!(
            "POST /fixture HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             User-Agent: XenoraTest/1.0\r\n\
             Accept: application/json\r\n\
             X-Trace: one, two\r\n\
             X-Once: transient\r\n\
             Content-Length: 3\r\n\
             Connection: close\r\n\r\n"
        )
        .into_bytes();
        expected.extend_from_slice(&body);
        assert_eq!(*captured, expected);
    }

    #[test]
    fn request_header_updates_reject_bad_pointers_handles_and_framing() {
        let mut fixture = Fixture::new();
        let (session, connection) = fixture.open_connection();
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
        write_wide(&mut fixture.memory, 0x2700, "X-Test: okay");
        for (args, error) in [
            (
                [session, 0x2700, 12, 0],
                ERROR_WINHTTP_INCORRECT_HANDLE_TYPE,
            ),
            ([request, 0, 0, 0], ERROR_INVALID_PARAMETER),
            ([request, 0x9000, 5, 0], ERROR_INVALID_PARAMETER),
            ([request, 0x2700, 4097, 0], ERROR_INVALID_PARAMETER),
            ([request, 0x2700, 12, 1], ERROR_NOT_SUPPORTED),
            ([1, 0x2700, 12, 0], ERROR_INVALID_HANDLE),
        ] {
            assert_eq!(fixture.call("WinHttpAddRequestHeaders", &args), 0);
            assert_eq!(fixture.state.last_error(0), error);
        }
        for (text, error) in [
            ("bad name: data", ERROR_INVALID_PARAMETER),
            ("X-Okay: value\nX-Other: data", ERROR_INVALID_PARAMETER),
            ("hOsT: outside", ERROR_NOT_SUPPORTED),
            ("Transfer-Encoding: gzip", ERROR_NOT_SUPPORTED),
        ] {
            write_wide(&mut fixture.memory, 0x2700, text);
            assert_eq!(
                fixture.call(
                    "WinHttpAddRequestHeaders",
                    &[request, 0x2700, u64::from(u32::MAX), 0]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), error);
        }
    }

    #[test]
    fn synchronous_plaintext_get_reads_status_and_response_body() {
        let (port, captured, server) = serve_once(
            b"HTTP/1.1 203 Non-Authoritative\r\nContent-Length: 11\r\nContent-Type: text/plain\r\nX-Server: fixture\r\n\r\nhello WORLD",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        write_wide(&mut fixture.memory, 0x2700, "X-Fixture: yes\r\n");
        assert_eq!(
            fixture.call(
                "WinHttpSendRequest",
                &[request, 0x2700, u32::MAX as u64, 0, 0, 0, 0xabc]
            ),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        fixture.memory.write_u32(0x2908, 2).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    (WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER) as u64,
                    0,
                    0x2900,
                    0x2908,
                    0,
                ]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
        assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), 4);
        fixture.memory.write_u32(0x2908, 4).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    (WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER) as u64,
                    0,
                    0x2900,
                    0x2908,
                    0,
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2900).unwrap(), 203);

        fixture.memory.write_u32(0x2928, 256).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    WINHTTP_QUERY_STATUS_TEXT as u64,
                    0,
                    0x3000,
                    0x2928,
                    0
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2928).unwrap(), 34);
        assert_eq!(
            read_test_wide(&fixture.memory, 0x3000, 34),
            "Non-Authoritative"
        );

        fixture.memory.write_u32(0x2938, 4).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    (WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER) as u64,
                    0,
                    0x3020,
                    0x2938,
                    0,
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x3020).unwrap(), 11);

        fixture.memory.write_u32(0x2948, 256).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    WINHTTP_QUERY_CONTENT_TYPE as u64,
                    0,
                    0x3040,
                    0x2948,
                    0
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2948).unwrap(), 20);
        assert_eq!(read_test_wide(&fixture.memory, 0x3040, 20), "text/plain");

        fixture.memory.write_u32(0x2958, 256).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[request, WINHTTP_QUERY_SERVER as u64, 0, 0x3080, 0x2958, 0]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_HEADER_NOT_FOUND);
        assert_eq!(
            fixture.call("WinHttpQueryDataAvailable", &[request, 0x2910]),
            1
        );
        let available = fixture.memory.read_u32(0x2910).unwrap();
        assert!((1..=11).contains(&available));
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a00, 5, 0x2914]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2914).unwrap(), 5);
        let mut first = [0; 5];
        fixture.memory.read_bytes(0x2a00, &mut first).unwrap();
        assert_eq!(&first, b"hello");
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a10, 64, 0x2914]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2914).unwrap(), 6);
        let mut rest = [0; 6];
        fixture.memory.read_bytes(0x2a10, &mut rest).unwrap();
        assert_eq!(&rest, b" WORLD");
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a20, 8, 0x2914]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2914).unwrap(), 0);
        server.join().unwrap();
        let wire = captured.lock().unwrap().clone();
        let wire = String::from_utf8(wire).unwrap();
        assert!(wire.starts_with("GET /fixture HTTP/1.1\r\n"));
        assert!(wire.contains(&format!("Host: 127.0.0.1:{port}\r\n")));
        assert!(wire.contains("User-Agent: XenoraTest/1.0\r\n"));
        assert!(wire.contains("Accept: text/plain\r\n"));
        assert!(wire.contains("X-Fixture: yes\r\n"));
        assert!(wire.contains("Connection: close\r\n\r\n"));
    }

    #[test]
    fn optional_request_data_is_copied_into_the_wire_body() {
        let (port, captured, server) =
            serve_once(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);
        fixture.memory.write_bytes(0x2800, b"data").unwrap();
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0x2800, 4, 4, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        let wire = captured.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&wire);
        assert!(text.starts_with("POST /fixture HTTP/1.1\r\n"));
        assert!(text.contains("Content-Length: 4\r\n"));
        assert!(wire.ends_with(b"data"));
    }

    #[test]
    fn chunked_upload_writes_verbatim_and_receives_after_the_terminal_chunk() {
        let (port, captured, server) =
            serve_once(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);
        write_wide(
            &mut fixture.memory,
            0x2700,
            "Transfer-Encoding: chunked\r\n",
        );
        assert_eq!(
            fixture.call(
                "WinHttpSendRequest",
                &[request, 0x2700, u32::MAX as u64, 0, 0, 0, 0]
            ),
            1
        );
        // The caller frames the chunks itself; the transport copies bytes verbatim.
        for chunk in [
            b"5\r\n" as &[u8],
            b"hello",
            b"\r\n6\r\n world",
            b"\r\n0\r\n\r\n",
        ] {
            fixture.memory.write_bytes(0x2800, chunk).unwrap();
            fixture.memory.write_u32(0x2904, 0).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpWriteData",
                    &[request, 0x2800, chunk.len() as u64, 0x2904]
                ),
                1
            );
            assert_eq!(
                fixture.memory.read_u32(0x2904).unwrap() as usize,
                chunk.len()
            );
        }
        // No fixed-length accounting completes the upload; the terminal chunk
        // is caller data, so receiving is accepted from the writing phase.
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        let wire = captured.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&wire);
        assert!(text.starts_with("POST /fixture HTTP/1.1\r\n"));
        assert!(text.contains("Transfer-Encoding: chunked\r\n"));
        assert!(!text.contains("Content-Length"));
        assert!(wire.ends_with(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"));
    }

    #[test]
    fn chunked_upload_rejects_length_combinations_and_extra_writes() {
        let (port, _captured, server) =
            serve_once(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);
        write_wide(
            &mut fixture.memory,
            0x2700,
            "Transfer-Encoding: chunked\r\n",
        );
        fixture.memory.write_bytes(0x2800, b"data").unwrap();
        // Optional data or a declared total length cannot combine with the
        // Transfer-Encoding header, as the WinHttpSendRequest reference states.
        for args in [
            [request, 0x2700, u32::MAX as u64, 0x2800, 4, 4, 0],
            [request, 0x2700, u32::MAX as u64, 0, 0, 8, 0],
        ] {
            assert_eq!(fixture.call("WinHttpSendRequest", &args), 0);
            assert_eq!(fixture.state.last_error(0), ERROR_INVALID_PARAMETER);
        }
        // A fixed-length request still completes and refuses further writes.
        write_wide(&mut fixture.memory, 0x2700, "X-Fixture: yes\r\n");
        assert_eq!(
            fixture.call(
                "WinHttpSendRequest",
                &[request, 0x2700, u32::MAX as u64, 0x2800, 4, 4, 0]
            ),
            1
        );
        assert_eq!(
            fixture.call("WinHttpWriteData", &[request, 0x2800, 1, 0x2904]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
    }

    #[test]
    fn raw_headers_crlf_returns_the_exact_wire_head_with_documented_sizing() {
        let (port, _captured, server) = serve_once(
            b"HTTP/1.1 203 Non-Authoritative\r\nContent-Length: 11\r\nContent-Type: text/plain\r\nX-Server: fixture\r\n\r\nhello WORLD",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        let expected = "HTTP/1.1 203 Non-Authoritative\r\nContent-Length: 11\r\nContent-Type: text/plain\r\nX-Server: fixture\r\n";
        let units = expected.encode_utf16().count();
        let required = ((units + 1) * 2) as u32;
        // Sizing: absent or short buffers report the required byte count
        // including the terminator and write nothing.
        for (buffer, capacity) in [(0u64, 0u32), (0x2a00, required - 2)] {
            fixture.memory.write_u32(0x2908, capacity).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[
                        request,
                        WINHTTP_QUERY_RAW_HEADERS_CRLF as u64,
                        0,
                        buffer,
                        0x2908,
                        0
                    ]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), required);
        }
        // Exact capacity succeeds and reports the count excluding the terminator.
        fixture.memory.write_u32(0x2908, required).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    WINHTTP_QUERY_RAW_HEADERS_CRLF as u64,
                    0,
                    0x2a00,
                    0x2908,
                    0
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), (units * 2) as u32);
        let mut bytes = vec![0u8; units * 2 + 2];
        fixture.memory.read_bytes(0x2a00, &mut bytes).unwrap();
        let (words, _) = bytes.as_chunks::<2>();
        let text = String::from_utf16(
            &words
                .iter()
                .copied()
                .map(u16::from_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(text, format!("{expected}\0"));
        server.join().unwrap();
    }

    #[test]
    fn duplicate_headers_enumerate_through_the_index_parameter() {
        let (port, _captured, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Dup: first\r\nX-Other: middle\r\nX-Dup: second\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        write_wide(&mut fixture.memory, 0x2700, "x-dup");
        let query = |fixture: &mut Fixture, index: u32| {
            fixture.memory.write_u32(0x2910, index).unwrap();
            fixture.memory.write_u32(0x2908, 128).unwrap();
            let result = fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    WINHTTP_QUERY_CUSTOM as u64,
                    0x2700,
                    0x2a00,
                    0x2908,
                    0x2910,
                ],
            );
            let mut bytes = [0u8; 64];
            fixture.memory.read_bytes(0x2a00, &mut bytes).unwrap();
            let text = String::from_utf16(
                &bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .copied()
                    .map(u16::from_le_bytes)
                    .take_while(|unit| *unit != 0)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            (result, text, fixture.memory.read_u32(0x2910).unwrap())
        };
        // Index 0 answers the first occurrence and advances the index, so the
        // enumeration loop the public evidence shows walks the duplicates.
        assert_eq!(query(&mut fixture, 0), (1, "first".to_owned(), 1));
        assert_eq!(query(&mut fixture, 1), (1, "second".to_owned(), 2));
        // An index past the last occurrence is the same not-found error as a
        // missing header, and the index stays where it was.
        let (result, _, index) = query(&mut fixture, 2);
        assert_eq!((result, index), (0, 2));
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_HEADER_NOT_FOUND);
        server.join().unwrap();
    }

    #[test]
    fn raw_headers_nul_separated_form_transcribes_the_wire_text_with_textual_sizing() {
        let (port, _captured, server) = serve_once(
            b"HTTP/1.1 203 Non-Authoritative\r\nContent-Length: 11\r\nContent-Type: text/plain\r\nX-Server: fixture\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        // The NUL form is the same head with every line ending transcribed:
        // each line NUL-terminated, the last terminator doubled by the empty
        // trailing string the form carries.
        let wire = "HTTP/1.1 203 Non-Authoritative\r\nContent-Length: 11\r\nContent-Type: text/plain\r\nX-Server: fixture\r\n";
        let expected = wire.replace("\r\n", "\0");
        let units = expected.encode_utf16().count();
        let required = ((units + 1) * 2) as u32;
        for (buffer, capacity) in [(0u64, 0u32), (0x2a00, required - 2)] {
            fixture.memory.write_u32(0x2908, capacity).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[
                        request,
                        WINHTTP_QUERY_RAW_HEADERS as u64,
                        0,
                        buffer,
                        0x2908,
                        0
                    ]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), required);
        }
        fixture.memory.write_u32(0x2908, required).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    WINHTTP_QUERY_RAW_HEADERS as u64,
                    0,
                    0x2a00,
                    0x2908,
                    0
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), (units * 2) as u32);
        let mut bytes = vec![0u8; units * 2 + 2];
        fixture.memory.read_bytes(0x2a00, &mut bytes).unwrap();
        let (words, _) = bytes.as_chunks::<2>();
        let text = String::from_utf16(
            &words
                .iter()
                .copied()
                .map(u16::from_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(text, format!("{expected}\0"));
        server.join().unwrap();
    }

    #[test]
    fn custom_named_header_queries_match_case_insensitively_with_textual_sizing() {
        let (port, _captured, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Server: fixture\r\nX-MixedCase: Value-Here\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        write_wide(&mut fixture.memory, 0x2700, "x-mixedcase");
        // Sizing reports the required byte count including the terminator.
        fixture.memory.write_u32(0x2908, 0).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[request, WINHTTP_QUERY_CUSTOM as u64, 0x2700, 0, 0x2908, 0]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
        assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), 11 * 2);
        fixture.memory.write_u32(0x2908, 11 * 2).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    WINHTTP_QUERY_CUSTOM as u64,
                    0x2700,
                    0x2a00,
                    0x2908,
                    0
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), 10 * 2);
        let mut bytes = vec![0u8; 20];
        fixture.memory.read_bytes(0x2a00, &mut bytes).unwrap();
        let (words, _) = bytes.as_chunks::<2>();
        let text = String::from_utf16(
            &words
                .iter()
                .copied()
                .map(u16::from_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(text, "Value-Here");
        // A missing header reports the documented not-found error.
        write_wide(&mut fixture.memory, 0x2700, "X-Absent");
        fixture.memory.write_u32(0x2908, 64).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    WINHTTP_QUERY_CUSTOM as u64,
                    0x2700,
                    0x2a00,
                    0x2908,
                    0
                ]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_HEADER_NOT_FOUND);
        // Invalid names and misplaced arguments fail explicitly.
        for (name, level) in [
            ("", WINHTTP_QUERY_CUSTOM),
            ("bad name", WINHTTP_QUERY_CUSTOM),
            ("X-Ok", WINHTTP_QUERY_STATUS_CODE),
        ] {
            write_wide(&mut fixture.memory, 0x2700, name);
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[request, level as u64, 0x2700, 0x2a00, 0x2908, 0]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INVALID_PARAMETER);
        }
        server.join().unwrap();
    }

    #[test]
    fn absolute_redirects_are_followed_to_the_final_response() {
        // The absolute location needs the port before the responses exist, so
        // the listener is bound first and driven explicitly.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let first: &'static [u8] = Box::leak(
            format!(
                "HTTP/1.1 301 Moved\r\nContent-Length: 0\r\nLocation: http://127.0.0.1:{port}/final?x=1\r\n\r\n"
            )
            .into_bytes()
            .into_boxed_slice(),
        );
        let responses: &'static [&'static [u8]] = Box::leak(Box::new([
            first,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi" as &[u8],
        ]));
        let (captured, server) = serve_chain_on(listener, responses);
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        fixture.memory.write_u32(0x2908, 4).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    (WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER) as u64,
                    0,
                    0x2900,
                    0x2908,
                    0,
                ]
            ),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2900).unwrap(), 200);
        server.join().unwrap();
        let wires = captured.lock().unwrap().clone();
        assert_eq!(wires.len(), 2);
        assert!(String::from_utf8_lossy(&wires[0]).starts_with("GET /fixture HTTP/1.1\r\n"));
        assert!(String::from_utf8_lossy(&wires[1]).starts_with("GET /final?x=1 HTTP/1.1\r\n"));
    }

    #[test]
    fn a_302_redirect_rewrites_post_to_get_and_drops_the_body() {
        let (port, captured, server) = serve_chain(&[
            b"HTTP/1.1 302 Redirect\r\nContent-Length: 0\r\nLocation: /target\r\n\r\n",
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
        ]);
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);
        fixture.memory.write_bytes(0x2800, b"data").unwrap();
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0x2800, 4, 4, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        let wires = captured.lock().unwrap().clone();
        assert_eq!(wires.len(), 2);
        assert!(String::from_utf8_lossy(&wires[0]).starts_with("POST /fixture HTTP/1.1\r\n"));
        let second = String::from_utf8_lossy(&wires[1]).into_owned();
        assert!(
            second.starts_with("POST /target HTTP/1.1\r\n")
                || second.starts_with("GET /target HTTP/1.1\r\n")
        );
        assert!(
            second.starts_with("GET /target HTTP/1.1\r\n"),
            "POST rewrites to GET: {second}"
        );
        assert!(!second.contains("Content-Length"));
        assert!(!second.ends_with("data"));
    }

    #[test]
    fn a_307_redirect_preserves_the_verb_and_replays_the_body() {
        let (port, captured, server) = serve_chain(&[
            b"HTTP/1.1 307 Keep Verb\r\nContent-Length: 0\r\nLocation: /target\r\n\r\n",
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
        ]);
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);
        fixture.memory.write_bytes(0x2800, b"data").unwrap();
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0x2800, 4, 4, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        let wires = captured.lock().unwrap().clone();
        assert_eq!(wires.len(), 2);
        let second = String::from_utf8_lossy(&wires[1]).into_owned();
        assert!(second.starts_with("POST /target HTTP/1.1\r\n"), "{second}");
        assert!(second.contains("Content-Length: 4\r\n"));
        assert!(wires[1].ends_with(b"data"));
    }

    #[test]
    fn redirect_loops_https_targets_and_missing_locations_fail_honestly() {
        // A missing Location header on a redirect status fails the request.
        let (port, _captured, server) =
            serve_chain(&[b"HTTP/1.1 301 Moved\r\nContent-Length: 0\r\n\r\n"]);
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_HEADER_NOT_FOUND);
        server.join().unwrap();
        // An HTTPS target cannot be followed because TLS is unsupported.
        let (port, _captured, server) = serve_chain(&[
            b"HTTP/1.1 301 Moved\r\nContent-Length: 0\r\nLocation: https://example.test/\r\n\r\n",
        ]);
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_REDIRECT_FAILED);
        server.join().unwrap();
    }

    #[test]
    fn parent_relative_redirects_resolve_against_the_current_path() {
        let (port, captured, server) = serve_chain(&[
            b"HTTP/1.1 302 Redirect\r\nContent-Length: 0\r\nLocation: sibling?q=2\r\n\r\n",
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
        ]);
        let mut fixture = Fixture::new();
        write_loopback_host(&mut fixture);
        let session = fixture.open_session();
        let connection = fixture.call("WinHttpConnect", &[session, 0x2100, port as u64, 0]);
        assert_ne!(connection, 0);
        write_wide(&mut fixture.memory, 0x2300, "/dir/page");
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0x2300, 0, 0, 0, 0]);
        assert_ne!(request, 0);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        let wires = captured.lock().unwrap().clone();
        assert_eq!(wires.len(), 2);
        assert!(
            String::from_utf8_lossy(&wires[1]).starts_with("GET /dir/sibling?q=2 HTTP/1.1\r\n"),
            "{}",
            String::from_utf8_lossy(&wires[1])
        );
    }

    #[test]
    fn transport_state_is_enforced_before_network_use() {
        let mut fixture = Fixture::new();
        let (_session, connection) = fixture.open_connection();
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a00, 1, 0x2914]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        fixture.memory.write_u32(0x2908, 4).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    (WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER) as u64,
                    0,
                    0x2900,
                    0x2908,
                    0,
                ]
            ),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
    }

    #[test]
    fn chunked_response_is_decoded_incrementally() {
        let (port, _captured, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;extension=yes\r\n WORLD\r\n0\r\nX-Checksum: yes\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        fixture.memory.write_u32(0x2958, 4).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    (WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER) as u64,
                    0,
                    0x3020,
                    0x2958,
                    0,
                ]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_HEADER_NOT_FOUND);
        assert_eq!(
            fixture.call("WinHttpQueryDataAvailable", &[request, 0x2910]),
            1
        );
        assert!(fixture.memory.read_u32(0x2910).unwrap() > 0);
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a00, 4, 0x2914]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2914).unwrap(), 4);
        let mut first = [0; 4];
        fixture.memory.read_bytes(0x2a00, &mut first).unwrap();
        assert_eq!(&first, b"hell");
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a10, 32, 0x2914]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2914).unwrap(), 7);
        let mut rest = [0; 7];
        fixture.memory.read_bytes(0x2a10, &mut rest).unwrap();
        assert_eq!(&rest, b"o WORLD");
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a20, 8, 0x2914]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x2914).unwrap(), 0);
        server.join().unwrap();
    }

    #[test]
    fn malformed_chunked_response_fails_during_body_read() {
        let (port, _captured, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nnot-hex\r\nhello\r\n0\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        assert_eq!(
            fixture.call("WinHttpReadData", &[request, 0x2a00, 8, 0x2914]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INVALID_SERVER_RESPONSE
        );
        server.join().unwrap();
    }

    #[test]
    fn malformed_response_fails_without_reporting_a_status() {
        let (port, _captured, server) = serve_once(b"this is not HTTP\r\n\r\n");
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INVALID_SERVER_RESPONSE
        );
        server.join().unwrap();
    }

    #[test]
    fn connection_failure_is_reported_without_a_response_state() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_CANNOT_CONNECT);
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
    }

    #[test]
    fn query_headers_converts_documented_date_headers_to_system_time() {
        let (port, _, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nDate: Sun, 06 Nov 1994 08:49:37 GMT\r\nExpires: Sunday, 06-Nov-94 08:49:37 GMT\r\nLast-Modified: Sun Nov  6 08:49:37 1994\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();

        for (level, expected) in [
            (WINHTTP_QUERY_DATE, [1994, 11, 0, 6, 8, 49, 37, 0]),
            (WINHTTP_QUERY_EXPIRES, [1994, 11, 0, 6, 8, 49, 37, 0]),
            (WINHTTP_QUERY_LAST_MODIFIED, [1994, 11, 0, 6, 8, 49, 37, 0]),
        ] {
            fixture.memory.write_u32(0x2908, 0).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[
                        request,
                        (level | WINHTTP_QUERY_FLAG_SYSTEMTIME) as u64,
                        0,
                        0,
                        0x2908,
                        0,
                    ]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), 16);

            fixture.memory.write_u32(0x2908, 15).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[
                        request,
                        (level | WINHTTP_QUERY_FLAG_SYSTEMTIME) as u64,
                        0,
                        0x2a00,
                        0x2908,
                        0,
                    ]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), 16);

            fixture.memory.write_bytes(0x2a00, &[0xaa; 16]).unwrap();
            fixture.memory.write_u32(0x2908, 16).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[
                        request,
                        (level | WINHTTP_QUERY_FLAG_SYSTEMTIME) as u64,
                        0,
                        0x2a00,
                        0x2908,
                        0,
                    ]
                ),
                1
            );
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), 16);
            assert_eq!(read_test_system_time(&fixture.memory, 0x2a00), expected);
        }
    }

    #[test]
    fn query_headers_system_time_reports_missing_and_malformed_dates() {
        for (response, level, error) in [
            (
                &b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"[..],
                WINHTTP_QUERY_DATE,
                ERROR_WINHTTP_HEADER_NOT_FOUND,
            ),
            (
                &b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nDate: not-a-date\r\n\r\n"[..],
                WINHTTP_QUERY_DATE,
                ERROR_WINHTTP_INVALID_SERVER_RESPONSE,
            ),
            (
                &b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nExpires: Sun, 29 Feb 2023 08:49:37 GMT\r\n\r\n"[..],
                WINHTTP_QUERY_EXPIRES,
                ERROR_WINHTTP_INVALID_SERVER_RESPONSE,
            ),
        ] {
            let (port, _, server) = serve_once(response);
            let mut fixture = Fixture::new();
            let request = open_loopback_request(&mut fixture, port);
            assert_eq!(
                fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
                1
            );
            assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
            server.join().unwrap();
            fixture.memory.write_u32(0x2908, 16).unwrap();
            fixture.memory.write_bytes(0x2a00, &[0xaa; 16]).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[
                        request,
                        (level | WINHTTP_QUERY_FLAG_SYSTEMTIME) as u64,
                        0,
                        0x2a00,
                        0x2908,
                        0,
                    ]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), error);
            assert_eq!(read_test_system_time(&fixture.memory, 0x2a00), [0xaaaa; 8]);
        }
    }

    #[test]
    fn query_headers_reads_documented_date_headers_as_text() {
        let (port, _, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nDate: Sun, 06 Nov 1994 08:49:37 GMT\r\nExpires: Sunday, 06-Nov-94 08:49:37 GMT\r\nLast-Modified: Sun Nov  6 08:49:37 1994\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();

        for (level, text) in [
            (WINHTTP_QUERY_DATE, "Sun, 06 Nov 1994 08:49:37 GMT"),
            (WINHTTP_QUERY_EXPIRES, "Sunday, 06-Nov-94 08:49:37 GMT"),
            (WINHTTP_QUERY_LAST_MODIFIED, "Sun Nov  6 08:49:37 1994"),
        ] {
            let required = ((text.len() + 1) * 2) as u32;
            let success_len = (text.len() * 2) as u32;
            fixture.memory.write_u32(0x2908, 0).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[request, u64::from(level), 0, 0, 0x2908, 0]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), required);

            fixture.memory.write_u32(0x2908, required - 1).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[request, u64::from(level), 0, 0x2a00, 0x2908, 0]
                ),
                0
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INSUFFICIENT_BUFFER);
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), required);

            fixture.memory.write_bytes(0x2a00, &[0xaa; 128]).unwrap();
            fixture.memory.write_u32(0x2908, required).unwrap();
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[request, u64::from(level), 0, 0x2a00, 0x2908, 0]
                ),
                1
            );
            assert_eq!(fixture.memory.read_u32(0x2908).unwrap(), success_len);
            assert_eq!(read_test_wide(&fixture.memory, 0x2a00, success_len), text);
            assert_eq!(
                fixture
                    .memory
                    .read_u16(0x2a00 + u64::from(success_len))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn query_headers_textual_date_reports_missing_header_and_incomplete_response() {
        let (port, _, server) = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        fixture.memory.write_u32(0x2908, 128).unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[request, u64::from(WINHTTP_QUERY_DATE), 0, 0x2a00, 0x2908, 0,]
            ),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[request, u64::from(WINHTTP_QUERY_DATE), 0, 0x2a00, 0x2908, 0,]
            ),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_WINHTTP_HEADER_NOT_FOUND);
    }

    #[test]
    fn query_headers_system_time_rejects_unsupported_forms_and_incomplete_responses() {
        let mut fixture = Fixture::new();
        let (_session, connection) = fixture.open_connection();
        let request = fixture.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
        fixture.memory.write_u32(0x2908, 16).unwrap();
        for level in [
            WINHTTP_QUERY_CONTENT_TYPE | WINHTTP_QUERY_FLAG_SYSTEMTIME,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_SYSTEMTIME,
            WINHTTP_QUERY_DATE | WINHTTP_QUERY_FLAG_SYSTEMTIME | WINHTTP_QUERY_FLAG_NUMBER,
        ] {
            assert_eq!(
                fixture.call(
                    "WinHttpQueryHeaders",
                    &[request, u64::from(level), 0, 0x2a00, 0x2908, 0]
                ),
                0,
                "{level:#x}"
            );
            assert_eq!(fixture.state.last_error(0), ERROR_NOT_SUPPORTED);
        }
        assert_eq!(
            fixture.call(
                "WinHttpQueryHeaders",
                &[
                    request,
                    (WINHTTP_QUERY_DATE | WINHTTP_QUERY_FLAG_SYSTEMTIME) as u64,
                    0,
                    0x2a00,
                    0x2908,
                    0,
                ]
            ),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
    }

    #[test]
    fn query_headers_date_output_invalid_memory_is_a_memory_fault() {
        let (port, _, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nDate: Sun, 06 Nov 1994 08:49:37 GMT\r\n\r\n",
        );
        let mut fixture = Fixture::new();
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0, 0, 0, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        fixture.memory.write_u32(0x2908, 128).unwrap();

        for (level, output, length_ptr) in [
            (
                WINHTTP_QUERY_DATE | WINHTTP_QUERY_FLAG_SYSTEMTIME,
                0x9000,
                0x2908,
            ),
            (
                WINHTTP_QUERY_DATE | WINHTTP_QUERY_FLAG_SYSTEMTIME,
                0x2a00,
                0x9000,
            ),
            (WINHTTP_QUERY_DATE, 0x9000, 0x2908),
        ] {
            fixture.memory.write_u32(0x2908, 128).unwrap();
            let mut cpu = CpuState::default();
            cpu.set(Register::Rcx, request);
            cpu.set(Register::Rdx, u64::from(level));
            cpu.set(Register::R9, output);
            cpu.set(Register::Rsp, 0x7000);
            fixture.memory.write_u64(0x7028, length_ptr).unwrap();
            fixture.memory.write_u64(0x7030, 0).unwrap();
            let descriptor = fixture
                .registry
                .resolve(MODULE, &ImportName::Name("WinHttpQueryHeaders".to_owned()))
                .unwrap();
            let result = (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut fixture.memory);
            assert!(
                matches!(result, Err(ExitReason::MemoryFault(_))),
                "{result:?}"
            );
        }
    }

    #[test]
    fn write_data_streams_the_declared_body_before_receiving() {
        let (port, captured, server) = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);
        fixture.memory.write_bytes(0x3100, b"0123").unwrap();
        fixture.memory.write_bytes(0x3200, b"456").unwrap();
        fixture.memory.write_bytes(0x3300, b"789A").unwrap();
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0x3100, 4, 11, 0]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );

        fixture.memory.write_u32(0x3400, 99).unwrap();
        assert_eq!(
            fixture.call("WinHttpWriteData", &[request, 0, 0, 0x3400]),
            1
        );
        assert_eq!(fixture.memory.read_u32(0x3400).unwrap(), 0);
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 0);
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );

        for (address, length) in [(0x3200, 3), (0x3300, 4)] {
            fixture.memory.write_u32(0x3400, 99).unwrap();
            assert_eq!(
                fixture.call("WinHttpWriteData", &[request, address, length, 0x3400]),
                1
            );
            assert_eq!(u64::from(fixture.memory.read_u32(0x3400).unwrap()), length);
        }
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        server.join().unwrap();
        let captured = captured.lock().unwrap();
        let request_text = String::from_utf8_lossy(&captured);
        assert!(
            request_text.contains("Content-Length: 11\r\n"),
            "{request_text}"
        );
        assert!(request_text.ends_with("0123456789A"), "{request_text}");
    }

    #[test]
    fn write_data_rejects_bad_state_memory_and_lengths_without_consuming_budget() {
        let (port, _captured, server) = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut fixture = Fixture::new();
        write_wide(&mut fixture.memory, 0x2200, "POST");
        let request = open_loopback_request(&mut fixture, port);
        assert_eq!(
            fixture.call("WinHttpWriteData", &[request, 0x3200, 1, 0x3400]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        fixture.memory.write_bytes(0x3200, b"abcd").unwrap();
        assert_eq!(
            fixture.call("WinHttpSendRequest", &[request, 0, 0, 0x3200, 1, 4, 0]),
            1
        );

        fixture.memory.write_u32(0x3400, 99).unwrap();
        for (args, error) in [
            ([request, 0x3200, 4, 0x3400], ERROR_INVALID_PARAMETER),
            ([request, 0x9000, 1, 0x3400], ERROR_INVALID_PARAMETER),
            ([request, 0x3200, 1, 0], ERROR_INVALID_PARAMETER),
            (
                [
                    request,
                    0x3200,
                    u64::from(MAX_OPTIONAL_BODY_BYTES) + 1,
                    0x3400,
                ],
                ERROR_OUTOFMEMORY,
            ),
        ] {
            assert_eq!(fixture.call("WinHttpWriteData", &args), 0, "{args:?}");
            assert_eq!(fixture.state.last_error(0), error);
        }
        assert_eq!(fixture.memory.read_u32(0x3400).unwrap(), 0);
        assert_eq!(
            fixture.call("WinHttpWriteData", &[9999, 0x3200, 1, 0x3400]),
            0
        );
        assert_eq!(fixture.state.last_error(0), ERROR_INVALID_HANDLE);

        fixture.memory.write_bytes(0x3300, b"bcd").unwrap();
        assert_eq!(
            fixture.call("WinHttpWriteData", &[request, 0x3300, 3, 0x3400]),
            1
        );
        assert_eq!(fixture.call("WinHttpReceiveResponse", &[request, 0]), 1);
        assert_eq!(
            fixture.call("WinHttpWriteData", &[request, 0x3300, 1, 0x3400]),
            0
        );
        assert_eq!(
            fixture.state.last_error(0),
            ERROR_WINHTTP_INCORRECT_HANDLE_STATE
        );
        server.join().unwrap();
    }

    #[test]
    fn plaintext_secure_requests_are_refused_before_transport() {
        let mut fixture = Fixture::new();
        write_loopback_host(&mut fixture);
        let session = fixture.open_session();
        let connection = fixture.call("WinHttpConnect", &[session, 0x2100, 443, 0]);
        let request = fixture.call(
            "WinHttpOpenRequest",
            &[connection, 0, 0, 0, 0, 0, WINHTTP_FLAG_SECURE as u64],
        );
        for (handle, expected) in [
            (request, ERROR_NOT_SUPPORTED),
            (session, ERROR_WINHTTP_INCORRECT_HANDLE_TYPE),
            (0, ERROR_INVALID_HANDLE),
        ] {
            let descriptor = fixture
                .registry
                .resolve(MODULE, &ImportName::Name("WinHttpSendRequest".to_owned()))
                .unwrap();
            let args = Win64Args {
                rcx: handle,
                rdx: 0x9000,
                r8: 4,
                r9: 0,
                rsp: 0x9000,
            };
            assert_eq!(
                (descriptor.handler)(&args, &mut CpuState::default(), &mut fixture.memory),
                Ok(0)
            );
            assert_eq!(fixture.state.last_error(0), expected);
        }
    }

    #[test]
    fn concurrent_creation_is_allowed_on_the_same_parent_handles() {
        use std::sync::{Arc, Barrier};

        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut fixture = Fixture {
            registry: ImportRegistry::new(),
            state: Arc::clone(&state),
            memory: GuestMemory::new(),
        };
        register(&mut fixture.registry, &fixture.state);
        fixture
            .memory
            .map_region(0x1000, 0x8000, MemoryPermissions::READ_WRITE)
            .unwrap();
        write_wide(&mut fixture.memory, 0x2000, "agent");
        write_wide(&mut fixture.memory, 0x2100, "example.test");
        let session = fixture.open_session();
        let connection = fixture.call("WinHttpConnect", &[session, 0x2100, 80, 0]);
        assert_ne!(session, 0);
        assert_ne!(connection, 0);

        let barrier = Arc::new(Barrier::new(8));
        let mut threads = Vec::new();
        for use_connection in [false, true] {
            for _ in 0..4 {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                threads.push(std::thread::spawn(move || {
                    let mut registry = ImportRegistry::new();
                    register(&mut registry, &state);
                    let mut memory = GuestMemory::new();
                    memory
                        .map_region(0x1000, 0x8000, MemoryPermissions::READ_WRITE)
                        .unwrap();
                    write_wide(&mut memory, 0x2100, "example.test");
                    barrier.wait();
                    let parent = if use_connection { connection } else { session };
                    let symbol = if use_connection {
                        "WinHttpOpenRequest"
                    } else {
                        "WinHttpConnect"
                    };
                    let arguments = if use_connection {
                        [parent, 0, 0, 0, 0, 0, 0]
                    } else {
                        [parent, 0x2100, 80, 0, 0, 0, 0]
                    };
                    let mut cpu = CpuState::default();
                    for (register, value) in
                        [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
                            .into_iter()
                            .zip(arguments.iter().copied())
                    {
                        cpu.set(register, value);
                    }
                    cpu.set(Register::Rsp, 0x7000);
                    for (index, value) in arguments.iter().copied().enumerate().skip(4) {
                        memory
                            .write_u64(0x7000 + 0x28 + (index as u64 - 4) * 8, value)
                            .unwrap();
                    }
                    let descriptor = registry
                        .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
                        .unwrap();
                    let handle =
                        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap();
                    assert_ne!(handle, 0);
                }));
            }
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(state.open_handle_count(), 10);
    }

    #[test]
    fn concurrent_services_share_one_thread_safe_handle_table() {
        use std::sync::{Arc, Barrier};

        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let barrier = Arc::new(Barrier::new(4));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let state = Arc::clone(&state);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let mut fixture = Fixture {
                    registry: ImportRegistry::new(),
                    state,
                    memory: GuestMemory::new(),
                };
                register(&mut fixture.registry, &fixture.state);
                fixture
                    .memory
                    .map_region(0x1000, 0x8000, MemoryPermissions::READ_WRITE)
                    .unwrap();
                write_wide(&mut fixture.memory, 0x2000, "agent");
                write_wide(&mut fixture.memory, 0x2100, "example.test");
                barrier.wait();
                let session = fixture.open_session();
                let connection = fixture.call("WinHttpConnect", &[session, 0x2100, 80, 0]);
                assert_ne!(session, 0);
                assert_ne!(connection, 0);
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(state.open_handle_count(), 8);
        let handles = state.winhttp.lock().unwrap();
        assert_eq!(handles.open_count(), 8);
    }

    #[test]
    fn http_time_to_system_time_parses_all_three_documented_forms() {
        let mut fixture = Fixture::new();
        for (text, expected) in [
            (
                "Sun, 06 Nov 1994 08:49:37 GMT",
                [1994, 11, 0, 6, 8, 49, 37, 0],
            ),
            (
                "Sunday, 06-Nov-94 08:49:37 GMT",
                [1994, 11, 0, 6, 8, 49, 37, 0],
            ),
            ("Sun Nov  6 08:49:37 1994", [1994, 11, 0, 6, 8, 49, 37, 0]),
            ("Sun Nov 06 08:49:37 1994", [1994, 11, 0, 6, 8, 49, 37, 0]),
            (
                "Mon, 06 Nov 1994 08:49:37 GMT",
                [1994, 11, 0, 6, 8, 49, 37, 0],
            ),
            (
                "Thu, 29 Feb 2024 00:00:00 GMT",
                [2024, 2, 4, 29, 0, 0, 0, 0],
            ),
        ] {
            write_wide(&mut fixture.memory, 0x5000, text);
            fixture.memory.write_bytes(0x5100, &[0xaa; 16]).unwrap();
            assert_eq!(
                fixture.call("WinHttpTimeToSystemTime", &[0x5000, 0x5100]),
                1,
                "{text}"
            );
            assert_eq!(read_test_system_time(&fixture.memory, 0x5100), expected);
        }
    }

    #[test]
    fn http_time_to_system_time_rolls_the_rfc850_year_window() {
        let mut fixture = Fixture::new();
        for (text, year) in [
            ("Sunday, 06-Nov-76 08:49:37 GMT", 2076),
            ("Sunday, 06-Nov-77 08:49:37 GMT", 1977),
        ] {
            write_wide(&mut fixture.memory, 0x5000, text);
            assert_eq!(
                fixture.call("WinHttpTimeToSystemTime", &[0x5000, 0x5100]),
                1,
                "{text}"
            );
            assert_eq!(read_test_system_time(&fixture.memory, 0x5100)[0], year);
        }
    }

    #[test]
    fn http_time_to_system_time_rejects_malformed_dates_and_bad_memory() {
        let mut fixture = Fixture::new();
        for text in [
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "sun, 06 Nov 1994 08:49:37 GMT",
            "Sun, 29 Feb 2023 08:49:37 GMT",
            "Sun, 06 Nov 1994 24:00:00 GMT",
            "Sun Nov 6 08:49:37 1994",
            "Sunday, 06-Nov-1994 08:49:37 GMT",
            "",
        ] {
            write_wide(&mut fixture.memory, 0x5000, text);
            fixture.memory.write_bytes(0x5100, &[0xaa; 16]).unwrap();
            assert_eq!(
                fixture.call("WinHttpTimeToSystemTime", &[0x5000, 0x5100]),
                0,
                "{text}"
            );
            assert_eq!(read_test_system_time(&fixture.memory, 0x5100), [0xaaaa; 8]);
            assert_eq!(fixture.state.last_error(0), ERROR_INVALID_PARAMETER);
        }
        assert_eq!(fixture.call("WinHttpTimeToSystemTime", &[0, 0x5100]), 0);
        assert_eq!(fixture.call("WinHttpTimeToSystemTime", &[0x5000, 0]), 0);
        assert_eq!(
            fixture.call("WinHttpTimeToSystemTime", &[0x5000, 0x9000]),
            0
        );
    }

    #[test]
    fn http_time_from_system_time_formats_rfc1123_and_round_trips() {
        let mut fixture = Fixture::new();
        // The input weekday is deliberately wrong: the calendar date determines it.
        write_test_system_time(
            &mut fixture.memory,
            0x5100,
            [1994, 11, 6, 6, 8, 49, 37, 123],
        );
        assert_eq!(
            fixture.call("WinHttpTimeFromSystemTime", &[0x5100, 0x5000]),
            1
        );
        assert_eq!(
            read_test_wide(&fixture.memory, 0x5000, 58),
            "Sun, 06 Nov 1994 08:49:37 GMT"
        );
        assert_eq!(fixture.memory.read_u16(0x5000 + 58).unwrap(), 0);

        assert_eq!(
            fixture.call("WinHttpTimeToSystemTime", &[0x5000, 0x5200]),
            1
        );
        assert_eq!(
            read_test_system_time(&fixture.memory, 0x5200),
            [1994, 11, 0, 6, 8, 49, 37, 0]
        );
    }

    #[test]
    fn http_time_from_system_time_rejects_invalid_times_and_bad_memory() {
        let mut fixture = Fixture::new();
        for fields in [
            [2023, 2, 0, 29, 8, 49, 37, 0],
            [1994, 11, 0, 6, 24, 0, 0, 0],
            [1994, 11, 0, 6, 8, 60, 0, 0],
            [1994, 11, 0, 6, 8, 0, 60, 0],
            [1994, 11, 0, 6, 8, 0, 0, 1000],
            [0, 11, 0, 6, 8, 0, 0, 0],
        ] {
            write_test_system_time(&mut fixture.memory, 0x5100, fields);
            assert_eq!(
                fixture.call("WinHttpTimeFromSystemTime", &[0x5100, 0x5000]),
                0,
                "{fields:?}"
            );
            assert_eq!(fixture.state.last_error(0), ERROR_INVALID_PARAMETER);
        }
        assert_eq!(fixture.call("WinHttpTimeFromSystemTime", &[0, 0x5000]), 0);
        assert_eq!(fixture.call("WinHttpTimeFromSystemTime", &[0x5100, 0]), 0);
        assert_eq!(
            fixture.call("WinHttpTimeFromSystemTime", &[0x5100, 0x9000]),
            0
        );
    }
}
