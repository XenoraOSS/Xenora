// SPDX-License-Identifier: GPL-3.0-or-later
//! Native Metal upload and readback of original BC6H and BC7 blocks.
//!
//! Every block below was written for this test, by the encoder here or by a
//! seeded generator. The full-texel reference is the `xenora-texture` CPU
//! decoder. The anchor texels are worked by hand from the Khronos Data Format
//! Specification 1.3 (section 20.1 "BC7", Tables 109 and 119 and Equation 2;
//! section 20.2 "BC6H", Tables 121 and 122 and the unquantization text) and
//! Microsoft's "BC6H Format" and "BC7 Format" pages. Apple's GPU decoder is
//! the value under test, so expected and actual values come from different
//! decoders.
//!
//! BC7 and BC6H are specified with integer arithmetic, so unlike BC1 to BC3
//! every texel must match exactly, with two named exceptions where Apple's
//! decoder was observed to differ from the CPU reference (see [`Divergence`]).
//! Each one the test meets is printed, not hidden. Two-subset BC6H modes are
//! left out because the CPU decoder does not implement them yet (#236).

use super::{context, native};
use xenora_texture::{
    BptcFormat, BptcUpload, BptcUploadDesc, decode_bc6h_sfloat, decode_bc6h_ufloat, decode_bc7,
};

/// Packs fields least significant bit first, the order both formats use.
struct Bits {
    value: u128,
    used: u32,
}

impl Bits {
    fn new() -> Self {
        Self { value: 0, used: 0 }
    }

    fn put(&mut self, field: u32, width: u32) {
        assert!(
            width <= 16 && field < 1 << width,
            "{field} does not fit {width} bits"
        );
        self.value |= u128::from(field) << self.used;
        self.used += width;
    }

    fn finish(self) -> [u8; 16] {
        assert_eq!(self.used, 128, "a block holds exactly 128 bits");
        self.value.to_le_bytes()
    }
}

/// One row of Table 109: subsets, partition, rotation and index-selection
/// bits, colour and alpha bits per endpoint, P-bits per endpoint (`unique`)
/// or per subset (`shared`), and the two index widths.
struct Bc7Mode {
    subsets: usize,
    partition: u32,
    rotation: u32,
    selection: u32,
    colour: u32,
    alpha: u32,
    unique: bool,
    shared: bool,
    index: u32,
    index2: u32,
}

#[rustfmt::skip]
const BC7_MODES: [Bc7Mode; 8] = [
    Bc7Mode { subsets: 3, partition: 4, rotation: 0, selection: 0, colour: 4, alpha: 0, unique: true,  shared: false, index: 3, index2: 0 },
    Bc7Mode { subsets: 2, partition: 6, rotation: 0, selection: 0, colour: 6, alpha: 0, unique: false, shared: true,  index: 3, index2: 0 },
    Bc7Mode { subsets: 3, partition: 6, rotation: 0, selection: 0, colour: 5, alpha: 0, unique: false, shared: false, index: 2, index2: 0 },
    Bc7Mode { subsets: 2, partition: 6, rotation: 0, selection: 0, colour: 7, alpha: 0, unique: true,  shared: false, index: 2, index2: 0 },
    Bc7Mode { subsets: 1, partition: 0, rotation: 2, selection: 1, colour: 5, alpha: 6, unique: false, shared: false, index: 2, index2: 3 },
    Bc7Mode { subsets: 1, partition: 0, rotation: 2, selection: 0, colour: 7, alpha: 8, unique: false, shared: false, index: 2, index2: 2 },
    Bc7Mode { subsets: 1, partition: 0, rotation: 0, selection: 0, colour: 7, alpha: 7, unique: true,  shared: false, index: 4, index2: 0 },
    Bc7Mode { subsets: 2, partition: 6, rotation: 0, selection: 0, colour: 5, alpha: 5, unique: true,  shared: false, index: 2, index2: 0 },
];

/// Stored fields of one BC7 block. `endpoints[subset][endpoint]` holds the
/// stored red, green, blue and alpha bits before P-bits and replication.
struct Bc7 {
    mode: usize,
    partition: u32,
    rotation: u32,
    selection: u32,
    endpoints: Vec<[[u32; 4]; 2]>,
    pbits: Vec<u32>,
    /// Texels whose primary index drops its top bit. The fixtures use
    /// partition 0 or 13 for two subsets (anchors 0 and 15) and partition 0
    /// for three (anchors 0, 3 and 15).
    anchors: &'static [usize],
    indices: [u32; 16],
    indices2: [u32; 16],
}

