// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded ARM64 machine instruction emitter for DBT blocks.

/// Standard ARM64 Register indices (0..=31, where 31 is SP or ZR depending on instruction).
pub const X0: u8 = 0;
pub const X1: u8 = 1;
pub const X2: u8 = 2;
pub const X3: u8 = 3;
pub const X4: u8 = 4;
pub const X5: u8 = 5;
pub const X6: u8 = 6;
pub const X7: u8 = 7;
pub const X8: u8 = 8;
pub const X9: u8 = 9;
pub const X10: u8 = 10;
pub const X11: u8 = 11;
pub const X12: u8 = 12;
pub const X13: u8 = 13;
pub const X14: u8 = 14;
pub const X15: u8 = 15;
pub const X16: u8 = 16;
pub const X17: u8 = 17;
pub const X18: u8 = 18;
pub const X19: u8 = 19;
pub const X20: u8 = 20;
pub const X21: u8 = 21;
pub const X22: u8 = 22;
pub const X23: u8 = 23;
pub const X24: u8 = 24;
pub const X25: u8 = 25;
pub const X26: u8 = 26;
pub const X27: u8 = 27;
pub const X28: u8 = 28;
pub const X29: u8 = 29; // Frame Pointer (FP)
pub const X30: u8 = 30; // Link Register (LR)
pub const SP: u8 = 31; // Stack Pointer
pub const XZR: u8 = 31; // Zero Register

/// ARM64 Condition codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    Eq = 0,  // Equal (Z == 1)
    Ne = 1,  // Not equal (Z == 0)
    Cs = 2,  // Carry set / unsigned higher or same (C == 1)
    Cc = 3,  // Carry clear / unsigned lower (C == 0)
    Mi = 4,  // Minus / negative (N == 1)
    Pl = 5,  // Plus / positive or zero (N == 0)
    Vs = 6,  // Overflow (V == 1)
    Vc = 7,  // No overflow (V == 0)
    Hi = 8,  // Unsigned higher (C == 1 && Z == 0)
    Ls = 9,  // Unsigned lower or same (C == 0 || Z == 1)
    Ge = 10, // Signed greater than or equal (N == V)
    Lt = 11, // Signed less than (N != V)
    Gt = 12, // Signed greater than (Z == 0 && N == V)
    Le = 13, // Signed less than or equal (Z == 1 || N != V)
    Al = 14, // Always
}

/// Helper for assembling 32-bit ARM64 instructions.
#[derive(Default)]
pub struct Arm64Assembler {
    code: Vec<u32>,
}

impl Arm64Assembler {
    pub fn new() -> Self {
        Self { code: Vec::new() }
    }

    pub fn instructions(&self) -> &[u32] {
        &self.code
    }

    pub(crate) fn clear(&mut self) {
        self.code.clear();
    }

    pub fn current_offset(&self) -> usize {
        self.code.len()
    }

    pub fn emit(&mut self, insn: u32) {
        self.code.push(insn);
    }

    /// RET [X30]
    pub fn ret(&mut self) {
        self.emit(0xd65f03c0);
    }

    /// NOP
    pub fn nop(&mut self) {
        self.emit(0xd503201f);
    }

    /// BR Xn (Branch to register)
    pub fn br(&mut self, xn: u8) {
        self.emit(0xd61f0000 | ((xn as u32 & 0x1f) << 5));
    }

    /// BLR Xn (Branch with link to register)
    pub fn blr(&mut self, xn: u8) {
        self.emit(0xd63f0000 | ((xn as u32 & 0x1f) << 5));
    }

    /// B imm26 (Unconditional branch)
    pub fn b(&mut self, offset_insns: i32) {
        let imm26 = (offset_insns as u32) & 0x03ff_ffff;
        self.emit(0x1400_0000 | imm26);
    }

    /// B.cond imm19 (Conditional branch)
    pub fn b_cond(&mut self, cond: Condition, offset_insns: i32) {
        let imm19 = ((offset_insns as u32) & 0x0007_ffff) << 5;
        self.emit(0x5400_0000 | imm19 | (cond as u32 & 0xf));
    }

    /// Patch previously emitted branch instruction at `index`.
    pub fn patch_b_cond(&mut self, index: usize, cond: Condition, offset_insns: i32) {
        let imm19 = ((offset_insns as u32) & 0x0007_ffff) << 5;
        self.code[index] = 0x5400_0000 | imm19 | (cond as u32 & 0xf);
    }

    /// Patch a previously emitted unconditional `B` at `index`.
    pub fn patch_b(&mut self, index: usize, offset_insns: i32) {
        let imm26 = (offset_insns as u32) & 0x03ff_ffff;
        self.code[index] = 0x1400_0000 | imm26;
    }

    /// CBZ Xt, imm19 (compare and branch on zero, 64-bit).
    pub fn cbz_64(&mut self, xt: u8, offset_insns: i32) {
        let imm19 = ((offset_insns as u32) & 0x0007_ffff) << 5;
        self.emit(0xb4000000 | imm19 | (xt as u32 & 0x1f));
    }

