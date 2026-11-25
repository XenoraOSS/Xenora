// SPDX-License-Identifier: GPL-3.0-or-later

//! Process-local Winsock loopback for the synthetic guest: UDP datagrams and
//! TCP streams between sockets of the same guest on 127.0.0.1. No host socket
//! is ever opened, so guest code cannot reach the host network.

use super::{ERROR_INVALID_PARAMETER, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::GuestMemory;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MODULE: &str = "ws2_32.dll";
const VERSION: u16 = 0x0202;
const WSADATA_SIZE: usize = 408;
const MAX_DATAGRAM: usize = 65_507;
const MAX_PENDING_DATAGRAMS: usize = 16;
const MAX_SOCKETS: usize = 64;
/// Bytes a stream socket buffers before senders see `WSAEWOULDBLOCK`.
const STREAM_BUFFER: usize = 64 * 1024;
/// Largest number of connections waiting in one listen queue.
const MAX_BACKLOG: usize = 16;
const SOCKET_ERROR: u64 = u32::MAX as u64;
const INVALID_SOCKET: u64 = u64::MAX;
const WSAEACCES: u32 = 10013;
const WSAEINVAL: u32 = 10022;
const WSAEWOULDBLOCK: u32 = 10035;
const WSAEMSGSIZE: u32 = 10040;
const WSAEADDRINUSE: u32 = 10048;
const WSAENOBUFS: u32 = 10055;
const WSAENOTCONN: u32 = 10057;
const WSAEFAULT: u32 = 10014;
const WSAEMFILE: u32 = 10024;
const WSAENOTSOCK: u32 = 10038;
const WSAEPROTONOSUPPORT: u32 = 10043;
const WSAESOCKTNOSUPPORT: u32 = 10044;
const WSAEOPNOTSUPP: u32 = 10045;
const WSAENOPROTOOPT: u32 = 10042;
const WSAEAFNOSUPPORT: u32 = 10047;
const WSASYSNOTREADY: u32 = 10091;
const WSAVERNOTSUPPORTED: u32 = 10092;
const WSANOTINITIALISED: u32 = 10093;
const WSAECONNRESET: u32 = 10054;
const WSAEISCONN: u32 = 10056;
const WSAESHUTDOWN: u32 = 10058;
const WSAECONNREFUSED: u32 = 10061;

#[derive(Default)]
pub(super) struct Winsock {
    refs: u32,
    serial: u32,
    sockets: BTreeMap<u64, Socket>,
    #[cfg(test)]
    poll_waiters: usize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    Datagram,
    Stream,
}

#[derive(Default)]
struct Socket {
    kind: Kind,
    port: Option<u16>,
    peer: Option<u16>,
    pending: VecDeque<(u16, Vec<u8>)>,
    stream: Stream,
}

/// Connection state of a stream socket.
#[derive(Default)]
struct Stream {
    /// Handles of connections waiting for `accept`, when listening.
    listen: Option<(usize, VecDeque<u64>)>,
    /// The other end of the connection while it is open.
    link: Option<u64>,
    /// Created by `connect` on a listener's port; shares that port.
    accepted: bool,
    inbound: VecDeque<u8>,
    /// The other end will send nothing more.
    peer_closed: bool,
    /// The other end was closed, so sending fails.
    peer_gone: bool,
    send_closed: bool,
    receive_closed: bool,
}

impl Winsock {
    /// Sockets the guest has open.
    pub(super) fn open_count(&self) -> usize {
        self.sockets.len()
    }

    /// Process exit: every socket is closed.
    pub(super) fn close_all(&mut self) {
        self.sockets.clear();
    }

    fn socket(&self, handle: u64) -> Result<&Socket, u32> {
        if self.refs == 0 {
            return Err(WSANOTINITIALISED);
        }
        self.sockets.get(&handle).ok_or(WSAENOTSOCK)
    }

    /// A local port for a socket of `kind`. TCP and UDP ports are separate,
    /// and accepted connections share their listener's port.
    fn port(&self, requested: u16, kind: Kind) -> Result<u16, u32> {
        let available = |port| {
            self.sockets.values().all(|socket| {
                socket.kind != kind || socket.stream.accepted || socket.port != Some(port)
            })
        };
        if requested != 0 {
            return available(requested)
                .then_some(requested)
                .ok_or(WSAEADDRINUSE);
        }
        (49152..=65535)
            .find(|&port| available(port))
            .ok_or(WSAEADDRINUSE)
    }

    fn allocate(&mut self, socket: Socket) -> Result<u64, u32> {
        if self.sockets.len() >= MAX_SOCKETS {
            return Err(WSAEMFILE);
        }
        let next = self.serial.checked_add(1).ok_or(WSAEMFILE)?;
        self.serial = next;
        let handle = 0x5300_0000_0000_0000 | u64::from(next);
        self.sockets.insert(handle, socket);
        Ok(handle)
    }

    /// The datagram socket a datagram from `source` to `port` reaches.
    fn datagram_target(&mut self, port: u16, source: u16) -> Option<&mut Socket> {
        self.sockets.values_mut().find(|socket| {
            socket.kind == Kind::Datagram
                && socket.port == Some(port)
                && socket.peer.is_none_or(|peer| peer == source)
        })
    }

    /// Close `handle`. The other end of a stream sees end of file, and
    /// connections still waiting in a listen queue are closed too.
    fn close(&mut self, handle: u64) -> bool {
        let Some(socket) = self.sockets.remove(&handle) else {
            return false;
        };
        let mut links: Vec<u64> = socket.stream.link.into_iter().collect();
        for waiting in socket
            .stream
            .listen
            .into_iter()
            .flat_map(|(_, queue)| queue)
        {
            if let Some(waiting) = self.sockets.remove(&waiting) {
                links.extend(waiting.stream.link);
            }
        }
        for link in links {
            if let Some(other) = self.sockets.get_mut(&link) {
                other.stream.link = None;
                other.stream.peer_closed = true;
                other.stream.peer_gone = true;
            }
        }
        true
    }

    fn connect_stream(&mut self, handle: u64, port: u16) -> Result<(), u32> {
        let socket = self.socket(handle)?;
        if socket.stream.listen.is_some() {
            return Err(WSAEINVAL);
        }
        if socket.peer.is_some() {
            return Err(WSAEISCONN);
        }
        let existing = socket.port;
        let listener = self
            .sockets
            .iter()
            .find(|(_, socket)| {
                socket.kind == Kind::Stream
                    && socket.port == Some(port)
                    && socket.stream.listen.is_some()
            })
            .map(|(&listener, socket)| (listener, socket.stream.listen.as_ref()))
            .and_then(|(listener, listen)| {
                listen.map(|(backlog, queue)| (listener, *backlog, queue.len()))
            });
        let Some((listener, backlog, waiting)) = listener else {
            return Err(WSAECONNREFUSED);
        };
        if waiting >= backlog {
            return Err(WSAECONNREFUSED);
        }
        let local = match existing {
            Some(local) => local,
            None => self.port(0, Kind::Stream)?,
        };
        let accepted = self.allocate(Socket {
            kind: Kind::Stream,
            port: Some(port),
            peer: Some(local),
            stream: Stream {
                link: Some(handle),
                accepted: true,
                ..Stream::default()
            },
            ..Socket::default()
        })?;
        let socket = self.sockets.get_mut(&handle).expect("validated socket");
        socket.port = Some(local);
        socket.peer = Some(port);
        socket.stream.link = Some(accepted);
        let (_, queue) = self
            .sockets
            .get_mut(&listener)
            .and_then(|listener| listener.stream.listen.as_mut())
            .expect("listener found above");
        queue.push_back(accepted);
        Ok(())
    }

    /// How many of `length` bytes a stream send may take now, and where they
    /// go. Checked before any guest memory is read.
    fn stream_room(&self, handle: u64, length: usize) -> Result<(u64, usize), u32> {
        let socket = self.socket(handle)?;
        if socket.stream.send_closed {
            return Err(WSAESHUTDOWN);
        }
        if socket.stream.peer_gone {
            return Err(WSAECONNRESET);
        }
        let link = socket.stream.link.ok_or(WSAENOTCONN)?;
        let target = &self.sockets[&link];
        if target.stream.receive_closed {
            return Err(WSAECONNRESET);
        }
        let room = STREAM_BUFFER - target.stream.inbound.len();
        if room == 0 && length != 0 {
            return Err(WSAEWOULDBLOCK);
        }
        Ok((link, length.min(room)))
    }
}

fn loopback_port(memory: &GuestMemory, address: u64, size: u32) -> Result<u16, u32> {
    let mut data = [0; 16];
    if (size as i32) < 16 || address == 0 || memory.read_bytes(address, &mut data).is_err() {
        return Err(WSAEFAULT);
    }
    if u16::from_le_bytes([data[0], data[1]]) != 2 {
        return Err(WSAEAFNOSUPPORT);
    }
    if data[4..8] != [127, 0, 0, 1] {
        return Err(WSAEACCES);
    }
    Ok(u16::from_be_bytes([data[2], data[3]]))
}

fn socket_result(
    state: &ServicesState,
    teb: u64,
    result: Result<u64, u32>,
) -> Result<u64, ExitReason> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => state.fail(teb, error, SOCKET_ERROR),
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let startup = Arc::clone(state);
    registry.register(MODULE, "WSAStartup", move |args, _, memory| {
        let requested = args.rcx as u16;
        let negotiated = match requested.to_le_bytes() {
            [2, minor] => u16::from_le_bytes([2, minor.min(2)]),
            _ => return Ok(u64::from(WSAVERNOTSUPPORTED)),
        };
        let mut net = startup.winsock.lock().expect("guest Winsock state");
        let Some(next) = net.refs.checked_add(1) else {
            return Ok(u64::from(WSASYSNOTREADY));
        };
        let mut data = [0; WSADATA_SIZE];
        data[..2].copy_from_slice(&negotiated.to_le_bytes());
        data[2..4].copy_from_slice(&VERSION.to_le_bytes());
        let description = b"Xenora synthetic Winsock";
        data[16..16 + description.len()].copy_from_slice(description);
        if args.rdx == 0 || memory.write_bytes(args.rdx, &data).is_err() {
            return Ok(u64::from(WSAEFAULT));
        }
        net.refs = next;
        Ok(0)
    });

    let cleanup = Arc::clone(state);
    registry.register(MODULE, "WSACleanup", move |_, cpu, _| {
        let mut net = cleanup.winsock.lock().expect("guest Winsock state");
        if net.refs == 0 {
            return cleanup.fail(cpu.gs_base, WSANOTINITIALISED, SOCKET_ERROR);
        }
        net.refs -= 1;
        if net.refs == 0 {
            net.sockets.clear();
            cleanup.winsock_changed.notify_all();
        }
        Ok(0)
    });

    let open = Arc::clone(state);
    registry.register(MODULE, "socket", move |args, cpu, _| {
        let mut net = open.winsock.lock().expect("guest Winsock state");
        let fail = |error| open.fail(cpu.gs_base, error, INVALID_SOCKET);
        if net.refs == 0 {
            return fail(WSANOTINITIALISED);
        }
        if args.rcx as u32 != 2 {
            return fail(WSAEAFNOSUPPORT);
        }
        let kind = match (args.rdx as u32, args.r8 as u32) {
            (2, 0 | 17) => Kind::Datagram,
            (1, 0 | 6) => Kind::Stream,
            (1 | 2, _) => return fail(WSAEPROTONOSUPPORT),
            _ => return fail(WSAESOCKTNOSUPPORT),
        };
        match net.allocate(Socket {
            kind,
            ..Socket::default()
        }) {
            Ok(socket) => Ok(socket),
            Err(error) => fail(error),
        }
    });

    let close = Arc::clone(state);
    registry.register(MODULE, "closesocket", move |args, cpu, _| {
        let mut net = close.winsock.lock().expect("guest Winsock state");
        if net.refs == 0 {
            return close.fail(cpu.gs_base, WSANOTINITIALISED, SOCKET_ERROR);
        }
        if !net.close(args.rcx) {
            return close.fail(cpu.gs_base, WSAENOTSOCK, SOCKET_ERROR);
        }
        close.winsock_changed.notify_all();
        Ok(0)
    });

    register_address_conversion(registry, state);
    register_loopback(registry, state);
    register_socket_names(registry, state);
    register_socket_options(registry, state);
    register_poll(registry, state);
    register_stream(registry, state);
    register_ioctl(registry, state);

    let get = Arc::clone(state);
    registry.register(MODULE, "WSAGetLastError", move |_, cpu, _| {
        Ok(u64::from(get.last_error(cpu.gs_base)))
    });
    let set = Arc::clone(state);
    registry.register(MODULE, "WSASetLastError", move |args, cpu, _| {
        set.set_last_error(cpu.gs_base, args.rcx as u32);
        Ok(0)
    });
    for symbol in ["WSAStartup", "WSACleanup", "socket", "closesocket"] {
        registry.set_status(MODULE, symbol, ServiceStatus::Partial);
    }
}

