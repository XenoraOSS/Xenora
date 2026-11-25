// SPDX-License-Identifier: GPL-3.0-or-later

//! Process-local TCP loopback through the guest import table. Two guest
//! sockets connect on 127.0.0.1 and exchange a byte stream; no host socket is
//! opened. Expected results follow the public Winsock reference pages for
//! socket, bind, listen, accept, connect, send, recv, shutdown, closesocket,
//! WSAPoll and getsockopt, and the Windows Sockets error code list.

mod support;

use support::*;

const WS2: &str = "ws2_32.dll";
const IMPORTS: &[(&str, &str)] = &[
    (WS2, "WSAStartup"),
    (WS2, "WSAGetLastError"),
    (WS2, "socket"),
    (WS2, "closesocket"),
    (WS2, "bind"),
    (WS2, "listen"),
    (WS2, "accept"),
    (WS2, "connect"),
    (WS2, "send"),
    (WS2, "recv"),
    (WS2, "sendto"),
    (WS2, "shutdown"),
    (WS2, "getsockname"),
    (WS2, "getsockopt"),
    (WS2, "WSAPoll"),
    (WS2, "ioctlsocket"),
];

const ERROR: u64 = u32::MAX as u64;
const INVALID_SOCKET: u64 = u64::MAX;
const WSAEACCES: u64 = 10013;
const WSAEINVAL: u64 = 10022;
const WSAEWOULDBLOCK: u64 = 10035;
const WSAEOPNOTSUPP: u64 = 10045;
const WSAECONNRESET: u64 = 10054;
const WSAEISCONN: u64 = 10056;
const WSAENOTCONN: u64 = 10057;
const WSAESHUTDOWN: u64 = 10058;
const WSAECONNREFUSED: u64 = 10061;
const POLLHUP: u16 = 0x0002;
const POLLWRNORM: u16 = 0x0010;
const POLLRDNORM: u16 = 0x0100;

const ADDRESS: u64 = SCRATCH;
const PEER: u64 = SCRATCH + 0x20;
const LENGTH: u64 = SCRATCH + 0x40;
const POLL: u64 = SCRATCH + 0x60;
const VALUE: u64 = SCRATCH + 0x80;
const WSADATA: u64 = SCRATCH + 0x100;
const BUFFER: u64 = SCRATCH + 0x400;
const CHUNK: u64 = 1024;

struct Net(Guest);

impl Net {
    fn new() -> Self {
        let mut guest = Guest::new(IMPORTS, &[]);
        assert_eq!(guest.call("WSAStartup", &[0x0202, WSADATA]), 0);
        Self(guest)
    }

    /// The function's result, or its WSAGetLastError code when it returned
    /// `failure`.
    fn call(&mut self, symbol: &str, args: &[u64], failure: u64) -> Result<u64, u64> {
        let result = self.0.call(symbol, args);
        if result == failure {
            Err(self.0.call("WSAGetLastError", &[]))
        } else {
            Ok(result)
        }
    }

    fn put_address(&mut self, ip: [u8; 4], port: u16) {
        let mut data = [0; 16];
        data[..2].copy_from_slice(&2_u16.to_le_bytes());
        data[2..4].copy_from_slice(&port.to_be_bytes());
        data[4..8].copy_from_slice(&ip);
        self.0.memory.write_bytes(ADDRESS, &data).unwrap();
    }

    fn stream(&mut self) -> u64 {
        self.call("socket", &[2, 1, 6], INVALID_SOCKET).unwrap()
    }

    fn bind(&mut self, socket: u64, port: u16) -> Result<u64, u64> {
        self.put_address([127, 0, 0, 1], port);
        self.call("bind", &[socket, ADDRESS, 16], ERROR)
    }

    fn connect(&mut self, socket: u64, ip: [u8; 4], port: u16) -> Result<u64, u64> {
        self.put_address(ip, port);
        self.call("connect", &[socket, ADDRESS, 16], ERROR)
    }