impl Bc7 {
    fn encode(&self) -> [u8; 16] {
        let mode = &BC7_MODES[self.mode];
        assert_eq!(self.endpoints.len(), mode.subsets);
        let mut bits = Bits::new();
        bits.put(1 << self.mode, self.mode as u32 + 1);
        bits.put(self.partition, mode.partition);
        bits.put(self.rotation, mode.rotation);
        bits.put(self.selection, mode.selection);
        let channels = if mode.alpha > 0 { 4 } else { 3 };
        for channel in 0..channels {
            let width = if channel == 3 {
                mode.alpha
            } else {
                mode.colour
            };
            for subset in &self.endpoints {
                for endpoint in subset {
                    bits.put(endpoint[channel], width);
                }
            }
        }
        let pbits = match (mode.unique, mode.shared) {
            (true, _) => 2 * mode.subsets,
            (_, true) => mode.subsets,
            _ => 0,
        };
        assert_eq!(self.pbits.len(), pbits);
        for &pbit in &self.pbits {
            bits.put(pbit, 1);
        }
        for (texel, &index) in self.indices.iter().enumerate() {
            bits.put(index, mode.index - u32::from(self.anchors.contains(&texel)));
        }
        if mode.index2 > 0 {
            for (texel, &index) in self.indices2.iter().enumerate() {
                bits.put(index, mode.index2 - u32::from(texel == 0));
            }
        }
        bits.finish()
    }
}

const TWO: &[usize] = &[0, 15];
const THREE: &[usize] = &[0, 3, 15];

/// Mode 0, partition 0: columns 0 and 1 are subset 0 except texel 8, columns
/// 2 and 3 subset 1 except texels 9, 10, and row 3 is subset 2.
fn bc7_mode0() -> [u8; 16] {
    Bc7 {
        mode: 0,
        partition: 0,
        rotation: 0,
        selection: 0,
        endpoints: vec![
            [[15, 0, 0, 0], [0, 15, 0, 0]],
            [[0, 0, 15, 0], [15, 15, 15, 0]],
            [[8, 4, 2, 0], [1, 3, 5, 0]],
        ],
        pbits: vec![1, 0, 1, 0, 0, 1],
        anchors: THREE,
        indices: [0, 7, 7, 0, 1, 2, 3, 4, 5, 7, 6, 5, 4, 3, 2, 0],
        indices2: [0; 16],
    }
    .encode()
}

/// Mode 1, partition 0: columns 0 and 1 subset 0, columns 2 and 3 subset 1.
fn bc7_mode1() -> [u8; 16] {
    Bc7 {
        mode: 1,
        partition: 0,
        rotation: 0,
        selection: 0,
        endpoints: vec![
            [[63, 0, 0, 0], [0, 63, 0, 0]],
            [[0, 0, 63, 0], [32, 32, 32, 0]],
        ],
        pbits: vec![1, 0],
        anchors: TWO,
        indices: [0, 1, 2, 3, 4, 5, 6, 7, 7, 6, 5, 4, 3, 2, 1, 0],
        indices2: [0; 16],
    }
    .encode()
}

/// Mode 2, three-subset partition 0, as mode 0.
fn bc7_mode2() -> [u8; 16] {
    Bc7 {
        mode: 2,
        partition: 0,
        rotation: 0,
        selection: 0,
        endpoints: vec![
            [[31, 0, 0, 0], [0, 0, 31, 0]],
            [[16, 16, 16, 0], [0, 31, 0, 0]],
            [[1, 2, 3, 0], [30, 29, 28, 0]],
        ],
        pbits: vec![],
        anchors: THREE,
        indices: [0, 3, 1, 0, 2, 3, 3, 1, 1, 3, 3, 3, 3, 3, 3, 1],
        indices2: [0; 16],
    }
    .encode()
}

/// Mode 3, partition 13: rows 0 and 1 subset 0, rows 2 and 3 subset 1.
fn bc7_mode3() -> [u8; 16] {
    Bc7 {
        mode: 3,
        partition: 13,
        rotation: 0,
        selection: 0,
        endpoints: vec![
            [[0x7f, 0x00, 0x55, 0], [0x00, 0x7f, 0x2a, 0]],
            [[0x40, 0x40, 0x40, 0], [0x10, 0x20, 0x30, 0]],
        ],
        pbits: vec![1, 0, 0, 1],
        anchors: TWO,
        indices: [0, 1, 2, 3, 3, 2, 1, 0, 0, 1, 2, 3, 3, 2, 1, 1],
        indices2: [0; 16],
    }
    .encode()
}

/// Mode 4 with the given rotation and index selection.
fn bc7_mode4(rotation: u32, selection: u32, indices: [u32; 16], indices2: [u32; 16]) -> [u8; 16] {
    Bc7 {
        mode: 4,
        partition: 0,
        rotation,
        selection,
        endpoints: vec![[[31, 0, 16, 63], [0, 31, 8, 0]]],
        pbits: vec![],
        anchors: &[0],
        indices,
        indices2,
    }
    .encode()
}