fn register_address_conversion(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for symbol in ["htons", "ntohs"] {
        registry.register(MODULE, symbol, |args, _, _| {
            Ok(u64::from((args.rcx as u16).swap_bytes()))
        });
    }
    for symbol in ["htonl", "ntohl"] {
        registry.register(MODULE, symbol, |args, _, _| {
            Ok(u64::from((args.rcx as u32).swap_bytes()))
        });
    }
    let format_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "inet_ntop",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let fail = |error| format_state.fail(cpu.gs_base, error, 0);
            if args.rcx as u32 != 2 {
                return fail(WSAEAFNOSUPPORT);
            }
            if args.r8 == 0 || args.r9 == 0 {
                return fail(ERROR_INVALID_PARAMETER);
            }
            let mut address = [0; 4];
            if args.rdx == 0 || memory.read_bytes(args.rdx, &mut address).is_err() {
                return fail(WSAEFAULT);
            }
            let mut text = std::net::Ipv4Addr::from(address).to_string().into_bytes();
            text.push(0);
            if (text.len() as u64) > args.r9 {
                return fail(ERROR_INVALID_PARAMETER);
            }
            if memory.write_bytes(args.r8, &text).is_err() {
                return fail(WSAEFAULT);
            }
            Ok(args.r8)
        },
    );
    let state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "inet_pton",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let fail = |error| state.fail(cpu.gs_base, error, SOCKET_ERROR);
            if args.rcx as u32 != 2 {
                return fail(WSAEAFNOSUPPORT);
            }
            if args.rdx == 0 || args.r8 == 0 {
                return fail(WSAEFAULT);
            }
            // The longest dotted decimal IPv4 spelling is 15 bytes plus NUL.
            let mut text = [0; 15];
            for len in 0..=text.len() {
                let Some(address) = args.rdx.checked_add(len as u64) else {
                    return fail(WSAEFAULT);
                };
                let Ok(byte) = memory.read_u8(address) else {
                    return fail(WSAEFAULT);
                };
                if byte == 0 {
                    let Some(ip) = std::str::from_utf8(&text[..len])
                        .ok()
                        .and_then(|text| text.parse::<std::net::Ipv4Addr>().ok())
                    else {
                        return Ok(0);
                    };
                    return if memory.write_bytes(args.r8, &ip.octets()).is_ok() {
                        Ok(1)
                    } else {
                        fail(WSAEFAULT)
                    };
                }
                if len == text.len() {
                    break;
                }
                text[len] = byte;
            }
            Ok(0)
        },
    );
}

