// SPDX-License-Identifier: GPL-3.0-or-later

//! Exact binary32 and binary64 arithmetic with x86 SSE rules.
//!
//! Every operation computes an exact or sticky integer result and rounds once,
//! in any MXCSR rounding mode. NaN selection, the negative default NaN,
//! denormals-are-zero, flush-to-zero, tininess after rounding and the sticky
//! exception flags follow the Intel SDM (Vol. 1, 4.8, 4.9 and 11.5). Host
//! floating point is never used: ARM64 differs in all of those.

use std::cmp::Ordering;

/// MXCSR exception flags, in their register positions.
pub const INVALID: u32 = 1;
pub const DENORMAL: u32 = 1 << 1;
pub const DIVIDE_BY_ZERO: u32 = 1 << 2;
pub const OVERFLOW: u32 = 1 << 3;
pub const UNDERFLOW: u32 = 1 << 4;
pub const PRECISION: u32 = 1 << 5;

/// Exceptions detected before a result is computed.
pub const PRE_COMPUTATION: u32 = INVALID | DENORMAL | DIVIDE_BY_ZERO;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Nearest,
    Down,
    Up,
    Zero,
}

impl Rounding {
    /// From the two-bit encoding MXCSR and the ROUND immediates share.
    pub fn from_bits(bits: u64) -> Self {
        match bits & 3 {
            0 => Rounding::Nearest,
            1 => Rounding::Down,
            2 => Rounding::Up,
            _ => Rounding::Zero,
        }
    }
}

/// Arithmetic controls from MXCSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Control {
    pub rounding: Rounding,
    pub denormals_are_zero: bool,
    pub flush_to_zero: bool,
    pub overflow_masked: bool,
    pub underflow_masked: bool,
}

impl Control {
    pub fn from_mxcsr(mxcsr: u32) -> Self {
        Self {
            rounding: Rounding::from_bits(u64::from(mxcsr >> 13)),
            denormals_are_zero: mxcsr & 1 << 6 != 0,
            flush_to_zero: mxcsr & 1 << 15 != 0,
            overflow_masked: mxcsr & 1 << 10 != 0,
            underflow_masked: mxcsr & 1 << 11 != 0,
        }
    }
}

/// A binary interchange format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub bits: u32,
    /// Stored significand bits.
    fraction: u32,
}

pub const F32: Format = Format {
    bits: 32,
    fraction: 23,
};
pub const F64: Format = Format {
    bits: 64,
    fraction: 52,
};

impl Format {
    fn exponent_max(self) -> u64 {
        (1 << (self.bits - 1 - self.fraction)) - 1
    }

    fn bias(self) -> i32 {
        (self.exponent_max() >> 1) as i32
    }

    fn sign_bit(self) -> u64 {
        1 << (self.bits - 1)
    }

    fn fraction_mask(self) -> u64 {
        (1 << self.fraction) - 1
    }

    fn quiet_bit(self) -> u64 {
        1 << (self.fraction - 1)
    }

    fn infinity(self, sign: bool) -> u64 {
        self.signed(sign, self.exponent_max() << self.fraction)
    }

    fn largest(self, sign: bool) -> u64 {
        self.infinity(sign) - 1
    }

    /// The x86 default NaN is negative and quiet.
    fn default_nan(self) -> u64 {
        self.infinity(true) | self.quiet_bit()
    }

    fn signed(self, sign: bool, magnitude: u64) -> u64 {
        magnitude | if sign { self.sign_bit() } else { 0 }
    }

    fn sign(self, value: u64) -> bool {
        value & self.sign_bit() != 0
    }

    fn magnitude(self, value: u64) -> u64 {
        value & (self.sign_bit() - 1)
    }

    fn is_nan(self, value: u64) -> bool {
        self.magnitude(value) > self.infinity(false)
    }

    fn is_signaling(self, value: u64) -> bool {
        self.is_nan(value) && value & self.quiet_bit() == 0
    }

    fn is_infinite(self, value: u64) -> bool {
        self.magnitude(value) == self.infinity(false)
    }