/// Mode 5 with rotation 2, which swaps alpha and green.
fn bc7_mode5() -> [u8; 16] {
    Bc7 {
        mode: 5,
        partition: 0,
        rotation: 2,
        selection: 0,
        endpoints: vec![[[127, 0, 64, 0x10], [0, 127, 32, 0xf0]]],
        pbits: vec![],
        anchors: &[0],
        indices: [0, 3, 1, 2, 3, 2, 1, 0, 0, 1, 2, 3, 3, 2, 1, 0],
        indices2: [1, 3, 2, 0, 0, 1, 2, 3, 3, 2, 1, 0, 1, 1, 2, 2],
    }
    .encode()
}

/// Mode 6, index `t` in texel `t`.
fn bc7_mode6() -> [u8; 16] {
    Bc7 {
        mode: 6,
        partition: 0,
        rotation: 0,
        selection: 0,
        endpoints: vec![[[0x7f, 0x00, 0x40, 0x7f], [0x00, 0x7f, 0x20, 0x00]]],
        pbits: vec![1, 0],
        anchors: &[0],
        indices: std::array::from_fn(|t| t as u32),
        indices2: [0; 16],
    }
    .encode()
}

/// Mode 7, two-subset partition 0, as mode 1.
fn bc7_mode7() -> [u8; 16] {
    Bc7 {
        mode: 7,
        partition: 0,
        rotation: 0,
        selection: 0,
        endpoints: vec![
            [[31, 0, 0, 31], [0, 31, 0, 0]],
            [[0, 0, 31, 16], [10, 20, 5, 31]],
        ],
        pbits: vec![1, 0, 0, 1],
        anchors: TWO,
        indices: [0, 3, 3, 0, 1, 2, 1, 2, 3, 0, 0, 3, 2, 1, 2, 1],
        indices2: [0; 16],
    }
    .encode()
}

/// A zero low byte selects no mode. The rest of the block is set so that a
/// decoder reading it anyway would not return zero.
const BC7_RESERVED: [u8; 16] = {
    let mut block = [0xff; 16];
    block[0] = 0;
    block
};

/// A deterministic generator for blocks no encoder chose.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn block(&mut self) -> [u8; 16] {
        let mut block = [0; 16];
        block[..8].copy_from_slice(&self.next().to_le_bytes());
        block[8..].copy_from_slice(&self.next().to_le_bytes());
        block
    }
}

/// Random BC7 bits with the low byte rewritten to select `mode`.
fn random_bc7(random: &mut SplitMix, mode: u8) -> [u8; 16] {
    let mut block = random.block();
    let above = u8::MAX.checked_shl(u32::from(mode) + 1).unwrap_or(0);
    block[0] = (block[0] & above) | (1 << mode);
    block
}

/// Random BC6H bits with the five mode bits rewritten.
fn random_bc6h(random: &mut SplitMix, mode: u8) -> [u8; 16] {
    let mut block = random.block();
    block[0] = (block[0] & 0xe0) | mode;
    block
}

/// The binary32 bit pattern of a binary16 value, exactly.
fn half_to_single(half: u16) -> u32 {
    let sign = u32::from(half >> 15) << 31;
    let exponent = u32::from(half >> 10) & 0x1f;
    let mantissa = u32::from(half) & 0x3ff;
    match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            // A subnormal half is mantissa * 2^-24, a normal single.
            let shift = mantissa.leading_zeros() - 21;
            let mantissa = (mantissa << shift) & 0x3ff;
            sign | ((127 - 15 + 1 - shift) << 23) | (mantissa << 13)
        }
        0x1f => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 127 - 15) << 23) | (mantissa << 13),
    }
}

const ONE: u32 = 0x3f80_0000;

/// GPU words of each texel: one for BC7, four for BC6H.
fn words_per_texel(format: BptcFormat) -> usize {
    match format {
        BptcFormat::Bc7Unorm => 1,
        BptcFormat::Bc6hUfloat | BptcFormat::Bc6hSfloat => 4,
    }
}

/// CPU-decoded words of texel `t` of `block`, in the GPU readback layout.
fn decoded(format: BptcFormat, block: [u8; 16], t: usize) -> Vec<u32> {
    match format {
        BptcFormat::Bc7Unorm => vec![u32::from_le_bytes(decode_bc7(block)[t])],
        BptcFormat::Bc6hUfloat | BptcFormat::Bc6hSfloat => {
            let decode = if format == BptcFormat::Bc6hUfloat {
                decode_bc6h_ufloat
            } else {
                decode_bc6h_sfloat
            };
            let [r, g, b] = decode(block).expect("single-subset or reserved mode")[t];
            vec![half_to_single(r), half_to_single(g), half_to_single(b), ONE]
        }
    }
}