    /// CBNZ Xt, imm19 (compare and branch on nonzero, 64-bit).
    pub fn cbnz_64(&mut self, xt: u8, offset_insns: i32) {
        let imm19 = ((offset_insns as u32) & 0x0007_ffff) << 5;
        self.emit(0xb5000000 | imm19 | (xt as u32 & 0x1f));
    }

    /// Patch a previously emitted `CBZ`/`CBNZ` at `index`.
    pub fn patch_cbz_64(&mut self, index: usize, xt: u8, offset_insns: i32) {
        let imm19 = ((offset_insns as u32) & 0x0007_ffff) << 5;
        let template = self.code[index] & 0xff00_0000;
        self.code[index] = template | imm19 | (xt as u32 & 0x1f);
    }

    /// ADD (extended/shifted register, 64-bit): ADD Xd, Xn, Xm
    pub fn add_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0x8b000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// ADD (shifted register, 64-bit): ADD Xd, Xn, Xm, LSL #shift
    pub fn add_reg_shifted_64(&mut self, xd: u8, xn: u8, xm: u8, shift: u8) {
        self.emit(
            0x8b000000
                | ((shift as u32 & 0x3f) << 10)
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// UXTB (32-bit/zero-extended): UXTB Wd, Wn (UBFM Wd, Wn, #0, #7)
    pub fn uxtb_32(&mut self, wd: u8, wn: u8) {
        self.emit(0x53001c00 | ((wn as u32 & 0x1f) << 5) | (wd as u32 & 0x1f));
    }

    /// UXTH (32-bit/zero-extended): UXTH Wd, Wn (UBFM Wd, Wn, #0, #15)
    pub fn uxth_32(&mut self, wd: u8, wn: u8) {
        self.emit(0x53003c00 | ((wn as u32 & 0x1f) << 5) | (wd as u32 & 0x1f));
    }

    /// SXTB (64-bit/sign-extended): SXTB Xd, Wn (SBFM Xd, Xn, #0, #7)
    pub fn sxtb_64(&mut self, xd: u8, wn: u8) {
        self.emit(0x93401c00 | ((wn as u32 & 0x1f) << 5) | (xd as u32 & 0x1f));
    }

    /// SXTH (64-bit/sign-extended): SXTH Xd, Wn (SBFM Xd, Xn, #0, #15)
    pub fn sxth_64(&mut self, xd: u8, wn: u8) {
        self.emit(0x93403c00 | ((wn as u32 & 0x1f) << 5) | (xd as u32 & 0x1f));
    }

    /// SXTB (32-bit/sign-extended): SXTB Wd, Wn (SBFM Wd, Wn, #0, #7)
    ///
    /// Writing the 32-bit destination form clears bits 63:32 of the
    /// destination register, which is what a 32-bit x86 destination requires.
    pub fn sxtb_32(&mut self, wd: u8, wn: u8) {
        self.emit(0x13001c00 | ((wn as u32 & 0x1f) << 5) | (wd as u32 & 0x1f));
    }

    /// SXTH (32-bit/sign-extended): SXTH Wd, Wn (SBFM Wd, Wn, #0, #15)
    ///
    /// Writing the 32-bit destination form clears bits 63:32 of the
    /// destination register, which is what a 32-bit x86 destination requires.
    pub fn sxth_32(&mut self, wd: u8, wn: u8) {
        self.emit(0x13003c00 | ((wn as u32 & 0x1f) << 5) | (wd as u32 & 0x1f));
    }

    /// LSR (immediate, 64-bit): LSR Xd, Xn, #shift (UBFM Xd, Xn, #shift, #63)
    pub fn lsr_imm_64(&mut self, xd: u8, xn: u8, shift: u8) {
        self.emit(
            0xd340fc00
                | ((u32::from(shift) & 0x3f) << 16)
                | ((u32::from(xn) & 0x1f) << 5)
                | (u32::from(xd) & 0x1f),
        );
    }

    /// CSET Wd, cond (CSINC Wd, WZR, WZR, inverted cond)
    pub fn cset_32(&mut self, wd: u8, cond: Condition) {
        self.emit(0x1a9f_07e0 | (((cond as u32) ^ 1) << 12) | (u32::from(wd) & 0x1f));
    }

    /// CSEL Xd, Xn, Xm, cond
    pub fn csel_64(&mut self, xd: u8, xn: u8, xm: u8, cond: Condition) {
        self.emit(
            0x9a80_0000
                | ((u32::from(xm) & 0x1f) << 16)
                | ((cond as u32) << 12)
                | ((u32::from(xn) & 0x1f) << 5)
                | (u32::from(xd) & 0x1f),
        );
    }

    /// STRB Wt, [Xn, #offset]
    pub fn strb_uoff(&mut self, wt: u8, xn: u8, offset: u32) {
        self.emit(
            0x3900_0000
                | ((offset & 0xfff) << 10)
                | ((u32::from(xn) & 0x1f) << 5)
                | (u32::from(wt) & 0x1f),
        );
    }

    /// LDRB Wt, [Xn, #offset]
    pub fn ldrb_uoff(&mut self, wt: u8, xn: u8, offset: u32) {
        self.emit(
            0x3940_0000
                | ((offset & 0xfff) << 10)
                | ((u32::from(xn) & 0x1f) << 5)
                | (u32::from(wt) & 0x1f),
        );
    }

    /// EOR Wd, Wn, Wm, LSR #shift
    pub fn eor_lsr_32(&mut self, wd: u8, wn: u8, wm: u8, shift: u8) {
        self.emit(
            0x4a40_0000
                | ((u32::from(wm) & 0x1f) << 16)
                | ((u32::from(shift) & 0x1f) << 10)
                | ((u32::from(wn) & 0x1f) << 5)
                | (u32::from(wd) & 0x1f),
        );
    }

    /// ORR Wd, Wn, Wm
    pub fn orr_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x2a00_0000
                | ((u32::from(wm) & 0x1f) << 16)
                | ((u32::from(wn) & 0x1f) << 5)
                | (u32::from(wd) & 0x1f),
        );
    }

