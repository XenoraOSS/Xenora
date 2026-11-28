// SPDX-License-Identifier: GPL-3.0-or-later

//! PE-bound checks for the BCrypt RNG subset.
//!
//! The parameter widths, provider names, flags, and handle rules come from
//! Microsoft Learn's BCryptOpenAlgorithmProvider, BCryptGenRandom,
//! BCryptCloseAlgorithmProvider, and CNG algorithm pseudo-handles references.
//! NTSTATUS bit patterns come from MS-ERREF.

mod support;

use std::sync::Arc;
use support::{BASE, CODE_RVA, Guest, SCRATCH, SCRATCH_LEN, THREAD_A};
use xenora_loader::cpu::{CpuState, ExitReason};
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::{ImportRegistry, Win64Args};
use xenora_loader::services::register_standard_services;

const BCRYPT: &str = "bcrypt.dll";
const KERNEL32: &str = "kernel32.dll";
const OPEN: &str = "BCryptOpenAlgorithmProvider";
const GENERATE: &str = "BCryptGenRandom";
const CLOSE: &str = "BCryptCloseAlgorithmProvider";
const IMPORTS: &[(&str, &str)] = &[
    (BCRYPT, OPEN),
    (BCRYPT, GENERATE),
    (BCRYPT, CLOSE),
    (KERNEL32, "SetLastError"),
    (KERNEL32, "GetLastError"),
    (KERNEL32, "CreateEventW"),
    (KERNEL32, "CloseHandle"),
    (KERNEL32, "ExitProcess"),
];

const STATUS_SUCCESS: u32 = 0;
const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_NO_MEMORY: u32 = 0xC000_0017;
const STATUS_NOT_SUPPORTED: u32 = 0xC000_00BB;
const STATUS_PROCESS_IS_TERMINATING: u32 = 0xC000_010A;
const STATUS_NOT_FOUND: u32 = 0xC000_0225;

const RNG_USE_ENTROPY_IN_BUFFER: u64 = 1;
const USE_SYSTEM_PREFERRED_RNG: u64 = 2;
const RNG_ALG_HANDLE: u64 = 0x81;
const UNKNOWN_FLAG: u64 = 0x8000_0000;

const HANDLE_OUT: u64 = SCRATCH + 8;
const RNG_NAME: u64 = SCRATCH + 0x40;
const PROVIDER_NAME: u64 = SCRATCH + 0x80;
const UNKNOWN_NAME: u64 = SCRATCH + 0xC0;
const BUFFER: u64 = SCRATCH + 0x100;
const SCRATCH_END: u64 = SCRATCH + SCRATCH_LEN;
const READ_ONLY: u64 = BASE + CODE_RVA as u64;
const LAST_ERROR: u64 = 0x1234_ABCD;

fn guest() -> Guest {
    let mut guest = Guest::new(IMPORTS, &[]);
    for (address, name) in [
        (RNG_NAME, "RNG"),
        (PROVIDER_NAME, "Microsoft Primitive Provider"),
        (UNKNOWN_NAME, "XenoraUnknown"),
    ] {
        let bytes: Vec<_> = name
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        guest.memory.write_bytes(address, &bytes).unwrap();
    }
    guest
}

fn expect_status(guest: &mut Guest, symbol: &str, args: &[u64], expected: u32) {
    guest.call("SetLastError", &[LAST_ERROR]);
    assert_eq!(
        guest.call(symbol, args) as u32,
        expected,
        "{symbol}{args:X?}"
    );
    assert_eq!(guest.error(), LAST_ERROR, "{symbol}{args:X?}");
}

fn read<const SIZE: usize>(guest: &Guest, address: u64) -> [u8; SIZE] {
    let mut bytes = [0; SIZE];
    guest.memory.read_bytes(address, &mut bytes).unwrap();
    bytes
}

fn open(guest: &mut Guest, provider_name: u64) -> u64 {
    const BEFORE: u64 = 0xA5A5_A5A5_A5A5_A5A5;
    guest
        .memory
        .write_bytes(HANDLE_OUT - 1, &[0xB3; 10])
        .unwrap();
    guest.memory.write_u64(HANDLE_OUT, BEFORE).unwrap();
    expect_status(
        guest,
        OPEN,
        &[HANDLE_OUT, RNG_NAME, provider_name, 0],
        STATUS_SUCCESS,
    );
    let handle = guest.u64_at(HANDLE_OUT);
    assert!(![0, BEFORE, RNG_ALG_HANDLE].contains(&handle));
    assert_eq!(read::<1>(guest, HANDLE_OUT - 1), [0xB3]);
    assert_eq!(read::<1>(guest, HANDLE_OUT + 8), [0xB3]);
    handle
}