/// A texel where Apple's decoder was observed to differ from the CPU
/// reference, on an Apple M1 Max (macOS 27.2.1). Each is accepted only as the
/// one alternative named here, and every occurrence is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Divergence {
    /// A BC7 block whose low byte is zero has no mode. The Khronos text and
    /// Microsoft's "BC7 Format Mode Reference" both return zero in every
    /// channel, as the CPU decoder does; the GPU returned opaque black, alpha
    /// 1.0 and the colour channels 0.
    Bc7ReservedOpaque,
    /// A BC6H SF16 channel that interpolates to -1 finishes as 0x8000 (-0.0)
    /// in the Khronos text, which the CPU decoder follows, and as 0x0000 in
    /// Microsoft's sample code. The GPU returned +0.0, Microsoft's reading.
    Bc6hNegativeZero,
}

/// The one alternative accepted for texel `t` of `block`, if any.
fn alternative(format: BptcFormat, block: [u8; 16], t: usize) -> Option<(Divergence, Vec<u32>)> {
    match format {
        BptcFormat::Bc7Unorm if block[0] == 0 => {
            Some((Divergence::Bc7ReservedOpaque, vec![0xff00_0000]))
        }
        BptcFormat::Bc6hSfloat => {
            let words = decoded(format, block, t);
            let positive: Vec<u32> = words
                .iter()
                .map(|&word| if word == 0x8000_0000 { 0 } else { word })
                .collect();
            (positive != words).then_some((Divergence::Bc6hNegativeZero, positive))
        }
        _ => None,
    }
}

/// Expected words of one texel, with its accepted alternative.
struct Expected {
    words: Vec<u32>,
    alternative: Option<(Divergence, Vec<u32>)>,
}

/// Crops a row-major grid of blocks to a `width`×`height` level of CPU-decoded
/// texels in the GPU readback layout.
fn reference(
    format: BptcFormat,
    blocks: &[[u8; 16]],
    blocks_x: usize,
    (width, height): (usize, usize),
) -> Vec<Expected> {
    let mut texels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let block = blocks[(y / 4) * blocks_x + x / 4];
            let t = (y % 4) * 4 + x % 4;
            texels.push(Expected {
                words: decoded(format, block, t),
                alternative: alternative(format, block, t),
            });
        }
    }
    texels
}

/// Fails on the first texel that matches neither the CPU reference nor its
/// named alternative, and prints every texel that matched the alternative.
fn assert_texels(label: &str, format: BptcFormat, actual: &[u32], expected: &[Expected]) {
    let words = words_per_texel(format);
    assert_eq!(actual.len(), expected.len() * words, "{label}: word count");
    let mut divergent: Vec<(Divergence, usize)> = Vec::new();
    for (texel, (got, want)) in actual.chunks_exact(words).zip(expected).enumerate() {
        if got == want.words {
            continue;
        }
        match &want.alternative {
            Some((divergence, alternative)) if got == alternative => {
                divergent.push((*divergence, texel));
            }
            _ => panic!(
                "{label}: texel {texel} got {got:08x?}, want {:08x?} (accepted alternative {:08x?})",
                want.words, want.alternative
            ),
        }
    }
    for divergence in [Divergence::Bc7ReservedOpaque, Divergence::Bc6hNegativeZero] {
        let texels: Vec<usize> = divergent
            .iter()
            .filter(|(kind, _)| *kind == divergence)
            .map(|&(_, texel)| texel)
            .collect();
        if !texels.is_empty() {
            println!(
                "OBSERVED {label}: {divergence:?} at {} texel(s) {texels:?}",
                texels.len()
            );
        }
    }
}

/// Uploads `blocks` as one `width`×`height` level with `row_pitch`, reads it
/// on the GPU and compares every texel with the CPU decoder.
fn read_level(
    format: BptcFormat,
    blocks: &[[u8; 16]],
    (width, height, row_pitch): (usize, usize, usize),
) -> Vec<u32> {
    let bytes = blocks.as_flattened();
    let upload = BptcUpload::new(
        bytes,
        BptcUploadDesc {
            width,
            height,
            mips: 1,
            layers: 1,
            format,
            row_pitch,
            slice_pitch: bytes.len(),
        },
    )
    .unwrap();
    let actual = native(context().read_bptc_upload(&upload, 0, 0));
    let blocks_x = row_pitch / BptcUpload::BLOCK_BYTES;
    let expected = reference(format, blocks, blocks_x, (width, height));
    assert_texels(
        &format!("{format:?} {width}x{height}"),
        format,
        &actual,
        &expected,
    );
    actual
}

const WIDTH: usize = 5;
const HEIGHT: usize = 5;
const ROW_PITCH: usize = 48;
const SLICE_PITCH: usize = 112;

/// Mode 6 with opaque magenta endpoints, in every padding slot, so a wrong
/// row or slice pitch reads visibly wrong texels.
fn pad() -> [u8; 16] {
    Bc7 {
        mode: 6,
        partition: 0,
        rotation: 0,
        selection: 0,
        endpoints: vec![[[0x7f, 0, 0x7f, 0x7f], [0x7f, 0, 0x7f, 0x7f]]],
        pbits: vec![1, 1],
        anchors: &[0],
        indices: [0; 16],
        indices2: [0; 16],
    }
    .encode()
}

