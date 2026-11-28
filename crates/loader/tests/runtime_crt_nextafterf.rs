// SPDX-License-Identifier: GPL-3.0-or-later

//! Original AMD64 PE calls the exact UCRT math API-set import.

mod support;

use support::{Guest, SCRATCH, import_slot, routine};
use xenora_loader::cpu::ExitReason;

const IMPORTS: &[(&str, &str)] = &[
    ("api-ms-win-crt-math-l1-1-0.dll", "nextafterf"),
    ("api-ms-win-crt-runtime-l1-1-0.dll", "_errno"),
];
const ERANGE: u32 = 34;
const ERRNO_SENTINEL: u32 = 22;

fn guest() -> Guest {
    let mut code = vec![0x48, 0xB8]; // mov rax, argument address
    code.extend(SCRATCH.to_le_bytes());
    code.extend([
        0xF3, 0x0F, 0x10, 0x00, // movss xmm0, [rax]
        0xF3, 0x0F, 0x10, 0x48, 0x04, // movss xmm1, [rax+4]
        0x48, 0x83, 0xEC, 0x20, // sub rsp, 32 (Win64 home area)
        0x48, 0xB8, // mov rax, nextafterf IAT slot address
    ]);
    code.extend(import_slot(IMPORTS, 0).to_le_bytes());
    code.extend([
        0xFF, 0x10, // call [rax]
        0x48, 0x83, 0xC4, 0x20, // add rsp, 32
        0xF4, // hlt
    ]);
    Guest::new(IMPORTS, &[code])
}

fn check(guest: &mut Guest, x: f32, y: f32, expected_bits: u32, expected_errno: u32) {
    let errno = guest.call("_errno", &[]);
    guest.memory.write_u32(errno, ERRNO_SENTINEL).unwrap();
    guest.memory.write_u32(SCRATCH, x.to_bits()).unwrap();
    guest.memory.write_u32(SCRATCH + 4, y.to_bits()).unwrap();
    let (cpu, exit) = guest.execute(routine(0), 32);
    assert_eq!(exit, ExitReason::Halt);
    assert_eq!(cpu.xmm[0] as u32, expected_bits, "nextafterf({x}, {y})");
    assert_eq!(
        guest.u32_at(errno),
        expected_errno,
        "errno after nextafterf({x}, {y})"
    );
}

#[test]
fn nextafterf_sets_erange_when_finite_inputs_step_to_infinity() {
    let mut guest = guest();
    check(&mut guest, f32::MAX, f32::INFINITY, 0x7F80_0000, ERANGE);
    check(
        &mut guest,
        -f32::MAX,
        f32::NEG_INFINITY,
        0xFF80_0000,
        ERANGE,
    );
}

#[test]
fn nextafterf_preserves_errno_when_no_overflow_occurs() {
    let mut guest = guest();
    for (x, y, expected) in [
        (1.0, 2.0, 0x3F80_0001),
        (f32::MAX, f32::MAX, 0x7F7F_FFFF),
        (f32::INFINITY, f32::INFINITY, 0x7F80_0000),
        (f32::INFINITY, 0.0, 0x7F7F_FFFF),
        (0.0, -0.0, 0x8000_0000),
        (0.0, 1.0, 1),
        (f32::from_bits(1), -1.0, 0),
    ] {
        check(&mut guest, x, y, expected, ERRNO_SENTINEL);
    }
}