    fn is_zero(self, value: u64) -> bool {
        self.magnitude(value) == 0
    }

    fn is_denormal(self, value: u64) -> bool {
        let magnitude = self.magnitude(value);
        magnitude != 0 && magnitude <= self.fraction_mask()
    }

    /// Significand and exponent with value `significand * 2^exponent`, for a
    /// finite number.
    fn unpack(self, value: u64) -> (u64, i32) {
        let field = (self.magnitude(value) >> self.fraction) as i32;
        let fraction = value & self.fraction_mask();
        let scale = self.bias() + self.fraction as i32;
        if field == 0 {
            (fraction, 1 - scale)
        } else {
            (fraction | 1 << self.fraction, field - scale)
        }
    }
}

/// A result and the exceptions it raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub bits: u64,
    pub flags: u32,
}

fn exact(bits: u64) -> Outcome {
    Outcome { bits, flags: 0 }
}

fn invalid(format: Format) -> Outcome {
    Outcome {
        bits: format.default_nan(),
        flags: INVALID,
    }
}

/// Divide `value` by 2^shift, rounding, where `sticky` marks a nonzero part
/// below the integer. Returns the quotient and whether it is inexact.
fn round_shift(
    value: u128,
    shift: i32,
    sticky: bool,
    sign: bool,
    rounding: Rounding,
) -> (u128, bool) {
    if shift <= 0 {
        // Callers keep enough bits that a sticky value always has to shift.
        return (value << -shift, sticky);
    }
    let (quotient, half) = if shift > 128 {
        (0, Ordering::Less)
    } else if shift == 128 {
        (0, value.cmp(&(1 << 127)))
    } else {
        let remainder = value & ((1 << shift) - 1);
        (value >> shift, remainder.cmp(&(1 << (shift - 1))))
    };
    let lost = if shift >= 128 {
        value != 0
    } else {
        value & ((1 << shift) - 1) != 0
    };
    let inexact = lost || sticky;
    let half = if half == Ordering::Equal && sticky {
        Ordering::Greater
    } else {
        half
    };
    let up = inexact
        && match rounding {
            Rounding::Nearest => match half {
                Ordering::Greater => true,
                Ordering::Equal => quotient & 1 == 1,
                Ordering::Less => false,
            },
            Rounding::Down => sign,
            Rounding::Up => !sign,
            Rounding::Zero => false,
        };
    (quotient + u128::from(up), inexact)
}

/// Round `significand * 2^exponent`, slightly larger in magnitude if `sticky`,
/// to `format`.
fn round(
    format: Format,
    sign: bool,
    significand: u128,
    exponent: i32,
    sticky: bool,
    control: &Control,
) -> Outcome {
    if significand == 0 {
        return exact(format.signed(sign, 0));
    }
    let precision = format.fraction as i32 + 1;
    let length = 128 - significand.leading_zeros() as i32;
    let shift = length - precision;
    let minimum = 1 - format.bias();
    let mut leading = exponent + length - 1;
    let round_at = |shift| round_shift(significand, shift, sticky, sign, control.rounding);

    // Tininess is detected after rounding to an unbounded exponent.
    let (mut value, mut inexact) = round_at(shift);
    if value >> precision != 0 {
        value >>= 1;
        leading += 1;
    }
    let tiny = leading < minimum;
    let mut flags = 0;
    if tiny && control.flush_to_zero && control.underflow_masked {
        return Outcome {
            bits: format.signed(sign, 0),
            flags: UNDERFLOW | PRECISION,
        };
    }
    let before = exponent + length - 1;
    if before < minimum {
        (value, inexact) = round_at(shift + (minimum - before));
        // A carry out of the fraction lands exactly on the smallest normal.
        leading = minimum;
    }
    if tiny && (inexact || !control.underflow_masked) {
        flags |= UNDERFLOW;
    }
    if inexact {
        flags |= PRECISION;
    }
    if leading > format.bias() {
        if !control.overflow_masked {
            // No result is delivered. Precision is reported only if rounding
            // with an unbounded exponent was inexact.
            return Outcome {
                bits: 0,
                flags: OVERFLOW | if inexact { PRECISION } else { 0 },
            };
        }
        let to_infinity = match control.rounding {
            Rounding::Nearest => true,
            Rounding::Down => sign,
            Rounding::Up => !sign,
            Rounding::Zero => false,
        };
        return Outcome {
            bits: if to_infinity {
                format.infinity(sign)
            } else {
                format.largest(sign)
            },
            flags: OVERFLOW | PRECISION,
        };
    }
    // The leading bit of a normal value adds one to the exponent field.
    let field = (leading - minimum) as u64;
    Outcome {
        bits: format.signed(sign, (field << format.fraction) + value as u64),
        flags,
    }
}