/// Logical blocks of one layer: the 2×2 base-level grid, then mip 1.
type Layer = [[u8; 16]; 5];

/// Two layers of a 5×5 texture with two mips, laid out as the BC2 and BC3
/// test lays them out: rows of two logical blocks and one padding block, a
/// padding block that closes each slice, and mip 1 in its slice's block zero.
fn upload_bytes(layers: &[Layer; 2]) -> Vec<u8> {
    let pad = pad();
    let mut bytes = Vec::new();
    for &[b0, b1, b2, b3, small] in layers {
        for slot in [b0, b1, pad, b2, b3, pad, pad] {
            bytes.extend_from_slice(&slot);
        }
        for slot in [small, pad, pad, pad, pad, pad, pad] {
            bytes.extend_from_slice(&slot);
        }
    }
    assert_eq!(bytes.len(), 2 * 2 * SLICE_PITCH);
    bytes
}

#[test]
fn reads_pitched_bc7_modes_across_mips_and_layers() {
    let format = BptcFormat::Bc7Unorm;
    // Rotation 1 swaps alpha and red; index selection 1 gives colour the
    // 3-bit indices and alpha the 2-bit ones.
    let rotated = bc7_mode4(
        1,
        1,
        [1, 0, 3, 2, 2, 3, 0, 1, 1, 1, 1, 1, 0, 0, 0, 0],
        [2, 7, 5, 0, 1, 3, 4, 6, 7, 7, 0, 0, 1, 2, 3, 4],
    );
    let plain = bc7_mode4(
        0,
        0,
        [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3],
        [3, 7, 0, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3, 4, 5],
    );
    let layers = [
        [bc7_mode0(), bc7_mode1(), bc7_mode2(), bc7_mode3(), plain],
        [bc7_mode5(), bc7_mode6(), bc7_mode7(), BC7_RESERVED, rotated],
    ];
    let bytes = upload_bytes(&layers);
    let upload = BptcUpload::new(
        &bytes,
        BptcUploadDesc {
            width: WIDTH,
            height: HEIGHT,
            mips: 2,
            layers: 2,
            format,
            row_pitch: ROW_PITCH,
            slice_pitch: SLICE_PITCH,
        },
    )
    .unwrap();
    let gpu = context();
    let reads: [[Vec<u32>; 2]; 2] = std::array::from_fn(|layer| {
        let base = native(gpu.read_bptc_upload(&upload, layer, 0));
        let expected = reference(format, &layers[layer][..4], 2, (WIDTH, HEIGHT));
        assert_texels(
            &format!("BC7 layer {layer} mip 0"),
            format,
            &base,
            &expected,
        );
        let small = native(gpu.read_bptc_upload(&upload, layer, 1));
        let expected = reference(format, &layers[layer][4..], 1, (2, 2));
        assert_texels(
            &format!("BC7 layer {layer} mip 1"),
            format,
            &small,
            &expected,
        );
        [base, small]
    });
    for (layer, mip) in [(2, 0), (0, 2)] {
        assert_eq!(
            gpu.read_bptc_upload(&upload, layer, mip)
                .unwrap_err()
                .operation,
            "texture subresource"
        );
    }

    // Hand-derived anchors, independent of the CPU decoder. Endpoints append
    // their P-bit and replicate their top bits to eight; interpolation is
    // ((64 - w) * e0 + w * e1 + 32) >> 6 with the weights of Table 119.
    // Words hold red in the lowest byte.
    let texel = |layer: usize, x: usize, y: usize| reads[layer][0][y * WIDTH + x];
    // Mode 0 at block (0, 0) of layer 0. Subset 0 endpoint 0 is 15 with
    // P-bit 1, 31 -> 255, and 0 with P-bit 1, 1 -> 8.
    assert_eq!(texel(0, 0, 0), 0xff08_08ff);
    // Texel 1, index 7: subset 0 endpoint 1, (0, 30, 0) -> (0, 247, 0).
    assert_eq!(texel(0, 1, 0), 0xff00_f700);
    // Texel 2, index 7: subset 1 endpoint 1, 30 -> 247 in every channel.
    assert_eq!(texel(0, 2, 0), 0xfff7_f7f7);
    // Texel 3 is subset 1's anchor, index 0: (1, 1, 31) -> (8, 8, 255).
    assert_eq!(texel(0, 3, 0), 0xffff_0808);
    // Texel 9, index 7: subset 2 endpoint 1, (3, 7, 11) -> (24, 57, 90).
    assert_eq!(texel(0, 1, 2), 0xff5a_3918);
    // Texel 15 is subset 2's anchor, index 0: (16, 8, 4) -> (132, 66, 33).
    assert_eq!(texel(0, 3, 3), 0xff21_4284);
    // Mode 1 at block (1, 0): only column 0, subset 0, is inside the level.
    // Texel 0 is endpoint 0 with shared P-bit 1: (127, 1, 1) -> (255, 2, 2).
    assert_eq!(texel(0, 4, 0), 0xff02_02ff);
    // Texel 8, index 7: endpoint 1, (1, 127, 1) -> (2, 255, 2).
    assert_eq!(texel(0, 4, 2), 0xff02_ff02);
    // Mode 2 at block (0, 1): only row 0 is inside. Texel 1, index 3: subset
    // 0 endpoint 1, blue 31 -> 255.
    assert_eq!(texel(0, 0, 4), 0xff00_00ff);
    assert_eq!(texel(0, 1, 4), 0xffff_0000);
    // Texel 2, subset 1, index 1 (w = 21) between 132 grey and green 255:
    // red and blue (43 * 132 + 32) >> 6 = 89, green (43 * 132 + 21 * 255
    // + 32) >> 6 = 172.
    assert_eq!(texel(0, 2, 4), 0xff59_ac59);
    // Texel 3 is subset 1's anchor, index 0: 16 -> 132 grey.
    assert_eq!(texel(0, 3, 4), 0xff84_8484);
    // Mode 3 at block (1, 1), texel 0: 7-bit endpoint plus P-bit 1 is the
    // byte itself, (0x7f, 0x00, 0x55) -> (0xff, 0x01, 0xab).
    assert_eq!(texel(0, 4, 4), 0xffab_01ff);
    // Mode 4 at layer 0 mip 1, rotation 0, selection 0. Texel 0: colour
    // index 0 is (31, 0, 16) -> (255, 0, 132); alpha index 3 (w = 27) is
    // (37 * 255 + 32) >> 6 = 147.
    assert_eq!(reads[0][1][0], 0x9384_00ff);
    // Texel 1: colour index 1 (w = 21) is (171, 84, 110), alpha index 7 is
    // endpoint 1, 0.
    assert_eq!(reads[0][1][1], 0x006e_54ab);
    // Mode 5 at layer 1 block (0, 0), rotation 2 swaps alpha and green.
    // Texel 0: colour (255, 0, 129); alpha index 1 (w = 21) is (43 * 16 +
    // 21 * 240 + 32) >> 6 = 90. After the swap: (255, 90, 129, 0).
    assert_eq!(texel(1, 0, 0), 0x0081_5aff);
    // Texel 1: colour endpoint 1 (0, 255, 64), alpha endpoint 1 240.
    assert_eq!(texel(1, 1, 0), 0xff40_f000);
    // Mode 6 at block (1, 0), texel 0: (0x7f, 0, 0x40, 0x7f) with P-bit 1
    // -> (255, 1, 129, 255).
    assert_eq!(texel(1, 4, 0), 0xff81_01ff);
    // Texel 8, index 8 (w = 34): (30 * 255 + 32) >> 6 = 120 for red and
    // alpha, (30 + 34 * 254 + 32) >> 6 = 135 green, (30 * 129 + 34 * 64 +
    // 32) >> 6 = 94 blue.
    assert_eq!(texel(1, 4, 2), 0x785e_8778);
    // Mode 7 at block (0, 1), row 0. 5-bit endpoints with P-bits replicate
    // from six bits: (63, 1, 1, 63) -> (255, 4, 4, 255) at texel 0, subset 0
    // endpoint 1 (0, 62, 0, 0) -> (0, 251, 0, 0) at texel 1, subset 1
    // endpoint 1 (21, 41, 11, 63) -> (85, 166, 44, 255) at texel 2 and its
    // endpoint 0 (0, 0, 62, 32) -> (0, 0, 251, 130) at texel 3.
    assert_eq!(
        [
            texel(1, 0, 4),
            texel(1, 1, 4),
            texel(1, 2, 4),
            texel(1, 3, 4)
        ],
        [0xff04_04ff, 0x0000_fb00, 0xff2c_a655, 0x82fb_0000]
    );
    // The reserved block at (1, 1) decodes to zero in every channel in both
    // public sources. Colour is zero either way; see Divergence for alpha.
    let reserved = texel(1, 4, 4);
    assert!(
        reserved == 0 || reserved == 0xff00_0000,
        "reserved block {reserved:#010x}"
    );
    println!("observed reserved BC7 block on this device: {reserved:#010x}");
    // Mode 4 at layer 1 mip 1, rotation 1 and selection 1. Texel 0: colour
    // uses the 3-bit index 2 (w = 18), (183, 72, 113); alpha the 2-bit index
    // 1 (w = 21), 171. Rotation 1 swaps alpha and red: (171, 72, 113, 183).
    assert_eq!(reads[1][1][0], 0xb771_48ab);
    println!(
        "checked 58 BC7 texels of all eight modes and a reserved block, exact apart from the reported divergences"
    );
}

