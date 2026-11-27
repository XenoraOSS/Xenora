// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent per-lane expected values for the four constant Wave32 VOPC
//! integer compares (#215): `v_cmp_f_i32`, `v_cmp_t_i32`, `v_cmp_f_u32` and
//! `v_cmp_t_u32`, `_e32` only, under the contract in
//! `docs/shaders/vector-compare.md` (sections 2-7).
//!
//! The expectations are written below from the public AMD "RDNA 2" Instruction
//! Set Architecture Reference Guide only: section 12.9 "VOPC Instructions"
//! (Table 59 "Eight Compare Operations" and Table 61 "VOPC Compare Opcodes"),
//! section 3.9 "Vector Compares: VCC and VCCZ", and the VOPC word layout of
//! section 13.3.3. Nothing here comes from the ISA crate, the semantic
//! registry, the decoder or the shader compiler. Every case runs on the
//! reference interpreter `xenora_ref` and on the second interpreter
//! `independent_ref`; the ignored test also runs the translated MSL on native
//! Metal.
//!
//! Readings of the guide (not hardware evidence):
//! - Table 61: F is `D[threadId] = 0`, T is `D[threadId] = 1`, with "D = VCC in
//!   VOPC encoding"; Table 59 gives the same as offsets F 0 and TRU 7.
//! - Section 3.9: `V_CMP_* => VCC[n] = EXEC[n] & (test passed for thread[n])`
//!   and "VCC is fully written; there are no partial mask updates". So T writes
//!   EXEC_LO masked to the lanes that exist, F writes 0 even for active lanes,
//!   and stale VCC bits never survive either.
//! - Section 3.9: in wave32 only VCC_LO contributes to VCCZ, so
//!   `s_cbranch_vccz` after F is taken, after T with EXEC_LO != 0 it is not,
//!   and after T with EXEC_LO = 0 it is.
//! - The guide does not say that F and T skip their sources. They keep the
//!   SRC0 allowlist and the contract section 5 rule: with EXEC_LO != 0 every
//!   source must be valid and initialized; with EXEC_LO = 0 the encoding is
//!   validated and no source is read. A lane-derived VCC_LO stays
//!   lane-derived even though F's value is always 0 (contract section 5).

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use std::collections::BTreeMap;

use harness::{Case, words};
use xenora_shader::{Architecture, CompileError, compile};

/// The four forms with their ENC_VOPC opcodes (guide 12.9, Table 61).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Form {
    FI32 = 128,
    TI32 = 135,
    FU32 = 192,
    TU32 = 199,
}

const FORMS: [(Form, &str); 4] = [
    (Form::FI32, "v_cmp_f_i32"),
    (Form::TI32, "v_cmp_t_i32"),
    (Form::FU32, "v_cmp_f_u32"),
    (Form::TU32, "v_cmp_t_u32"),
];

/// The per-lane test. The operands are taken so every call site names them,
/// but neither form depends on them.
fn test(form: Form, _s0: u32, _s1: u32) -> bool {
    match form {
        // 128 V_CMP_F_I32 / 192 V_CMP_F_U32: D[threadId] = 0.
        Form::FI32 | Form::FU32 => false,
        // 135 V_CMP_T_I32 / 199 V_CMP_T_U32: D[threadId] = 1.
        Form::TI32 | Form::TU32 => true,
    }
}

/// Section 3.9 and contract section 3: VCC_LO = OR over the lanes n that
/// exist (`present`) of (EXEC[n] & test(n)) << n, with S0 and S1 of lane n
/// from `operands(n)`.
fn vcc(form: Form, exec: u32, present: u32, operands: impl Fn(usize) -> (u32, u32)) -> u32 {
    (0..32)
        .filter(|&n| (present & exec) >> n & 1 == 1)
        .filter(|&n| {
            let (s0, s1) = operands(n);
            test(form, s0, s1)
        })
        .fold(0, |mask, n| mask | 1 << n)
}

