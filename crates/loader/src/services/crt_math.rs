// SPDX-License-Identifier: GPL-3.0-or-later

//! The Universal C runtime's `math.h` functions beyond `sqrt` and `fabs`
//! (#468), imported through `api-ms-win-crt-math-l1-1-0.dll`.
//!
//! Two kinds of function, as the registry entries say:
//!
//! - Functions whose result IEC 60559 fixes exactly (rounding to an integer,
//!   `copysign`, `fmod`, `remainder`, `fmax`, `fmin`, `fdim`, `frexp`, `modf`,
//!   `lround`, `llround`, `nextafterf`) are computed here on exact operations, so the result
//!   is the one any conforming C library returns.
//! - The others (trigonometric, hyperbolic, exponential, logarithmic, `pow`,
//!   `cbrt`, `hypot`, `erf`, `tgamma`) return the host math library's result.
//!   The C standard does not fix them to the last place, so they can differ
//!   from the Universal CRT's by one unit in the last place.
//!
//! What none of them models: `errno` is never set, no floating-point exception
//! flag is raised, and the guest MXCSR is not visible to a service, so
//! `rint` and `nearbyint` round to nearest, ties to even. A NaN result keeps the
//! first NaN argument, quieted, as SSE does; a NaN produced from non-NaN
//! arguments is the x86 default NaN, which is negative.

use crate::cpu::ExitReason;
use crate::runtime::{Float32, Float64, OutPtr, ServiceContext};

type Outcome<T> = Result<T, ExitReason>;

const DEFAULT_NAN64: u64 = 0xFFF8_0000_0000_0000;
const DEFAULT_NAN32: u32 = 0xFFC0_0000;
const QUIET64: u64 = 1 << 51;
const QUIET32: u32 = 1 << 22;
const SIGN64: u64 = 1 << 63;

/// Host functions the Rust standard library does not offer on a stable
/// toolchain. On macOS they come from libSystem, which is native ARM64.
mod host {
    unsafe extern "C" {
        pub safe fn erf(x: f64) -> f64;
        pub safe fn tgamma(x: f64) -> f64;
        pub safe fn scalbn(x: f64, n: i32) -> f64;
    }
}

fn value64(x: Float64) -> f64 {
    f64::from_bits(x.0)
}

fn value32(x: Float32) -> f32 {
    f32::from_bits(x.0)
}

/// `result` as guest bits, with the NaN rule of the module documentation.
fn result64(arguments: &[Float64], result: f64) -> Float64 {
    if !result.is_nan() {
        return Float64(result.to_bits());
    }
    let first_nan = arguments.iter().find(|x| value64(**x).is_nan());
    Float64(first_nan.map_or(DEFAULT_NAN64, |x| x.0 | QUIET64))
}

fn result32(arguments: &[Float32], result: f32) -> Float32 {
    if !result.is_nan() {
        return Float32(result.to_bits());
    }
    let first_nan = arguments.iter().find(|x| value32(**x).is_nan());
    Float32(first_nan.map_or(DEFAULT_NAN32, |x| x.0 | QUIET32))
}

macro_rules! unary {
    ($float:ident, $value:ident, $result:ident, $host:ty: $($name:ident => $function:expr),+ $(,)?) => {
        $(
            pub fn $name(_: &mut ServiceContext<'_>, x: $float) -> Outcome<$float> {
                let function: fn($host) -> $host = $function;
                Ok($result(&[x], function($value(x))))
            }
        )+
    };
}

macro_rules! binary {
    ($float:ident, $value:ident, $result:ident, $host:ty: $($name:ident => $function:expr),+ $(,)?) => {
        $(
            pub fn $name(_: &mut ServiceContext<'_>, x: $float, y: $float) -> Outcome<$float> {
                let function: fn($host, $host) -> $host = $function;
                Ok($result(&[x, y], function($value(x), $value(y))))
            }
        )+
    };
}

// Exact: IEC 60559 roundToIntegral and its relatives.
unary!(Float64, value64, result64, f64:
    ceil => f64::ceil,
    floor => f64::floor,
    trunc => f64::trunc,
    round => f64::round,
    rint => f64::round_ties_even,
    nearbyint => f64::round_ties_even,
);
unary!(Float32, value32, result32, f32:
    ceilf => f32::ceil,
    floorf => f32::floor,
    roundf => f32::round,
);

// Exact: the remainder of a truncating division is representable.
binary!(Float64, value64, result64, f64: fmod => |x, y| x % y, remainder => remainder_exact, fdim => difference);
binary!(Float32, value32, result32, f32:
    fmodf => |x, y| x % y,
    remainderf => remainder_exact32,
    nextafterf => next_after32,
);

