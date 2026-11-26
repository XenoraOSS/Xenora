// SPDX-License-Identifier: GPL-3.0-or-later

//! Process-local Winsock UDP loopback for the synthetic guest.

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
const WSAESHUTDOWN: u32 = 10058;
const WSAEAFNOSUPPORT: u32 = 10047;
const WSASYSNOTREADY: u32 = 10091;
const WSAVERNOTSUPPORTED: u32 = 10092;
const WSANOTINITIALISED: u32 = 10093;

#[derive(Default)]
pub(super) struct Winsock {
    refs: u32,
    serial: u32,
    sockets: BTreeMap<u64, Socket>,
    #[cfg(test)]
    poll_waiters: usize,
}

#[derive(Default)]
struct Socket {
    port: Option<u16>,
    peer: Option<u16>,
    pending: VecDeque<(u16, Vec<u8>)>,
    shutdown_receive: bool,
    shutdown_send: bool,
    reuse_address: bool,
    broadcast: bool,
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

    fn port(&self, requested: u16) -> Result<u16, u32> {
        let available = |port| {
            self.sockets
                .values()
                .all(|socket| socket.port != Some(port))
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
        if args.rdx as u32 != 2 {
            return fail(WSAESOCKTNOSUPPORT);
        }
        if !matches!(args.r8 as u32, 0 | 17) {
            return fail(WSAEPROTONOSUPPORT);
        }
        if net.sockets.len() >= MAX_SOCKETS {
            return fail(WSAEMFILE);
        }
        let Some(next) = net.serial.checked_add(1) else {
            return fail(WSAEMFILE);
        };
        net.serial = next;
        let socket = 0x5300_0000_0000_0000 | u64::from(next);
        net.sockets.insert(socket, Socket::default());
        Ok(socket)
    });

    let close = Arc::clone(state);
    registry.register(MODULE, "closesocket", move |args, cpu, _| {
        let mut net = close.winsock.lock().expect("guest Winsock state");
        if net.refs == 0 {
            return close.fail(cpu.gs_base, WSANOTINITIALISED, SOCKET_ERROR);
        }
        if net.sockets.remove(&args.rcx).is_none() {
            return close.fail(cpu.gs_base, WSAENOTSOCK, SOCKET_ERROR);
        }
        close.winsock_changed.notify_all();
        Ok(0)
    });