/// Boundary operands.
const VALUES: [u32; 8] = [
    0,
    1,
    0x1234_5678,
    0x7fff_ffff,
    0x8000_0000,
    0x8000_0001,
    0xffff_fffe,
    0xffff_ffff,
];
/// Per-lane inputs: the boundary values first, then 24 more, all different.
const MORE_LANES: [u32; 24] = [
    2,
    0x0000_00ff,
    0x0000_ffff,
    0x00ff_ffff,
    0x0f0f_0f0f,
    0x4000_0000,
    0x5555_5555,
    0x7fff_fffe,
    0x8765_4321,
    0xaaaa_aaaa,
    0xc000_0000,
    0xf0f0_f0f0,
    0xff00_0000,
    0xffff_0000,
    0xffff_ff00,
    0xffff_fff0,
    3,
    16,
    64,
    65,
    0x0000_0100,
    0x00c0_ffee,
    0x1fff_ffff,
    0xdead_beef,
];
const LITERALS: [u32; 5] = [0, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX];
const SGPR_VALUES: [u32; 3] = [0, 0x8000_0000, u32::MAX];
/// The last one is set explicitly, as a full EXEC written by `s_mov_b32`.
const EXEC_MASKS: [u32; 7] = [
    0,
    0x5555_5555,
    0xaaaa_aaaa,
    0x8000_0001,
    0x0000_ffff,
    0x7fff_fffe,
    u32::MAX,
];
/// VCC_LO before the compare in the EXEC cases: bits inside and outside every
/// mask above, so a partial VCC update would show.
const STALE_VCC: u32 = 0xa5a5_c33c;
/// XORed with v1 to give every lane its own old v2 before a masked move.
const OLD: u32 = 0x0bad_f00d;

const COUNTS: [(&str, usize); 12] = [
    ("vopc domain", 4 * 8 * 2),
    ("vopc aliasing", 4 * 2),
    ("vopc inline integer", 4 * 81),
    ("vopc literal", 4 * 5),
    ("vopc sgpr source", 4 * 2 * 3),
    ("vopc vcc_lo source", 4 * 4),
    ("vopc max register", 4 * 3),
    ("vopc exec", 4 * (7 + 2)),
    ("vopc empty exec reads nothing", 4 * 3),
    ("vopc partial wave", 4 * 3),
    ("vopc vccz branch", 4 * 4),
    ("vopc exec unchanged", 4 * 2),
];
/// Per form: 4 uninitialized sources, 2 sources with a load outstanding, a missing literal, 9 float inline
/// constants, 9 other special SRC0 selectors, a float constant under empty
/// EXEC, SDWA, DPP16 and two DPP8 forms, the VOP3 `_e64` form, the matching
/// `v_cmpx`, and a lane-derived F result branching over a scalar instruction.
const REJECTIONS: usize = 4 * (4 + 2 + 1 + 9 + 9 + 1 + 4 + 1 + 1 + 1);

const LITERAL: u32 = 255;
const VCC_LO: u32 = 106;
const EXEC_LO: u32 = 126;

/// ENCODING 0b0111110 in bits 25-31, OP 17-24, VSRC1 9-16, SRC0 0-8 (guide
/// 13.3.3); SRC0 256 + n is VGPR n.
fn vopc(op: u32, src0: u32, vsrc1: u32) -> u32 {
    0b011_1110 << 25 | op << 17 | vsrc1 << 9 | src0
}

const fn vgpr(n: u32) -> u32 {
    256 + n
}

/// `v_mov_b32 v<reg>, literal` (VOP1 opcode 1).
fn vmov(reg: u32, value: u32) -> [u32; 2] {
    [0x7e00_02ff | reg << 17, value]
}

/// `s_mov_b32 s<reg>, literal` (SOP1 opcode 3).
fn smov(reg: u32, value: u32) -> [u32; 2] {
    [0xbe80_03ff | reg << 16, value]
}