#[test]
fn reads_random_bc7_blocks_of_every_mode_exactly() {
    let mut random = SplitMix(0x6237_bc7b_c7bc_7001);
    // 16 by 4 blocks: eight blocks of each mode, with block 63 reserved.
    let mut blocks: Vec<[u8; 16]> = (0..64u8)
        .map(|index| random_bc7(&mut random, index % 8))
        .collect();
    blocks[63] = BC7_RESERVED;
    read_level(BptcFormat::Bc7Unorm, &blocks, (64, 16, 16 * 16));
    println!(
        "checked 1024 BC7 texels of 63 random blocks and a reserved block, exact apart from the reported divergences"
    );
}

/// Mode 3 (Microsoft's mode 11): ten-bit endpoints stored whole, red, green
/// and blue of endpoint 0, then those of endpoint 1, then 63 index bits.
fn bc6h_mode3(e0: [u32; 3], e1: [u32; 3], indices: [u32; 16]) -> [u8; 16] {
    let mut bits = Bits::new();
    bits.put(0b00011, 5);
    for channel in e0.into_iter().chain(e1) {
        bits.put(channel & 0x3ff, 10);
    }
    for (texel, index) in indices.into_iter().enumerate() {
        bits.put(index, 4 - u32::from(texel == 0));
    }
    bits.finish()
}

