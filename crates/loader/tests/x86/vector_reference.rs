// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference for the SSE to SSE4.2 moves and packed integer forms, for the tests.
//!
//! It reads only Intel-syntax text and works on byte arrays with exact integer
//! arithmetic, following the operation sections of the Intel SDM Vol. 2. It shares
//! nothing with the registry, decoder, production semantics or NEON lowering.

use xenora_loader::cpu::RFlags;

pub type Bytes = [u8; 16];

#[derive(Clone)]
pub struct Machine {
    pub gpr: [u64; 16],
    pub xmm: [Bytes; 16],
    pub operand: Bytes,
    pub mxcsr: u32,
    /// Flags after the instruction. Every instruction starts from the same.
    pub flags: RFlags,
}

pub const FLAGS: RFlags = RFlags {
    cf: true,
    zf: false,
    sf: true,
    of: false,
    pf: true,
    af: false,
};

// --- reference -------------------------------------------------------------

/// Unsigned lanes of `width` bytes, lane 0 first.
pub fn split(bytes: &[u8], width: usize) -> Vec<i128> {
    bytes
        .chunks(width)
        .map(|chunk| {
            chunk
                .iter()
                .rev()
                .fold(0, |value, byte| value * 256 + i128::from(*byte))
        })
        .collect()
}

pub fn join(lanes: &[i128], width: usize) -> Bytes {
    let mut out = Vec::new();
    for lane in lanes {
        let mut rest = lane.rem_euclid(1i128 << (8 * width));
        for _ in 0..width {
            out.push((rest % 256) as u8);
            rest /= 256;
        }
    }
    out.try_into().unwrap()
}

pub fn negative_form(value: i128, width: usize) -> i128 {
    let modulus = 1i128 << (8 * width);
    if value >= modulus / 2 {
        value - modulus
    } else {
        value
    }
}

pub fn zip(a: &Bytes, b: &Bytes, width: usize, f: impl Fn(i128, i128) -> i128) -> Bytes {
    let pairs: Vec<i128> = split(a, width)
        .into_iter()
        .zip(split(b, width))
        .map(|(x, y)| f(x, y))
        .collect();
    join(&pairs, width)
}

pub fn zip_signed(a: &Bytes, b: &Bytes, width: usize, f: impl Fn(i128, i128) -> i128) -> Bytes {
    zip(a, b, width, |x, y| {
        f(negative_form(x, width), negative_form(y, width))
    })
}

/// Lane width in bytes named by a mnemonic suffix.
pub fn suffix(name: &str) -> usize {
    match name.as_bytes()[name.len() - 1] {
        b'b' => 1,
        b'w' => 2,
        b'd' => 4,
        _ => 8,
    }
}