    /// BIC Wd, Wn, Wm (Wn & !Wm)
    pub fn bic_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x0a20_0000
                | ((u32::from(wm) & 0x1f) << 16)
                | ((u32::from(wn) & 0x1f) << 5)
                | (u32::from(wd) & 0x1f),
        );
    }

    /// MVN Xd, Xm (ORN Xd, XZR, Xm)
    pub fn mvn_64(&mut self, xd: u8, xm: u8) {
        self.emit(0xaa20_03e0 | ((u32::from(xm) & 0x1f) << 16) | (u32::from(xd) & 0x1f));
    }

    /// UBFX Wd, Wn, #bit, #1
    pub fn ubfx_bit_32(&mut self, wd: u8, wn: u8, bit: u8) {
        self.emit(
            0x5300_0000
                | ((u32::from(bit) & 0x1f) << 16)
                | ((u32::from(bit) & 0x1f) << 10)
                | ((u32::from(wn) & 0x1f) << 5)
                | (u32::from(wd) & 0x1f),
        );
    }

    /// REV Xd, Xn, or REV Wd, Wn when not `wide`
    pub fn rev(&mut self, d: u8, n: u8, wide: bool) {
        self.emit(
            (if wide { 0xdac0_0c00 } else { 0x5ac0_0800 })
                | ((u32::from(n) & 0x1f) << 5)
                | (u32::from(d) & 0x1f),
        );
    }

    /// RBIT Xd, Xn: reverse the bit order of the source.
    /// Panics if a register number exceeds 31.
    pub fn rbit_64(&mut self, xd: u8, xn: u8) {
        assert!(xd <= XZR && xn <= XZR, "RBIT register must be in 0..=31");
        self.emit(0xdac0_0000 | (u32::from(xn) << 5) | u32::from(xd));
    }

    /// CLZ Xd, Xn: count the leading zero bits of the source.
    /// Panics if a register number exceeds 31.
    pub fn clz_64(&mut self, xd: u8, xn: u8) {
        assert!(xd <= XZR && xn <= XZR, "CLZ register must be in 0..=31");
        self.emit(0xdac0_1000 | (u32::from(xn) << 5) | u32::from(xd));
    }

    /// LSLV Xd, Xn, Xm: shift the source left by the amount in Xm.
    /// Panics if a register number exceeds 31.
    pub fn lslv_64(&mut self, xd: u8, xn: u8, xm: u8) {
        assert!(
            xd <= XZR && xn <= XZR && xm <= XZR,
            "LSLV register must be in 0..=31"
        );
        self.emit(0x9ac0_2000 | (u32::from(xm) << 16) | (u32::from(xn) << 5) | u32::from(xd));
    }

    /// LDR Qt, [Xn, #offset] (offset a multiple of 16)
    pub fn ldr_q_uoff(&mut self, qt: u8, xn: u8, offset: u32) {
        self.emit(
            0x3dc0_0000
                | (((offset / 16) & 0xfff) << 10)
                | ((u32::from(xn) & 0x1f) << 5)
                | (u32::from(qt) & 0x1f),
        );
    }

    /// STR Qt, [Xn, #offset] (offset a multiple of 16)
    pub fn str_q_uoff(&mut self, qt: u8, xn: u8, offset: u32) {
        self.emit(
            0x3d80_0000
                | (((offset / 16) & 0xfff) << 10)
                | ((u32::from(xn) & 0x1f) << 5)
                | (u32::from(qt) & 0x1f),
        );
    }

    /// FMOV Dd, Xn (clears the upper half of Vd)
    pub fn fmov_d_from_x(&mut self, vd: u8, xn: u8) {
        self.emit(0x9e67_0000 | ((u32::from(xn) & 0x1f) << 5) | (u32::from(vd) & 0x1f));
    }

    /// MOV Vd.D[1], Xn
    pub fn ins_d1_from_x(&mut self, vd: u8, xn: u8) {
        self.emit(0x4e18_1c00 | ((u32::from(xn) & 0x1f) << 5) | (u32::from(vd) & 0x1f));
    }

    /// Three-register vector instruction from its base word
    pub fn neon3(&mut self, base: u32, vd: u8, vn: u8, vm: u8) {
        self.emit(
            base | ((u32::from(vm) & 0x1f) << 16)
                | ((u32::from(vn) & 0x1f) << 5)
                | (u32::from(vd) & 0x1f),
        );
    }

    /// SHL Vd, Vn, #shift for 0 <= shift < lane
    pub fn shl_vector(&mut self, vd: u8, vn: u8, lane: u32, shift: u32) {
        self.emit(
            0x4f00_5400
                | ((lane + shift) << 16)
                | ((u32::from(vn) & 0x1f) << 5)
                | (u32::from(vd) & 0x1f),
        );
    }

    /// USHR Vd, Vn, #shift for 1 <= shift <= lane
    pub fn ushr_vector(&mut self, vd: u8, vn: u8, lane: u32, shift: u32) {
        self.emit(
            0x6f00_0400
                | ((2 * lane - shift) << 16)
                | ((u32::from(vn) & 0x1f) << 5)
                | (u32::from(vd) & 0x1f),
        );
    }

    /// SSHR Vd, Vn, #shift for 1 <= shift <= lane
    pub fn sshr_vector(&mut self, vd: u8, vn: u8, lane: u32, shift: u32) {
        self.emit(
            0x4f00_0400
                | ((2 * lane - shift) << 16)
                | ((u32::from(vn) & 0x1f) << 5)
                | (u32::from(vd) & 0x1f),
        );
    }

    /// SXTW (64-bit/sign-extended): SXTW Xd, Wn (SBFM Xd, Xn, #0, #31)
    pub fn sxtw_64(&mut self, xd: u8, wn: u8) {
        self.emit(0x93407c00 | ((wn as u32 & 0x1f) << 5) | (xd as u32 & 0x1f));
    }

    /// ADD (extended/shifted register, 32-bit): ADD Wd, Wn, Wm
    pub fn add_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x0b000000
                | ((wm as u32 & 0x1f) << 16)
                | ((wn as u32 & 0x1f) << 5)
                | (wd as u32 & 0x1f),
        );
    }

    /// ADDS (64-bit, flags updating): ADDS Xd, Xn, Xm
    pub fn adds_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xab000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// ADDS (32-bit, flags updating): ADDS Wd, Wn, Wm
    pub fn adds_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x2b000000
                | ((wm as u32 & 0x1f) << 16)
                | ((wn as u32 & 0x1f) << 5)
                | (wd as u32 & 0x1f),
        );
    }

    /// ADCS (64-bit, flags updating): ADCS Xd, Xn, Xm computes Xn + Xm + C.
    pub fn adcs_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xba000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// ADCS (32-bit, flags updating): ADCS Wd, Wn, Wm computes Wn + Wm + C.
    pub fn adcs_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x3a000000
                | ((wm as u32 & 0x1f) << 16)
                | ((wn as u32 & 0x1f) << 5)
                | (wd as u32 & 0x1f),
        );
    }

    /// SBCS (64-bit, flags updating): SBCS Xd, Xn, Xm computes Xn - Xm - !C.
    pub fn sbcs_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xfa000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// SBCS (32-bit, flags updating): SBCS Wd, Wn, Wm computes Wn - Wm - !C.
    pub fn sbcs_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x7a000000
                | ((wm as u32 & 0x1f) << 16)
                | ((wn as u32 & 0x1f) << 5)
                | (wd as u32 & 0x1f),
        );
    }

    /// ADD (immediate, 64-bit): ADD Xd, Xn, #imm12
    pub fn add_imm_64(&mut self, xd: u8, xn: u8, imm12: u16) {
        self.emit(
            0x91000000
                | (((imm12 & 0x0fff) as u32) << 10)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// SUB (immediate, 64-bit): SUB Xd, Xn, #imm12
    pub fn sub_imm_64(&mut self, xd: u8, xn: u8, imm12: u16) {
        self.emit(
            0xd1000000
                | (((imm12 & 0x0fff) as u32) << 10)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// SUB (register, 64-bit): SUB Xd, Xn, Xm
    pub fn sub_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xcb000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// SUB (register, 32-bit): SUB Wd, Wn, Wm
    pub fn sub_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x4b000000
                | ((wm as u32 & 0x1f) << 16)
                | ((wn as u32 & 0x1f) << 5)
                | (wd as u32 & 0x1f),
        );
    }

    /// SUBS (64-bit, flags updating): SUBS Xd, Xn, Xm
    pub fn subs_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xeb000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// SUBS (32-bit, flags updating): SUBS Wd, Wn, Wm
    pub fn subs_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x6b000000
                | ((wm as u32 & 0x1f) << 16)
                | ((wn as u32 & 0x1f) << 5)
                | (wd as u32 & 0x1f),
        );
    }

    /// CMP (register, 64-bit): SUBS XZR, Xn, Xm
    pub fn cmp_reg_64(&mut self, xn: u8, xm: u8) {
        self.subs_reg_64(XZR, xn, xm);
    }

    /// CMP (register, 32-bit): SUBS WZR, Wn, Wm
    pub fn cmp_reg_32(&mut self, wn: u8, wm: u8) {
        self.subs_reg_32(XZR, wn, wm);
    }

    /// AND (register, 64-bit): AND Xd, Xn, Xm
    pub fn and_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0x8a000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// ANDS (register, 64-bit, flags updating / TST): ANDS Xd, Xn, Xm
    pub fn ands_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xea000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// ANDS (register, 32-bit, flags updating / TST): ANDS Wd, Wn, Wm
    pub fn ands_reg_32(&mut self, wd: u8, wn: u8, wm: u8) {
        self.emit(
            0x6a000000
                | ((wm as u32 & 0x1f) << 16)
                | ((wn as u32 & 0x1f) << 5)
                | (wd as u32 & 0x1f),
        );
    }

    /// TST (register, 64-bit): ANDS XZR, Xn, Xm
    pub fn tst_reg_64(&mut self, xn: u8, xm: u8) {
        self.ands_reg_64(XZR, xn, xm);
    }

    /// TST (register, 32-bit): ANDS WZR, Wn, Wm
    pub fn tst_reg_32(&mut self, wn: u8, wm: u8) {
        self.ands_reg_32(XZR, wn, wm);
    }

    /// ORR (register, 64-bit): ORR Xd, Xn, Xm
    pub fn orr_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xaa000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// EOR (register, 64-bit, XOR): EOR Xd, Xn, Xm
    pub fn eor_reg_64(&mut self, xd: u8, xn: u8, xm: u8) {
        self.emit(
            0xca000000
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xd as u32 & 0x1f),
        );
    }

    /// MOV (register, 64-bit): ORR Xd, XZR, Xm
    pub fn mov_reg_64(&mut self, xd: u8, xm: u8) {
        self.orr_reg_64(xd, XZR, xm);
    }

    /// MOV (register, 32-bit zero-extended): ORR Wd, WZR, Wm
    pub fn mov_reg_32(&mut self, wd: u8, wm: u8) {
        self.emit(0x2a000000 | ((wm as u32 & 0x1f) << 16) | (31 << 5) | (wd as u32 & 0x1f));
    }

    /// MOVZ (64-bit with shift): MOVZ Xd, #imm16, LSL #shift
    pub fn movz_64(&mut self, xd: u8, imm16: u16, shift_idx: u8) {
        let hw = (shift_idx & 3) as u32;
        self.emit(0xd2800000 | (hw << 21) | ((imm16 as u32) << 5) | (xd as u32 & 0x1f));
    }

    /// MOVK (64-bit with shift): MOVK Xd, #imm16, LSL #shift
    pub fn movk_64(&mut self, xd: u8, imm16: u16, shift_idx: u8) {
        let hw = (shift_idx & 3) as u32;
        self.emit(0xf2800000 | (hw << 21) | ((imm16 as u32) << 5) | (xd as u32 & 0x1f));
    }

    /// MOVN (64-bit with shift): MOVN Xd, #imm16, LSL #shift writes
    /// NOT (imm16 << shift), so every other halfword becomes 0xFFFF.
    pub fn movn_64(&mut self, xd: u8, imm16: u16, shift_idx: u8) {
        let hw = (shift_idx & 3) as u32;
        self.emit(0x92800000 | (hw << 21) | ((imm16 as u32) << 5) | (xd as u32 & 0x1f));
    }

    /// Load a full 64-bit constant into register Xd with the fewest MOVZ or
    /// MOVN plus MOVK instructions: one for each halfword that differs from
    /// the background the first instruction leaves, and always at least one.
    /// MOVZ leaves zero halfwords and MOVN leaves 0xFFFF ones, so MOVN is used
    /// when more halfwords are 0xFFFF than zero.
    pub fn load_imm_64(&mut self, xd: u8, val: u64) {
        let halves: [u16; 4] = std::array::from_fn(|i| (val >> (16 * i)) as u16);
        let zeros = halves.iter().filter(|&&h| h == 0).count();
        let ones = halves.iter().filter(|&&h| h == 0xffff).count();
        let background = if ones > zeros { 0xffff } else { 0 };
        // The first halfword that differs from the background, or halfword 0
        // when none does (val is 0 or all ones).
        let first = halves.iter().position(|&h| h != background).unwrap_or(0);
        if background == 0 {
            self.movz_64(xd, halves[first], first as u8);
        } else {
            self.movn_64(xd, !halves[first], first as u8);
        }
        for (index, &half) in halves.iter().enumerate().skip(first + 1) {
            if half != background {
                self.movk_64(xd, half, index as u8);
            }
        }
    }

    /// LDR (64-bit, unsigned offset): LDR Xt, [Xn, #imm]
    pub fn ldr_uoff_64(&mut self, xt: u8, xn: u8, offset_bytes: u32) {
        let pimm = (offset_bytes / 8) & 0x0fff;
        self.emit(0xf9400000 | (pimm << 10) | ((xn as u32 & 0x1f) << 5) | (xt as u32 & 0x1f));
    }

    /// STR (64-bit, unsigned offset): STR Xt, [Xn, #imm]
    pub fn str_uoff_64(&mut self, xt: u8, xn: u8, offset_bytes: u32) {
        let pimm = (offset_bytes / 8) & 0x0fff;
        self.emit(0xf9000000 | (pimm << 10) | ((xn as u32 & 0x1f) << 5) | (xt as u32 & 0x1f));
    }

    /// LDR (32-bit, unsigned offset): LDR Wt, [Xn, #imm]
    pub fn ldr_uoff_32(&mut self, wt: u8, xn: u8, offset_bytes: u32) {
        let pimm = (offset_bytes / 4) & 0x0fff;
        self.emit(0xb9400000 | (pimm << 10) | ((xn as u32 & 0x1f) << 5) | (wt as u32 & 0x1f));
    }

    /// STR (32-bit, unsigned offset): STR Wt, [Xn, #imm]
    pub fn str_uoff_32(&mut self, wt: u8, xn: u8, offset_bytes: u32) {
        let pimm = (offset_bytes / 4) & 0x0fff;
        self.emit(0xb9000000 | (pimm << 10) | ((xn as u32 & 0x1f) << 5) | (wt as u32 & 0x1f));
    }

    /// LDR (64-bit register offset): LDR Xt, [Xn, Xm]
    pub fn ldr_reg_64(&mut self, xt: u8, xn: u8, xm: u8) {
        self.emit(
            0xf8606800
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xt as u32 & 0x1f),
        );
    }

    /// STR (64-bit register offset): STR Xt, [Xn, Xm]
    pub fn str_reg_64(&mut self, xt: u8, xn: u8, xm: u8) {
        self.emit(
            0xf8206800
                | ((xm as u32 & 0x1f) << 16)
                | ((xn as u32 & 0x1f) << 5)
                | (xt as u32 & 0x1f),
        );
    }

    /// STP (64-bit pre-index): STP Xt1, Xt2, [Xn, #imm7]!
    pub fn stp_pre_64(&mut self, xt1: u8, xt2: u8, xn: u8, offset_bytes: i32) {
        debug_assert_eq!(
            offset_bytes % 8,
            0,
            "STP pre-index offset must be 8-byte aligned"
        );
        debug_assert!(
            (-512..=504).contains(&offset_bytes),
            "STP pre-index offset out of imm7 range"
        );
        let imm7 = ((offset_bytes / 8) as u32) & 0x7f;
        self.emit(
            0xa9800000
                | (imm7 << 15)
                | ((xt2 as u32 & 0x1f) << 10)
                | ((xn as u32 & 0x1f) << 5)
                | (xt1 as u32 & 0x1f),
        );
    }

    /// LDP (64-bit post-index): LDP Xt1, Xt2, [Xn], #imm7
    pub fn ldp_post_64(&mut self, xt1: u8, xt2: u8, xn: u8, offset_bytes: i32) {
        debug_assert_eq!(
            offset_bytes % 8,
            0,
            "LDP post-index offset must be 8-byte aligned"
        );
        debug_assert!(
            (-512..=504).contains(&offset_bytes),
            "LDP post-index offset out of imm7 range"
        );
        let imm7 = ((offset_bytes / 8) as u32) & 0x7f;
        self.emit(
            0xa8c00000
                | (imm7 << 15)
                | ((xt2 as u32 & 0x1f) << 10)
                | ((xn as u32 & 0x1f) << 5)
                | (xt1 as u32 & 0x1f),
        );
    }

    /// MRS Xt, NZCV (Read flags into register)
    pub fn mrs_nzcv(&mut self, xt: u8) {
        self.emit(0xd53b4200 | (xt as u32 & 0x1f));
    }

    /// MSR NZCV, Xt (Write flags from register)
    pub fn msr_nzcv(&mut self, xt: u8) {
        self.emit(0xd51b4200 | (xt as u32 & 0x1f));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_pair_encodings_cover_every_used_offset() {
        // Cross-checked against the architectural encodings:
        //   stp x29, x30, [sp, #-16]! == 0xa9bf7bfd
        //   ldp x29, x30, [sp], #16  == 0xa8c17bfd
        let mut asm = Arm64Assembler::new();
        asm.stp_pre_64(X29, X30, SP, -16);
        asm.ldp_post_64(X29, X30, SP, 16);
        asm.stp_pre_64(X29, X30, SP, -32);
        asm.ldp_post_64(X29, X30, SP, 32);
        asm.stp_pre_64(X29, X30, SP, -48);
        asm.ldp_post_64(X29, X30, SP, 48);
        assert_eq!(
            asm.instructions(),
            &[
                0xa9bf7bfd, 0xa8c17bfd, 0xa9be7bfd, 0xa8c27bfd, 0xa9bd7bfd, 0xa8c37bfd,
            ]
        );
    }

    #[test]
    fn stp_pre_64_respects_the_requested_frame_size() {
        let mut asm = Arm64Assembler::new();
        asm.stp_pre_64(X19, X20, SP, -64);
        assert_eq!(asm.instructions()[0], 0xa9bc53f3);
        asm.stp_pre_64(X19, X20, SP, 496);
        assert_eq!(asm.instructions()[1], 0xa99f53f3);
    }

    #[test]
    fn alu_and_memory_encodings_match_llvm_mc() {
        // Golden words produced by llvm-mc 23.1.6 for the same instructions.
        let mut asm = Arm64Assembler::new();
        asm.add_reg_64(X9, X9, X10);
        asm.sub_reg_64(X9, X9, X10);
        asm.and_reg_64(X9, X9, X10);
        asm.orr_reg_64(X19, XZR, X0);
        asm.eor_reg_64(X9, X9, X10);
        asm.add_imm_64(X1, X1, 8);
        asm.sub_imm_64(X1, X1, 8);
        asm.movz_64(X9, 0x1234, 0);
        asm.movk_64(X9, 0xabcd, 2);
        asm.ldr_uoff_64(X9, X19, 128);
        asm.str_uoff_64(X21, SP, 32);
        asm.ldr_uoff_32(X0, X19, 136);
        asm.str_uoff_32(X0, X19, 136);
        asm.ldr_reg_64(X9, X19, X10);
        asm.str_reg_64(X9, X19, X10);
        asm.mrs_nzcv(X9);
        asm.msr_nzcv(X9);
        assert_eq!(
            asm.instructions(),
            &[
                0x8b0a0129, 0xcb0a0129, 0x8a0a0129, 0xaa0003f3, 0xca0a0129, 0x91002021, 0xd1002021,
                0xd2824689, 0xf2d579a9, 0xf9404269, 0xf90013f5, 0xb9408a60, 0xb9008a60, 0xf86a6a69,
                0xf82a6a69, 0xd53b4209, 0xd51b4209,
            ]
        );
    }

    #[test]
    fn compare_and_branch_encodings_match_the_assembler() {
        // llvm-mc -triple=aarch64: cbz x5, #28 -> b40000e5;
        // cbnz x9, #-8 -> b5ffffc9; b #20 -> 14000005.
        let mut asm = Arm64Assembler::new();
        asm.cbz_64(X5, 7);
        asm.cbnz_64(X9, -2);
        assert_eq!(asm.instructions(), &[0xb40000e5, 0xb5ffffc9]);
        asm.patch_cbz_64(0, X5, 1);
        assert_eq!(asm.instructions()[0], 0xb4000025);
        let mut branch = Arm64Assembler::new();
        branch.b(0);
        branch.patch_b(0, 5);
        assert_eq!(branch.instructions()[0], 0x14000005);
    }

    #[test]
    fn rbit_clz_lslv_encodings_match_llvm() {
        // A64 words from Apple Clang's integrated assembler and llvm-objdump.
        let mut asm = Arm64Assembler::new();
        asm.rbit_64(X10, X9); // rbit x10, x9
        asm.rbit_64(X0, XZR); // rbit x0, xzr
        asm.clz_64(X10, X10); // clz x10, x10
        asm.clz_64(XZR, X30); // clz xzr, x30
        asm.lslv_64(X11, X11, X10); // lslv x11, x11, x10
        asm.lslv_64(X0, X1, X0); // lslv x0, x1, x0
        asm.lslv_64(XZR, X30, XZR); // lslv xzr, x30, xzr
        assert_eq!(
            asm.instructions(),
            [
                0xdac0012a, 0xdac003e0, 0xdac0114a, 0xdac013df, 0x9aca216b, 0x9ac02020, 0x9adf23df,
            ]
        );
    }

    #[test]
    fn rbit_clz_lslv_reject_out_of_range_registers() {
        let invalid_operands: [fn(&mut Arm64Assembler); 7] = [
            |asm| asm.rbit_64(32, X0),
            |asm| asm.rbit_64(X0, 32),
            |asm| asm.clz_64(32, X0),
            |asm| asm.clz_64(X0, 32),
            |asm| asm.lslv_64(32, X0, X0),
            |asm| asm.lslv_64(X0, 32, X0),
            |asm| asm.lslv_64(X0, X0, 32),
        ];
        for (index, emit) in invalid_operands.into_iter().enumerate() {
            let mut asm = Arm64Assembler::new();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| emit(&mut asm)));
            assert!(result.is_err(), "invalid operand case {index} was accepted");
            assert!(asm.instructions().is_empty());
        }
    }

    #[test]
    fn unsigned_offset_stores_scale_by_access_size() {
        let mut asm = Arm64Assembler::new();
        asm.str_uoff_64(X21, SP, 32);
        asm.ldr_uoff_64(X9, X19, 128);
        asm.str_uoff_32(X0, X19, 136);
        assert_eq!(asm.instructions(), &[0xf90013f5, 0xf9404269, 0xb9008a60]);
    }

    /// Constants at the halfword boundaries the MOVZ/MOVN choice depends on.
    const IMMEDIATE_EDGES: [u64; 16] = [
        0,
        1,
        0xffff,
        0x1_0000,
        0x8000_0000,
        0xffff_ffff,
        0x1_0000_0000,
        0x1234_0000_0000_0000,
        0x0000_0001_0000_0005,
        0x1234_5678_9abc_def0,
        0x7fff_ffff_ffff_ffff,
        0x8000_0000_0000_0000,
        0xffff_0000_ffff_ffff,
        0xffff_ffff_edcc_5678,
        0xffff_ffff_ffff_1234,
        u64::MAX,
    ];

    #[test]
    fn immediate_sequences_match_llvm_encodings() {
        // llvm-mc -triple=aarch64-apple-macos -show-encoding.
        let cases: &[(u64, &[u32])] = &[
            (0, &[0xd2800000]),                                 // movz x0, #0
            (u64::MAX, &[0x92800000]),                          // movn x0, #0
            (0xffff_ffff_ffff_1234, &[0x929db960]),             // movn x0, #0xedcb
            (0x1234_0000_0000_0000, &[0xd2e24680]),             // movz x0, #0x1234, lsl #48
            (0xffff_0000_ffff_ffff, &[0x92dfffe0]),             // movn x0, #0xffff, lsl #32
            (0x0000_0001_0000_0005, &[0xd28000a0, 0xf2c00020]), // movz #5; movk #1, lsl #32
            // movn x0, #0xa987; movk x0, #0xedcc, lsl #16
            (0xffff_ffff_edcc_5678, &[0x929530e0, 0xf2bdb980]),
        ];
        for &(value, expected) in cases {
            let mut asm = Arm64Assembler::new();
            asm.load_imm_64(X0, value);
            assert_eq!(asm.instructions(), expected, "{value:#x}");
        }
        // movz x9, #0xdef0; movk x9, #0x9abc, lsl #16; #0x5678, lsl #32;
        // #0x1234, lsl #48
        let mut asm = Arm64Assembler::new();
        asm.load_imm_64(X9, 0x1234_5678_9abc_def0);
        assert_eq!(
            asm.instructions(),
            &[0xd29bde09, 0xf2b35789, 0xf2cacf09, 0xf2e24689]
        );
    }

    /// Fewest MOVZ/MOVN/MOVK instructions for `value`: every halfword that is
    /// neither zero (after MOVZ) nor 0xFFFF (after MOVN), and at least one.
    fn shortest_wide_move(value: u64) -> usize {
        let halves = (0..4).map(|i| (value >> (16 * i)) & 0xffff);
        let not_zero = halves.clone().filter(|&h| h != 0).count();
        let not_ones = halves.filter(|&h| h != 0xffff).count();
        not_zero.min(not_ones).max(1)
    }

    #[test]
    fn immediates_run_on_the_host_and_use_the_fewest_wide_moves() {
        let mut values = IMMEDIATE_EDGES.to_vec();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..512 {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let raw = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
            // Force some halfwords to zero or all ones so both MOVZ and MOVN
            // sequences of every length occur.
            let mut value = raw;
            for half in 0..4 {
                match (raw >> (60 - 2 * half)) & 3 {
                    0 => value &= !(0xffff << (16 * half)),
                    1 => value |= 0xffff << (16 * half),
                    _ => {}
                }
            }
            values.push(value);
        }
        let mut jit = crate::jit::JitMemory::new(64 * 1024).unwrap();
        for value in values {
            for register in [X0, X9] {
                let mut asm = Arm64Assembler::new();
                asm.load_imm_64(register, value);
                assert_eq!(
                    asm.instructions().len(),
                    shortest_wide_move(value),
                    "{value:#x}"
                );
                if register != X0 {
                    asm.mov_reg_64(X0, register);
                }
                asm.ret();
                let entry = jit.emit_block(asm.instructions()).unwrap();
                // SAFETY: the block is a complete AAPCS64 leaf function that
                // writes only X0 (and X9, a caller-saved register) and returns.
                let function: extern "C" fn() -> u64 = unsafe { std::mem::transmute(entry) };
                assert_eq!(function(), value, "{value:#x} in x{register}");
            }
        }
    }
}
