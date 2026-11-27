// SPDX-License-Identifier: GPL-3.0-or-later

//! The publicly documented XNetworking connectivity-hint query.
//!
//! Xenora's network machine is deliberately narrower than a host or console
//! network stack: it provides process-local loopback sockets and no external
//! network route. The query therefore reports an initialized networking
//! facility with no device-wide connectivity level, unknown cost and the IANA
//! software-loopback interface type. It never claims Internet access.

use super::ERROR_INVALID_PARAMETER;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::runtime::HResult;

const MODULE: &str = "xgameruntime.dll";
const LEVEL_NONE: u32 = 1;
const COST_UNKNOWN: u32 = 0;
const IANA_SOFTWARE_LOOPBACK: u32 = 24;

/// Register the implemented query. Registration and change-notification
/// functions, security-information queries and configuration settings remain
/// separate unsupported surfaces.
pub(super) fn register(registry: &mut ImportRegistry) {
    registry.register_with_status(
        MODULE,
        "XNetworkingGetConnectivityHint",
        ServiceStatus::Implemented,
        |args, _, memory| {
            let output = args.rcx;
            if output == 0 {
                return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
            }
            let mut packed = [0_u8; 16];
            packed[0..4].copy_from_slice(&LEVEL_NONE.to_le_bytes());
            packed[4..8].copy_from_slice(&COST_UNKNOWN.to_le_bytes());
            packed[8..12].copy_from_slice(&IANA_SOFTWARE_LOOPBACK.to_le_bytes());
            packed[12] = 1; // networkInitialized: the bounded loopback facility exists.
            // approachingDataLimit, overDataLimit and roaming stay false: the
            // machine has no data plan, no roaming interface and no external route.
            if memory.write_bytes(output, &packed).is_err() {
                return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
            }
            Ok(HResult::OK.guest_return())
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuState;
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::{GuestMemory, MemoryPermissions};

    fn call(memory: &mut GuestMemory, output: u64) -> u64 {
        let mut registry = ImportRegistry::new();
        register(&mut registry);
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, output);
        let descriptor = registry
            .resolve(
                MODULE,
                &ImportName::Name("XNetworkingGetConnectivityHint".to_owned()),
            )
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    }

    #[test]
    fn connectivity_hint_reports_initialized_loopback_without_internet() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_bytes(0x1200, &[0xaa; 16]).unwrap();

        assert_eq!(call(&mut memory, 0x1200), 0);
        assert_eq!(memory.read_u32(0x1200).unwrap(), 1, "connectivity None");
        assert_eq!(memory.read_u32(0x1204).unwrap(), 0, "cost Unknown");
        assert_eq!(
            memory.read_u32(0x1208).unwrap(),
            24,
            "IANA software loopback"
        );
        assert_eq!(memory.read_u8(0x120c).unwrap(), 1, "network initialized");
        assert_eq!(memory.read_u8(0x120d).unwrap(), 0, "no data-limit warning");
        assert_eq!(memory.read_u8(0x120e).unwrap(), 0, "not over a data limit");
        assert_eq!(memory.read_u8(0x120f).unwrap(), 0, "not roaming");
    }

    #[test]
    fn null_or_unwritable_output_fails_without_claiming_success() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();

        assert_eq!(call(&mut memory, 0), 0x8007_0057);
        assert_eq!(call(&mut memory, 0x5000), 0x8007_0057);
        assert_eq!(call(&mut memory, 0x1ff8), 0x8007_0057);
    }

    #[test]
    fn concurrent_queries_write_independent_guest_buffers() {
        use std::sync::{Arc, Barrier};

        let barrier = Arc::new(Barrier::new(4));
        let mut threads = Vec::new();
        for _ in 0..4_u64 {
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let mut memory = GuestMemory::new();
                memory
                    .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
                    .unwrap();
                barrier.wait();
                let result = call(&mut memory, 0x1100);
                (
                    result,
                    memory.read_u32(0x1100).unwrap(),
                    memory.read_u8(0x110c).unwrap(),
                )
            }));
        }
        for thread in threads {
            let (result, level, initialized) = thread.join().unwrap();
            assert_eq!(result, 0);
            assert_eq!(level, 1);
            assert_eq!(initialized, 1);
        }
    }
}