/// Mode 15 (Microsoft's mode 14): a 16-bit endpoint 0 and 4-bit deltas.
/// The high six bits of each base are stored in reverse order; this test
/// uses only bases whose high six bits read the same either way.
fn bc6h_mode15(base: [u32; 3], delta: [u32; 3], indices: [u32; 16]) -> [u8; 16] {
    let mut bits = Bits::new();
    bits.put(0b01111, 5);
    for channel in base {
        bits.put(channel & 0x3ff, 10);
    }
    for (channel, delta) in base.into_iter().zip(delta) {
        let high = channel >> 10;
        assert_eq!(high.reverse_bits() >> 26, high, "palindromic high bits");
        bits.put(delta, 4);
        bits.put(high, 6);
    }
    for (texel, index) in indices.into_iter().enumerate() {
        bits.put(index, 4 - u32::from(texel == 0));
    }
    bits.finish()
}

/// Index 0 in the left half of each row, 15 in the right half, with 8 at
/// texel 9 and the anchor kept within its three bits.
const HALVES: [u32; 16] = [0, 0, 15, 15, 0, 0, 15, 15, 0, 8, 15, 15, 0, 0, 15, 15];

#[test]
fn reads_bc6h_hdr_negative_and_signed_zero_anchors() {
    // Unsigned 10-bit endpoints: x = 1023 unquantizes to 0xffff and finishes
    // as (0xffff * 31) >> 6 = 0x7bff = 65504; 512 -> 32800 -> 0x3e0f; 700 ->
    // 44832 -> 0x54d3 = 77.1875; 1 -> 96 -> 46 = 0x002e, a subnormal.
    let unsigned = bc6h_mode3([1023, 512, 0], [0, 700, 1], HALVES);
    let words = read_level(BptcFormat::Bc6hUfloat, &[unsigned], (4, 4, 16));
    assert_eq!(
        words[..4],
        [half_to_single(0x7bff), half_to_single(0x3e0f), 0, ONE]
    );
    assert_eq!(f32::from_bits(words[0]), 65504.0);
    assert_eq!(f32::from_bits(words[8 + 1]), 77.1875);
    // 46 * 2^-24 = 1.4375 * 2^-19 is the single 0x3638_0000.
    assert_eq!(words[8..12], [0, half_to_single(0x54d3), 0x3638_0000, ONE]);
    // Texel 9, index 8 (w = 34) interpolates before finishing: red
    // (30 * 0xffff + 32) >> 6 = 30720 -> (30720 * 31) >> 6 = 0x3a20.
    assert_eq!(words[9 * 4], half_to_single(0x3a20));

    // Signed 10-bit endpoints are sign-extended: 511 and -512 reach the
    // magnitude limit 0x7fff -> 0x7bff and 0xfbff; -1 -> -96 -> 0x805d; 256
    // -> 16416 -> 0x3e1f; -300 -> -19232 -> 0xc8c7.
    let signed = bc6h_mode3(
        [511, 0x3ff, 0x200],
        [0, 256, 0x3ff & -300i32 as u32],
        HALVES,
    );
    let words = read_level(BptcFormat::Bc6hSfloat, &[signed], (4, 4, 16));
    assert_eq!(
        words[..4],
        [
            half_to_single(0x7bff),
            half_to_single(0x805d),
            half_to_single(0xfbff),
            ONE
        ]
    );
    assert_eq!(
        words[8..12],
        [0, half_to_single(0x3e1f), half_to_single(0xc8c7), ONE]
    );

    // A 16-bit signed endpoint of -1 interpolates to -1 at index 0. The
    // Khronos text keeps its sign, so it finishes as 0x8000 (-0.0);
    // Microsoft's sample code returns 0x0000. Endpoint 1 is -1 + 2 = 1,
    // which finishes as (31 * 1) >> 5 = 0 at index 15.
    let zero = bc6h_mode15([0xffff, 0, 0], [2, 0, 0], HALVES);
    let words = read_level(BptcFormat::Bc6hSfloat, &[zero], (4, 4, 16));
    assert!(
        words[0] == 0x8000_0000 || words[0] == 0,
        "-1 finished as {:#010x}",
        words[0]
    );
    println!(
        "observed BC6H SF16 -1 at index 0 on this device: {:#010x} ({})",
        words[0],
        if words[0] == 0 {
            "+0.0, Microsoft's reading"
        } else {
            "-0.0, the Khronos reading"
        }
    );
    assert_eq!(words[8], 0);
    println!(
        "checked 48 BC6H texels with hand-derived anchors, exact apart from the reported divergences"
    );
}