    register_address_conversion(registry, state);
    register_loopback(registry, state);
    register_socket_names(registry, state);
    register_socket_options(registry, state);
    register_datagrams(registry, state);
    register_host_identity(registry, state);
    register_lifecycle(registry, state);
    register_select(registry, state);
    register_poll(registry, state);

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
                    let existing = socket.port;
                    let requested = loopback_port(memory, args.rdx, args.r8 as u32)?;
                    if connect && requested == 0 || !connect && existing.is_some() {
                        return Err(WSAEINVAL);
                    }
                    let local = match (connect, existing) {
                        (true, Some(port)) => port,
                        (true, None) => net.port(0)?,
                        (false, _) => net.port(requested)?,
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
                if socket.shutdown_send {
                    return Err(WSAESHUTDOWN);
                }
                let peer = socket.peer.ok_or(WSAENOTCONN)?;
                let source = socket.port.expect("connected socket is bound");
                let length = args.r8 as i32;
                if length < 0 || args.r9 as u32 != 0 {
                    return Err(WSAEINVAL);
                }
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
                if let Some(target) = net.sockets.values_mut().find(|socket| {
                    socket.port == Some(peer) && socket.peer.is_none_or(|port| port == source)
                }) {
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
                if net.socket(args.rcx)?.port.is_none() {
                    return Err(WSAEINVAL);
                }
                if net.socket(args.rcx)?.shutdown_receive {
                    return Err(WSAESHUTDOWN);
                }
                let length = args.r8 as i32;
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
}

fn register_datagrams(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let sendto = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "sendto",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut net = sendto.winsock.lock().expect("guest Winsock state");
            let result = (|| {
                let length = args.r8 as i32;
                if length < 0 || args.r9 as u32 != 0 {
                    return Err(WSAEINVAL);
                }
                let length = length as usize;
                if length > MAX_DATAGRAM {
                    return Err(WSAEMSGSIZE);
                }
                // The destination is explicit: an absent address is the
                // connected form, which send already serves.
                if args.get(4, memory).map_err(|_| WSAEFAULT)? == 0 {
                    return Err(WSAEINVAL);
                }
                let peer = loopback_port(memory, args.get(4, memory).map_err(|_| WSAEFAULT)?, {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        args.get(5, memory).map_err(|_| WSAEFAULT)? as u32
                    }
                })?;
                // An unbound socket auto-binds an ephemeral port, as the
                // real stack does on its first send.
                if net.socket(args.rcx)?.port.is_none() {
                    let port = net.port(0)?;
                    net.sockets
                        .get_mut(&args.rcx)
                        .expect("validated socket")
                        .port = Some(port);
                }
                let source = net.socket(args.rcx)?.port.expect("bound above");
                let mut data = vec![0; length];
                if length != 0 {
                    memory
                        .read_bytes(args.rdx, &mut data)
                        .map_err(|_| WSAEFAULT)?;
                }
                if let Some(target) = net.sockets.values_mut().find(|socket| {
                    socket.port == Some(peer) && socket.peer.is_none_or(|port| port == source)
                }) {
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
                if net.socket(args.rcx)?.port.is_none() {
                    return Err(WSAEINVAL);
                }
                let length = args.r8 as i32;
                if length < 0 || args.r9 as u32 != 0 {
                    return Err(WSAEINVAL);
                }
                let source_address = args.get(4, memory).map_err(|_| WSAEFAULT)?;
                let source_length = args.get(5, memory).map_err(|_| WSAEFAULT)?;
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                let (source, data) = socket.pending.front().ok_or(WSAEWOULDBLOCK)?;
                let (source, copied) = (*source, (length as usize).min(data.len()));
                if copied != 0 {
                    memory
                        .write_bytes(args.rdx, &data[..copied])
                        .map_err(|_| WSAEFAULT)?;
                }
                let truncated = copied < data.len();
                socket.pending.pop_front();
                recvfrom.winsock_changed.notify_all();
                // The source address reports where the datagram came from
                // when the caller asked for it.
                if source_address != 0 && source_length != 0 {
                    let capacity = memory.read_u32(source_length).map_err(|_| WSAEFAULT)? as i32;
                    if capacity < 16 {
                        return Err(WSAEFAULT);
                    }
                    let mut address = [0; 16];
                    address[..2].copy_from_slice(&2_u16.to_le_bytes());
                    address[2..4].copy_from_slice(&source.to_be_bytes());
                    address[4..8].copy_from_slice(&[127, 0, 0, 1]);
                    memory
                        .write_bytes(source_address, &address)
                        .map_err(|_| WSAEFAULT)?;
                    memory.write_u32(source_length, 16).map_err(|_| WSAEFAULT)?;
                }
                if truncated {
                    return Err(WSAEMSGSIZE);
                }
                Ok(copied as u64)
            })();
            socket_result(&recvfrom, cpu.gs_base, result)
        },
    );
}