/// Replace a denormal by a signed zero under DAZ, otherwise note it.
fn operand(format: Format, value: u64, control: &Control) -> (u64, u32) {
    if !format.is_denormal(value) {
        (value, 0)
    } else if control.denormals_are_zero {
        (value & format.sign_bit(), 0)
    } else {
        (value, DENORMAL)
    }
}

/// The result of a two-operand form with a NaN source: the first NaN, quieted.
fn select_nan(format: Format, a: u64, b: u64) -> Option<Outcome> {
    let chosen = if format.is_nan(a) {
        a
    } else if format.is_nan(b) {
        b
    } else {
        return None;
    };
    let signaling = format.is_signaling(a) || format.is_signaling(b);
    Some(Outcome {
        bits: chosen | format.quiet_bit(),
        flags: if signaling { INVALID } else { 0 },
    })
}

/// Two-operand arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arithmetic {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

pub fn arithmetic(kind: Arithmetic, format: Format, a: u64, b: u64, control: &Control) -> Outcome {
    let (a, a_flag) = operand(format, a, control);
    let (b, b_flag) = operand(format, b, control);
    if matches!(kind, Arithmetic::Min | Arithmetic::Max) {
        return extremum(kind == Arithmetic::Min, format, a, b, a_flag | b_flag);
    }
    if let Some(outcome) = select_nan(format, a, b) {
        return outcome;
    }
    let (sign_a, sign_b) = (format.sign(a), format.sign(b));
    let sign = sign_a != sign_b;
    let infinite = format.is_infinite(a) || format.is_infinite(b);
    let special = match kind {
        Arithmetic::Add | Arithmetic::Sub if infinite => {
            let sign_b = sign_b != (kind == Arithmetic::Sub);
            Some(if !format.is_infinite(b) {
                exact(a)
            } else if format.is_infinite(a) && sign_a != sign_b {
                invalid(format)
            } else {
                exact(format.infinity(sign_b))
            })
        }
        Arithmetic::Mul if infinite => Some(if format.is_zero(a) || format.is_zero(b) {
            invalid(format)
        } else {
            exact(format.infinity(sign))
        }),
        Arithmetic::Div => {
            let undefined = (format.is_infinite(a) && format.is_infinite(b))
                || (format.is_zero(a) && format.is_zero(b));
            if undefined {
                Some(invalid(format))
            } else if format.is_infinite(a) {
                Some(exact(format.infinity(sign)))
            } else if format.is_infinite(b) {
                Some(exact(format.signed(sign, 0)))
            } else if format.is_zero(b) {
                Some(Outcome {
                    bits: format.infinity(sign),
                    flags: DIVIDE_BY_ZERO,
                })
            } else {
                None
            }
        }
        _ => None,
    };
    let mut outcome = special.unwrap_or_else(|| {
        let (sa, ea) = format.unpack(a);
        let (sb, eb) = format.unpack(b);
        match kind {
            Arithmetic::Mul => round(
                format,
                sign,
                u128::from(sa) * u128::from(sb),
                ea + eb,
                false,
                control,
            ),
            Arithmetic::Div if sa == 0 => exact(format.signed(sign, 0)),
            Arithmetic::Div => {
                let shift = u128::from(sa).leading_zeros() as i32 - 1;
                let dividend = u128::from(sa) << shift;
                let divisor = u128::from(sb);
                round(
                    format,
                    sign,
                    dividend / divisor,
                    ea - eb - shift,
                    !dividend.is_multiple_of(divisor),
                    control,
                )
            }
            _ => {
                let sign_b = sign_b != (kind == Arithmetic::Sub);
                sum(format, (sign_a, sa, ea), (sign_b, sb, eb), control)
            }
        }
    });
    // Invalid operation and divide by zero take priority over a denormal operand.
    if outcome.flags & (INVALID | DIVIDE_BY_ZERO) == 0 {
        outcome.flags |= a_flag | b_flag;
    }
    outcome
}