pub fn clamp(value: i128, low: i128, high: i128) -> i128 {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

pub fn shifted(name: &str, value: &Bytes, count: i128) -> Bytes {
    let width = suffix(name);
    let bits = 8 * width as i128;
    let lanes: Vec<i128> = split(value, width)
        .into_iter()
        .map(|lane| {
            let mut lane = if name.starts_with("psra") {
                negative_form(lane, width)
            } else {
                lane
            };
            for _ in 0..clamp(count, 0, bits) {
                lane = if name.starts_with("psll") {
                    (lane * 2).rem_euclid(1 << bits)
                } else {
                    lane.div_euclid(2) // floors, so negative lanes keep their sign
                };
            }
            lane
        })
        .collect();
    join(&lanes, width)
}

pub fn packed(name: &str, a: &Bytes, b: &Bytes) -> Bytes {
    let canonical = match name {
        "andps" | "andpd" => "pand",
        "andnps" | "andnpd" => "pandn",
        "orps" | "orpd" => "por",
        "xorps" | "xorpd" => "pxor",
        "unpcklps" => "punpckldq",
        "unpcklpd" => "punpcklqdq",
        "unpckhps" => "punpckhdq",
        "unpckhpd" => "punpckhqdq",
        other => other,
    };
    let name = canonical;
    let width = suffix(name);
    let top = 1i128 << (8 * width);
    let truth = |value: bool| if value { -1 } else { 0 };
    match name {
        "pand" => zip(a, b, 1, |x, y| x & y),
        "pandn" => zip(a, b, 1, |x, y| (255 - x) & y),
        "por" => zip(a, b, 1, |x, y| x | y),
        "pxor" => zip(a, b, 1, |x, y| x ^ y),
        "paddb" | "paddw" | "paddd" | "paddq" => zip(a, b, width, |x, y| x + y),
        "psubb" | "psubw" | "psubd" | "psubq" => zip(a, b, width, |x, y| x - y),
        "paddsb" | "paddsw" => zip_signed(a, b, width, |x, y| clamp(x + y, -top / 2, top / 2 - 1)),
        "psubsb" | "psubsw" => zip_signed(a, b, width, |x, y| clamp(x - y, -top / 2, top / 2 - 1)),
        "paddusb" | "paddusw" => zip(a, b, width, |x, y| clamp(x + y, 0, top - 1)),
        "psubusb" | "psubusw" => zip(a, b, width, |x, y| clamp(x - y, 0, top - 1)),
        "pcmpeqb" | "pcmpeqw" | "pcmpeqd" => zip(a, b, width, |x, y| truth(x == y)),
        "pcmpgtb" | "pcmpgtw" | "pcmpgtd" => zip_signed(a, b, width, |x, y| truth(x > y)),
        "pminub" => zip(a, b, 1, |x, y| if x < y { x } else { y }),
        "pmaxub" => zip(a, b, 1, |x, y| if x > y { x } else { y }),
        "pminsw" => zip_signed(a, b, 2, |x, y| if x < y { x } else { y }),
        "pmaxsw" => zip_signed(a, b, 2, |x, y| if x > y { x } else { y }),
        "pminuw" | "pminud" => zip(a, b, width, |x, y| if x < y { x } else { y }),
        "pmaxuw" | "pmaxud" => zip(a, b, width, |x, y| if x > y { x } else { y }),
        "pminsb" | "pminsd" => zip_signed(a, b, width, |x, y| if x < y { x } else { y }),
        "pmaxsb" | "pmaxsd" => zip_signed(a, b, width, |x, y| if x > y { x } else { y }),
        "pmulld" => zip(a, b, 4, |x, y| x * y),
        "pcmpgtq" => zip_signed(a, b, 8, |x, y| truth(x > y)),
        "pcmpeqq" => zip(a, b, 8, |x, y| truth(x == y)),
        "pmuldq" => {
            let (x, y) = (split(a, 4), split(b, 4));
            join(
                &[0, 2].map(|i| negative_form(x[i], 4) * negative_form(y[i], 4)),
                8,
            )
        }
        "packusdw" => {
            let lanes: Vec<i128> = [split(a, 4), split(b, 4)]
                .concat()
                .into_iter()
                .map(|lane| clamp(negative_form(lane, 4), 0, 65535))
                .collect();
            join(&lanes, 2)
        }
        "phminposuw" => {
            let words = split(b, 2);
            let mut best = 0;
            for at in 1..8 {
                if words[at] < words[best] {
                    best = at;
                }
            }
            join(&[words[best], best as i128, 0, 0, 0, 0, 0, 0], 2)
        }
        _ if name.starts_with("pmovsx") || name.starts_with("pmovzx") => {
            let size = |letter: u8| match letter {
                b'b' => 1,
                b'w' => 2,
                b'd' => 4,
                _ => 8,
            };
            let (from, to) = (size(name.as_bytes()[6]), size(name.as_bytes()[7]));
            let lanes: Vec<i128> = split(&b[..16 / to * from], from)
                .into_iter()
                .map(|lane| match name.starts_with("pmovsx") {
                    true => negative_form(lane, from),
                    false => lane,
                })
                .collect();
            join(&lanes, to)
        }
        "pavgb" | "pavgw" => zip(a, b, width, |x, y| (x + y + 1) / 2),
        "pmullw" => zip(a, b, 2, |x, y| x * y),
        "pmulhw" => zip_signed(a, b, 2, |x, y| (x * y).div_euclid(65536)),
        "pmulhuw" => zip(a, b, 2, |x, y| x * y / 65536),
        "pmuludq" => zip(a, b, 8, |x, y| (x % (1 << 32)) * (y % (1 << 32))),
        "pmaddwd" => {
            let (x, y) = (split(a, 2), split(b, 2));
            let product = |i: usize| negative_form(x[i], 2) * negative_form(y[i], 2);
            join(&[0, 2, 4, 6].map(|i| product(i) + product(i + 1)), 4)
        }
        "pshufb" => {
            let mut out = [0; 16];
            for (to, control) in b.iter().enumerate() {
                if *control < 128 {
                    out[to] = a[usize::from(*control) % 16];
                }
            }
            out
        }
        // Neighbours of the destination, then neighbours of the source.
        "phaddw" | "phaddd" | "phaddsw" | "phsubw" | "phsubd" | "phsubsw" => {
            let width = if name.ends_with('d') { 4 } else { 2 };
            let lanes: Vec<i128> = [split(a, width), split(b, width)].concat();
            let results: Vec<i128> = lanes
                .chunks(2)
                .map(|pair| {
                    let (x, y) = (negative_form(pair[0], width), negative_form(pair[1], width));
                    let value = if name.starts_with("phadd") {
                        x + y
                    } else {
                        x - y
                    };
                    if name.ends_with("sw") {
                        clamp(value, -32768, 32767)
                    } else {
                        value
                    }
                })
                .collect();
            join(&results, width)
        }
        // Unsigned destination bytes by signed source bytes.
        "pmaddubsw" => {
            let product = |i: usize| i128::from(a[i]) * negative_form(i128::from(b[i]), 1);
            let sums: Vec<i128> = (0..8)
                .map(|i| clamp(product(2 * i) + product(2 * i + 1), -32768, 32767))
                .collect();
            join(&sums, 2)
        }
        "psignb" | "psignw" | "psignd" => zip_signed(a, b, width, |x, y| {
            if y < 0 {
                -x
            } else if y == 0 {
                0
            } else {
                x
            }
        }),
        "pmulhrsw" => zip_signed(a, b, 2, |x, y| {
            ((x * y).div_euclid(1 << 14) + 1).div_euclid(2)
        }),
        "pabsb" | "pabsw" | "pabsd" => zip_signed(a, b, width, |_, y| if y < 0 { -y } else { y }),
        "psadbw" => {
            let difference = |i: usize| (i128::from(a[i]) - i128::from(b[i])).abs();
            join(
                &[(0..8).map(difference).sum(), (8..16).map(difference).sum()],
                8,
            )
        }
        "packsswb" | "packssdw" | "packuswb" => {
            let from = if name == "packssdw" { 4 } else { 2 };
            let half = 1i128 << (4 * from);
            let (low, high) = if name == "packuswb" {
                (0, half - 1)
            } else {
                (-half / 2, half / 2 - 1)
            };
            let lanes: Vec<i128> = split(a, from)
                .into_iter()
                .chain(split(b, from))
                .map(|lane| clamp(negative_form(lane, from), low, high))
                .collect();
            join(&lanes, from / 2)
        }
        _ if name.starts_with("punpck") => {
            let width = match &name[7..] {
                "bw" => 1,
                "wd" => 2,
                "dq" => 4,
                _ => 8,
            };
            let start = if name.as_bytes()[6] == b'h' { 8 } else { 0 };
            let mut out = Vec::new();
            for at in (start..start + 8).step_by(width) {
                out.extend_from_slice(&a[at..at + width]);
                out.extend_from_slice(&b[at..at + width]);
            }
            out.try_into().unwrap()
        }
        _ if name.starts_with("psll") || name.starts_with("psrl") || name.starts_with("psra") => {
            shifted(name, a, split(&b[..8], 8)[0])
        }
        // Full-width loads.
        "movaps" | "movapd" | "movups" | "movupd" | "movdqa" | "movdqu" | "movntdqa" => *b,
        other => panic!("no reference for {other}"),
    }
}

pub fn register(text: &str) -> Option<usize> {
    text.strip_prefix("xmm")
        .and_then(|index| index.parse().ok())
}

/// Index and width in bytes of a general register used by the fixture.
pub fn general(text: &str) -> Option<(usize, usize)> {
    Some(match text {
        "eax" => (0, 4),
        "ebx" => (3, 4),
        "ebp" => (5, 4),
        "rbx" => (3, 8),
        "r9d" => (9, 4),
        "r10d" => (10, 4),
        "r9" => (9, 8),
        _ => return None,
    })
}

/// A string as its characters, the valid ones first.
pub type Text = Vec<i128>;

/// The SSE4.2 string compares by what each aggregation means for strings, after
/// Intel SDM Vol. 2B, 4.1: `set` is the first operand, `hay` the second.
pub fn string_compare(
    m: &mut Machine,
    name: &str,
    first: Bytes,
    second: Bytes,
    imm: usize,
    wide: bool,
) {
    let width = 1 + imm % 2;
    let count = 16 / width;
    let is_signed = imm / 2 % 2 == 1;
    let length = |register: u64, value: &Bytes| -> usize {
        if name.starts_with("pcmpistr") {
            let characters = split(value, width);
            return characters
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(count);
        }
        let bytes = if wide { 8 } else { 4 };
        let register = split(&register.to_le_bytes()[..bytes], bytes)[0];
        let magnitude = negative_form(register, bytes).abs();
        clamp(magnitude, 0, count as i128) as usize
    };
    let string = |value: &Bytes, length: usize| -> Text {
        split(value, width)
            .into_iter()
            .take(length)
            .map(|character| match is_signed {
                true => negative_form(character, width),
                false => character,
            })
            .collect()
    };
    let set = string(&first, length(m.gpr[0], &first));
    let hay = string(&second, length(m.gpr[2], &second));
    let mut hits: Vec<bool> = (0..count)
        .map(|at| match imm / 4 % 4 {
            // The character is one of the set.
            0 => hay.get(at).is_some_and(|character| set.contains(character)),
            // It is within a range; a lower bound without an upper one is none.
            1 => hay.get(at).is_some_and(|character| {
                set.as_chunks::<2>()
                    .0
                    .iter()
                    .any(|range| (range[0]..=range[1]).contains(character))
            }),
            // The strings agree here: both characters, or both strings ended.
            2 => set.get(at) == hay.get(at),
            // The needle starts here, as far as the register shows.
            _ => set
                .iter()
                .take(count - at)
                .enumerate()
                .all(|(offset, character)| hay.get(at + offset) == Some(character)),
        })
        .collect();
    for (at, hit) in hits.iter_mut().enumerate() {
        let negate = match imm / 16 % 4 {
            1 => true,
            3 => at < hay.len(),
            _ => false,
        };
        *hit = *hit != negate;
    }
    let last = imm / 64 % 2 == 1;
    if name.ends_with('i') {
        let found = match last {
            true => hits.iter().rposition(|hit| *hit),
            false => hits.iter().position(|hit| *hit),
        };
        m.gpr[1] = found.unwrap_or(count) as u64;
    } else {
        let mut out = [0; 16];
        for at in (0..count).filter(|at| hits[*at]) {
            match last {
                true => out[at * width..(at + 1) * width].fill(0xFF),
                false => out[at / 8] += 1 << (at % 8),
            }
        }
        m.xmm[0] = out;
    }
    m.flags = RFlags {
        cf: hits.contains(&true),
        zf: hay.len() < count,
        sf: set.len() < count,
        of: hits[0],
        pf: false,
        af: false,
    };
}

fn qword(bytes: &Bytes, high: bool) -> u64 {
    let half = if high { &bytes[8..] } else { &bytes[..8] };
    u64::from_le_bytes(half.try_into().unwrap())
}

fn carryless_product(a: u64, b: u64) -> u128 {
    let mut product = 0u128;
    for index in 0..64 {
        if b >> index & 1 != 0 {
            for bit in 0..64 {
                if a >> bit & 1 != 0 {
                    product ^= 1u128 << (bit + index);
                }
            }
        }
    }
    product
}

pub fn reference(m: &mut Machine, text: &str) {
    let (name, rest) = text.split_once(' ').unwrap();
    let parts: Vec<&str> = rest.split(", ").collect();
    let width = |text: &str| match text.split(' ').next().unwrap() {
        "xmmword" => 16,
        "qword" => 8,
        "dword" => 4,
        "word" => 2,
        "byte" => 1,
        other => panic!("width {other}"),
    };
    if name == "stmxcsr" {
        m.operand[..4].copy_from_slice(&m.mxcsr.to_le_bytes());
        return;
    }
    let destination = parts[0];
    if destination.contains(" ptr ") {
        // Stores write exactly their operand width.
        let source = m.xmm[register(parts[1]).unwrap()];
        let bytes = width(destination);
        let start = match name {
            _ if name.starts_with("movh") => 8,
            // The lane number wraps at the number of lanes.
            _ if name.starts_with("pextr") || name == "extractps" => {
                parts[2].parse::<usize>().unwrap() % (16 / bytes) * bytes
            }
            _ => 0,
        };
        m.operand[..bytes].copy_from_slice(&source[start..start + bytes]);
        return;
    }
    let source_text = parts.get(1).copied().unwrap_or("");
    let memory = source_text.contains(" ptr ");
    if let Some((index, bytes)) = general(destination) {
        let source = m.xmm[register(source_text).unwrap()];
        let lanes = |width: usize| -> u64 {
            split(&source, width)
                .iter()
                .enumerate()
                .filter(|(_, lane)| **lane >= 1i128 << (8 * width - 1))
                .map(|(at, _)| 1u64 << at)
                .sum()
        };
        m.gpr[index] = match name {
            "movmskps" => lanes(4),
            "movmskpd" => lanes(8),
            "pmovmskb" => lanes(1),
            _ if name.starts_with("pextr") => {
                let lanes = split(&source, suffix(name));
                lanes[parts[2].parse::<usize>().unwrap() % lanes.len()] as u64
            }
            "extractps" => split(&source, 4)[parts[2].parse::<usize>().unwrap() % 4] as u64,
            _ => split(&source, bytes)[0] as u64,
        };
        return;
    }
    let target = register(destination).unwrap();
    let old = m.xmm[target];
    if let Some((index, bytes)) = general(source_text) {
        let value = m.gpr[index].to_le_bytes();
        if name.starts_with("pinsr") {
            let wide = suffix(name);
            let at = parts[2].parse::<usize>().unwrap() % (16 / wide) * wide;
            m.xmm[target][at..at + wide].copy_from_slice(&value[..wide]);
        } else {
            let mut out = [0; 16];
            out[..bytes].copy_from_slice(&value[..bytes]);
            m.xmm[target] = out;
        }
        return;
    }
    if let Ok(count) = source_text.parse::<i128>() {
        m.xmm[target] = match name {
            "pslldq" | "psrldq" => {
                let mut out = [0; 16];
                for (at, byte) in out.iter_mut().enumerate() {
                    let from = at as i128 + if name == "pslldq" { -count } else { count };
                    if (0..16).contains(&from) {
                        *byte = old[from as usize];
                    }
                }
                out
            }
            _ => shifted(name, &old, count),
        };
        return;
    }
    let source = if memory {
        m.operand
    } else {
        m.xmm[register(source_text).unwrap()]
    };
    let imm = parts.get(2).and_then(|text| text.parse::<usize>().ok());
    if name == "ptest" {
        // ZF: no common set bit. CF: no bit of the source outside the target.
        let pairs = || old.iter().zip(&source);
        m.flags = RFlags {
            zf: pairs().all(|(a, b)| a & b == 0),
            cf: pairs().all(|(a, b)| b & (255 - a) == 0),
            ..RFlags::default()
        };
        return;
    }
    if name.starts_with("pcmpestr") || name.starts_with("pcmpistr") {
        string_compare(m, name, old, source, imm.unwrap(), false);
        return;
    }
    let mut out = old;
    match name {
        "movss" | "movsd" | "movq" => {
            let bytes = match name {
                "movss" => 4,
                _ => 8,
            };
            if memory || name == "movq" {
                out = [0; 16];
            }
            out[..bytes].copy_from_slice(&source[..bytes]);
        }
        "movlps" | "movlpd" => out[..8].copy_from_slice(&source[..8]),
        "movhps" | "movhpd" => out[8..].copy_from_slice(&source[..8]),
        "movhlps" => out[..8].copy_from_slice(&source[8..]),
        "movlhps" => out[8..].copy_from_slice(&source[..8]),
        "movd" => {
            out = [0; 16];
            out[..4].copy_from_slice(&source[..4]);
        }
        "pinsrb" | "pinsrw" | "pinsrd" | "pinsrq" => {
            let wide = suffix(name);
            let at = imm.unwrap() % (16 / wide) * wide;
            out[at..at + wide].copy_from_slice(&source[..wide]);
        }
        "pblendvb" | "blendvps" | "blendvpd" => {
            // The last byte of a lane holds its sign.
            let bytes = [1, 4, 8][["pblendvb", "blendvps", "blendvpd"]
                .iter()
                .position(|other| *other == name)
                .unwrap()];
            for (at, byte) in out.iter_mut().enumerate() {
                if m.xmm[0][at / bytes * bytes + bytes - 1] >= 128 {
                    *byte = source[at];
                }
            }
        }
        "pblendw" | "blendps" | "blendpd" => {
            let bytes = match name {
                "pblendw" => 2,
                "blendps" => 4,
                _ => 8,
            };
            for (at, byte) in out.iter_mut().enumerate() {
                if imm.unwrap() / 2usize.pow((at / bytes) as u32) % 2 == 1 {
                    *byte = source[at];
                }
            }
        }
        "insertps" => {
            let digits = [imm.unwrap() / 64, imm.unwrap() / 16 % 4];
            let from = if memory { 0 } else { 4 * digits[0] };
            let value = source[from..from + 4].to_vec();
            out[4 * digits[1]..4 * digits[1] + 4].copy_from_slice(&value);
            for single in 0..4 {
                if imm.unwrap() / 2usize.pow(single) % 2 == 1 {
                    out[4 * single as usize..][..4].fill(0);
                }
            }
        }
        "mpsadbw" => {
            let block = imm.unwrap() % 4 * 4;
            let first = imm.unwrap() / 4 % 2 * 4;
            let sums: Vec<i128> = (0..8)
                .map(|word| {
                    (0..4)
                        .map(|at| {
                            let a = i128::from(old[first + word + at]);
                            (a - i128::from(source[block + at])).abs()
                        })
                        .sum()
                })
                .collect();
            out = join(&sums, 2);
        }
        "pshufd" | "shufps" => {
            for lane in 0..4 {
                let from = imm.unwrap() / 4usize.pow(lane) % 4;
                let lane = lane as usize;
                let input = if name == "shufps" && lane < 2 {
                    old
                } else {
                    source
                };
                out[4 * lane..4 * lane + 4].copy_from_slice(&input[4 * from..4 * from + 4]);
            }
        }
        "pshuflw" | "pshufhw" => {
            out = source;
            let base = if name == "pshufhw" { 8 } else { 0 };
            for lane in 0..4 {
                let from = base + 2 * (imm.unwrap() / 4usize.pow(lane) % 4);
                let to = base + 2 * lane as usize;
                out[to..to + 2].copy_from_slice(&source[from..from + 2]);
            }
        }
        "palignr" => {
            // The source, the destination above it, and zeros beyond.
            let joined = [&source[..], &old[..], &[0; 256][..]].concat();
            out.copy_from_slice(&joined[imm.unwrap()..imm.unwrap() + 16]);
        }
        "pclmulqdq" => {
            // imm8[1:0] selects the destination qword and imm8[3:2] the source
            // qword. XOR every shifted set bit of the first operand where the
            // second has a set bit, without carries between bits.
            let selector = imm.unwrap();
            let a = qword(&old, selector & 1 != 0);
            let b = qword(&source, selector >> 2 & 1 != 0);
            out = carryless_product(a, b).to_le_bytes();
        }
        "shufpd" => {
            let low = 8 * (imm.unwrap() % 2);
            let high = 8 * (imm.unwrap() / 2 % 2);
            out[..8].copy_from_slice(&old[low..low + 8]);
            out[8..].copy_from_slice(&source[high..high + 8]);
        }
        _ => out = packed(name, &old, &source),
    }
    m.xmm[target] = out;
}
