// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use xenora_loader::cpu::ExitReason;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

const PE: &[u8] =
    include_bytes!("../../../fixtures/pe/winhttp-indexed-headers/winhttp-indexed-headers.exe");
const TIMEOUT: Duration = Duration::from_secs(5);

fn receive_headers(listener: TcpListener) -> Vec<String> {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("receive the guest's HTTP connection: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();

    let mut lines = Vec::new();
    let mut reader = BufReader::new(&mut stream);
    loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "incomplete request"
        );
        if line == "\r\n" {
            break;
        }
        lines.push(line.strip_suffix("\r\n").expect("CRLF line").to_owned());
        assert!(lines.len() <= 256, "too many request headers");
    }
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .unwrap();
    lines
}

#[test]
fn original_guest_replaces_and_removes_only_selected_wire_occurrences() {
    let report = probe_pe_bytes(PE, &ProbeConfig::default()).unwrap();
    assert_eq!(report.import_error, None);
    let symbol = ImportName::Name("WinHttpAddRequestHeaders".into());
    assert_eq!(
        report
            .imports
            .iter()
            .filter(|entry| entry.import_name == symbol)
            .map(|entry| entry.dll.as_str())
            .collect::<Vec<_>>(),
        ["winhttp.dll"]
    );

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    state
        .set_program_arguments(&["winhttp-indexed-headers.exe", &port])
        .unwrap();
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let server = thread::spawn(move || receive_headers(listener));
    let result = run_synthetic_startup(PE, layout, registry, 100_000).unwrap();
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "a nonzero guest exit identifies the failing fixture source line"
    );
    let lines = server.join().expect("read the complete loopback request");
    assert_eq!(lines[0], "GET /indexed HTTP/1.1");
    let test_headers: Vec<_> = lines
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|line| line.starts_with("X-") || line.starts_with("x-"))
        .collect();
    assert_eq!(
        test_headers,
        [
            "X-List: default",
            "X-Unrelated: stays",
            "x-list: replacement",
            "X-List: fourth",
            "X-Added: fallback",
            "X-Control: one, two",
        ],
    );
}