fn register_loopback(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for (symbol, connect) in [("bind", false), ("connect", true)] {
        let state = Arc::clone(state);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let mut net = state.winsock.lock().expect("guest Winsock state");
                let result = (|| {
                    let socket = net.socket(args.rcx)?;
                    let (kind, existing) = (socket.kind, socket.port);
                    let requested = loopback_port(memory, args.rdx, args.r8 as u32)?;
                    if connect && requested == 0 || !connect && existing.is_some() {
                        return Err(WSAEINVAL);
                    }
                    if connect && kind == Kind::Stream {
                        net.connect_stream(args.rcx, requested)?;
                        state.winsock_changed.notify_all();
                        return Ok(0);
                    }
                    let local = match (connect, existing) {
                        (true, Some(port)) => port,
                        (true, None) => net.port(0, kind)?,
                        (false, _) => net.port(requested, kind)?,
                    };
                    let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                    socket.port = Some(local);
                    if connect {
                        socket.peer = Some(requested);
                        socket.pending.retain(|(source, _)| *source == requested);
                    }
                    state.winsock_changed.notify_all();
                    Ok(0)
                })();
                socket_result(&state, cpu.gs_base, result)
            },
        );
    }

    let send = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "send",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut net = send.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let socket = net.socket(args.rcx)?;
                let length = args.r8 as i32;
                if length < 0 || args.r9 as u32 != 0 {
                    return Err(WSAEINVAL);
                }
                if socket.kind == Kind::Stream {
                    // Read only what the peer can take, so a short send
                    // never touches guest bytes it does not report.
                    let (link, length) = net.stream_room(args.rcx, length as usize)?;
                    let mut data = vec![0; length];
                    if length != 0 {
                        memory
                            .read_bytes(args.rdx, &mut data)
                            .map_err(|_| WSAEFAULT)?;
                    }
                    let target = net.sockets.get_mut(&link).expect("linked socket exists");
                    target.stream.inbound.extend(data);
                    send.winsock_changed.notify_all();
                    return Ok(length as u64);
                }
                let peer = socket.peer.ok_or(WSAENOTCONN)?;
                let source = socket.port.expect("connected socket is bound");
                let length = length as usize;
                if length > MAX_DATAGRAM {
                    return Err(WSAEMSGSIZE);
                }
                let mut data = vec![0; length];
                if length != 0 {
                    memory
                        .read_bytes(args.rdx, &mut data)
                        .map_err(|_| WSAEFAULT)?;
                }
                if let Some(target) = net.datagram_target(peer, source) {
                    if target.pending.len() == MAX_PENDING_DATAGRAMS {
                        return Err(WSAENOBUFS);
                    }
                    target.pending.push_back((source, data));
                    send.winsock_changed.notify_all();
                }
                Ok(length as u64)
            })();
            socket_result(&send, cpu.gs_base, result)
        },
    );

    let recv = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "recv",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut net = recv.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let socket = net.socket(args.rcx)?;
                let length = args.r8 as i32;
                if socket.kind == Kind::Stream {
                    if length < 0 || args.r9 as u32 != 0 {
                        return Err(WSAEINVAL);
                    }
                    if socket.peer.is_none() {
                        return Err(WSAENOTCONN);
                    }
                    if socket.stream.receive_closed {
                        return Err(WSAESHUTDOWN);
                    }
                    let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                    let stream = &mut socket.stream;
                    if stream.inbound.is_empty() {
                        return if stream.peer_closed {
                            Ok(0)
                        } else {
                            Err(WSAEWOULDBLOCK)
                        };
                    }
                    let copied = (length as usize).min(stream.inbound.len());
                    let data: Vec<u8> = stream.inbound.range(..copied).copied().collect();
                    if copied != 0 {
                        memory.write_bytes(args.rdx, &data).map_err(|_| WSAEFAULT)?;
                    }
                    stream.inbound.drain(..copied);
                    recv.winsock_changed.notify_all();
                    return Ok(copied as u64);
                }
                if socket.port.is_none() {
                    return Err(WSAEINVAL);
                }
                if length < 0 || args.r9 as u32 != 0 {
                    return Err(WSAEINVAL);
                }
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                let (_, data) = socket.pending.front().ok_or(WSAEWOULDBLOCK)?;
                let copied = (length as usize).min(data.len());
                if copied != 0 {
                    memory
                        .write_bytes(args.rdx, &data[..copied])
                        .map_err(|_| WSAEFAULT)?;
                }
                let truncated = copied < data.len();
                socket.pending.pop_front();
                recv.winsock_changed.notify_all();
                if truncated {
                    return Err(WSAEMSGSIZE);
                }
                Ok(copied as u64)
            })();
            socket_result(&recv, cpu.gs_base, result)
        },
    );

    let sendto = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "sendto",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut net = sendto.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let length = args.r8 as i32;
                let flags = args.r9 as u32;
                let to = args.get(4, memory).map_err(|_| WSAEFAULT)?;
                let tolen = args.get(5, memory).map_err(|_| WSAEFAULT)? as u32;
                if length < 0 || flags != 0 {
                    return Err(WSAEINVAL);
                }
                if net.socket(args.rcx)?.kind == Kind::Stream {
                    return Err(WSAEOPNOTSUPP);
                }
                let peer = loopback_port(memory, to, tolen)?;
                let (bound, connected) = {
                    let socket = net.socket(args.rcx)?;
                    (socket.port, socket.peer)
                };
                if connected.is_some_and(|port| port != peer) {
                    return Err(WSAEINVAL);
                }
                let source = match bound {
                    Some(port) => port,
                    None => {
                        let local = net.port(0, Kind::Datagram)?;
                        net.sockets
                            .get_mut(&args.rcx)
                            .expect("validated socket")
                            .port = Some(local);
                        local
                    }
                };
                let length = length as usize;
                if length > MAX_DATAGRAM {
                    return Err(WSAEMSGSIZE);
                }
                let mut data = vec![0; length];
                if length != 0 {
                    memory
                        .read_bytes(args.rdx, &mut data)
                        .map_err(|_| WSAEFAULT)?;
                }
                if let Some(target) = net.datagram_target(peer, source) {
                    if target.pending.len() == MAX_PENDING_DATAGRAMS {
                        return Err(WSAENOBUFS);
                    }
                    target.pending.push_back((source, data));
                    sendto.winsock_changed.notify_all();
                }
                Ok(length as u64)
            })();
            socket_result(&sendto, cpu.gs_base, result)
        },
    );

    let recvfrom = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "recvfrom",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut net = recvfrom.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let socket = net.socket(args.rcx)?;
                if socket.kind == Kind::Stream {
                    return Err(WSAEOPNOTSUPP);
                }
                if socket.port.is_none() {
                    return Err(WSAEINVAL);
                }
                let length = args.r8 as i32;
                let flags = args.r9 as u32;
                let from = args.get(4, memory).map_err(|_| WSAEFAULT)?;
                let fromlen = args.get(5, memory).map_err(|_| WSAEFAULT)?;
                if length < 0 || flags != 0 {
                    return Err(WSAEINVAL);
                }
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                let (source, data) = socket.pending.front().ok_or(WSAEWOULDBLOCK)?;
                let copied = (length as usize).min(data.len());
                if copied != 0 {
                    memory
                        .write_bytes(args.rdx, &data[..copied])
                        .map_err(|_| WSAEFAULT)?;
                }
                let truncated = copied < data.len();
                let source = *source;
                socket.pending.pop_front();
                if from != 0 {
                    if fromlen == 0 {
                        return Err(WSAEFAULT);
                    }
                    let capacity = memory.read_u32(fromlen).map_err(|_| WSAEFAULT)? as i32;
                    let mut address = [0; 16];
                    if capacity < address.len() as i32 {
                        return Err(WSAEFAULT);
                    }
                    memory
                        .check_write(from, address.len())
                        .map_err(|_| WSAEFAULT)?;
                    memory.check_write(fromlen, 4).map_err(|_| WSAEFAULT)?;
                    address[..2].copy_from_slice(&2_u16.to_le_bytes());
                    address[2..4].copy_from_slice(&source.to_be_bytes());
                    address[4..8].copy_from_slice(&[127, 0, 0, 1]);
                    memory.write_bytes(from, &address).map_err(|_| WSAEFAULT)?;
                    memory
                        .write_u32(fromlen, address.len() as u32)
                        .map_err(|_| WSAEFAULT)?;
                }
                recvfrom.winsock_changed.notify_all();
                if truncated {
                    return Err(WSAEMSGSIZE);
                }
                Ok(copied as u64)
            })();
            socket_result(&recvfrom, cpu.gs_base, result)
        },
    );
}