fn register_host_identity(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let hostname = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "gethostname",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = (|| {
                if hostname.winsock.lock().expect("guest Winsock state").refs == 0 {
                    return Err(WSANOTINITIALISED);
                }
                // The machine has one fixed name on its simulated LAN.
                let name = b"XENORA\0";
                if (args.rdx as i32) < name.len() as i32 {
                    return Err(WSAEFAULT);
                }
                memory.write_bytes(args.rcx, name).map_err(|_| WSAEFAULT)?;
                Ok(0)
            })();
            socket_result(&hostname, cpu.gs_base, result)
        },
    );

    // inet_addr is the legacy parse-only form: dotted decimal or 255.255.255.255
    // for a failure, exactly the ambiguity the CRT documents.
    let inet = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "inet_addr",
        ServiceStatus::Partial,
        move |args, _cpu, memory| {
            let _ = &inet;
            let mut text = [0u8; 15];
            let mut len = 0usize;
            loop {
                let byte = memory
                    .read_u8(args.rcx + len as u64)
                    .map_err(ExitReason::MemoryFault)?;
                if byte == 0 {
                    break;
                }
                if len == text.len() {
                    return Ok(u64::from(u32::MAX));
                }
                text[len] = byte;
                len += 1;
            }
            let parsed = std::str::from_utf8(&text[..len])
                .ok()
                .and_then(|text| text.parse::<std::net::Ipv4Addr>().ok());
            match parsed {
                Some(ip) => Ok(u64::from(u32::from_le_bytes(ip.octets()))),
                None => Ok(u64::from(u32::MAX)),
            }
        },
    );
}

fn register_lifecycle(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let shutdown = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "shutdown",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let _ = memory;
            let result = (|| {
                let mut net = shutdown.winsock.lock().expect("guest Winsock state");
                if net.socket(args.rcx)?.peer.is_none() {
                    return Err(WSAENOTCONN);
                }
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                match args.rdx as i32 {
                    0 => socket.shutdown_receive = true,
                    1 => socket.shutdown_send = true,
                    2 => {
                        socket.shutdown_receive = true;
                        socket.shutdown_send = true;
                    }
                    _ => return Err(WSAEINVAL),
                }
                shutdown.winsock_changed.notify_all();
                Ok(0)
            })();
            socket_result(&shutdown, cpu.gs_base, result)
        },
    );

    // The options the loopback model stores. They are vacuous - loopback
    // datagrams have no broadcast domain to permit and no port sharing to
    // relax - but they are stored honestly rather than silently dropped,
    // and getsockopt can grow to report them.
    let setsockopt = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "setsockopt",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            const SO_REUSEADDR: u32 = 0x0004;
            const SO_BROADCAST: u32 = 0x0020;
            let result = (|| {
                let mut net = setsockopt.winsock.lock().expect("guest Winsock state");
                net.socket(args.rcx)?;
                if args.rdx as u32 != 0xffff {
                    return Err(WSAENOPROTOOPT);
                }
                let length = args.get(4, memory).map_err(|_| WSAEFAULT)? as i32;
                if args.r9 == 0 || length < 4 {
                    return Err(WSAEFAULT);
                }
                let value = memory.read_u32(args.r9).map_err(|_| WSAEFAULT)? != 0;
                let socket = net.sockets.get_mut(&args.rcx).expect("validated socket");
                match args.r8 as u32 {
                    SO_REUSEADDR => socket.reuse_address = value,
                    SO_BROADCAST => socket.broadcast = value,
                    _ => return Err(WSAENOPROTOOPT),
                }
                Ok(0)
            })();
            socket_result(&setsockopt, cpu.gs_base, result)
        },
    );
}