fn remainder_exact32(x: f32, y: f32) -> f32 {
    // Both operands widen exactly and the IEEE remainder is representable
    // in the original precision. No quotient rounding is involved.
    remainder_exact(f64::from(x), f64::from(y)) as f32
}

fn next_after32(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        f32::NAN
    } else if x == y {
        // Equality includes opposite signed zeros. The target supplies the sign.
        y
    } else if x < y {
        x.next_up()
    } else {
        x.next_down()
    }
}

// Host library: correct to within an ulp or so, not to the last place.
unary!(Float64, value64, result64, f64:
    acos => f64::acos,
    asin => f64::asin,
    atan => f64::atan,
    cbrt => f64::cbrt,
    cos => f64::cos,
    cosh => f64::cosh,
    erf => |x| host::erf(x),
    exp => f64::exp,
    exp2 => f64::exp2,
    expm1 => f64::exp_m1,
    log => f64::ln,
    log10 => f64::log10,
    log1p => f64::ln_1p,
    log2 => f64::log2,
    sin => f64::sin,
    sinh => f64::sinh,
    tan => f64::tan,
    tanh => f64::tanh,
    tgamma => |x| host::tgamma(x),
);
binary!(Float64, value64, result64, f64: atan2 => f64::atan2, hypot => f64::hypot, pow => f64::powf);
unary!(Float32, value32, result32, f32:
    cosf => f32::cos,
    expf => f32::exp,
    logf => f32::ln,
    sinf => f32::sin,
    tanf => f32::tan,
);
binary!(Float32, value32, result32, f32: atan2f => f32::atan2, powf => f32::powf);

pub fn copysign(_: &mut ServiceContext<'_>, x: Float64, y: Float64) -> Outcome<Float64> {
    Ok(Float64(x.0 & !SIGN64 | y.0 & SIGN64))
}

/// IEC 60559 maxNum: a NaN loses to a number, and +0 is above -0.
pub fn fmax(_: &mut ServiceContext<'_>, x: Float64, y: Float64) -> Outcome<Float64> {
    Ok(select(x, y, |a, b| a > b || a == b && b.is_sign_negative()))
}

/// IEC 60559 minNum: a NaN loses to a number, and -0 is below +0.
pub fn fmin(_: &mut ServiceContext<'_>, x: Float64, y: Float64) -> Outcome<Float64> {
    Ok(select(x, y, |a, b| a < b || a == b && a.is_sign_negative()))
}

/// `x` when `prefer_x(x, y)` or `y` is NaN, otherwise `y`; NaN only when both are.
fn select(x: Float64, y: Float64, prefer_x: fn(f64, f64) -> bool) -> Float64 {
    let (a, b) = (value64(x), value64(y));
    match (a.is_nan(), b.is_nan()) {
        (true, true) => result64(&[x, y], a),
        (true, false) => y,
        (false, true) => x,
        (false, false) if prefer_x(a, b) => x,
        (false, false) => y,
    }
}

/// `x - y` when `x > y`, otherwise +0.
fn difference(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        f64::NAN
    } else if x > y {
        x - y
    } else {
        0.0
    }
}