fn register_socket_names(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for (symbol, peer) in [("getsockname", false), ("getpeername", true)] {
        let state = Arc::clone(state);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let result = (|| {
                    let net = state.winsock.lock().expect("guest Winsock state");
                    let socket = net.socket(args.rcx)?;
                    let port = if peer {
                        socket.peer.ok_or(WSAENOTCONN)?
                    } else {
                        socket.port.ok_or(WSAEINVAL)?
                    };
                    if args.rdx == 0 || args.r8 == 0 {
                        return Err(WSAEFAULT);
                    }
                    let length = memory.read_u32(args.r8).map_err(|_| WSAEFAULT)? as i32;
                    let mut address = [0; 16];
                    if length < address.len() as i32 {
                        return Err(WSAEFAULT);
                    }
                    memory
                        .check_write(args.rdx, address.len())
                        .map_err(|_| WSAEFAULT)?;
                    memory.check_write(args.r8, 4).map_err(|_| WSAEFAULT)?;
                    address[..2].copy_from_slice(&2_u16.to_le_bytes());
                    address[2..4].copy_from_slice(&port.to_be_bytes());
                    address[4..8].copy_from_slice(&[127, 0, 0, 1]);
                    memory
                        .write_bytes(args.rdx, &address)
                        .map_err(|_| WSAEFAULT)?;
                    memory
                        .write_u32(args.r8, address.len() as u32)
                        .map_err(|_| WSAEFAULT)?;
                    Ok(0)
                })();
                socket_result(&state, cpu.gs_base, result)
            },
        );
    }
}