/// Sum of two finite numbers given as sign, significand and exponent.
fn sum(format: Format, a: (bool, u64, i32), b: (bool, u64, i32), control: &Control) -> Outcome {
    // Alignment is exact up to this distance. Beyond it the larger operand is
    // normal and the smaller is below 2^-17 of its unit in the last place, so it
    // moves the exact result only within a quarter unit of the larger operand,
    // or of the half-sized unit just below a power of two.
    const EXACT: i32 = 70;
    if a.1 == 0 || b.1 == 0 {
        if a.1 == 0 && b.1 == 0 {
            let negative = if a.0 == b.0 {
                a.0
            } else {
                control.rounding == Rounding::Down
            };
            return exact(format.signed(negative, 0));
        }
        let (sign, significand, exponent) = if a.1 == 0 { b } else { a };
        return round(
            format,
            sign,
            u128::from(significand),
            exponent,
            false,
            control,
        );
    }
    let (high, low) = if a.2 >= b.2 { (a, b) } else { (b, a) };
    let distance = high.2 - low.2;
    if distance > EXACT {
        let wide = u128::from(high.1) << EXACT;
        // Any value strictly inside that interval rounds as the exact one does.
        let significand = if high.0 == low.0 { wide } else { wide - 1 };
        return round(format, high.0, significand, high.2 - EXACT, true, control);
    }
    let x = (u128::from(high.1) << distance) as i128 * if high.0 { -1 } else { 1 };
    let y = i128::from(low.1) * if low.0 { -1 } else { 1 };
    let total = x + y;
    if total == 0 {
        return exact(format.signed(control.rounding == Rounding::Down, 0));
    }
    round(
        format,
        total < 0,
        total.unsigned_abs(),
        low.2,
        false,
        control,
    )
}