fn check_output(guest: &mut Guest, handle: u64, flags: u64) {
    const SIZE: usize = 63;
    const INITIAL: [u8; SIZE + 2] = [0xA6; SIZE + 2];

    // Observe each byte being written across repeated draws. Any one random
    // byte may equal the sentinel. This is a smoke check for write coverage.
    let mut touched = [false; SIZE];
    for _ in 0..16 {
        guest.memory.write_bytes(BUFFER - 1, &INITIAL).unwrap();
        expect_status(
            guest,
            GENERATE,
            &[handle, BUFFER, SIZE as u64, flags],
            STATUS_SUCCESS,
        );
        let actual = read::<{ SIZE + 2 }>(guest, BUFFER - 1);
        assert_eq!(actual[0], INITIAL[0]);
        assert_eq!(actual[SIZE + 1], INITIAL[SIZE + 1]);
        for (written, output) in touched.iter_mut().zip(&actual[1..=SIZE]) {
            *written |= *output != INITIAL[0];
        }
    }
    assert!(touched.into_iter().all(|written| written));
    let before = read::<{ SIZE + 2 }>(guest, BUFFER - 1);
    expect_status(guest, GENERATE, &[handle, BUFFER, 0, flags], STATUS_SUCCESS);
    assert_eq!(read::<{ SIZE + 2 }>(guest, BUFFER - 1), before);
}

#[test]
fn default_and_named_rng_providers_have_independent_lifetimes() {
    let mut guest = guest();
    let default = open(&mut guest, 0);
    let named = open(&mut guest, PROVIDER_NAME);
    assert_ne!(default, named);

    expect_status(
        &mut guest,
        CLOSE,
        &[default, UNKNOWN_FLAG],
        STATUS_INVALID_PARAMETER,
    );
    check_output(&mut guest, default, 0);
    expect_status(&mut guest, CLOSE, &[default, 0], STATUS_SUCCESS);

    check_output(&mut guest, named, RNG_USE_ENTROPY_IN_BUFFER);
    expect_status(&mut guest, CLOSE, &[named, 0], STATUS_SUCCESS);

    let before = read::<64>(&guest, BUFFER);
    for stale in [default, named] {
        expect_status(
            &mut guest,
            GENERATE,
            &[stale, BUFFER, 64, 0],
            STATUS_INVALID_HANDLE,
        );
        expect_status(&mut guest, CLOSE, &[stale, 0], STATUS_INVALID_HANDLE);
    }
    assert_eq!(read::<64>(&guest, BUFFER), before);
}

#[test]
fn system_preferred_rng_and_algorithm_pseudo_handle_work_without_opening() {
    let mut guest = guest();
    check_output(&mut guest, 0, USE_SYSTEM_PREFERRED_RNG);
    expect_status(
        &mut guest,
        CLOSE,
        &[RNG_ALG_HANDLE, 0],
        STATUS_INVALID_HANDLE,
    );
    check_output(&mut guest, RNG_ALG_HANDLE, 0);
}

#[test]
fn invalid_provider_arguments_leave_guest_memory_untouched() {
    let mut guest = guest();
    let before = [0x52; 16];
    guest.memory.write_bytes(HANDLE_OUT, &before).unwrap();
    let tail = SCRATCH_END - 4;
    guest.memory.write_bytes(tail, &[0x4A; 4]).unwrap();
    let code = read::<8>(&guest, READ_ONLY);

    for (args, status) in [
        ([0, RNG_NAME, 0, 0], STATUS_INVALID_PARAMETER),
        ([tail, RNG_NAME, 0, 0], STATUS_INVALID_PARAMETER),
        ([READ_ONLY, RNG_NAME, 0, 0], STATUS_INVALID_PARAMETER),
        ([HANDLE_OUT, 0, 0, 0], STATUS_INVALID_PARAMETER),
        ([HANDLE_OUT, SCRATCH_END, 0, 0], STATUS_INVALID_PARAMETER),
        ([HANDLE_OUT, RNG_NAME, tail, 0], STATUS_INVALID_PARAMETER),
        (
            [HANDLE_OUT, RNG_NAME, 0, UNKNOWN_FLAG],
            STATUS_NOT_SUPPORTED,
        ),
        ([HANDLE_OUT, UNKNOWN_NAME, 0, 0], STATUS_NOT_FOUND),
        ([HANDLE_OUT, RNG_NAME, UNKNOWN_NAME, 0], STATUS_NOT_FOUND),
    ] {
        expect_status(&mut guest, OPEN, &args, status);
        assert_eq!(read::<16>(&guest, HANDLE_OUT), before);
        assert_eq!(read::<4>(&guest, tail), [0x4A; 4]);
        assert_eq!(read::<8>(&guest, READ_ONLY), code);
    }
}

