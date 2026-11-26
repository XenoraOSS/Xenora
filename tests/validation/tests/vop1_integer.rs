// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent per-lane expected values for six 32-bit VOP1 integer and bit
//! forms (#215): `v_not_b32`, `v_bfrev_b32`, `v_ffbh_u32`, `v_ffbl_b32`,
//! `v_ffbh_i32` and `v_swap_b32`, all `_e32`.
//!
//! The expectations are written below from the public AMD "RDNA 2" Instruction
//! Set Architecture Reference Guide only: section 12.8 "VOP1 Instructions"
//! (pseudocode, descriptions and functional examples), section 13.3.2 for the
//! VOP1 word and the vector operand table for SRC0 (SGPRs, `vcc_lo`, inline
//! integers, the literal, VGPRs). Nothing here comes from the ISA crate, the
//! semantic registry, the decoder or the shader compiler. Every case runs on
//! the reference interpreter `xenora_ref` and on the second interpreter
//! `independent_ref`; the ignored test also runs the translated MSL on native
//! Metal.
//!
//! Readings of the guide (not hardware evidence):
//! - The bit scans return -1 (all ones) when the loop finds nothing, as the
//!   pseudocode's `D.i = -1` initial value says; V_FFBH_I32 starts at bit 30,
//!   so 0 and -1 both give -1 (the guide's own examples).
//! - V_SWAP_B32 is `tmp = D; D = S0; S0 = tmp`, so SRC0 must be a VGPR; with
//!   VDST = SRC0 the register keeps its value.
//! - None of the six writes VCC or SCC (no carry or compare output is listed).
//! - A lane whose EXEC bit is clear keeps its old VGPRs (guide section 3.3).
//!
//! Left out on purpose: float ops and conversions (they wait for the float
//! contract), V_READFIRSTLANE_B32 (lane-crossing, SGPR result), the
//! M0-relative moves (V_MOVREL*, V_SWAPREL_B32), V_NOP, V_PIPEFLUSH and
//! V_CLREXCP (no integer result), and V_SAT_PK_U8_I16, whose guide entry
//! calls it a "saturating add" and does not define `sat8`.

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use std::collections::BTreeMap;

use harness::{Case, words};
use xenora_shader::{Architecture, CompileError, compile};

/// The five one-source forms with their ENC_VOP1 opcodes (guide 12.8).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Form {
    Not = 55,
    Bfrev = 56,
    FfbhU32 = 57,
    FfblB32 = 58,
    FfbhI32 = 59,
}

const FORMS: [(Form, &str); 5] = [
    (Form::Not, "v_not_b32"),
    (Form::Bfrev, "v_bfrev_b32"),
    (Form::FfbhU32, "v_ffbh_u32"),
    (Form::FfblB32, "v_ffbl_b32"),
    (Form::FfbhI32, "v_ffbh_i32"),
];

const SWAP: u32 = 101;
const SWAP_NAME: &str = "v_swap_b32";

/// S0 bit `i` (0 = LSB).
fn bit(s: u32, i: u32) -> u32 {
    (s >> i) & 1
}

/// D for S0 = `s`, transcribed from the guide 12.8 pseudocode.
fn expect(form: Form, s: u32) -> u32 {
    match form {
        // D.u = ~S0.u.
        Form::Not => s ^ 0xffff_ffff,
        // D.u[31:0] = S0.u[0:31].
        Form::Bfrev => {
            let mut d = 0;
            for i in 0..32 {
                d |= bit(s, i) << (31 - i);
            }
            d
        }
        // for i in 0..31: if S0.u[31 - i] == 1 then D.i = i.
        Form::FfbhU32 => {
            for i in 0..32 {
                if bit(s, 31 - i) == 1 {
                    return i;
                }
            }
            u32::MAX
        }
        // for i in 0..31: if S0.u[i] == 1 then D.i = i.
        Form::FfblB32 => {
            for i in 0..32 {
                if bit(s, i) == 1 {
                    return i;
                }
            }
            u32::MAX
        }
        // for i in 1..31: if S0.i[31 - i] != S0.i[31] then D.i = i.
        Form::FfbhI32 => {
            for i in 1..32 {
                if bit(s, 31 - i) != bit(s, 31) {
                    return i;
                }
            }
            u32::MAX
        }
    }
}

