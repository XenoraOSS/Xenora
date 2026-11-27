//! Reusable integer semantic primitives selected by the generated registry dispatch.
//!
//! Each function implements one closed [`crate::semantics::Operation`] at one
//! signedness, with the condition-code rule the RDNA 2 guide gives for it
//! (section 5.3, "Scalar Condition Code", and the SOP2 pseudocode in 12.1).
//! Forms share a primitive only when their declared operation, signedness and
//! SCC effect are identical; the generator never selects one by mnemonic.

/// Unsigned `a + b + carry_in` and the carry-out of bit 31.
pub const fn add_carry_u32(a: u32, b: u32, carry_in: bool) -> (u32, bool) {
    let (partial, first) = a.overflowing_add(b);
    let (value, second) = partial.overflowing_add(carry_in as u32);
    (value, first || second)
}

/// Unsigned `a - b - borrow_in` and the borrow (`b + borrow_in > a`).
pub const fn sub_borrow_u32(a: u32, b: u32, borrow_in: bool) -> (u32, bool) {
    let (partial, first) = a.overflowing_sub(b);
    let (value, second) = partial.overflowing_sub(borrow_in as u32);
    (value, first || second)
}

/// Two's-complement `a + b` and whether the exact sum leaves the i32 range.
pub const fn add_overflow_i32(a: u32, b: u32) -> (u32, bool) {
    let (value, overflow) = (a as i32).overflowing_add(b as i32);
    (value as u32, overflow)
}

/// Two's-complement `a - b` and whether the exact difference leaves the i32 range.
pub const fn sub_overflow_i32(a: u32, b: u32) -> (u32, bool) {
    let (value, overflow) = (a as i32).overflowing_sub(b as i32);
    (value as u32, overflow)
}

/// Bitwise AND and whether the result is nonzero.
pub const fn and_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = a & b;
    (value, value != 0)
}

/// Bitwise OR and whether the result is nonzero.
pub const fn or_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = a | b;
    (value, value != 0)
}

/// Bitwise XOR and whether the result is nonzero.
pub const fn xor_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = a ^ b;
    (value, value != 0)
}

/// Bitwise `a & !b` and whether the result is nonzero.
pub const fn andn2_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = a & !b;
    (value, value != 0)
}

/// Bitwise `a | !b` and whether the result is nonzero.
pub const fn orn2_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = a | !b;
    (value, value != 0)
}

/// Bitwise `!(a & b)` and whether the result is nonzero.
pub const fn nand_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = !(a & b);
    (value, value != 0)
}

/// Bitwise `!(a | b)` and whether the result is nonzero.
pub const fn nor_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = !(a | b);
    (value, value != 0)
}

/// Bitwise `!(a ^ b)` and whether the result is nonzero.
pub const fn xnor_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    let value = !(a ^ b);
    (value, value != 0)
}

/// Bitwise `!a & b` and whether the result is nonzero.
///
/// The `andn1` forms negate the first operand. Guide 12.3, printed page 122.
pub const fn andn1_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    andn2_nonzero_b32(b, a)
}

/// Bitwise `!a | b` and whether the result is nonzero.
///
/// The `orn1` forms negate the first operand. Guide 12.3, printed page 122.
pub const fn orn1_nonzero_b32(a: u32, b: u32) -> (u32, bool) {
    orn2_nonzero_b32(b, a)
}

/// `a << count` and whether the result is nonzero. The caller masks the count
/// to the declared count width, so `count` is below 32.
pub const fn lshl_nonzero_b32(a: u32, count: u32) -> (u32, bool) {
    let value = a << count;
    (value, value != 0)
}

/// Logical `a >> count` (zero fill) and whether the result is nonzero; `count` is below 32.
pub const fn lshr_nonzero_b32(a: u32, count: u32) -> (u32, bool) {
    let value = a >> count;
    (value, value != 0)
}