/// IEC 60559 remainder: `x - n * y` with `n` the integer nearest `x / y`, ties
/// to even, computed without rounding.
fn remainder_exact(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() || x.is_infinite() || y == 0.0 {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    let divisor = y.abs();
    let mut r = x.abs();
    // Reduce modulo 2|y| first: what remains has the same nearest-integer
    // parity, and `%` is exact.
    if divisor <= f64::MAX / 2.0 {
        r %= 2.0 * divisor;
    }
    let mut odd = false;
    if r >= divisor {
        // Exact: divisor <= r < 2 * divisor.
        r -= divisor;
        odd = true;
    }
    // r is in [0, divisor). Compare it with divisor / 2 without rounding.
    let above_half = if r <= f64::MAX / 2.0 {
        2.0 * r > divisor || 2.0 * r == divisor && odd
    } else {
        // Here r > MAX / 2 >= divisor / 2, so divisor - r is exact.
        r > divisor - r || r == divisor - r && odd
    };
    if above_half {
        // Exact: divisor / 2 <= r < divisor.
        r -= divisor;
    }
    if x.is_sign_negative() { -r } else { r }
}

/// Splits a finite nonzero `x` into a fraction in [0.5, 1) and a power of two.
pub fn frexp(
    context: &mut ServiceContext<'_>,
    x: Float64,
    exponent: OutPtr<4>,
) -> Outcome<Float64> {
    let (fraction, power) = split_exponent(x.0);
    exponent
        .store(context.memory, power.to_le_bytes())
        .map_err(ExitReason::MemoryFault)?;
    Ok(result64(&[x], f64::from_bits(fraction)))
}

/// Zero, an infinity and NaN are returned unchanged with exponent 0.
fn split_exponent(bits: u64) -> (u64, i32) {
    const FRACTION: u64 = (1 << 52) - 1;
    let value = f64::from_bits(bits);
    if value == 0.0 || !value.is_finite() {
        return (bits, 0);
    }
    let (bits, bias) = if bits >> 52 & 0x7FF == 0 {
        // Subnormal: scaling by 2^54 is exact and normalizes it.
        (
            (value * f64::from_bits(0x4350_0000_0000_0000)).to_bits(),
            54,
        )
    } else {
        (bits, 0)
    };
    let field = (bits >> 52 & 0x7FF) as i32;
    let fraction = bits & (SIGN64 | FRACTION) | 1022 << 52;
    (fraction, field - 1022 - bias)
}

/// Splits `x` into integral and fractional parts, both with the sign of `x`.
/// `ldexp` scales by a power of two, exactly as IEC 60559 `scalbn` fixes it:
/// no rounding, a subnormal result where the scaling underflows gradually.
/// The host C library's `scalbn` is that operation.
pub fn ldexp(_: &mut ServiceContext<'_>, x: Float64, exponent: i32) -> Outcome<Float64> {
    Ok(result64(&[x], host::scalbn(value64(x), exponent)))
}

pub fn modf(context: &mut ServiceContext<'_>, x: Float64, integral: OutPtr<8>) -> Outcome<Float64> {
    let value = value64(x);
    let whole = result64(&[x], value.trunc());
    integral
        .store(context.memory, whole.0.to_le_bytes())
        .map_err(ExitReason::MemoryFault)?;
    let fraction = if value.is_infinite() {
        0.0
    } else {
        // Exact: the fractional part of a double is representable.
        (value - value.trunc()).abs()
    };
    let fraction = if value.is_sign_negative() {
        -fraction
    } else {
        fraction
    };
    Ok(result64(&[x], fraction))
}

pub fn modff(
    context: &mut ServiceContext<'_>,
    x: Float32,
    integral: OutPtr<4>,
) -> Outcome<Float32> {
    let value = value32(x);
    let whole = value.trunc();
    integral
        .store(context.memory, result32(&[x], whole).0.to_le_bytes())
        .map_err(ExitReason::MemoryFault)?;
    let fraction = if value.is_infinite() {
        0.0
    } else {
        value - whole
    };
    Ok(result32(&[x], fraction.copysign(value)))
}

pub fn lround(_: &mut ServiceContext<'_>, x: Float64) -> Outcome<i32> {
    Ok(round_windows_long(value64(x)))
}

pub fn lroundf(_: &mut ServiceContext<'_>, x: Float32) -> Outcome<i32> {
    Ok(round_windows_long(f64::from(value32(x))))
}

/// `long` is 32 bits on Windows. A NaN or a rounded value outside the range
/// returns `LONG_MIN`, the value of x86 integer conversion. ISO C leaves that
/// result unspecified.
fn round_windows_long(value: f64) -> i32 {
    let rounded = value.round();
    let inside = (-2_147_483_648.0..2_147_483_648.0).contains(&rounded);
    if inside { rounded as i32 } else { i32::MIN }
}

/// As `lround`, for `long long`; returns the two's-complement bits.
pub fn llround(_: &mut ServiceContext<'_>, x: Float64) -> Outcome<u64> {
    let rounded = value64(x).round();
    let inside = (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&rounded);
    Ok(if inside { rounded as i64 } else { i64::MIN } as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remainder_is_exact_at_ties_and_extremes() {
        for (x, y, expected) in [
            (5.0, 2.0, 1.0),
            (7.0, 2.0, -1.0),
            (3.0, 2.0, -1.0),
            (-5.0, 2.0, -1.0),
            (4.0, 2.0, 0.0),
            (-4.0, 2.0, -0.0),
            // MAX = (2^53 - 1) 2^971 is 2 modulo 3.
            (f64::MAX, 3.0, -1.0),
            (f64::MAX, f64::MAX, 0.0),
            (f64::MAX, 2f64.powi(1023), -(2f64.powi(971))),
        ] {
            let r = remainder_exact(x, y);
            assert_eq!(r.to_bits(), f64::to_bits(expected), "{x} {y}: {r}");
        }
    }

    #[test]
    fn split_exponent_normalizes_subnormals() {
        assert_eq!(split_exponent(1), (0x3FE0_0000_0000_0000, -1073));
        assert_eq!(split_exponent(8.0f64.to_bits()), (0x3FE0_0000_0000_0000, 4));
        assert_eq!(
            split_exponent((-0.75f64).to_bits()),
            ((-0.75f64).to_bits(), 0)
        );
    }
}