#[test]
fn reads_random_bc6h_single_subset_and_reserved_blocks_exactly() {
    for (format, seed) in [
        (BptcFormat::Bc6hUfloat, 0x6236_bc6b_c6bc_6001),
        (BptcFormat::Bc6hSfloat, 0x6236_bc6b_c6bc_6002),
    ] {
        let mut random = SplitMix(seed);
        // 8 by 4 blocks: seven of each single-subset mode and one of each
        // reserved mode.
        let modes = [3, 7, 11, 15];
        let mut blocks: Vec<[u8; 16]> = (0..28)
            .map(|index| random_bc6h(&mut random, modes[index % 4]))
            .collect();
        for mode in [19, 23, 27, 31] {
            blocks.push(random_bc6h(&mut random, mode));
        }
        read_level(format, &blocks, (32, 16, 8 * 16));
    }
    println!(
        "checked 1024 BC6H texels of 56 random and 8 reserved blocks, exact apart from the reported divergences"
    );
}

#[test]
fn reads_bptc_edge_blocks_and_rejects_oversized_levels() {
    // 5x3 is one row of two blocks: Metal receives a zero row stride, and
    // both blocks are cut at row 3, the second also at column 1. The row
    // pitch keeps one padding block.
    let wide = (5, 3, 48);
    let bc7 = read_level(
        BptcFormat::Bc7Unorm,
        &[bc7_mode6(), bc7_mode3(), pad()],
        wide,
    );
    // (0, 0) is mode 6's endpoint 0 and (4, 0) mode 3's.
    assert_eq!([bc7[0], bc7[4]], [0xff81_01ff, 0xffab_01ff]);
    let bc6h = [
        bc6h_mode3([1023, 512, 0], [0, 700, 1], HALVES),
        bc6h_mode15([0xffff, 0, 0], [2, 0, 0], HALVES),
        pad(),
    ];
    read_level(BptcFormat::Bc6hUfloat, &bc6h, wide);
    read_level(BptcFormat::Bc6hSfloat, &bc6h, wide);
    // 1x1 is a single block cut to its first texel.
    assert_eq!(
        read_level(BptcFormat::Bc7Unorm, &[bc7_mode0()], (1, 1, 16)),
        [0xff08_08ff]
    );
    assert_eq!(
        read_level(
            BptcFormat::Bc6hUfloat,
            &[bc6h_mode3([1023, 512, 0], [0, 700, 1], HALVES)],
            (1, 1, 16)
        ),
        [half_to_single(0x7bff), half_to_single(0x3e0f), 0, ONE]
    );

    // A BC6H level returns four words per texel, so it holds at most 1024
    // texels; BC7 keeps the 4096-texel limit.
    let gpu = context();
    for (format, width, height, limit) in [
        (BptcFormat::Bc6hUfloat, 36, 32, 1024),
        (BptcFormat::Bc6hSfloat, 36, 32, 1024),
        (BptcFormat::Bc7Unorm, 68, 64, 4096),
    ] {
        let row_pitch = width / 4 * 16;
        let bytes = vec![0; row_pitch * height / 4];
        let upload = BptcUpload::new(
            &bytes,
            BptcUploadDesc {
                width,
                height,
                mips: 1,
                layers: 1,
                format,
                row_pitch,
                slice_pitch: bytes.len(),
            },
        )
        .unwrap();
        let error = gpu.read_bptc_upload(&upload, 0, 0).unwrap_err();
        assert_eq!(error.operation, "texture bounds", "{format:?}");
        assert_eq!(error.detail, format!("expected 1..={limit} texels"));
    }
    println!("checked BC6H and BC7 5x3 and 1x1 edge levels and the per-format texel limits");
}