/// Arithmetic `a >> count` (sign fill) and whether the result is nonzero; `count` is below 32.
pub const fn ashr_nonzero_i32(a: u32, count: u32) -> (u32, bool) {
    let value = ((a as i32) >> count) as u32;
    (value, value != 0)
}

/// The VOPC F test: false for every operand pair.
pub const fn false_b32(_a: u32, _b: u32) -> bool {
    false
}

/// The VOPC T test: true for every operand pair.
pub const fn true_b32(_a: u32, _b: u32) -> bool {
    true
}

/// `a == b` on the raw bits; signedness does not matter for equality.
pub const fn eq_b32(a: u32, b: u32) -> bool {
    a == b
}

/// `a != b` on the raw bits (the guide's LG, "less or greater").
pub const fn ne_b32(a: u32, b: u32) -> bool {
    a != b
}

/// Two's-complement `a > b`.
pub const fn gt_i32(a: u32, b: u32) -> bool {
    (a as i32) > (b as i32)
}

/// Two's-complement `a >= b`.
pub const fn ge_i32(a: u32, b: u32) -> bool {
    (a as i32) >= (b as i32)
}

/// Two's-complement `a < b`.
pub const fn lt_i32(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}

/// Two's-complement `a <= b`.
pub const fn le_i32(a: u32, b: u32) -> bool {
    (a as i32) <= (b as i32)
}

/// Unsigned `a > b`.
pub const fn gt_u32(a: u32, b: u32) -> bool {
    a > b
}

/// Unsigned `a >= b`.
pub const fn ge_u32(a: u32, b: u32) -> bool {
    a >= b
}

/// Unsigned `a < b`.
pub const fn lt_u32(a: u32, b: u32) -> bool {
    a < b
}

/// Unsigned `a <= b`.
pub const fn le_u32(a: u32, b: u32) -> bool {
    a <= b
}

/// `!a` and whether the result is nonzero.
pub const fn not_nonzero_b32(a: u32) -> (u32, bool) {
    let value = !a;
    (value, value != 0)
}

/// `a` with bit `i` moved to bit `31 - i`.
pub const fn brev_b32(a: u32) -> u32 {
    a.reverse_bits()
}

/// Number of zero bits and whether that count is nonzero.
pub const fn bcnt0_nonzero_b32(a: u32) -> (u32, bool) {
    let value = a.count_zeros();
    (value, value != 0)
}

/// Number of one bits and whether that count is nonzero.
pub const fn bcnt1_nonzero_b32(a: u32) -> (u32, bool) {
    let value = a.count_ones();
    (value, value != 0)
}

/// Index of the lowest zero bit, or `0xffffffff` (-1) when every bit is one.
pub const fn ff0_b32(a: u32) -> u32 {
    if a == u32::MAX {
        u32::MAX
    } else {
        a.trailing_ones()
    }
}

/// Index of the lowest one bit, or `0xffffffff` (-1) when every bit is zero.
pub const fn ff1_b32(a: u32) -> u32 {
    if a == 0 { u32::MAX } else { a.trailing_zeros() }
}

/// Zeros above the highest one bit, or `0xffffffff` (-1) when `a` is zero.
pub const fn flbit_b32(a: u32) -> u32 {
    if a == 0 { u32::MAX } else { a.leading_zeros() }
}

/// Bits above the highest bit that differs from the sign bit, or `0xffffffff`
/// (-1) when every bit equals the sign bit (`a` is 0 or -1).
pub const fn flbit_i32(a: u32) -> u32 {
    let differing = if a >> 31 == 0 { a } else { !a };
    if differing == 0 {
        u32::MAX
    } else {
        differing.leading_zeros()
    }
}

/// The low `bits` of `a` sign-extended to 32 bits; `bits` is 8 or 16.
pub const fn sext_i32(a: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    (((a << shift) as i32) >> shift) as u32
}