/// The guide's functional examples for the three bit scans (12.8).
const GUIDE_EXAMPLES: [(Form, u32, u32); 17] = [
    (Form::FfbhU32, 0x0000_0000, 0xffff_ffff),
    (Form::FfbhU32, 0x8000_00ff, 0),
    (Form::FfbhU32, 0x1000_00ff, 3),
    (Form::FfbhU32, 0x0000_ffff, 16),
    (Form::FfbhU32, 0x0000_0001, 31),
    (Form::FfblB32, 0x0000_0000, 0xffff_ffff),
    (Form::FfblB32, 0xff00_0001, 0),
    (Form::FfblB32, 0xff00_0008, 3),
    (Form::FfblB32, 0xffff_0000, 16),
    (Form::FfblB32, 0x8000_0000, 31),
    (Form::FfbhI32, 0x0000_0000, 0xffff_ffff),
    (Form::FfbhI32, 0x4000_0000, 1),
    (Form::FfbhI32, 0x8000_0000, 1),
    (Form::FfbhI32, 0x0fff_ffff, 4),
    (Form::FfbhI32, 0xffff_0000, 16),
    (Form::FfbhI32, 0xffff_fffe, 31),
    (Form::FfbhI32, 0xffff_ffff, 0xffff_ffff),
];

const VALUES: [u32; 18] = [
    0x0000_0000,
    0x0000_0001,
    0x0000_0002,
    0x007f_ffff,
    0x0080_0000,
    0x0080_0001,
    0x00ff_ffff,
    0x0100_0000,
    0x1234_5678,
    0x5555_5555,
    0x7fff_ffff,
    0x8000_0000,
    0x8000_0001,
    0xaaaa_aaaa,
    0xff00_0001,
    0xff80_0000,
    0xffff_fffe,
    0xffff_ffff,
];
/// The other 14 lane inputs, so all 32 lanes differ.
const MORE_LANES: [u32; 14] = [
    0x007f_fffe,
    0x00ff_fffe,
    0x0100_0001,
    0x00c0_0000,
    0xff7f_ffff,
    0xff80_0001,
    0xffff_0000,
    0x0000_ffff,
    0x4000_0000,
    0xc000_0000,
    0x8765_4321,
    0x0f0f_0f0f,
    0x7fff_fffe,
    0x1fff_ffff,
];
const LITERALS: [u32; 9] = [
    0,
    1,
    0x7fff_ffff,
    0x8000_0000,
    u32::MAX,
    0x00ff_ffff,
    0x0080_0000,
    0xff80_0000,
    0x0100_0001,
];
const SGPR_VALUES: [u32; 3] = [0x8000_0000, 0x00ff_ffff, 0xff80_0001];
/// XORed with v1 to make a second per-lane VGPR that differs from v1.
const PAIR_KEYS: [u32; 5] = [0x00ff_ffff, 0x8000_0000, u32::MAX, 0x0080_0001, 0x5555_5555];
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
/// XORed with v1 to give every lane its own old VGPR before a masked write.
const OLD: u32 = 0x0bad_f00d;

const COUNTS: [(&str, usize); 17] = [
    ("vop1 domain", 5 * (1 + 18)),
    ("vop1 bit positions", 5 * 4),
    ("vop1 aliasing", 5),
    ("vop1 inline integer", 5 * 81),
    ("vop1 literal", 5 * 9),
    ("vop1 sgpr source", 5 * 2 * 3),
    ("vop1 vcc_lo source", 5 * 3),
    ("vop1 max register", 5 * 2),
    ("vop1 exec", 5 * (7 + 3)),
    ("vop1 partial wave", 5 * 3),
    ("vop1 no vcc or scc write", 5 * 5),
    ("swap lane pairs", 5 * 2),
    ("swap same register", 1),
    ("swap max register", 4),
    ("swap exec", 7 * 2),
    ("swap partial wave", 3),
    ("swap no vcc or scc write", 5),
];
/// Per one-source form: a missing literal and every SRC0 selector outside the
/// supported subset (107-127 and 209-254, with the SDWA and DPP extension
/// dword where one belongs); for V_SWAP_B32 every selector 0-255; and for
/// all six a read of and a write to a VGPR with a load outstanding.
const REJECTIONS: usize = 5 * (1 + 21 + 46) + 256 + 2 + 6 * 2;