#[test]
fn invalid_generation_leaves_buffers_and_foreign_handles_untouched() {
    let mut guest = guest();
    let event = guest.call("CreateEventW", &[0, 0, 1, 0]);
    assert_ne!(event, 0);
    let provider = open(&mut guest, 0);
    let before = [0x57; 66];
    guest.memory.write_bytes(BUFFER - 1, &before).unwrap();
    let tail = SCRATCH_END - 32;
    guest.memory.write_bytes(tail, &[0x4B; 32]).unwrap();
    let code = read::<8>(&guest, READ_ONLY);

    for (args, status) in [
        ([0, BUFFER, 64, 0], STATUS_INVALID_HANDLE),
        ([event, BUFFER, 64, 0], STATUS_INVALID_HANDLE),
        (
            [provider, BUFFER, 64, USE_SYSTEM_PREFERRED_RNG],
            STATUS_INVALID_PARAMETER,
        ),
        (
            [provider, BUFFER, 64, UNKNOWN_FLAG],
            STATUS_INVALID_PARAMETER,
        ),
        ([provider, 0, 64, 0], STATUS_INVALID_PARAMETER),
        ([provider, READ_ONLY, 8, 0], STATUS_INVALID_PARAMETER),
        ([provider, tail, 33, 0], STATUS_INVALID_PARAMETER),
        (
            [provider, BUFFER, u32::MAX as u64, 0],
            STATUS_INVALID_PARAMETER,
        ),
    ] {
        expect_status(&mut guest, GENERATE, &args, status);
        assert_eq!(read::<66>(&guest, BUFFER - 1), before);
        assert_eq!(read::<32>(&guest, tail), [0x4B; 32]);
        assert_eq!(read::<8>(&guest, READ_ONLY), code);
    }
    expect_status(&mut guest, CLOSE, &[event, 0], STATUS_INVALID_HANDLE);
    assert_eq!(guest.call("CloseHandle", &[event]), 1);
    expect_status(&mut guest, CLOSE, &[provider, 0], STATUS_SUCCESS);
}

#[test]
fn rng_handles_belong_to_the_guest_that_opened_them() {
    let mut first = guest();
    let mut second = guest();
    let first_handle = open(&mut first, 0);
    let second_handle = open(&mut second, 0);
    for (guest, foreign, own) in [
        (&mut first, second_handle, first_handle),
        (&mut second, first_handle, second_handle),
    ] {
        let before = [0x9A; 64];
        guest.memory.write_bytes(BUFFER, &before).unwrap();
        expect_status(
            guest,
            GENERATE,
            &[foreign, BUFFER, 64, 0],
            STATUS_INVALID_HANDLE,
        );
        expect_status(guest, CLOSE, &[foreign, 0], STATUS_INVALID_HANDLE);
        assert_eq!(read::<64>(guest, BUFFER), before);
        expect_status(guest, GENERATE, &[own, BUFFER, 0, 0], STATUS_SUCCESS);
        expect_status(guest, CLOSE, &[own, 0], STATUS_SUCCESS);
    }
}