/// SIMM16 sign-extended to 32 bits (SOPK, guide 12.2).
pub const fn sext_simm16(simm16: u16) -> u32 {
    simm16 as i16 as i32 as u32
}

/// Two's-complement absolute value (wrapping, so `0x80000000` stays) and
/// whether the result is nonzero.
pub const fn abs_nonzero_i32(a: u32) -> (u32, bool) {
    let value = (a as i32).wrapping_abs() as u32;
    (value, value != 0)
}

/// Two's-complement minimum and whether S0 was strictly smaller (so equal
/// operands give S1 and `false`).
pub const fn min_i32(a: u32, b: u32) -> (u32, bool) {
    let first = (a as i32) < (b as i32);
    (if first { a } else { b }, first)
}

/// Unsigned minimum and whether S0 was strictly smaller.
pub const fn min_u32(a: u32, b: u32) -> (u32, bool) {
    let first = a < b;
    (if first { a } else { b }, first)
}

/// Two's-complement maximum and whether S0 was strictly greater (so equal
/// operands give S1 and `false`).
pub const fn max_i32(a: u32, b: u32) -> (u32, bool) {
    let first = (a as i32) > (b as i32);
    (if first { a } else { b }, first)
}

/// Unsigned maximum and whether S0 was strictly greater.
pub const fn max_u32(a: u32, b: u32) -> (u32, bool) {
    let first = a > b;
    (if first { a } else { b }, first)
}

/// `a` when `condition` (SCC) is set, otherwise `b`.
pub const fn cselect_b32(a: u32, b: u32, condition: bool) -> u32 {
    if condition { a } else { b }
}

/// `((1 << a[4:0]) - 1) << b[4:0]`: a mask of `a[4:0]` ones starting at bit `b[4:0]`.
pub const fn bfm_b32(a: u32, b: u32) -> u32 {
    ((1u32 << (a & 0x1f)) - 1) << (b & 0x1f)
}

/// The low 32 bits of the product (identical for signed and unsigned operands).
pub const fn mul_lo_i32(a: u32, b: u32) -> u32 {
    a.wrapping_mul(b)
}

/// The high 32 bits of the unsigned 64-bit product.
pub const fn mul_hi_u32(a: u32, b: u32) -> u32 {
    ((a as u64 * b as u64) >> 32) as u32
}

/// The high 32 bits of the two's-complement 64-bit product.
pub const fn mul_hi_i32(a: u32, b: u32) -> u32 {
    ((a as i32 as i64 * b as i32 as i64) >> 32) as u32
}

/// The `bfe` field descriptor: offset `b[4:0]` and width `b[22:16]` (0-127).
const fn bfe_field(b: u32) -> (u32, u32) {
    (b & 0x1f, (b >> 16) & 0x7f)
}

/// Unsigned bit-field extract: `(a >> offset) & ((1 << width) - 1)`, where a
/// width of 32 or more keeps every shifted bit; and whether the result is nonzero.
pub const fn bfe_nonzero_u32(a: u32, b: u32) -> (u32, bool) {
    let (offset, width) = bfe_field(b);
    let shifted = a >> offset;
    let value = if width == 0 {
        0
    } else if width >= 32 {
        shifted
    } else {
        shifted & ((1 << width) - 1)
    };
    (value, value != 0)
}

/// Signed bit-field extract: `a` shifted arithmetically by the offset, the low
/// `width` bits kept and sign-extended from bit `width - 1` (a width of 32 or
/// more keeps the whole shifted word); and whether the result is nonzero.
pub const fn bfe_nonzero_i32(a: u32, b: u32) -> (u32, bool) {
    let (offset, width) = bfe_field(b);
    let shifted = ((a as i32) >> offset) as u32;
    let value = if width == 0 {
        0
    } else if width >= 32 {
        shifted
    } else {
        let unused = 32 - width;
        (((shifted << unused) as i32) >> unused) as u32
    };
    (value, value != 0)
}