// select uses the Winsock fd_set layout: a u32 count followed by the socket
// handles at offset 8, filtered in place. Readiness mirrors WSAPoll.
fn register_select(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    const MAX_SELECT_MS: u64 = 1_000;
    const FD_SETSIZE: usize = 64;

    let select_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "select",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = (|| {
                let mut net = select_state.winsock.lock().expect("guest Winsock state");
                if net.refs == 0 {
                    return Err(WSANOTINITIALISED);
                }
                let read_set = args.rdx;
                let write_set = args.r8;
                if read_set == 0 && write_set == 0 {
                    return Err(WSAEINVAL);
                }
                // The timeout is seconds and microseconds, null meaning
                // infinite - which the single-executor machine caps like
                // WSAPoll does rather than deadlocking the guest.
                let mut timeout_ms = MAX_SELECT_MS;
                let timeout_address = args.get(4, memory).map_err(|_| WSAEFAULT)?;
                if timeout_address != 0 {
                    let seconds = memory.read_u64(timeout_address).map_err(|_| WSAEFAULT)?;
                    let micros = memory
                        .read_u64(timeout_address + 8)
                        .map_err(|_| WSAEFAULT)?;
                    timeout_ms = (seconds * 1_000 + micros / 1_000).min(MAX_SELECT_MS);
                }
                let read_handles = |memory: &GuestMemory| -> Result<Vec<u64>, u32> {
                    if read_set == 0 {
                        return Ok(Vec::new());
                    }
                    let count = memory.read_u32(read_set).map_err(|_| WSAEFAULT)? as usize;
                    if count > FD_SETSIZE {
                        return Err(WSAEINVAL);
                    }
                    let mut handles = Vec::with_capacity(count);
                    for index in 0..count {
                        handles.push(
                            memory
                                .read_u64(read_set + 8 + index as u64 * 8)
                                .map_err(|_| WSAEFAULT)?,
                        );
                    }
                    Ok(handles)
                };
                let write_handles = |memory: &GuestMemory| -> Result<Vec<u64>, u32> {
                    if write_set == 0 {
                        return Ok(Vec::new());
                    }
                    let count = memory.read_u32(write_set).map_err(|_| WSAEFAULT)? as usize;
                    if count > FD_SETSIZE {
                        return Err(WSAEINVAL);
                    }
                    let mut handles = Vec::with_capacity(count);
                    for index in 0..count {
                        handles.push(
                            memory
                                .read_u64(write_set + 8 + index as u64 * 8)
                                .map_err(|_| WSAEFAULT)?,
                        );
                    }
                    Ok(handles)
                };
                let reads = read_handles(memory)?;
                let writes = write_handles(memory)?;
                if read_set != 0 {
                    memory
                        .check_write(read_set, 8 + reads.len() * 8)
                        .map_err(|_| WSAEFAULT)?;
                }
                if write_set != 0 {
                    memory
                        .check_write(write_set, 8 + writes.len() * 8)
                        .map_err(|_| WSAEFAULT)?;
                }
                let deadline = Instant::now() + Duration::from_millis(timeout_ms);
                let (ready_reads, ready_writes) = loop {
                    let readable = |net: &Winsock, handle: u64| {
                        net.sockets.get(&handle).is_some_and(|socket| {
                            !socket.pending.is_empty() && !socket.shutdown_receive
                        })
                    };
                    let writable = |net: &Winsock, handle: u64| {
                        net.sockets.get(&handle).is_some_and(|socket| {
                            !socket.shutdown_send
                                && socket.peer.is_none_or(|peer| {
                                    !net.sockets.values().any(|target| {
                                        target.port == Some(peer)
                                            && target.pending.len() == MAX_PENDING_DATAGRAMS
                                    })
                                })
                        })
                    };
                    let ready_reads: Vec<u64> = reads
                        .iter()
                        .copied()
                        .filter(|&h| readable(&net, h))
                        .collect();
                    let ready_writes: Vec<u64> = writes
                        .iter()
                        .copied()
                        .filter(|&h| writable(&net, h))
                        .collect();
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if !ready_reads.is_empty() || !ready_writes.is_empty() || remaining.is_zero() {
                        break (ready_reads, ready_writes);
                    }
                    (net, _) = select_state
                        .winsock_changed
                        .wait_timeout(net, remaining)
                        .expect("guest Winsock state");
                };
                // Each set reports only its ready sockets, in place.
                if read_set != 0 {
                    memory
                        .write_u32(read_set, ready_reads.len() as u32)
                        .map_err(|_| WSAEFAULT)?;
                    for (index, handle) in ready_reads.iter().enumerate() {
                        memory
                            .write_u64(read_set + 8 + index as u64 * 8, *handle)
                            .map_err(|_| WSAEFAULT)?;
                    }
                }
                if write_set != 0 {
                    memory
                        .write_u32(write_set, ready_writes.len() as u32)
                        .map_err(|_| WSAEFAULT)?;
                    for (index, handle) in ready_writes.iter().enumerate() {
                        memory
                            .write_u64(write_set + 8 + index as u64 * 8, *handle)
                            .map_err(|_| WSAEFAULT)?;
                    }
                }
                Ok((ready_reads.len() + ready_writes.len()) as u64)
            })();
            socket_result(&select_state, cpu.gs_base, result)
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
    const SOCK_DGRAM: u32 = 2;

    let state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "getsockopt",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = (|| {
                let net = state.winsock.lock().expect("guest Winsock state");
                net.socket(args.rcx)?;
                let value = match (args.rdx as u32, args.r8 as u32) {
                    (SOL_SOCKET, SO_TYPE) => SOCK_DGRAM,
                    (SOL_SOCKET, SO_MAX_MSG_SIZE) => MAX_DATAGRAM as u32,
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
                            if !socket.pending.is_empty() {
                                result |= events & POLLRDNORM;
                            }
                            if let Some(peer) = socket.peer
                                && !net.sockets.values().any(|target| {
                                    target.port == Some(peer)
                                        && target.peer.is_none_or(|port| Some(port) == socket.port)
                                        && target.pending.len() == MAX_PENDING_DATAGRAMS
                                })
                            {
                                result |= events & POLLWRNORM;
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

#[cfg(test)]
#[test]
fn sendto_recvfrom_hostname_and_inet_addr() {
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
        .map_region(0x1000, 8192, MemoryPermissions::READ_WRITE)
        .unwrap();
    let call = |memory: &mut GuestMemory, symbol: &str, args: &[u64]| {
        let mut cpu = CpuState::default();
        for (reg, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(reg, value);
        }
        // Parameters past the fourth arrive on the stack per the Win64
        // calling convention.
        cpu.set(Register::Rsp, 0x2000);
        for (index, &value) in args.iter().enumerate().skip(4) {
            memory
                .write_u64(0x2000 + 0x28 + (index as u64 - 4) * 8, value)
                .unwrap();
        }
        let descriptor = registry
            .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    };
    assert_eq!(call(&mut memory, "WSAStartup", &[0x0202, 0x1000]), 0);
    let server = call(&mut memory, "socket", &[2, 2, 17]);
    let client = call(&mut memory, "socket", &[2, 2, 17]);
    // Bind the server; the client stays unbound and auto-binds on sendto.
    let mut address = [0; 16];
    address[..8].copy_from_slice(&[2, 0, 0x13, 0x88, 127, 0, 0, 1]);
    memory.write_bytes(0x1200, &address).unwrap();
    assert_eq!(call(&mut memory, "bind", &[server, 0x1200, 16]), 0);
    memory.write_bytes(0x1300, b"ping").unwrap();
    // sendto with the destination in the stack parameters.
    assert_eq!(
        call(&mut memory, "sendto", &[client, 0x1300, 4, 0, 0x1200, 16]),
        4
    );
    // recvfrom reports the source address of the datagram.
    memory.write_u32(0x1500, 16).unwrap();
    assert_eq!(
        call(
            &mut memory,
            "recvfrom",
            &[server, 0x1400, 8, 0, 0x1600, 0x1500]
        ),
        4
    );
    let mut received = [0; 4];
    memory.read_bytes(0x1400, &mut received).unwrap();
    assert_eq!(&received, b"ping");
    let mut source = [0; 8];
    memory.read_bytes(0x1600, &mut source).unwrap();
    assert_eq!(&source[..2], &2_u16.to_le_bytes(), "AF_INET source");
    assert_eq!(&source[4..8], &[127, 0, 0, 1]);
    // The auto-bound client port is ephemeral.
    let source_port = u16::from_be_bytes([source[2], source[3]]);
    assert!(source_port >= 49152);
    // gethostname writes the fixed machine name.
    assert_eq!(call(&mut memory, "gethostname", &[0x1700, 32]), 0);
    let mut name = [0; 7];
    memory.read_bytes(0x1700, &mut name).unwrap();
    assert_eq!(&name, b"XENORA\0");
    // inet_addr parses dotted decimal into network order, or all-ones on
    // a failure - the documented ambiguity.
    memory.write_bytes(0x1800, b"127.0.0.1\0").unwrap();
    assert_eq!(
        call(&mut memory, "inet_addr", &[0x1800]),
        u64::from(u32::from_le_bytes([127, 0, 0, 1]))
    );
    memory.write_bytes(0x1800, b"not-an-address\0").unwrap();
    assert_eq!(
        call(&mut memory, "inet_addr", &[0x1800]),
        u64::from(u32::MAX)
    );
}

#[cfg(test)]
#[test]
fn select_readiness_shutdown_and_socket_options() {
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
        .map_region(0x1000, 8192, MemoryPermissions::READ_WRITE)
        .unwrap();
    let call = |memory: &mut GuestMemory, symbol: &str, args: &[u64]| {
        let mut cpu = CpuState::default();
        for (reg, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(reg, value);
        }
        cpu.set(Register::Rsp, 0x2000);
        for (index, &value) in args.iter().enumerate().skip(4) {
            memory
                .write_u64(0x2000 + 0x28 + (index as u64 - 4) * 8, value)
                .unwrap();
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
    // select on an idle socket times out to zero ready within the cap.
    memory.write_bytes(0x1800, b"go").unwrap();
    // The read set: count 1, then the server handle at offset 8.
    memory.write_u32(0x1900, 1).unwrap();
    memory.write_u64(0x1908, server).unwrap();
    memory.write_u64(0x1a00, 0).unwrap(); // timeout: 0 sec 0 usec = poll once
    memory.write_u64(0x1a08, 0).unwrap();
    assert_eq!(call(&mut memory, "select", &[0, 0x1900, 0, 0, 0x1a00]), 0);
    // A datagram arriving makes the server readable.
    assert_eq!(
        call(&mut memory, "sendto", &[client, 0x1800, 2, 0, 0x1200, 16]),
        2
    );
    memory.write_u32(0x1900, 1).unwrap();
    memory.write_u64(0x1908, server).unwrap();
    assert_eq!(call(&mut memory, "select", &[0, 0x1900, 0, 0, 0x1a00]), 1);
    assert_eq!(
        memory.read_u32(0x1900).unwrap(),
        1,
        "one ready handle reported"
    );
    assert_eq!(memory.read_u64(0x1908).unwrap(), server);
    // shutdown requires a connected socket.
    assert_eq!(call(&mut memory, "shutdown", &[server, 2]), SOCKET_ERROR);
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAENOTCONN)
    );
    // Connect the client, then shutdown both directions.
    memory.write_u8(0x1202, 0x13).unwrap();
    memory.write_u8(0x1203, 0x88).unwrap();
    assert_eq!(call(&mut memory, "connect", &[client, 0x1200, 16]), 0);
    assert_eq!(call(&mut memory, "shutdown", &[client, 1]), 0);
    memory.write_bytes(0x1800, b"xx").unwrap();
    assert_eq!(
        call(&mut memory, "send", &[client, 0x1800, 2, 0]),
        SOCKET_ERROR
    );
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAESHUTDOWN)
    );
    // setsockopt stores the vacuous options and refuses the rest.
    memory.write_u32(0x1b00, 1).unwrap();
    assert_eq!(
        call(
            &mut memory,
            "setsockopt",
            &[client, 0xffff, 0x20, 0x1b00, 4]
        ),
        0
    );
    assert_eq!(
        call(
            &mut memory,
            "setsockopt",
            &[client, 0xffff, 0x9999, 0x1b00, 4]
        ),
        SOCKET_ERROR
    );
    assert_eq!(
        call(&mut memory, "WSAGetLastError", &[]),
        u64::from(WSAENOPROTOOPT)
    );
}