fn register_socket_options(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    const SOL_SOCKET: u32 = 0xffff;
    const SO_TYPE: u32 = 0x1008;
    const SO_MAX_MSG_SIZE: u32 = 0x2003;
    const SOCK_STREAM: u32 = 1;
    const SOCK_DGRAM: u32 = 2;

    let state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "getsockopt",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = (|| {
                let net = state.winsock.lock().expect("guest Winsock state");
                let kind = net.socket(args.rcx)?.kind;
                let value = match (args.rdx as u32, args.r8 as u32, kind) {
                    (SOL_SOCKET, SO_TYPE, Kind::Datagram) => SOCK_DGRAM,
                    (SOL_SOCKET, SO_TYPE, Kind::Stream) => SOCK_STREAM,
                    (SOL_SOCKET, SO_MAX_MSG_SIZE, Kind::Datagram) => MAX_DATAGRAM as u32,
                    _ => return Err(WSAENOPROTOOPT),
                };
                let length = args.get(4, memory).map_err(|_| WSAEFAULT)?;
                if args.r9 == 0 || length == 0 {
                    return Err(WSAEFAULT);
                }
                if (memory.read_u32(length).map_err(|_| WSAEFAULT)? as i32) < 4 {
                    return Err(WSAEFAULT);
                }
                memory.check_write(args.r9, 4).map_err(|_| WSAEFAULT)?;
                memory.check_write(length, 4).map_err(|_| WSAEFAULT)?;
                memory.write_u32(args.r9, value).map_err(|_| WSAEFAULT)?;
                memory.write_u32(length, 4).map_err(|_| WSAEFAULT)?;
                Ok(0)
            })();
            socket_result(&state, cpu.gs_base, result)
        },
    );
}