    fn accept(&mut self, listener: u64) -> Result<(u64, u16), u64> {
        self.0.memory.write_u32(LENGTH, 16).unwrap();
        let socket = self.call("accept", &[listener, PEER, LENGTH], INVALID_SOCKET)?;
        assert_eq!(self.0.u32_at(LENGTH), 16);
        assert_eq!(self.0.u16_at(PEER), 2);
        let mut ip = [0; 4];
        self.0.memory.read_bytes(PEER + 4, &mut ip).unwrap();
        assert_eq!(ip, [127, 0, 0, 1]);
        Ok((socket, u16::from_be(self.0.u16_at(PEER + 2))))
    }

    fn local_port(&mut self, socket: u64) -> u16 {
        self.0.memory.write_u32(LENGTH, 16).unwrap();
        self.call("getsockname", &[socket, PEER, LENGTH], ERROR)
            .unwrap();
        u16::from_be(self.0.u16_at(PEER + 2))
    }

    fn send(&mut self, socket: u64, bytes: &[u8]) -> Result<u64, u64> {
        self.0.memory.write_bytes(BUFFER, bytes).unwrap();
        self.call("send", &[socket, BUFFER, bytes.len() as u64, 0], ERROR)
    }

    fn recv(&mut self, socket: u64, capacity: u64) -> Result<Vec<u8>, u64> {
        let received = self.call("recv", &[socket, BUFFER, capacity, 0], ERROR)?;
        let mut bytes = vec![0; received as usize];
        self.0.memory.read_bytes(BUFFER, &mut bytes).unwrap();
        Ok(bytes)
    }

    fn poll(&mut self, socket: u64, events: u16) -> u16 {
        self.0.memory.write_u64(POLL, socket).unwrap();
        self.0.memory.write_u16(POLL + 8, events).unwrap();
        self.0.memory.write_u16(POLL + 10, 0xffff).unwrap();
        self.call("WSAPoll", &[POLL, 1, 0], ERROR).unwrap();
        self.0.u16_at(POLL + 10)
    }
}

#[test]
fn loopback_tcp_connects_streams_and_closes_between_guest_sockets() {
    let mut net = Net::new();
    let listener = net.stream();
    // listen needs a bound socket.
    assert_eq!(net.call("listen", &[listener, 4], ERROR), Err(WSAEINVAL));
    assert_eq!(net.bind(listener, 5000), Ok(0));
    assert_eq!(net.call("listen", &[listener, 0x7fff_ffff], ERROR), Ok(0));
    assert_eq!(net.accept(listener), Err(WSAEWOULDBLOCK));
    assert_eq!(net.poll(listener, POLLRDNORM), 0);

    let client = net.stream();
    assert_eq!(net.connect(client, [127, 0, 0, 1], 5000), Ok(0));
    assert_eq!(net.connect(client, [127, 0, 0, 1], 5000), Err(WSAEISCONN));
    assert_eq!(net.poll(listener, POLLRDNORM), POLLRDNORM);
    let client_port = net.local_port(client);
    let (server, peer_port) = net.accept(listener).unwrap();
    assert_eq!(peer_port, client_port);
    assert_eq!(net.local_port(server), 5000);
    assert_eq!(net.accept(listener), Err(WSAEWOULDBLOCK));

    // A byte stream keeps no message boundaries.
    assert_eq!(net.recv(server, 16), Err(WSAEWOULDBLOCK));
    assert_eq!(net.send(client, b"hello"), Ok(5));
    assert_eq!(net.send(client, b" world"), Ok(6));
    assert_eq!(
        net.poll(server, POLLRDNORM | POLLWRNORM),
        POLLRDNORM | POLLWRNORM
    );
    assert_eq!(net.recv(server, 3).unwrap(), b"hel");
    assert_eq!(net.recv(server, 64).unwrap(), b"lo world");
    assert_eq!(net.recv(server, 64), Err(WSAEWOULDBLOCK));

    // The receiver buffers 64 KiB; then sends would block until it reads.
    let chunk = vec![0x5a; CHUNK as usize];
    let mut queued = 0;
    while net.send(client, &chunk) == Ok(CHUNK) {
        queued += CHUNK;
        assert!(queued <= 64 * 1024);
    }
    assert_eq!(queued, 64 * 1024);
    assert_eq!(net.send(client, &chunk), Err(WSAEWOULDBLOCK));
    assert_eq!(net.poll(client, POLLWRNORM), 0);
    assert_eq!(net.recv(server, CHUNK).unwrap(), chunk);
    assert_eq!(net.poll(client, POLLWRNORM), POLLWRNORM);
    assert_eq!(net.send(client, &chunk), Ok(CHUNK));
    while queued != 0 {
        assert_eq!(net.recv(server, CHUNK).unwrap().len() as u64, CHUNK);
        queued -= CHUNK;
    }

    // Shutting down the client's sending side is end of file for the server;
    // the server can still answer.
    assert_eq!(net.send(client, b"last"), Ok(4));
    assert_eq!(net.call("shutdown", &[client, 1], ERROR), Ok(0));
    assert_eq!(net.send(client, b"more"), Err(WSAESHUTDOWN));
    assert_eq!(net.recv(server, 64).unwrap(), b"last");
    assert_eq!(net.recv(server, 64).unwrap(), b"");
    assert_eq!(net.send(server, b"bye"), Ok(3));
    assert_eq!(net.recv(client, 64).unwrap(), b"bye");
    assert_eq!(net.call("shutdown", &[client, 9], ERROR), Err(WSAEINVAL));

    // Closing one end gives the other end of file, a hang-up and resets.
    assert_eq!(net.call("closesocket", &[client], ERROR), Ok(0));
    assert_eq!(net.recv(server, 64).unwrap(), b"");
    assert_eq!(net.poll(server, POLLRDNORM), POLLRDNORM | POLLHUP);
    assert_eq!(net.send(server, b"x"), Err(WSAECONNRESET));
    assert_eq!(net.call("closesocket", &[server], ERROR), Ok(0));
    assert_eq!(net.call("closesocket", &[listener], ERROR), Ok(0));
}