const LITERAL: u32 = 255;
const VCC_LO: u32 = 106;

/// ENCODING 0b0111111 in bits 25-31, VDST 17-24, OP 9-16, SRC0 0-8 (guide
/// 13.3.2); SRC0 256 + n is VGPR n.
fn vop1(op: u32, dst: u32, src0: u32) -> u32 {
    0x7e00_0000 | dst << 17 | op << 9 | src0
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

/// `v_xor_b32 v<dst>, v<a>, v<b>` (VOP2 opcode 29, supported before this set).
fn vxor(dst: u32, a: u32, b: u32) -> u32 {
    29 << 25 | dst << 17 | b << 9 | vgpr(a)
}

/// `s_mov_b32 exec_lo, -1`.
const FULL_EXEC: u32 = 0xbefe_03c1;
/// `s_cmp_eq_u32 0, 0` (SCC = 1) and `s_cmp_eq_u32 0, 1` (SCC = 0).
const SCC_SET: u32 = 0xbf06_8080;
const SCC_CLEAR: u32 = 0xbf06_8180;
/// v6 = vcc_lo; v2 ^= v6; `s_cbranch_scc0` over v2 ^= v1.
const FLAG_TAIL: [u32; 4] = [0x7e0c_026a, 0x3a04_0506, 0xbf84_0001, 0x3a04_0501];

/// Load v1 = lane input, then `setup`, the ALU words, `after`, then
/// v2 = v<result> (unless it is v2) and store v2.
fn program(setup: &[u32], alu: &[u32], after: &[u32], result: u32) -> Vec<u8> {
    let mut code = vec![0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
    code.extend(setup);
    code.extend(alu);
    code.extend(after);
    if result != 2 {
        code.push(0x7e04_0300 | result);
    }
    code.extend([0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000]);
    words(&code)
}

/// The ALU word's byte offset in `program` with `setup`.
fn alu_offset(setup: &[u32]) -> usize {
    12 + 4 * setup.len()
}

fn lanes() -> Vec<u32> {
    VALUES.iter().chain(&MORE_LANES).copied().collect()
}

/// A case whose lane `n` (dispatch index) with input `x` expects `lane(n, x)`.
fn case(
    name: &'static str,
    family: &'static str,
    label: String,
    code: Vec<u8>,
    input: Vec<u32>,
    lane: impl Fn(usize, u32) -> u32,
) -> Case {
    let expected = input.iter().enumerate().map(|(n, &x)| lane(n, x)).collect();
    Case {
        form: name,
        family,
        label: format!("{name} {label}"),
        code,
        input,
        expected,
    }
}

/// D where the lane is active under `exec` (bit `n % 32`), else the old value.
fn masked(exec: u32, n: usize, d: u32, old: u32) -> u32 {
    if exec >> (n % 32) & 1 == 1 { d } else { old }
}

fn inline_integer(selector: u32) -> u32 {
    match selector {
        128..=192 => selector - 128,
        193..=208 => (-((selector - 192) as i32)) as u32,
        _ => unreachable!("not an integer inline constant"),
    }
}

/// Lane `n`'s input in a bit-position case.
type Pattern = fn(usize) -> u32;

/// 45 lanes: one full wave and a final wave of 13.
fn long_lanes() -> Vec<u32> {
    lanes()
        .into_iter()
        .chain(MORE_LANES.into_iter().take(13))
        .collect()
}

fn unary_cases(cases: &mut Vec<Case>) {
    for (form, name) in FORMS {
        let op = form as u32;
        let f = move |s| expect(form, s);
        // The 32 lane inputs directly, then each boundary value from a VGPR.
        let code = program(&[], &[vop1(op, 2, vgpr(1))], &[], 2);
        cases.push(case(
            name,
            "vop1 domain",
            "v1".into(),
            code,
            lanes(),
            |_, x| f(x),
        ));
        for b in VALUES {
            let code = program(&vmov(3, b), &[vop1(op, 2, vgpr(3))], &[], 2);
            let label = format!("v3 = {b:#x}");
            cases.push(case(name, "vop1 domain", label, code, lanes(), |_, _| f(b)));
        }
        // Every single-bit position, every single zero, and every run of ones
        // from the MSB and from the LSB: lane n gets the shifted pattern.
        let patterns: [(&str, Pattern); 4] = [
            ("1 << n", |n| 1 << n),
            ("!(1 << n)", |n| !(1 << n)),
            ("-1 << n", |n| u32::MAX << n),
            ("-1 >> n", |n| u32::MAX >> n),
        ];
        for (label, pattern) in patterns {
            let input = (0..32).map(pattern).collect();
            let code = program(&[], &[vop1(op, 2, vgpr(1))], &[], 2);
            let family = "vop1 bit positions";
            cases.push(case(name, family, label.into(), code, input, |_, x| f(x)));
        }
        // VDST equal to SRC0.
        let code = program(&[], &[vop1(op, 1, vgpr(1))], &[], 1);
        let family = "vop1 aliasing";
        cases.push(case(
            name,
            family,
            "v1 = v1".into(),
            code,
            lanes(),
            |_, x| f(x),
        ));
        // Every integer inline constant (-16..64) as SRC0.
        for selector in 128..=208 {
            let value = inline_integer(selector);
            let code = program(&[], &[vop1(op, 2, selector)], &[], 2);
            let label = format!("inline {selector} ({value:#x})");
            let family = "vop1 inline integer";
            cases.push(case(name, family, label, code, lanes(), |_, _| f(value)));
        }
        // One literal dword after the VOP1 word as SRC0.
        for literal in LITERALS {
            let code = program(&[], &[vop1(op, 2, LITERAL), literal], &[], 2);
            let label = format!("literal {literal:#x}");
            let family = "vop1 literal";
            cases.push(case(name, family, label, code, lanes(), |_, _| f(literal)));
        }
        // SGPR SRC0: s8 and s105, the highest SGPR (guide 5.2), and vcc_lo.
        for (reg, family) in [
            (8, "vop1 sgpr source"),
            (105, "vop1 sgpr source"),
            (VCC_LO, "vop1 vcc_lo source"),
        ] {
            for value in SGPR_VALUES {
                let code = program(&smov(reg, value), &[vop1(op, 2, reg)], &[], 2);
                let label = format!("s{reg} = {value:#x}");
                cases.push(case(name, family, label, code, lanes(), |_, _| f(value)));
            }
        }
        // v255, the highest VGPR, as VDST and as SRC0.
        let family = "vop1 max register";
        let code = program(&[], &[vop1(op, 255, vgpr(1))], &[], 255);
        cases.push(case(
            name,
            family,
            "v255 = v1".into(),
            code,
            lanes(),
            |_, x| f(x),
        ));
        let b = 0x8000_0001;
        let code = program(&vmov(255, b), &[vop1(op, 2, vgpr(255))], &[], 2);
        cases.push(case(
            name,
            family,
            "v2 = v255".into(),
            code,
            lanes(),
            |_, _| f(b),
        ));
        // EXEC around the ALU word: v2 = v1 ^ OLD before it, so each inactive
        // lane must keep its own old value; EXEC is -1 again for the store.
        let mut setup = vmov(5, OLD).to_vec();
        setup.push(vxor(2, 5, 1));
        let family = "vop1 exec";
        for exec in EXEC_MASKS {
            let mut before = setup.clone();
            before.extend(smov(126, exec));
            let code = program(&before, &[vop1(op, 2, vgpr(1))], &[FULL_EXEC], 2);
            let label = format!("exec {exec:#x}, v1");
            cases.push(case(name, family, label, code, lanes(), |n, x| {
                masked(exec, n, f(x), x ^ OLD)
            }));
        }
        // Partial EXEC with a scalar SRC0 (s8), and a literal also under
        // empty EXEC: its dword is still consumed.
        let exec = 0x0f0f_0f0f;
        let value = 0xff80_0003;
        let mut before = setup.clone();
        before.extend(smov(8, value));
        before.extend(smov(126, exec));
        let code = program(&before, &[vop1(op, 2, 8)], &[FULL_EXEC], 2);
        let label = format!("exec {exec:#x}, s8 = {value:#x}");
        cases.push(case(name, family, label, code, lanes(), |n, x| {
            masked(exec, n, f(value), x ^ OLD)
        }));
        for exec in [exec, 0] {
            let mut before = setup.clone();
            before.extend(smov(126, exec));
            let alu = [vop1(op, 2, LITERAL), value];
            let code = program(&before, &alu, &[FULL_EXEC], 2);
            let label = format!("exec {exec:#x}, literal {value:#x}");
            cases.push(case(name, family, label, code, lanes(), |n, x| {
                masked(exec, n, f(value), x ^ OLD)
            }));
        }
        // A final wave with absent lanes: 7 lanes, and 45 lanes, also with
        // partial EXEC in both waves.
        let family = "vop1 partial wave";
        let code = program(&setup, &[vop1(op, 2, vgpr(1))], &[], 2);
        let short = lanes()[..7].to_vec();
        let label = "7 lanes".into();
        cases.push(case(name, family, label, code.clone(), short, |_, x| f(x)));
        let label = "45 lanes".into();
        cases.push(case(name, family, label, code, long_lanes(), |_, x| f(x)));
        let exec = 0x5555_5555;
        let mut before = setup.clone();
        before.extend(smov(126, exec));
        let code = program(&before, &[vop1(op, 2, vgpr(1))], &[FULL_EXEC], 2);
        let label = format!("45 lanes, exec {exec:#x}");
        cases.push(case(name, family, label, code, long_lanes(), |n, x| {
            masked(exec, n, f(x), x ^ OLD)
        }));
        // VCC_LO seeded to 0 or -1 and SCC to 0 or 1 before the ALU word.
        // Afterwards each lane outputs D ^ VCC_LO, XORed with its input when
        // SCC is set, so any VCC or SCC write changes the output.
        let family = "vop1 no vcc or scc write";
        for vcc in [0, u32::MAX] {
            for scc in [false, true] {
                let mut setup = smov(VCC_LO, vcc).to_vec();
                setup.push(if scc { SCC_SET } else { SCC_CLEAR });
                let code = program(&setup, &[vop1(op, 2, vgpr(1))], &FLAG_TAIL, 2);
                let label = format!("vcc_lo {vcc:#x}, scc {scc}");
                cases.push(case(name, family, label, code, lanes(), |_, x| {
                    f(x) ^ vcc ^ if scc { x } else { 0 }
                }));
            }
        }
        // The same under partial EXEC with SCC = 1 and VCC_LO bits both inside
        // and outside EXEC, so a VCC write limited to active lanes also shows.
        let (exec, vcc) = (0x0f0f_0f0f, 0x0000_00f3);
        let mut before = setup.clone();
        before.extend(smov(VCC_LO, vcc));
        before.push(SCC_SET);
        before.extend(smov(126, exec));
        let mut after = vec![FULL_EXEC];
        after.extend(FLAG_TAIL);
        let code = program(&before, &[vop1(op, 2, vgpr(1))], &after, 2);
        let label = format!("exec {exec:#x}, vcc_lo {vcc:#x}, scc true");
        cases.push(case(name, family, label, code, lanes(), |n, x| {
            masked(exec, n, f(x), x ^ OLD) ^ vcc ^ x
        }));
    }
}

fn swap_cases(cases: &mut Vec<Case>) {
    let name = SWAP_NAME;
    // v4 = v1 ^ key, then swap v4 (VDST) with v1 (SRC0); either register is
    // stored, so both writes are checked.
    for key in [PAIR_KEYS[0], PAIR_KEYS[2], PAIR_KEYS[4]]
        .into_iter()
        .chain([0x1234_5678, 0x8000_0001])
    {
        let mut setup = vmov(3, key).to_vec();
        setup.push(vxor(4, 3, 1));
        let alu = [vop1(SWAP, 4, vgpr(1))];
        let family = "swap lane pairs";
        let code = program(&setup, &alu, &[], 4);
        let label = format!("v4 = v1 ^ {key:#x}, read v4");
        cases.push(case(name, family, label, code, lanes(), |_, x| x));
        let code = program(&setup, &alu, &[], 1);
        let label = format!("v4 = v1 ^ {key:#x}, read v1");
        cases.push(case(name, family, label, code, lanes(), |_, x| x ^ key));
    }
    let code = program(&[], &[vop1(SWAP, 1, vgpr(1))], &[], 1);
    let family = "swap same register";
    cases.push(case(name, family, "v1, v1".into(), code, lanes(), |_, x| x));
    // v255, the highest VGPR, as VDST and as SRC0, with v255 = b.
    let b = 0x8000_0001;
    let family = "swap max register";
    let code = program(&vmov(255, b), &[vop1(SWAP, 255, vgpr(1))], &[], 1);
    cases.push(case(
        name,
        family,
        "v255, v1".into(),
        code,
        lanes(),
        |_, _| b,
    ));
    let code = program(&vmov(255, b), &[vop1(SWAP, 1, vgpr(255))], &[], 255);
    cases.push(case(
        name,
        family,
        "v1, v255".into(),
        code,
        lanes(),
        |_, x| x,
    ));
    // The other register of each pair, so both writes are seen.
    let code = program(&vmov(255, b), &[vop1(SWAP, 255, vgpr(1))], &[], 255);
    let label = "v255, v1, read v255".into();
    cases.push(case(name, family, label, code, lanes(), |_, x| x));
    let code = program(&vmov(255, b), &[vop1(SWAP, 1, vgpr(255))], &[], 1);
    let label = "v1, v255, read v1".into();
    cases.push(case(name, family, label, code, lanes(), |_, _| b));
    // EXEC: v4 = v1 ^ OLD; active lanes exchange, inactive lanes keep both.
    let mut setup = vmov(5, OLD).to_vec();
    setup.push(vxor(4, 5, 1));
    let family = "swap exec";
    for exec in EXEC_MASKS {
        let mut before = setup.clone();
        before.extend(smov(126, exec));
        let alu = [vop1(SWAP, 4, vgpr(1))];
        let code = program(&before, &alu, &[FULL_EXEC], 4);
        let label = format!("exec {exec:#x}, read v4");
        cases.push(case(name, family, label, code, lanes(), |n, x| {
            masked(exec, n, x, x ^ OLD)
        }));
        let code = program(&before, &alu, &[FULL_EXEC], 1);
        let label = format!("exec {exec:#x}, read v1");
        cases.push(case(name, family, label, code, lanes(), |n, x| {
            masked(exec, n, x ^ OLD, x)
        }));
    }
    let family = "swap partial wave";
    let alu = [vop1(SWAP, 4, vgpr(1))];
    let code = program(&setup, &alu, &[], 4);
    let short = lanes()[..7].to_vec();
    cases.push(case(
        name,
        family,
        "7 lanes".into(),
        code.clone(),
        short,
        |_, x| x,
    ));
    cases.push(case(
        name,
        family,
        "45 lanes".into(),
        code,
        long_lanes(),
        |_, x| x,
    ));
    let exec = 0x5555_5555;
    let mut before = setup.clone();
    before.extend(smov(126, exec));
    let code = program(&before, &alu, &[FULL_EXEC], 1);
    let label = format!("45 lanes, exec {exec:#x}, read v1");
    cases.push(case(name, family, label, code, long_lanes(), |n, x| {
        masked(exec, n, x ^ OLD, x)
    }));
    // Output v4 (= x after the swap) ^ VCC_LO, XORed with v1 (= x ^ OLD after
    // the swap) when SCC is set.
    let family = "swap no vcc or scc write";
    for vcc in [0, u32::MAX] {
        for scc in [false, true] {
            let mut before = setup.clone();
            before.extend(smov(VCC_LO, vcc));
            before.push(if scc { SCC_SET } else { SCC_CLEAR });
            let mut after = vec![0x7e04_0304]; // v2 = v4
            after.extend(FLAG_TAIL);
            let code = program(&before, &alu, &after, 2);
            let label = format!("vcc_lo {vcc:#x}, scc {scc}");
            cases.push(case(name, family, label, code, lanes(), |_, x| {
                x ^ vcc ^ if scc { x ^ OLD } else { 0 }
            }));
        }
    }
    // Partial EXEC with VCC_LO bits inside and outside EXEC, so a VCC write
    // limited to active lanes, or one derived from the swapped data, shows.
    let (exec, vcc) = (0x0f0f_0f0f, 0x0000_00f3);
    let mut before = setup.clone();
    before.extend(smov(VCC_LO, vcc));
    before.push(SCC_SET);
    before.extend(smov(126, exec));
    let mut after = vec![FULL_EXEC, 0x7e04_0304]; // v2 = v4
    after.extend(FLAG_TAIL);
    let code = program(&before, &alu, &after, 2);
    let label = format!("exec {exec:#x}, vcc_lo {vcc:#x}, scc true");
    cases.push(case(name, family, label, code, lanes(), |n, x| {
        masked(exec, n, x, x ^ OLD) ^ vcc ^ masked(exec, n, x ^ OLD, x)
    }));
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    unary_cases(&mut cases);
    swap_cases(&mut cases);
    cases
}

/// Programs both interpreters must refuse at the ALU word, with that offset.
fn rejections() -> Vec<(String, Vec<u8>, usize)> {
    let setup = vmov(3, 7);
    let offset = alu_offset(&setup);
    let mut all = Vec::new();
    // SDWA (249) with SRC0 v1 and DWORD selects, DPP16 (250) with the
    // identity quad_perm, DPP8 (233) and DPP8FI (234) with the identity lane
    // selects; other selectors get no extension dword.
    let extension = |src0: u32| match src0 {
        249 => vec![0x0606_0601],
        250 => vec![0xff00_e401],
        233 | 234 => vec![0xfac6_8801],
        _ => vec![],
    };
    for (form, name) in FORMS {
        let op = form as u32;
        // A literal SRC0 with no literal dword: the program ends at the word.
        let mut code = vec![0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
        code.extend(setup);
        code.push(vop1(op, 2, LITERAL));
        all.push((format!("{name} missing literal"), words(&code), offset));
        // Outside the accepted subset (project policy, not a guide limit):
        // VCC_HI, TTMPs, M0, NULL, EXEC, the reserved and float inline
        // selectors, DPP8, VCCZ, EXECZ, SCC, SDWA, DPP16 and 254 (LDS direct
        // in Table 20, reserved in Table 77).
        for src0 in (107..=127).chain(209..=254) {
            let mut alu = vec![vop1(op, 2, src0)];
            alu.extend(extension(src0));
            let code = program(&setup, &alu, &[], 2);
            all.push((format!("{name} src0 {src0}"), code, offset));
        }
    }
    // V_SWAP_B32 writes SRC0, so every non-VGPR selector is refused; VDST v2
    // is initialized, so only the selector can fail.
    let mut swap_setup = setup.to_vec();
    swap_setup.extend(vmov(2, 5));
    let swap_offset = alu_offset(&swap_setup);
    for src0 in 0..=255 {
        let mut alu = vec![vop1(SWAP, 2, src0)];
        alu.extend(extension(src0));
        if src0 == LITERAL {
            alu.push(7);
        }
        let code = program(&swap_setup, &alu, &[], 2);
        all.push((format!("{SWAP_NAME} src0 {src0}"), code, swap_offset));
    }
    // V_SWAP_B32 also writes v0 (the dispatch index) through either operand:
    // v0 as SRC0 and v0 as VDST, under full EXEC.
    for (dst, src0, what) in [(2, 0, "SRC0"), (0, 2, "VDST")] {
        let code = program(&swap_setup, &[vop1(SWAP, dst, vgpr(src0))], &[], 2);
        all.push((format!("{SWAP_NAME} v0 as {what}"), code, swap_offset));
    }
    // v3 with a load into it outstanding (no wait): read as SRC0, then
    // written as VDST. The swap reads its VDST first, so for it the second
    // case is also a read; no supported form writes v3 without reading it.
    let mut pending = swap_setup.clone();
    pending.extend(PENDING_LOAD_V3);
    let pending_offset = alu_offset(&pending);
    let ops = FORMS.iter().map(|&(form, name)| (form as u32, name));
    for (op, name) in ops.chain([(SWAP, SWAP_NAME)]) {
        for (dst, src0, what) in [(2, 3, "read"), (3, if op == SWAP { 2 } else { 1 }, "write")] {
            let code = program(&pending, &[vop1(op, dst, vgpr(src0))], &[], 2);
            let what = if op == SWAP && what == "write" {
                "VDST read"
            } else {
                what
            };
            let label = format!("{name} {what} of v3 with its load outstanding");
            all.push((label, code, pending_offset));
        }
    }
    all
}

/// `buffer_load_dword v3, v0, s[0:3], 0 idxen` without the wait.
const PENDING_LOAD_V3: [u32; 2] = [0xe030_2000, 0x8000_0300];

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
            let first = format!("{}: {output:?}, expected {:?}", case.label, case.expected);
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
fn expectations_reproduce_the_worked_examples() {
    for (form, s0, d) in GUIDE_EXAMPLES {
        assert_eq!(expect(form, s0), d, "{form:?}({s0:#x})");
    }
}

#[test]
fn vop1_integer_forms_match_the_guide_on_the_reference() {
    let cases = checked_cases();
    let failed = failures(&cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert!(
        failed.is_empty(),
        "reference disagrees with the guide:\n{}",
        failed.join("\n")
    );
    eprintln!("vop1_integer: {} reference cases", cases.len());
    for (form, n) in tally(&cases, |case| case.form) {
        eprintln!("  form {form}: {n}");
    }
}

#[test]
fn vop1_integer_forms_match_the_guide_on_the_independent_interpreter() {
    let cases = checked_cases();
    let failed = failures(&cases, independent_ref::execute);
    assert!(
        failed.is_empty(),
        "independent interpreter disagrees with the guide:\n{}",
        failed.join("\n")
    );
    eprintln!(
        "vop1_integer: {} independent-interpreter cases",
        cases.len()
    );
}

#[test]
fn unsupported_vop1_integer_encodings_are_rejected() {
    let rejections = rejections();
    assert_eq!(rejections.len(), REJECTIONS);
    for (label, code, offset) in &rejections {
        assert_rejected_at(label, code, *offset, &lanes());
    }
    eprintln!("vop1_integer: {} rejections", rejections.len());
}

#[test]
fn swap_scalar_sources_fail_to_decode() {
    // The SRC0 refusal is a decode error, not a failed read of an
    // uninitialized SGPR or VCC_LO at run time.
    for src0 in 0..=255 {
        let code = words(&[vop1(SWAP, 2, src0), 7, 0xbf81_0000]);
        match xenora_ref::decode(&code) {
            Err(error) => assert_eq!(error.offset, 0, "src0 {src0}: {error:?}"),
            Ok(program) => panic!("reference decoded v_swap_b32 src0 {src0}: {program:?}"),
        }
    }
}

#[test]
fn other_vop1_opcodes_stay_unsupported() {
    // Every VOP1 opcode (0-255) except V_MOV_B32 (1, supported before this
    // set), the integer to binary32 conversions (5, 6 and 17-20,
    // tests/vop1_int_to_float.rs), the binary32 rounding forms (33-36,
    // tests/vop1_float_rounding.rs) and these six must be refused at the ALU word. VDST and SRC0 are
    // initialized, so a form that reads only those cannot fail on them; other
    // prerequisites (M0 for the relative moves) are not set up, and the offset
    // alone does not say why a form was refused.
    let supported = [
        1, 5, 6, 17, 18, 19, 20, 33, 34, 35, 36, 55, 56, 57, 58, 59, SWAP,
    ];
    let mut setup = vmov(3, 7).to_vec();
    setup.extend(vmov(2, 5));
    for op in (0..=255).filter(|op| !supported.contains(op)) {
        let code = program(&setup, &[vop1(op, 2, vgpr(3))], &[], 2);
        assert_rejected_at(&format!("opcode {op}"), &code, alu_offset(&setup), &lanes());
    }
}

#[test]
fn compiler_matches_the_vop1_integer_operand_domain() {
    let cases = checked_cases();
    for case in &cases {
        compile(Architecture::Gfx1030, &case.code)
            .unwrap_or_else(|error| panic!("compiler rejected {}: {error:?}", case.label));
    }
    check_compiler_rejections();
    eprintln!(
        "vop1_integer: {} compiler cases and {REJECTIONS} rejections",
        cases.len()
    );
}

fn check_compiler_rejections() {
    for (label, code, offset) in rejections() {
        // Refused at the same word as by the interpreters, so a later failure
        // cannot stand in.
        let at = match compile(Architecture::Gfx1030, &code) {
            Err(CompileError::Decode(error)) => error.offset,
            Err(CompileError::Verify { offset, .. }) => offset,
            other => panic!("compiler did not reject {label}: {other:?}"),
        };
        assert_eq!(at, offset, "compiler rejected {label} elsewhere");
    }
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation"]
fn vop1_integer_forms_match_the_guide_on_native_metal() {
    assert_eq!(std::env::var("MTL_DEBUG_LAYER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MTL_SHADER_VALIDATION").as_deref(), Ok("1"));
    let cases = checked_cases();
    check_compiler_rejections();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "vop1_integer: {} Metal cases in {programs} programs",
        cases.len()
    );
}