fn register_poll(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    const MAX_POLL_MS: i32 = 1_000;
    const POLLFD_SIZE: usize = 16;
    const POLLHUP: u16 = 0x0002;
    const POLLNVAL: u16 = 0x0004;
    const POLLWRNORM: u16 = 0x0010;
    const POLLRDNORM: u16 = 0x0100;
    const POLLRDBAND: u16 = 0x0200;

    let state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "WSAPoll",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = (|| {
                let mut net = state.winsock.lock().expect("guest Winsock state");
                if net.refs == 0 {
                    return Err(WSANOTINITIALISED);
                }
                let count = args.rdx as u32 as usize;
                if !(1..=MAX_SOCKETS).contains(&count) {
                    return Err(WSAEINVAL);
                }
                let timeout = args.r8 as i32;
                if !(0..=MAX_POLL_MS).contains(&timeout) {
                    return Err(WSAEOPNOTSUPP);
                }
                if args.rcx == 0 {
                    return Err(WSAEFAULT);
                }
                let mut storage = [0; MAX_SOCKETS * POLLFD_SIZE];
                let descriptors = &mut storage[..count * POLLFD_SIZE];
                memory
                    .read_bytes(args.rcx, descriptors)
                    .map_err(|_| WSAEFAULT)?;
                memory
                    .check_write(args.rcx, descriptors.len())
                    .map_err(|_| WSAEFAULT)?;
                let deadline = Instant::now() + Duration::from_millis(timeout as u64);
                let ready = loop {
                    if net.refs == 0 {
                        return Err(WSANOTINITIALISED);
                    }
                    let mut ready = 0;
                    for fd in descriptors.as_chunks_mut::<POLLFD_SIZE>().0 {
                        let handle = u64::from_le_bytes(fd[..8].try_into().expect("socket handle"));
                        let events = u16::from_le_bytes([fd[8], fd[9]]);
                        if events & !(POLLWRNORM | POLLRDNORM | POLLRDBAND) != 0 {
                            return Err(WSAEINVAL);
                        }
                        let mut result = POLLNVAL;
                        if let Some(socket) = net.sockets.get(&handle) {
                            result = 0;
                            if socket.kind == Kind::Stream {
                                let stream = &socket.stream;
                                let readable = match &stream.listen {
                                    Some((_, queue)) => !queue.is_empty(),
                                    None => !stream.inbound.is_empty() || stream.peer_closed,
                                };
                                if readable {
                                    result |= events & POLLRDNORM;
                                }
                                if stream.peer_closed {
                                    result |= POLLHUP;
                                }
                                if !stream.send_closed
                                    && stream
                                        .link
                                        .and_then(|link| net.sockets.get(&link))
                                        .is_some_and(|target| {
                                            target.stream.inbound.len() < STREAM_BUFFER
                                        })
                                {
                                    result |= events & POLLWRNORM;
                                }
                            } else {
                                if !socket.pending.is_empty() {
                                    result |= events & POLLRDNORM;
                                }
                                if let Some(peer) = socket.peer
                                    && !net.sockets.values().any(|target| {
                                        target.kind == Kind::Datagram
                                            && target.port == Some(peer)
                                            && target
                                                .peer
                                                .is_none_or(|port| Some(port) == socket.port)
                                            && target.pending.len() == MAX_PENDING_DATAGRAMS
                                    })
                                {
                                    result |= events & POLLWRNORM;
                                }
                            }
                        }
                        fd[10..12].copy_from_slice(&result.to_le_bytes());
                        ready += u64::from(result != 0);
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if ready != 0 || remaining.is_zero() {
                        break ready;
                    }
                    #[cfg(test)]
                    {
                        net.poll_waiters += 1;
                    }
                    (net, _) = state
                        .winsock_changed
                        .wait_timeout(net, remaining)
                        .expect("guest Winsock state");
                    #[cfg(test)]
                    {
                        net.poll_waiters -= 1;
                    }
                };
                memory
                    .write_bytes(args.rcx, descriptors)
                    .map_err(|_| WSAEFAULT)?;
                Ok(ready)
            })();
            socket_result(&state, cpu.gs_base, result)
        },
    );
}