#[test]
fn ulong_arguments_ignore_the_upper_halves_of_win64_registers() {
    let mut guest = guest();
    const UPPER: u64 = 0xFFFF_FFFF_0000_0000;
    expect_status(
        &mut guest,
        OPEN,
        &[HANDLE_OUT, RNG_NAME, 0, UPPER],
        STATUS_SUCCESS,
    );
    let handle = guest.u64_at(HANDLE_OUT);
    assert_ne!(handle, 0);

    let before = [0xD7; 64];
    guest.memory.write_bytes(BUFFER, &before).unwrap();
    expect_status(
        &mut guest,
        GENERATE,
        &[handle, BUFFER, UPPER, UPPER],
        STATUS_SUCCESS,
    );
    assert_eq!(read::<64>(&guest, BUFFER), before);
    expect_status(
        &mut guest,
        GENERATE,
        &[0, BUFFER, UPPER | 32, UPPER | USE_SYSTEM_PREFERRED_RNG],
        STATUS_SUCCESS,
    );
    assert_eq!(read::<32>(&guest, BUFFER + 32), [0xD7; 32]);

    expect_status(&mut guest, CLOSE, &[handle, UPPER], STATUS_SUCCESS);
    expect_status(&mut guest, CLOSE, &[handle, 0], STATUS_INVALID_HANDLE);
}

#[test]
fn closing_a_provider_frees_capacity_for_another() {
    let mut guest = guest();
    // This capacity is Xenora's resource bound.
    let providers: Vec<_> = (0..64).map(|_| open(&mut guest, 0)).collect();
    let before = guest.u64_at(HANDLE_OUT);
    expect_status(
        &mut guest,
        OPEN,
        &[HANDLE_OUT, RNG_NAME, 0, 0],
        STATUS_NO_MEMORY,
    );
    assert_eq!(guest.u64_at(HANDLE_OUT), before);
    expect_status(&mut guest, CLOSE, &[providers[0], 0], STATUS_SUCCESS);
    let replacement = open(&mut guest, 0);
    assert!(!providers.contains(&replacement));
    expect_status(&mut guest, CLOSE, &[providers[0], 0], STATUS_INVALID_HANDLE);
    expect_status(&mut guest, CLOSE, &[replacement, 0], STATUS_SUCCESS);
    for provider in &providers[1..] {
        expect_status(&mut guest, CLOSE, &[*provider, 0], STATUS_SUCCESS);
    }
}

#[test]
fn bcrypt_services_reject_calls_after_guest_exit() {
    let mut guest = guest();
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&guest.state));

    let mut cpu = CpuState::new(0, 0);
    cpu.gs_base = THREAD_A;
    let mut call_service = |guest: &mut Guest, symbol: &str, [rcx, rdx, r8, r9]: [u64; 4]| {
        let mut args = Win64Args::new(&cpu);
        args.rcx = rcx;
        args.rdx = rdx;
        args.r8 = r8;
        args.r9 = r9;
        let handler = &registry
            .resolve(BCRYPT, &ImportName::Name(symbol.into()))
            .unwrap()
            .handler;
        handler(&args, &mut cpu, &mut guest.memory)
    };

    // Use the same registry for the live provider and post-exit checks.
    assert_eq!(
        call_service(&mut guest, OPEN, [HANDLE_OUT, RNG_NAME, 0, 0]),
        Ok(0)
    );
    let provider = guest.u64_at(HANDLE_OUT);
    let before = [0x6B; 64];
    guest.memory.write_bytes(BUFFER, &before).unwrap();
    guest.call("SetLastError", &[LAST_ERROR]);
    assert_eq!(guest.run("ExitProcess", &[7]).1, ExitReason::ProcessExit(7));
    assert_eq!(guest.state.exit_status(), Some(7));
    assert_eq!(
        guest.run(GENERATE, &[provider, BUFFER, 64, 0]).1,
        ExitReason::ProcessExit(7)
    );

    // The exited engine cannot dispatch further imports. Invoke the handlers
    // from the standard registry associated with the same process state.
    for (symbol, args) in [
        (OPEN, [HANDLE_OUT, RNG_NAME, 0, 0]),
        (GENERATE, [provider, BUFFER, 64, 0]),
        (CLOSE, [provider, 0, 0, 0]),
    ] {
        assert_eq!(
            call_service(&mut guest, symbol, args),
            Ok(u64::from(STATUS_PROCESS_IS_TERMINATING)),
            "{symbol}"
        );
        assert_eq!(guest.state.last_error(THREAD_A), LAST_ERROR as u32);
        assert_eq!(guest.u64_at(HANDLE_OUT), provider);
        assert_eq!(read::<64>(&guest, BUFFER), before);
    }
}