/// `s_mov_b32 exec_lo, -1`.
const FULL_EXEC: u32 = 0xbefe_03c1;
/// `v_mov_b32 v2, vcc_lo`.
const READ_VCC: u32 = 0x7e04_026a;
/// `v_cmp_lt_u32 vcc_lo, v1, v3`: a supported compare, so VCC_LO becomes
/// lane-derived from v1 < v3.
const LT_U32_V1_V3: u32 = 0x7d82_0701;
/// `s_cbranch_vccz` over one word (SOPP opcode 6, SIMM16 = 1).
const VCCZ_OVER_ONE: u32 = 0xbf86_0001;
/// `v_xor_b32 v2, v1, v2` (VOP2 opcode 29), the skipped region.
const XOR_V2_V1: u32 = 29 << 25 | 2 << 17 | 2 << 9 | (256 + 1);

const PREFIX: [u32; 3] = [0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
const STORE_V2: [u32; 4] = [0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000];

/// Load v1 = lane input, `setup`, EXEC_LO = `exec` (unless `None`), the compare
/// words, EXEC_LO = -1 again, v2 = vcc_lo, `tail`, then store v2. Returns the
/// program and the compare's byte offset.
fn program(setup: &[u32], exec: Option<u32>, compare: &[u32], tail: &[u32]) -> (Vec<u8>, usize) {
    let mut code = PREFIX.to_vec();
    code.extend(setup);
    if let Some(exec) = exec {
        code.extend(smov(EXEC_LO, exec));
    }
    let offset = 4 * code.len();
    code.extend(compare);
    if exec.is_some() {
        code.push(FULL_EXEC);
    }
    code.push(READ_VCC);
    code.extend(tail);
    code.extend(STORE_V2);
    (words(&code), offset)
}

fn lanes() -> Vec<u32> {
    VALUES.iter().chain(&MORE_LANES).copied().collect()
}

/// The lanes of each wave of `input` that exist, as a mask per dispatch index.
fn present(input: &[u32], n: usize) -> u32 {
    let in_wave = (input.len() - n / 32 * 32).min(32);
    if in_wave == 32 {
        u32::MAX
    } else {
        (1 << in_wave) - 1
    }
}

/// A case whose lane `n` (dispatch index) of `input` expects
/// `lane(n, x, present-lane mask of its wave)`.
fn case(
    (_, name): (Form, &'static str),
    family: &'static str,
    label: String,
    code: Vec<u8>,
    input: Vec<u32>,
    lane: impl Fn(usize, u32, u32) -> u32,
) -> Case {
    let expected = input
        .iter()
        .enumerate()
        .map(|(n, &x)| lane(n, x, present(&input, n)))
        .collect();
    Case {
        form: name,
        family,
        label: format!("{name} {label}"),
        code,
        input,
        expected,
    }
}

fn inline_integer(selector: u32) -> u32 {
    match selector {
        128..=192 => selector - 128,
        193..=208 => (-((selector - 192) as i32)) as u32,
        _ => unreachable!("not an integer inline constant"),
    }
}

/// The expected lane output under full EXEC: VCC_LO of the lane's wave.
fn full(form: Form, operands: impl Fn(usize) -> (u32, u32)) -> impl Fn(usize, u32, u32) -> u32 {
    move |_, _, present| vcc(form, u32::MAX, present, &operands)
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let input = lanes();
    // S0/S1 for lane n of a wave of `input`, with v1 = input.
    let v1 = |n: usize| input[n % 32];
    for entry @ (form, _) in FORMS {
        let op = form as u32;
        // Every boundary value against all 32 lane inputs, in each slot.
        for b in VALUES {
            let setup = vmov(3, b);
            let (code, _) = program(&setup, None, &[vopc(op, vgpr(1), 3)], &[]);
            let label = format!("v1, {b:#x}");
            let f = full(form, |n| (v1(n), b));
            cases.push(case(entry, "vopc domain", label, code, lanes(), f));
            let (code, _) = program(&setup, None, &[vopc(op, vgpr(3), 1)], &[]);
            let label = format!("{b:#x}, v1");
            let f = full(form, |n| (b, v1(n)));
            cases.push(case(entry, "vopc domain", label, code, lanes(), f));
        }
        // Both sources the same register.
        let b = 0x8000_0000;
        let family = "vopc aliasing";
        let (code, _) = program(&vmov(3, b), None, &[vopc(op, vgpr(1), 1)], &[]);
        let f = full(form, |n| (v1(n), v1(n)));
        cases.push(case(entry, family, "v1, v1".into(), code, lanes(), f));
        let (code, _) = program(&vmov(3, b), None, &[vopc(op, vgpr(3), 3)], &[]);
        let f = full(form, |_| (b, b));
        cases.push(case(entry, family, "v3, v3".into(), code, lanes(), f));
        // Every integer inline constant (-16..64) as SRC0.
        for selector in 128..=208 {
            let value = inline_integer(selector);
            let (code, _) = program(&[], None, &[vopc(op, selector, 1)], &[]);
            let label = format!("inline {selector} ({value:#x}), v1");
            let f = full(form, |n| (value, v1(n)));
            cases.push(case(entry, "vopc inline integer", label, code, lanes(), f));
        }
        // One literal dword after the VOPC word as SRC0.
        for literal in LITERALS {
            let (code, _) = program(&[], None, &[vopc(op, LITERAL, 1), literal], &[]);
            let label = format!("literal {literal:#x}, v1");
            let f = full(form, |n| (literal, v1(n)));
            cases.push(case(entry, "vopc literal", label, code, lanes(), f));
        }
        // SGPR SRC0: s8 and s105, the highest SGPR.
        for reg in [8, 105] {
            for value in SGPR_VALUES {
                let (code, _) = program(&smov(reg, value), None, &[vopc(op, reg, 1)], &[]);
                let label = format!("s{reg} = {value:#x}, v1");
                let f = full(form, |n| (value, v1(n)));
                cases.push(case(entry, "vopc sgpr source", label, code, lanes(), f));
            }
        }
        // vcc_lo SRC0: known (three values), and lane-derived from a supported
        // compare; the old VCC_LO is read before the new value is written.
        let family = "vopc vcc_lo source";
        for value in SGPR_VALUES {
            let (code, _) = program(&smov(VCC_LO, value), None, &[vopc(op, VCC_LO, 1)], &[]);
            let label = format!("vcc_lo = {value:#x}, v1");
            let f = full(form, |n| (value, v1(n)));
            cases.push(case(entry, family, label, code, lanes(), f));
        }
        let b = 0x8000_0000;
        let mut setup = vmov(3, b).to_vec();
        setup.push(LT_U32_V1_V3);
        let (code, _) = program(&setup, None, &[vopc(op, VCC_LO, 1)], &[]);
        // The earlier v_cmp_lt_u32 result, from its own guide relation.
        let old = (0..32)
            .filter(|&n| v1(n) < b)
            .fold(0u32, |mask, n| mask | 1 << n);
        let f = full(form, |n| (old, v1(n)));
        let label = "vcc_lo lane-derived (v1 < 0x80000000), v1".into();
        cases.push(case(entry, family, label, code, lanes(), f));
        // v255, the highest VGPR, in each slot and both.
        let b = 0x7fff_ffff;
        let family = "vopc max register";
        let setup = vmov(255, b);
        for (src0, vsrc1, label) in [
            (vgpr(255), 1, "v255, v1"),
            (vgpr(1), 255, "v1, v255"),
            (vgpr(255), 255, "v255, v255"),
        ] {
            let (code, _) = program(&setup, None, &[vopc(op, src0, vsrc1)], &[]);
            let f = full(form, |n| {
                let pick = |is_v255: bool| if is_v255 { b } else { v1(n) };
                (pick(src0 == vgpr(255)), pick(vsrc1 == 255))
            });
            cases.push(case(entry, family, label.into(), code, lanes(), f));
        }
        // EXEC around the compare, with a stale VCC_LO: inactive lanes' bits
        // are cleared, active lanes take the test.
        let family = "vopc exec";
        let b = 0x0f0f_0f0f;
        let mut setup = vmov(3, b).to_vec();
        setup.extend(smov(VCC_LO, STALE_VCC));
        for exec in EXEC_MASKS {
            let (code, _) = program(&setup, Some(exec), &[vopc(op, vgpr(1), 3)], &[]);
            let label = format!("exec {exec:#x}, vcc_lo was {STALE_VCC:#x}, v1, {b:#x}");
            cases.push(case(entry, family, label, code, lanes(), |_, _, p| {
                vcc(form, exec, p, |n| (v1(n), b))
            }));
        }
        // A literal SRC0 under partial and empty EXEC: its dword is consumed.
        for exec in [0x00f0_0f00, 0] {
            let compare = [vopc(op, LITERAL, 3), 0x8000_0000];
            let (code, _) = program(&setup, Some(exec), &compare, &[]);
            let label = format!("exec {exec:#x}, literal 0x80000000, v3");
            cases.push(case(entry, family, label, code, lanes(), |_, _, p| {
                vcc(form, exec, p, |_| (0x8000_0000, b))
            }));
        }
        // EXEC_LO = 0: no source is read, so uninitialized ones are accepted
        // (contract section 5) and VCC_LO becomes 0.
        let family = "vopc empty exec reads nothing";
        for (src0, vsrc1, label) in [
            (20, 1, "s20 (unknown), v1"),
            (VCC_LO, 1, "vcc_lo (uninitialized), v1"),
            (vgpr(9), 9, "v9, v9 (uninitialized)"),
        ] {
            let (code, _) = program(&[], Some(0), &[vopc(op, src0, vsrc1)], &[]);
            let label = format!("exec 0, {label}");
            cases.push(case(entry, family, label, code, lanes(), |_, _, p| {
                vcc(form, 0, p, |_| (0, 0))
            }));
        }
        // A final wave with absent lanes: 7 lanes, and 45 lanes (one full wave
        // and 13 lanes), also with partial EXEC in both waves.
        let family = "vopc partial wave";
        let long: Vec<u32> = lanes()
            .into_iter()
            .chain(lanes().into_iter().take(13))
            .collect();
        // Lane k of the wave holding dispatch index n; `vcc` asks only for
        // lanes that exist.
        let wave_lane = |input: &[u32], n: usize, k: usize| input[n / 32 * 32 + k];
        let short = lanes()[..7].to_vec();
        for (exec, input, label) in [
            (u32::MAX, short, "7 lanes".to_string()),
            (u32::MAX, long.clone(), "45 lanes".to_string()),
            (0x5555_5555, long, "45 lanes, exec 0x55555555".to_string()),
        ] {
            let exec_word = (exec != u32::MAX).then_some(exec);
            let (code, _) = program(&vmov(3, 5), exec_word, &[vopc(op, vgpr(1), 3)], &[]);
            let copy = input.clone();
            cases.push(case(entry, family, label, code, input, |n, _, p| {
                vcc(form, exec, p, |k| (wave_lane(&copy, n, k), 5))
            }));
        }
        // s_cbranch_vccz after the compare (guide 3.9: VCCZ = VCC_LO == 0 in
        // wave32). Taken: v2 = VCC_LO (0). Not taken: v2 = VCC_LO ^ v1.
        let family = "vopc vccz branch";
        for exec in [u32::MAX, 0x0000_8000, 0] {
            let (code, _) = program(
                &vmov(3, 5),
                Some(exec),
                &[vopc(op, vgpr(1), 3)],
                &[VCCZ_OVER_ONE, XOR_V2_V1],
            );
            let label = format!("exec {exec:#x}, then s_cbranch_vccz");
            cases.push(case(entry, family, label, code, lanes(), |_, x, p| {
                let mask = vcc(form, exec, p, |n| (v1(n), 5));
                if mask == 0 { 0 } else { mask ^ x }
            }));
        }
        // Under EXEC_LO = 0 the result is a known 0 (contract section 5), so
        // the branch is resolved statically and may skip a scalar
        // instruction (`s_mov_b32 s8, 0`); it is taken, v2 = 0.
        let tail = [VCCZ_OVER_ONE, 0xbe88_0380];
        let (code, _) = program(&vmov(3, 5), Some(0), &[vopc(op, vgpr(1), 3)], &tail);
        let label = "exec 0, then s_cbranch_vccz over s_mov".into();
        cases.push(case(entry, family, label, code, lanes(), |_, _, _| 0));
        // EXEC_LO is unchanged by the compare (contract section 3): v2 =
        // vcc_lo runs under the compare's partial EXEC, so active lanes take
        // VCC_LO and inactive lanes keep their own old v2 = v1 ^ OLD.
        let family = "vopc exec unchanged";
        for exec in [0x5555_5555, 0x8000_0001] {
            let mut code = PREFIX.to_vec();
            code.extend(vmov(3, 5));
            code.extend(vmov(5, OLD));
            code.push(29 << 25 | 2 << 17 | 1 << 9 | vgpr(5)); // v_xor_b32 v2, v5, v1
            code.extend(smov(EXEC_LO, exec));
            code.extend([vopc(op, vgpr(1), 3), READ_VCC, FULL_EXEC]);
            code.extend(STORE_V2);
            let label = format!("exec {exec:#x}, v2 = vcc_lo before EXEC is restored");
            cases.push(case(
                entry,
                family,
                label,
                words(&code),
                lanes(),
                |n, x, p| {
                    if exec >> (n % 32) & 1 == 1 {
                        vcc(form, exec, p, |k| (v1(k), 5))
                    } else {
                        x ^ OLD
                    }
                },
            ));
        }
    }
    cases
}

/// Programs both interpreters must refuse, with the refusing byte offset.
fn rejections() -> Vec<(String, Vec<u8>, usize)> {
    let setup = vmov(3, 7);
    let mut all = Vec::new();
    for (form, name) in FORMS {
        let op = form as u32;
        let mut reject = |label: String, (code, offset): (Vec<u8>, usize)| {
            all.push((format!("{name} {label}"), code, offset));
        };
        // Uninitialized sources under nonzero EXEC (contract sections 4 and 5).
        for (src0, vsrc1, label) in [
            (20, 3, "s20 unknown"),
            (VCC_LO, 3, "vcc_lo uninitialized"),
            (vgpr(9), 3, "v9 uninitialized as SRC0"),
            (vgpr(1), 9, "v9 uninitialized as VSRC1"),
        ] {
            reject(
                label.into(),
                program(&setup, None, &[vopc(op, src0, vsrc1)], &[]),
            );
        }
        // A source whose VGPR has a load outstanding (no wait after
        // `buffer_load_dword v3`): refused at the compare.
        for (src0, vsrc1, label) in [(vgpr(1), 3, "VSRC1"), (vgpr(3), 1, "SRC0")] {
            let mut code = PREFIX.to_vec();
            code.extend(setup);
            code.extend([0xe030_2000, 0x8000_0300]);
            let offset = 4 * code.len();
            code.push(vopc(op, src0, vsrc1));
            code.extend([READ_VCC, 0xbf8c_3f70]);
            code.extend(STORE_V2);
            let label = format!("v3 load outstanding as {label}");
            reject(label, (words(&code), offset));
        }
        // A literal SRC0 with no literal dword: the program ends at the word.
        let mut code = PREFIX.to_vec();
        code.extend(setup);
        let offset = 4 * code.len();
        code.push(vopc(op, LITERAL, 3));
        reject("missing literal".into(), (words(&code), offset));
        // Float inline constants 240-248, VCC_HI, TTMP-adjacent and special
        // selectors outside the allowlist (contract section 2).
        for src0 in (240..=248).chain([107, 124, 125, 126, 127, 251, 252, 253, 254]) {
            let label = format!("src0 {src0}");
            reject(label, program(&setup, None, &[vopc(op, src0, 3)], &[]));
        }
        // Rejected at decode independent of EXEC.
        let label = "src0 240 under empty exec".into();
        reject(label, program(&setup, Some(0), &[vopc(op, 240, 3)], &[]));
        // SDWA (249), DPP16 (250), DPP8 (233) and DPP8FI (234) with SRC0 v1.
        for (src0, extension) in [
            (249, 0x0606_0601),
            (250, 0xff00_e401),
            (233, 0xfac6_8801),
            (234, 0xfac6_8801),
        ] {
            let label = format!("src0 {src0} extension");
            reject(
                label,
                program(&setup, None, &[vopc(op, src0, 3), extension], &[]),
            );
        }
        // The VOP3 `_e64` form (ENCODING 0b110101, OP = the VOPC opcode,
        // SDST vcc_lo): v_cmp vcc_lo, v1, v3.
        let vop3 = [0xd400_0000 | op << 16 | VCC_LO, vgpr(1) | vgpr(3) << 9];
        reject("_e64".into(), program(&setup, None, &vop3, &[]));
        // The matching v_cmpx (writes EXEC): opcode + 16 (Table 61: 144, 151,
        // 208, 215).
        let label = format!("v_cmpx opcode {}", op + 16);
        reject(
            label,
            program(&setup, None, &[vopc(op + 16, vgpr(1), 3)], &[]),
        );
        // F under nonzero EXEC is lane-derived even though its value is 0, so
        // a vccz branch over `s_mov_b32 s8, 0` is refused at that word.
        // Layout: compare, v2 = vcc_lo, branch, then the s_mov at +12.
        let tail = [VCCZ_OVER_ONE, 0xbe88_0380];
        let (code, compare) = program(&setup, None, &[vopc(op, vgpr(1), 3)], &tail);
        reject("lane-derived vccz over s_mov".into(), (code, compare + 12));
    }
    all
}

fn assert_rejected_at(label: &str, code: &[u8], offset: usize, input: &[u32]) {
    match xenora_ref::execute(code, input) {
        Ok(_) => panic!("reference accepted {label}"),
        Err(error) => assert_eq!(
            error.offset, offset,
            "reference rejected {label} elsewhere: {error:?}"
        ),
    }
    match independent_ref::execute(code, input) {
        Ok(_) => panic!("independent accepted {label}"),
        Err(error) => assert!(
            error.starts_with(&format!("{offset:#x}:")),
            "independent rejected {label} elsewhere: {error}"
        ),
    }
}

fn tally(cases: &[Case], key: fn(&Case) -> &'static str) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for case in cases {
        *counts.entry(key(case)).or_default() += 1;
    }
    counts
}

/// Runs `execute` on every case and returns one line per failing family: the
/// number of failures and the first one.
fn failures(
    cases: &[Case],
    execute: impl Fn(&[u8], &[u32]) -> Result<Vec<u32>, String>,
) -> Vec<String> {
    let mut failed: BTreeMap<&str, (usize, String)> = BTreeMap::new();
    for case in cases {
        let output = execute(&case.code, &case.input);
        if output.as_deref().ok() != Some(case.expected.as_slice()) {
            let first = format!("{}: {output:x?}, expected {:x?}", case.label, case.expected);
            failed.entry(case.family).or_insert((0, first)).0 += 1;
        }
    }
    failed
        .into_iter()
        .map(|(family, (n, first))| format!("  {family}: {n} failed, first: {first}"))
        .collect()
}

fn checked_cases() -> Vec<Case> {
    let cases = cases();
    assert_eq!(tally(&cases, |case| case.family), BTreeMap::from(COUNTS));
    cases
}

#[test]
fn expectations_follow_the_guide_examples() {
    // Worked by hand from section 3.9 and Table 61.
    for (form, exec, present, mask) in [
        (Form::TI32, u32::MAX, u32::MAX, u32::MAX),
        (Form::TU32, 0x5555_5555, u32::MAX, 0x5555_5555),
        (Form::TI32, u32::MAX, 0x7f, 0x7f),
        (Form::TU32, 0xf0f0_f0f0, 0x1fff, 0x10f0),
        (Form::TI32, 0, u32::MAX, 0),
        (Form::FI32, u32::MAX, u32::MAX, 0),
        (Form::FU32, 0x8000_0001, u32::MAX, 0),
    ] {
        assert_eq!(
            vcc(form, exec, present, |_| (0, 0)),
            mask,
            "{form:?} {exec:#x}"
        );
    }
}

#[test]
fn vopc_f_t_match_the_guide_on_the_reference() {
    let cases = checked_cases();
    let failed = failures(&cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert!(
        failed.is_empty(),
        "reference disagrees with the guide:\n{}",
        failed.join("\n")
    );
    eprintln!("vopc_f_t: {} reference cases", cases.len());
    for (form, n) in tally(&cases, |case| case.form) {
        eprintln!("  form {form}: {n}");
    }
}

#[test]
fn vopc_f_t_match_the_guide_on_the_independent_interpreter() {
    let cases = checked_cases();
    let failed = failures(&cases, independent_ref::execute);
    assert!(
        failed.is_empty(),
        "independent interpreter disagrees with the guide:\n{}",
        failed.join("\n")
    );
    eprintln!("vopc_f_t: {} independent-interpreter cases", cases.len());
}

#[test]
fn vopc_f_t_outside_the_contract_are_rejected() {
    let rejections = rejections();
    assert_eq!(rejections.len(), REJECTIONS);
    for (label, code, offset) in &rejections {
        assert_rejected_at(label, code, *offset, &lanes());
    }
    eprintln!("vopc_f_t: {} rejections", rejections.len());
}

#[test]
fn other_constant_compares_stay_unsupported() {
    // The constant compares outside the sixteen V_CMP_*_F32 forms of Table 61
    // that #786 implements: the CMPX forms of the same groups and the
    // neighbours of the two 32-bit integer groups.
    // V_CMPX_F_F32 16, V_CMPX_TRU_F32 31, V_CMPX_F_I64 176, V_CMPX_T_I64 183,
    // V_CMPX_F_U64 240, V_CMPX_T_U64 247. The 64-bit V_CMP_F/T (160, 167,
    // 224, 231) are covered by vopc_wide.rs. V_CMP_F_F32 0 and
    // V_CMP_TRU_F32 15 are no longer listed: they are two of the sixteen
    // binary32 forms #786 implements, and vopc_f32.rs covers them.
    let setup = vmov(3, 7);
    // Also the neighbours of the two 32-bit integer groups: 127 and 191 (not
    // in Table 61), 136 V_CMP_CLASS_F32 and 200 V_CMP_F_F16.
    for op in [16, 31, 127, 136, 176, 183, 191, 200, 240, 247] {
        let (code, offset) = program(&setup, None, &[vopc(op, vgpr(1), 3)], &[]);
        assert_rejected_at(&format!("opcode {op}"), &code, offset, &lanes());
    }
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation, and the translator's F/T forms"]
fn vopc_f_t_match_the_guide_on_native_metal() {
    assert_eq!(std::env::var("MTL_DEBUG_LAYER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MTL_SHADER_VALIDATION").as_deref(), Ok("1"));
    for (label, code, offset) in rejections() {
        let at = match compile(Architecture::Gfx1030, &code) {
            Err(CompileError::Decode(error)) => error.offset,
            Err(CompileError::Verify { offset, .. }) => offset,
            other => panic!("compiler did not reject {label}: {other:?}"),
        };
        assert_eq!(at, offset, "compiler rejected {label} elsewhere");
    }
    let cases = checked_cases();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "vopc_f_t: {} Metal cases in {programs} programs",
        cases.len()
    );
}