/// `|a - b|` of the wrapped 32-bit difference (so `0x80000000` stays) and
/// whether the result is nonzero.
pub const fn absdiff_nonzero_i32(a: u32, b: u32) -> (u32, bool) {
    let value = (a.wrapping_sub(b) as i32).unsigned_abs();
    (value, value != 0)
}

/// `(a << shift) + b` and the carry out of the exact sum, which counts the bits
/// shifted out of `a`; `shift` is 1 to 4.
pub const fn lshl_add_carry_u32(a: u32, b: u32, shift: u32) -> (u32, bool) {
    let sum = ((a as u64) << shift) + b as u64;
    (sum as u32, sum >> 32 != 0)
}

/// A bitwise copy of the source (RDNA 2 guide section 12.3).
pub(crate) const fn mov_b32(src: u32) -> u32 {
    src
}

// ---- VOPC binary32 compares (#786) ----
// Guide 12.9 Table 61 (pages 152-153). The operands are binary32 bit patterns
// compared as IEEE 754 values: every relation except the N* forms and U is
// false when either operand is a NaN (quiet or signalling), +0 equals -0 and
// subnormals keep their exact value (MODE input denormals allowed).

/// `a < b` (`V_CMP_LT_F32`).
pub const fn lt_f32(a: u32, b: u32) -> bool {
    f32::from_bits(a) < f32::from_bits(b)
}

/// `a == b` (`V_CMP_EQ_F32`).
pub const fn eq_f32(a: u32, b: u32) -> bool {
    f32::from_bits(a) == f32::from_bits(b)
}

/// `a <= b` (`V_CMP_LE_F32`).
pub const fn le_f32(a: u32, b: u32) -> bool {
    f32::from_bits(a) <= f32::from_bits(b)
}

/// `a > b` (`V_CMP_GT_F32`).
pub const fn gt_f32(a: u32, b: u32) -> bool {
    f32::from_bits(a) > f32::from_bits(b)
}

/// `a <> b`, less or greater: false on unordered operands (`V_CMP_LG_F32`).
pub const fn lg_f32(a: u32, b: u32) -> bool {
    lt_f32(a, b) || gt_f32(a, b)
}

/// `a >= b` (`V_CMP_GE_F32`).
pub const fn ge_f32(a: u32, b: u32) -> bool {
    f32::from_bits(a) >= f32::from_bits(b)
}

/// Neither operand is a NaN (`V_CMP_O_F32`).
pub const fn o_f32(a: u32, b: u32) -> bool {
    !u_f32(a, b)
}

/// Either operand is a NaN (`V_CMP_U_F32`, per-opcode table).
pub const fn u_f32(a: u32, b: u32) -> bool {
    f32::from_bits(a).is_nan() || f32::from_bits(b).is_nan()
}

/// `!(a >= b)`: true on unordered operands (`V_CMP_NGE_F32`).
pub const fn nge_f32(a: u32, b: u32) -> bool {
    !ge_f32(a, b)
}

/// `!(a <> b)`: true on unordered operands (`V_CMP_NLG_F32`).
pub const fn nlg_f32(a: u32, b: u32) -> bool {
    !lg_f32(a, b)
}

/// `!(a > b)`: true on unordered operands (`V_CMP_NGT_F32`).
pub const fn ngt_f32(a: u32, b: u32) -> bool {
    !gt_f32(a, b)
}

/// `!(a <= b)`: true on unordered operands (`V_CMP_NLE_F32`).
pub const fn nle_f32(a: u32, b: u32) -> bool {
    !le_f32(a, b)
}

/// `!(a == b)`: true on unordered operands (`V_CMP_NEQ_F32`).
pub const fn neq_f32(a: u32, b: u32) -> bool {
    !eq_f32(a, b)
}

/// `!(a < b)`: true on unordered operands (`V_CMP_NLT_F32`).
pub const fn nlt_f32(a: u32, b: u32) -> bool {
    !lt_f32(a, b)
}