/// Write a 127.0.0.1 `sockaddr_in` for `port` through `address` and its
/// length through `length`, both checked before either is written.
fn write_loopback_address(
    memory: &mut GuestMemory,
    address: u64,
    length: u64,
    port: u16,
) -> Result<(), u32> {
    if length == 0 {
        return Err(WSAEFAULT);
    }
    let capacity = memory.read_u32(length).map_err(|_| WSAEFAULT)? as i32;
    let mut data = [0; 16];
    if capacity < data.len() as i32 {
        return Err(WSAEFAULT);
    }
    memory
        .check_write(address, data.len())
        .map_err(|_| WSAEFAULT)?;
    memory.check_write(length, 4).map_err(|_| WSAEFAULT)?;
    data[..2].copy_from_slice(&2_u16.to_le_bytes());
    data[2..4].copy_from_slice(&port.to_be_bytes());
    data[4..8].copy_from_slice(&[127, 0, 0, 1]);
    memory.write_bytes(address, &data).map_err(|_| WSAEFAULT)?;
    memory
        .write_u32(length, data.len() as u32)
        .map_err(|_| WSAEFAULT)
}

fn register_stream(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let listen = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "listen",
        ServiceStatus::Partial,
        move |args, cpu, _| {
            let mut net = listen.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let socket = net.socket(args.rcx)?;
                if socket.kind != Kind::Stream {
                    return Err(WSAEOPNOTSUPP);
                }
                if socket.peer.is_some() {
                    return Err(WSAEISCONN);
                }
                if socket.port.is_none() {
                    return Err(WSAEINVAL);
                }
                // SOMAXCONN and larger requests are clamped to the queue limit.
                let backlog = (args.rdx as i32).clamp(1, MAX_BACKLOG as i32) as usize;
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                match &mut socket.stream.listen {
                    Some((limit, _)) => *limit = backlog,
                    listen => *listen = Some((backlog, VecDeque::new())),
                }
                Ok(0)
            })();
            socket_result(&listen, cpu.gs_base, result)
        },
    );

    let accept = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "accept",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut net = accept.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let socket = net.socket(args.rcx)?;
                let Some((_, queue)) = &socket.stream.listen else {
                    return Err(WSAEINVAL);
                };
                let &waiting = queue.front().ok_or(WSAEWOULDBLOCK)?;
                let peer = net.sockets[&waiting]
                    .peer
                    .expect("accepted socket has a peer");
                if args.rdx != 0 {
                    write_loopback_address(memory, args.rdx, args.r8, peer)?;
                }
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                socket
                    .stream
                    .listen
                    .as_mut()
                    .expect("listening socket")
                    .1
                    .pop_front();
                accept.winsock_changed.notify_all();
                Ok(waiting)
            })();
            match result {
                Ok(handle) => Ok(handle),
                Err(error) => accept.fail(cpu.gs_base, error, INVALID_SOCKET),
            }
        },
    );

    let shutdown = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "shutdown",
        ServiceStatus::Partial,
        move |args, cpu, _| {
            let mut net = shutdown.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let socket = net.socket(args.rcx)?;
                if socket.kind != Kind::Stream {
                    return Err(WSAEOPNOTSUPP);
                }
                if socket.peer.is_none() {
                    return Err(WSAENOTCONN);
                }
                let (receive, send) = match args.rdx as u32 {
                    0 => (true, false),
                    1 => (false, true),
                    2 => (true, true),
                    _ => return Err(WSAEINVAL),
                };
                let link = socket.stream.link;
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                socket.stream.receive_closed |= receive;
                socket.stream.send_closed |= send;
                if send && let Some(other) = link.and_then(|link| net.sockets.get_mut(&link)) {
                    other.stream.peer_closed = true;
                }
                shutdown.winsock_changed.notify_all();
                Ok(0)
            })();
            socket_result(&shutdown, cpu.gs_base, result)
        },
    );
}

fn register_ioctl(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    const FIONREAD: u32 = 0x4004_667f;
    const FIONBIO: u32 = 0x8004_667e;

    let state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "ioctlsocket",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let net = state.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let socket = net.socket(args.rcx)?;
                let command = args.rdx as u32;
                if !matches!(command, FIONREAD | FIONBIO) {
                    return Err(WSAEINVAL);
                }
                if args.r8 == 0 {
                    return Err(WSAEFAULT);
                }
                if command == FIONBIO {
                    // Every socket here is already non-blocking; blocking mode
                    // is not supported rather than silently ignored.
                    let enable = memory.read_u32(args.r8).map_err(|_| WSAEFAULT)?;
                    return if enable != 0 {
                        Ok(0)
                    } else {
                        Err(WSAEOPNOTSUPP)
                    };
                }
                // FIONREAD: unread stream bytes, or the size of the next
                // datagram.
                let available = match socket.kind {
                    Kind::Stream if socket.stream.listen.is_some() => return Err(WSAEINVAL),
                    Kind::Stream => socket.stream.inbound.len(),
                    Kind::Datagram => socket.pending.front().map_or(0, |(_, data)| data.len()),
                };
                memory
                    .write_u32(args.r8, available as u32)
                    .map_err(|_| WSAEFAULT)?;
                Ok(0)
            })();
            socket_result(&state, cpu.gs_base, result)
        },
    );
}