#[test]
fn loopback_tcp_refuses_what_it_cannot_reach() {
    let mut net = Net::new();
    let listener = net.stream();
    assert_eq!(net.bind(listener, 5001), Ok(0));
    assert_eq!(net.call("listen", &[listener, 1], ERROR), Ok(0));

    // Nothing listens on 5002; external addresses never leave the process.
    let client = net.stream();
    assert_eq!(
        net.connect(client, [127, 0, 0, 1], 5002),
        Err(WSAECONNREFUSED)
    );
    assert_eq!(net.connect(client, [10, 0, 0, 1], 5001), Err(WSAEACCES));
    assert_eq!(net.send(client, b"x"), Err(WSAENOTCONN));
    assert_eq!(net.recv(client, 4), Err(WSAENOTCONN));

    // A backlog of one holds one waiting connection.
    assert_eq!(net.connect(client, [127, 0, 0, 1], 5001), Ok(0));
    let second = net.stream();
    assert_eq!(
        net.connect(second, [127, 0, 0, 1], 5001),
        Err(WSAECONNREFUSED)
    );
    // Data sent before accept waits for the accepted socket.
    assert_eq!(net.send(client, b"early"), Ok(5));
    let (server, _) = net.accept(listener).unwrap();
    assert_eq!(net.recv(server, 64).unwrap(), b"early");
    assert_eq!(net.connect(second, [127, 0, 0, 1], 5001), Ok(0));

    // Closing a listener closes connections it has not accepted.
    assert_eq!(net.call("closesocket", &[listener], ERROR), Ok(0));
    assert_eq!(net.recv(second, 64).unwrap(), b"");
    assert_eq!(net.send(second, b"x"), Err(WSAECONNRESET));
    // The accepted connection is unaffected.
    assert_eq!(net.send(server, b"ok"), Ok(2));
    assert_eq!(net.recv(client, 64).unwrap(), b"ok");

    // TCP and UDP ports are separate, and each keeps its own operations.
    let datagram = net.call("socket", &[2, 2, 17], INVALID_SOCKET).unwrap();
    assert_eq!(net.bind(datagram, 5001), Ok(0));
    assert_eq!(
        net.call("listen", &[datagram, 1], ERROR),
        Err(WSAEOPNOTSUPP)
    );
    assert_eq!(
        net.call("shutdown", &[datagram, 1], ERROR),
        Err(WSAEOPNOTSUPP)
    );
    net.0.memory.write_u32(LENGTH, 4).unwrap();
    assert_eq!(
        net.call(
            "getsockopt",
            &[client, 0xffff, 0x1008, VALUE, LENGTH],
            ERROR
        ),
        Ok(0)
    );
    assert_eq!(net.0.u32_at(VALUE), 1); // SOCK_STREAM
    net.put_address([127, 0, 0, 1], 5001);
    assert_eq!(
        net.call("sendto", &[client, BUFFER, 1, 0, ADDRESS, 16], ERROR),
        Err(WSAEOPNOTSUPP)
    );
    // Mismatched protocols are refused.
    assert_eq!(
        net.call("socket", &[2, 1, 17], INVALID_SOCKET),
        Err(10043) // WSAEPROTONOSUPPORT
    );
}

