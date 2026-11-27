// SPDX-License-Identifier: GPL-3.0-or-later

//! Winsock AMD64 ordinal imports through an original PE32+ ordinal IAT.

mod support;

use std::sync::Arc;
use support::*;
use xenora_loader::cpu::{CpuState, ExitReason, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::{ImportError, ImportRegistry};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::pe::ParsedPe;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

const MODULE: &str = "ws2_32.dll";
const ORDINALS: [(&str, u16); 21] = [
    ("accept", 1),
    ("bind", 2),
    ("closesocket", 3),
    ("connect", 4),
    ("getsockname", 6),
    ("getsockopt", 7),
    ("htons", 9),
    ("inet_addr", 11),
    ("listen", 13),
    ("recv", 16),
    ("recvfrom", 17),
    ("select", 18),
    ("send", 19),
    ("sendto", 20),
    ("setsockopt", 21),
    ("shutdown", 22),
    ("socket", 23),
    ("gethostname", 57),
    ("WSAGetLastError", 111),
    ("WSAStartup", 115),
    ("__WSAFDIsSet", 151),
];

fn registry() -> ImportRegistry {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    registry
}

fn loaded_ordinal_guest() -> (Vec<u8>, xenora_loader::pe::ParsedPe, GuestMemory) {
    let imports: Vec<_> = ORDINALS
        .iter()
        .map(|(_, ordinal)| (MODULE, *ordinal))
        .collect();
    let (bytes, _) = build_ordinal_image(&imports);
    let pe = ParsedPe::parse(&bytes).unwrap();
    let mut memory = pe.load_into_memory(&bytes).unwrap().memory;
    memory
        .map_region(SCRATCH, SCRATCH_LEN as usize, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory
        .map_region(STACK, 0x1000, MemoryPermissions::READ_WRITE)
        .unwrap();
    (bytes, pe, memory)
}

fn bind_error(bytes: &[u8]) -> ImportError {
    let pe = ParsedPe::parse(bytes).unwrap();
    let mut memory = pe.load_into_memory(bytes).unwrap().memory;
    registry()
        .bind_imports(&pe, bytes, BASE, &mut memory)
        .unwrap_err()
}

#[test]
fn ordinal_lookup_aliases_the_named_implementations() {
    let registry = registry();
    for (name, ordinal) in ORDINALS {
        let named = registry
            .resolve(MODULE, &ImportName::Name(name.to_owned()))
            .unwrap_or_else(|| panic!("{name} should be registered"));
        let by_ordinal = registry
            .resolve(MODULE, &ImportName::Ordinal(ordinal))
            .unwrap_or_else(|| panic!("ordinal {ordinal} should resolve"));
        assert!(
            std::ptr::eq(named, by_ordinal),
            "ordinal {ordinal} should alias {name}"
        );
    }

    // A literal export-looking name is not ordinal syntax. Ordinals whose
    // named implementations do not exist, and undocumented ordinals, do not
    // resolve.
    assert!(
        registry
            .resolve(MODULE, &ImportName::Name("Ordinal#7".to_owned()))
            .is_none()
    );
    for ordinal in [8, 10, 52] {
        assert!(
            registry
                .resolve(MODULE, &ImportName::Ordinal(ordinal))
                .is_none(),
            "ordinal {ordinal} should remain unresolved"
        );
    }
}

#[test]
fn ordinal_iat_entries_bind_and_run_representative_socket_calls() {
    let (bytes, pe, mut memory) = loaded_ordinal_guest();
    let mut registry = registry();
    assert_eq!(
        registry
            .bind_imports(&pe, &bytes, BASE, &mut memory)
            .unwrap(),
        ORDINALS.len()
    );
    let mut engine = DbtEngine::new(256 * 1024).unwrap();
    engine.set_import_registry(registry);
    let mut call = |memory: &mut GuestMemory, ordinal: u16, args: &[u64]| {
        let index = ORDINALS
            .iter()
            .position(|(_, number)| *number == ordinal)
            .unwrap() as u64;
        let mut cpu = CpuState::new(BASE + u64::from(CODE_RVA) + 16 * index, STACK_TOP);
        for (register, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(register, value);
        }
        let result = engine.run(&mut cpu, memory, 8);
        assert_eq!(result.exit_reason, ExitReason::Halt, "ordinal {ordinal}");
        cpu.get(Register::Rax)
    };

    assert_eq!(call(&mut memory, 115, &[0x0202, SCRATCH]), 0);
    assert_eq!(call(&mut memory, 9, &[5000]), 0x8813);

    // Exercise a real loopback datagram through ordinal-imported slots, with
    // the sockaddr_in bytes written directly in network order.
    let address = SCRATCH + 0x200;
    let outgoing = SCRATCH + 0x220;
    let incoming = SCRATCH + 0x240;
    let address_length = SCRATCH + 0x2b0;
    memory.write_u16(address, 2).unwrap();
    memory.write_u16(address + 2, 0).unwrap();
    memory.write_u32(address + 4, 0x0100_007f).unwrap();
    memory.write_bytes(outgoing, b"ordinal").unwrap();
    let receiver = call(&mut memory, 23, &[2, 2, 17]);
    let sender = call(&mut memory, 23, &[2, 2, 17]);
    assert_ne!(receiver, u64::MAX);
    assert_ne!(sender, u64::MAX);
    assert_eq!(call(&mut memory, 2, &[receiver, address, 16]), 0);
    memory.write_u32(address_length, 16).unwrap();
    assert_eq!(
        call(&mut memory, 6, &[receiver, address, address_length]),
        0
    );
    assert_eq!(call(&mut memory, 4, &[sender, address, 16]), 0);
    assert_eq!(call(&mut memory, 19, &[sender, outgoing, 7, 0]), 7);
    assert_eq!(call(&mut memory, 16, &[receiver, incoming, 16, 0]), 7);
    let mut received = [0; 7];
    memory.read_bytes(incoming, &mut received).unwrap();
    assert_eq!(&received, b"ordinal");
    assert_eq!(call(&mut memory, 3, &[sender]), 0);
    assert_eq!(call(&mut memory, 3, &[receiver]), 0);
    assert_eq!(call(&mut memory, 111, &[]), 0);
}

#[test]
fn ordinal_iat_entries_without_named_implementations_stay_missing() {
    for ordinal in [8, 10, 52] {
        let (bytes, _) = build_ordinal_image(&[(MODULE, ordinal)]);
        assert!(
            matches!(bind_error(&bytes), ImportError::UnresolvedImport { .. }),
            "ordinal {ordinal} should remain unresolved"
        );
    }

    let (bytes, _) = build_image(&[(MODULE, "Ordinal#7")], &[]);
    assert!(
        matches!(bind_error(&bytes), ImportError::UnresolvedImport { .. }),
        "the literal name Ordinal#7 must remain distinct from ordinal 7"
    );
}