#[cfg(test)]
#[test]
fn loopback_datagrams_preserve_boundaries_and_reject_external_addresses() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::MemoryPermissions;
    use crate::services::MockClock;

    let mut registry = ImportRegistry::new();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    register(&mut registry, &state);
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 4096, MemoryPermissions::READ_WRITE)
        .unwrap();
    let call = |memory: &mut GuestMemory, symbol: &str, args: &[u64]| {
        let mut cpu = CpuState::default();
        for (reg, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(reg, value);
        }
        let descriptor = registry
            .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    };
    assert_eq!(call(&mut memory, "WSAStartup", &[0x0202, 0x1000]), 0);
    let server = call(&mut memory, "socket", &[2, 2, 17]);
    let client = call(&mut memory, "socket", &[2, 2, 17]);
    let mut address = [0; 16];
    address[..8].copy_from_slice(&[2, 0, 0x13, 0x88, 127, 0, 0, 1]);
    memory.write_bytes(0x1200, &address).unwrap();
    assert_eq!(call(&mut memory, "bind", &[server, 0x1200, 16]), 0);
    assert_eq!(
        call(&mut memory, "bind", &[client, 0x1200, 16]),
        SOCKET_ERROR
    );
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAEADDRINUSE)
    );
    memory.write_u8(0x1204, 192).unwrap();
    assert_eq!(
        call(&mut memory, "connect", &[client, 0x1200, 16]),
        SOCKET_ERROR
    );
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAEACCES)
    );
    memory.write_u8(0x1204, 127).unwrap();
    // Unconnected sendto / recvfrom before the connected path.
    let mut ephemeral = [0; 16];
    ephemeral[..8].copy_from_slice(&[2, 0, 0x13, 0x88, 127, 0, 0, 1]);
    memory.write_bytes(0x1600, &ephemeral).unwrap();
    memory.write_bytes(0x1300, b"ping").unwrap();
    memory.write_u64(0x1700, 0x1600).unwrap(); // to
    memory.write_u64(0x1708, 16).unwrap(); // tolen
    let mut cpu = CpuState::default();
    cpu.set(Register::Rcx, client);
    cpu.set(Register::Rdx, 0x1300);
    cpu.set(Register::R8, 4);
    cpu.set(Register::R9, 0);
    cpu.set(Register::Rsp, 0x1800);
    memory.write_u64(0x1800 + 40, 0x1600).unwrap();
    memory.write_u64(0x1800 + 48, 16).unwrap();
    let descriptor = registry
        .resolve(MODULE, &ImportName::Name("sendto".into()))
        .unwrap();
    assert_eq!(
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        4
    );
    memory.write_u64(0x1900, 0x1a00).unwrap(); // from buffer ptr slot unused; pass from/fromlen
    memory.write_u32(0x1b00, 16).unwrap();
    cpu.set(Register::Rcx, server);
    cpu.set(Register::Rdx, 0x1400);
    cpu.set(Register::R8, 4);
    cpu.set(Register::R9, 0);
    memory.write_u64(0x1800 + 40, 0x1a00).unwrap();
    memory.write_u64(0x1800 + 48, 0x1b00).unwrap();
    let descriptor = registry
        .resolve(MODULE, &ImportName::Name("recvfrom".into()))
        .unwrap();
    assert_eq!(
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        4
    );
    let mut got = [0; 4];
    memory.read_bytes(0x1400, &mut got).unwrap();
    assert_eq!(&got, b"ping");
    assert_eq!(memory.read_u16(0x1a00).unwrap(), 2);
    let mut ip = [0; 4];
    memory.read_bytes(0x1a04, &mut ip).unwrap();
    assert_eq!(ip, [127, 0, 0, 1]);

    assert_eq!(call(&mut memory, "connect", &[client, 0x1200, 16]), 0);
    memory.write_bytes(0x1300, b"hello").unwrap();
    assert_eq!(call(&mut memory, "send", &[client, 0x1300, 5, 0]), 5);
    assert_eq!(call(&mut memory, "send", &[client, 0x1300, 2, 0]), 2);
    assert_eq!(
        call(&mut memory, "recv", &[server, 0xff, 4, 0]),
        SOCKET_ERROR
    );
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAEFAULT)
    );
    assert_eq!(
        call(&mut memory, "recv", &[server, 0x1400, 4, 0]),
        SOCKET_ERROR
    );
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAEMSGSIZE)
    );
    let mut received = [0; 4];
    memory.read_bytes(0x1400, &mut received).unwrap();
    assert_eq!(&received, b"hell");
    assert_eq!(call(&mut memory, "recv", &[server, 0x1400, 4, 0]), 2);
    assert_eq!(
        call(&mut memory, "recv", &[server, 0x1400, 4, 0]),
        SOCKET_ERROR
    );
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAEWOULDBLOCK)
    );

    let mut polling_memory = memory.clone();
    polling_memory.write_u64(0x1500, server).unwrap();
    polling_memory.write_u16(0x1508, 0x0100).unwrap();
    std::thread::scope(|scope| {
        let (finished, completion) = std::sync::mpsc::channel();
        let polling_memory = &mut polling_memory;
        scope.spawn(move || {
            let ready = call(polling_memory, "WSAPoll", &[0x1500, 1, 1000]);
            finished.send(ready).unwrap();
        });
        let deadline = Instant::now() + Duration::from_millis(500);
        while state.winsock.lock().unwrap().poll_waiters == 0 {
            assert!(Instant::now() < deadline, "poll did not enter its wait");
            std::thread::yield_now();
        }
        assert_eq!(call(&mut memory, "send", &[client, 0x1300, 5, 0]), 5);
        assert_eq!(
            completion.recv_timeout(Duration::from_millis(400)).unwrap(),
            1
        );
    });
    assert_eq!(polling_memory.read_u16(0x150a).unwrap(), 0x0100);
}