/// ioctlsocket FIONREAD reports unread stream bytes and the size of the next
/// datagram; FIONBIO accepts non-blocking mode only, since sockets here never
/// block. Unknown commands and bad pointers fail.
#[test]
fn ioctlsocket_reports_readable_bytes_and_non_blocking_mode() {
    const FIONREAD: u64 = 0x4004_667f;
    const FIONBIO: u64 = 0x8004_667e;
    let mut net = Net::new();
    let listener = net.stream();
    assert_eq!(net.bind(listener, 5003), Ok(0));
    assert_eq!(net.call("listen", &[listener, 1], ERROR), Ok(0));
    let client = net.stream();
    assert_eq!(net.connect(client, [127, 0, 0, 1], 5003), Ok(0));
    let (server, _) = net.accept(listener).unwrap();
    let readable = |net: &mut Net, socket: u64| {
        net.0.memory.write_u32(VALUE, 0xdead_beef).unwrap();
        net.call("ioctlsocket", &[socket, FIONREAD, VALUE], ERROR)
            .map(|_| net.0.u32_at(VALUE))
    };
    assert_eq!(readable(&mut net, server), Ok(0));
    assert_eq!(net.send(client, b"abc"), Ok(3));
    assert_eq!(net.send(client, b"de"), Ok(2));
    assert_eq!(readable(&mut net, server), Ok(5));
    assert_eq!(net.recv(server, 4).unwrap(), b"abcd");
    assert_eq!(readable(&mut net, server), Ok(1));
    assert_eq!(readable(&mut net, listener), Err(WSAEINVAL));

    let receiver = net.call("socket", &[2, 2, 17], INVALID_SOCKET).unwrap();
    let sender = net.call("socket", &[2, 2, 17], INVALID_SOCKET).unwrap();
    assert_eq!(net.bind(receiver, 5004), Ok(0));
    assert_eq!(net.connect(sender, [127, 0, 0, 1], 5004), Ok(0));
    assert_eq!(readable(&mut net, receiver), Ok(0));
    assert_eq!(net.send(sender, b"seven!!"), Ok(7));
    assert_eq!(net.send(sender, b"two"), Ok(3));
    assert_eq!(readable(&mut net, receiver), Ok(7));

    net.0.memory.write_u32(VALUE, 1).unwrap();
    assert_eq!(
        net.call("ioctlsocket", &[server, FIONBIO, VALUE], ERROR),
        Ok(0)
    );
    net.0.memory.write_u32(VALUE, 0).unwrap();
    assert_eq!(
        net.call("ioctlsocket", &[server, FIONBIO, VALUE], ERROR),
        Err(WSAEOPNOTSUPP)
    );
    assert_eq!(
        net.call("ioctlsocket", &[server, FIONREAD, 0], ERROR),
        Err(10014)
    );
    assert_eq!(
        net.call("ioctlsocket", &[server, 0x1234, VALUE], ERROR),
        Err(WSAEINVAL)
    );
    assert_eq!(
        net.call("ioctlsocket", &[0x77, FIONREAD, VALUE], ERROR),
        Err(10038)
    );
}
