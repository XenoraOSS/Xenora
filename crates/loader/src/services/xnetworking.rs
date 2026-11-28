// SPDX-License-Identifier: GPL-3.0-or-later

//! The publicly documented XNetworking connectivity-hint and statistics
//! queries.
//!
//! Xenora's network machine is deliberately narrower than a host or console
//! network stack: it provides process-local loopback sockets and no external
//! network route. The connectivity query therefore reports an initialized
//! networking facility with no device-wide connectivity level, unknown cost
//! and the IANA software-loopback interface type. It never claims Internet
//! access. The machine also has no kernel-mode TCP queued-receive-buffer
//! allocator, so the documented statistics counters are honestly zero.

use super::ERROR_INVALID_PARAMETER;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::runtime::HResult;

const MODULE: &str = "xgameruntime.dll";
const LEVEL_NONE: u32 = 1;
const COST_UNKNOWN: u32 = 0;
const IANA_SOFTWARE_LOOPBACK: u32 = 24;
const MAX_STATISTICS_TYPE: u32 = 2;
const MAX_CONFIGURATION_SETTING: u32 = 2;
const TCP_QUEUED_RECEIVE_STATISTICS_SIZE: usize = 40;
/// The documented default preferred UDP multiplayer port. Xenora's bounded
/// network machine has no external route, UPnP negotiation or user override,
/// so this fixed value is the exact preference of its loopback machine rather
/// than Internet or NAT evidence.
const PREFERRED_LOCAL_UDP_MULTIPLAYER_PORT: u16 = 3074;

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

    // All three currently documented statistics types share the same output
    // structure. Xenora does not implement the kernel-mode TCP buffering
    // facility those counters describe, so zero is the truthful value for
    // current, peak, total and dropped byte counts.
    registry.register_with_status(
        MODULE,
        "XNetworkingQueryStatistics",
        ServiceStatus::Implemented,
        |args, _, memory| {
            let statistics_type = args.rcx as u32;
            let output = args.rdx;
            if statistics_type > MAX_STATISTICS_TYPE || output == 0 {
                return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
            }
            let zeroed = [0_u8; TCP_QUEUED_RECEIVE_STATISTICS_SIZE];
            if memory.write_bytes(output, &zeroed).is_err() {
                return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
            }
            Ok(HResult::OK.guest_return())
        },
    );

    // The documented configuration query is read-only here. Zero is not a
    // claimed Xbox default: it is the exact limit of this machine, which has
    // no configurable kernel-mode TCP queued-receive-buffer allocator.
    registry.register_with_status(
        MODULE,
        "XNetworkingQueryConfigurationSetting",
        ServiceStatus::Implemented,
        |args, _, memory| {
            let setting = args.rcx as u32;
            let output = args.rdx;
            if setting > MAX_CONFIGURATION_SETTING || output == 0 {
                return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
            }
            if memory.write_u64(output, 0).is_err() {
                return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
            }
            Ok(HResult::OK.guest_return())
        },
    );

    // The synchronous preferred-port query. The fixed value is meaningful for
    // a UDP bind inside Xenora's loopback network only: no router traversal,
    // NAT type or external reachability follows from it.
    registry.register_with_status(
        MODULE,
        "XNetworkingQueryPreferredLocalUdpMultiplayerPort",
        ServiceStatus::Implemented,
        |args, _, memory| {
            let output = args.rcx;
            if output == 0
                || memory
                    .write_u16(output, PREFERRED_LOCAL_UDP_MULTIPLAYER_PORT)
                    .is_err()
            {
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

    fn call_statistics(memory: &mut GuestMemory, statistics_type: u64, output: u64) -> u64 {
        let mut registry = ImportRegistry::new();
        register(&mut registry);
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, statistics_type);
        cpu.set(crate::cpu::Register::Rdx, output);
        let descriptor = registry
            .resolve(
                MODULE,
                &ImportName::Name("XNetworkingQueryStatistics".to_owned()),
            )
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    }

    fn call_configuration(memory: &mut GuestMemory, setting: u64, output: u64) -> u64 {
        let mut registry = ImportRegistry::new();
        register(&mut registry);
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, setting);
        cpu.set(crate::cpu::Register::Rdx, output);
        let descriptor = registry
            .resolve(
                MODULE,
                &ImportName::Name("XNetworkingQueryConfigurationSetting".to_owned()),
            )
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    }

    fn call_preferred_port(memory: &mut GuestMemory, output: u64) -> u64 {
        let mut registry = ImportRegistry::new();
        register(&mut registry);
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, output);
        let descriptor = registry
            .resolve(
                MODULE,
                &ImportName::Name("XNetworkingQueryPreferredLocalUdpMultiplayerPort".to_owned()),
            )
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    }

    #[test]
    fn preferred_udp_port_is_the_fixed_loopback_default_and_writes_two_bytes() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_bytes(0x1200, &[0xaa; 8]).unwrap();

        assert_eq!(call_preferred_port(&mut memory, 0x1200), 0);
        assert_eq!(memory.read_u16(0x1200).unwrap(), 3074);
        let mut tail = [0; 6];
        memory.read_bytes(0x1202, &mut tail).unwrap();
        assert_eq!(tail, [0xaa; 6]);
    }

    #[test]
    fn preferred_udp_port_rejects_invalid_output_without_writing() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_bytes(0x1200, &[0xaa; 8]).unwrap();

        assert_eq!(call_preferred_port(&mut memory, 0), 0x8007_0057);
        assert_eq!(call_preferred_port(&mut memory, 0x5000), 0x8007_0057);
        assert_eq!(call_preferred_port(&mut memory, 0x1fff), 0x8007_0057);
        let mut unchanged = [0; 8];
        memory.read_bytes(0x1200, &mut unchanged).unwrap();
        assert_eq!(unchanged, [0xaa; 8]);
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
    fn statistics_queries_zero_exact_structure_and_preserve_union_tail() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();

        for statistics_type in 0..=2_u64 {
            memory.write_bytes(0x1200, &[0xaa; 64]).unwrap();
            assert_eq!(call_statistics(&mut memory, statistics_type, 0x1200), 0);
            let mut written = [0xaa; 40];
            memory.read_bytes(0x1200, &mut written).unwrap();
            assert_eq!(written, [0; 40], "type {statistics_type} counters");
            let mut tail = [0; 24];
            memory.read_bytes(0x1228, &mut tail).unwrap();
            assert_eq!(tail, [0xaa; 24], "bytes beyond the structure");
        }
    }

    #[test]
    fn statistics_rejects_unknown_types_and_invalid_output() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_bytes(0x1200, &[0xaa; 64]).unwrap();

        assert_eq!(call_statistics(&mut memory, 3, 0x1200), 0x8007_0057);
        assert_eq!(call_statistics(&mut memory, u64::MAX, 0x1200), 0x8007_0057);
        let mut unchanged = [0; 40];
        memory.read_bytes(0x1200, &mut unchanged).unwrap();
        assert_eq!(unchanged, [0xaa; 40]);
        assert_eq!(call_statistics(&mut memory, 0, 0), 0x8007_0057);
        assert_eq!(call_statistics(&mut memory, 0, 0x5000), 0x8007_0057);
        assert_eq!(call_statistics(&mut memory, 0, 0x1ff8), 0x8007_0057);
    }

    #[test]
    fn repeated_concurrent_statistics_queries_are_independent() {
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
                let mut results = Vec::new();
                for statistics_type in 0..=2_u64 {
                    memory.write_bytes(0x1100, &[0xaa; 40]).unwrap();
                    results.push((
                        call_statistics(&mut memory, statistics_type, 0x1100),
                        memory.read_u64(0x1100).unwrap(),
                        memory.read_u64(0x1120).unwrap(),
                    ));
                }
                results
            }));
        }
        for thread in threads {
            for (result, first_counter, last_counter) in thread.join().unwrap() {
                assert_eq!(result, 0);
                assert_eq!(first_counter, 0);
                assert_eq!(last_counter, 0);
            }
        }
    }

    #[test]
    fn configuration_queries_report_zero_and_preserve_neighbor_bytes() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();

        for setting in 0..=2_u64 {
            memory.write_bytes(0x1200, &[0xaa; 24]).unwrap();
            assert_eq!(call_configuration(&mut memory, setting, 0x1200), 0);
            assert_eq!(memory.read_u64(0x1200).unwrap(), 0);
            assert_eq!(memory.read_u64(0x1208).unwrap(), 0xaaaa_aaaa_aaaa_aaaa);
            assert_eq!(memory.read_u64(0x1210).unwrap(), 0xaaaa_aaaa_aaaa_aaaa);
        }

        for _ in 0..8 {
            assert_eq!(call_configuration(&mut memory, 0, 0x1200), 0);
        }
    }

    #[test]
    fn configuration_rejects_unknown_settings_and_invalid_output() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_u64(0x1200, u64::MAX).unwrap();

        assert_eq!(call_configuration(&mut memory, 3, 0x1200), 0x8007_0057);
        assert_eq!(
            call_configuration(&mut memory, u64::MAX, 0x1200),
            0x8007_0057
        );
        assert_eq!(memory.read_u64(0x1200).unwrap(), u64::MAX);
        assert_eq!(call_configuration(&mut memory, 0, 0), 0x8007_0057);
        assert_eq!(call_configuration(&mut memory, 0, 0x5000), 0x8007_0057);
        assert_eq!(call_configuration(&mut memory, 0, 0x1ffc), 0x8007_0057);
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