/// MIN and MAX return the second operand unless the first is strictly smaller
/// or larger; any NaN is an invalid operation and selects the second operand.
fn extremum(minimum: bool, format: Format, a: u64, b: u64, denormal: u32) -> Outcome {
    if format.is_nan(a) || format.is_nan(b) {
        return Outcome {
            bits: b,
            flags: INVALID,
        };
    }
    let order = compare_numbers(format, a, b);
    let first = order
        == if minimum {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    Outcome {
        bits: if first { a } else { b },
        flags: denormal,
    }
}

/// Order of two non-NaN values; zeros of either sign are equal.
fn compare_numbers(format: Format, a: u64, b: u64) -> Ordering {
    let key = |value| {
        let magnitude = format.magnitude(value) as i128;
        if format.sign(value) {
            -magnitude
        } else {
            magnitude
        }
    };
    key(a).cmp(&key(b))
}

/// Compare, with `None` for unordered. A quiet NaN is invalid only when
/// `signaling` is set.
pub fn compare(
    format: Format,
    a: u64,
    b: u64,
    signaling: bool,
    control: &Control,
) -> (Option<Ordering>, u32) {
    let (a, a_flag) = operand(format, a, control);
    let (b, b_flag) = operand(format, b, control);
    if format.is_nan(a) || format.is_nan(b) {
        let raises = signaling || format.is_signaling(a) || format.is_signaling(b);
        return (None, if raises { INVALID } else { 0 });
    }
    (Some(compare_numbers(format, a, b)), a_flag | b_flag)
}

pub fn square_root(format: Format, value: u64, control: &Control) -> Outcome {
    let (value, denormal) = operand(format, value, control);
    if format.is_nan(value) {
        return Outcome {
            bits: value | format.quiet_bit(),
            flags: if format.is_signaling(value) {
                INVALID
            } else {
                0
            },
        };
    }
    if format.is_zero(value) {
        return exact(value);
    }
    if format.sign(value) {
        return invalid(format);
    }
    if format.is_infinite(value) {
        return exact(value);
    }
    let (significand, exponent) = format.unpack(value);
    let mut shift = u128::from(significand).leading_zeros() as i32 - 1;
    if (exponent - shift) % 2 != 0 {
        shift -= 1;
    }
    let wide = u128::from(significand) << shift;
    let root = wide.isqrt();
    let mut outcome = round(
        format,
        false,
        root,
        (exponent - shift) / 2,
        root * root != wide,
        control,
    );
    outcome.flags |= denormal;
    outcome
}

/// Convert between binary32 and binary64.
pub fn convert(from: Format, to: Format, value: u64, control: &Control) -> Outcome {
    let (value, denormal) = operand(from, value, control);
    let sign = from.sign(value);
    if from.is_nan(value) {
        let fraction = value & from.fraction_mask();
        let moved = if to.fraction >= from.fraction {
            fraction << (to.fraction - from.fraction)
        } else {
            fraction >> (from.fraction - to.fraction)
        };
        return Outcome {
            bits: to.infinity(sign) | moved | to.quiet_bit(),
            flags: if from.is_signaling(value) { INVALID } else { 0 },
        };
    }
    if from.is_infinite(value) {
        return exact(to.infinity(sign));
    }
    let (significand, exponent) = from.unpack(value);
    let mut outcome = round(to, sign, u128::from(significand), exponent, false, control);
    outcome.flags |= denormal;
    outcome
}

/// Convert a signed integer.
pub fn from_integer(format: Format, value: i64, control: &Control) -> Outcome {
    round(
        format,
        value < 0,
        u128::from(value.unsigned_abs()),
        0,
        false,
        control,
    )
}

/// Round to an integral value in the same format, keeping the sign of a zero.
/// ROUND reports no denormal operand, and invalid only for a signaling NaN.
pub fn round_to_integral(
    format: Format,
    value: u64,
    rounding: Rounding,
    control: &Control,
) -> Outcome {
    let (value, _) = operand(format, value, control);
    if format.is_nan(value) {
        return Outcome {
            bits: value | format.quiet_bit(),
            flags: if format.is_signaling(value) {
                INVALID
            } else {
                0
            },
        };
    }
    let sign = format.sign(value);
    let (significand, exponent) = format.unpack(value);
    if format.is_infinite(value) || exponent >= 0 {
        return exact(value);
    }
    let (magnitude, inexact) =
        round_shift(u128::from(significand), -exponent, false, sign, rounding);
    // The magnitude fits the significand, so this only normalizes it.
    let mut outcome = round(format, sign, magnitude, 0, false, control);
    if inexact {
        outcome.flags |= PRECISION;
    }
    outcome
}

/// Convert to a signed integer of `bits`. A NaN or a value outside the range
/// gives the integer indefinite value and raises invalid.
pub fn to_integer(
    format: Format,
    value: u64,
    bits: u32,
    truncate: bool,
    control: &Control,
) -> Outcome {
    let indefinite = Outcome {
        bits: 1 << (bits - 1),
        flags: INVALID,
    };
    let (value, _) = operand(format, value, control);
    if format.is_nan(value) || format.is_infinite(value) {
        return indefinite;
    }
    let sign = format.sign(value);
    let (significand, exponent) = format.unpack(value);
    if exponent > 64 && significand != 0 {
        return indefinite;
    }
    let rounding = if truncate {
        Rounding::Zero
    } else {
        control.rounding
    };
    let (magnitude, inexact) =
        round_shift(u128::from(significand), -exponent, false, sign, rounding);
    let limit = 1u128 << (bits - 1);
    if magnitude > limit || (magnitude == limit && !sign) {
        return indefinite;
    }
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let integer = if sign {
        (magnitude as u64).wrapping_neg()
    } else {
        magnitude as u64
    };
    Outcome {
        bits: integer & mask,
        flags: if inexact { PRECISION } else { 0 },
    }
}
