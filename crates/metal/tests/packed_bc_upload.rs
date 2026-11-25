// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg(all(feature = "gpu-tests", target_arch = "aarch64", target_os = "macos"))]

use xenora_metal::{MetalError, NativeCompute};
use xenora_texture::{
    BcAlphaFormat, BcChannelFormat, BcCopyAlignment, BcFormat, BcTextureDesc, BcTextureLayout,
    PackedBcUpload,
};

const EXTENTS: [(usize, usize); 4] = [(9, 5), (4, 2), (2, 1), (1, 1)];
const BLOCK_COUNTS: [usize; 4] = [6, 1, 1, 1];

// Zero selector bits pick the first endpoint. These RGB565 endpoints encode
// exact red, green, blue and white.
const COLORS: [[u8; 2]; 4] = [[0x00, 0xf8], [0xe0, 0x07], [0x1f, 0x00], [0xff, 0xff]];
const PIXELS: [[u8; 4]; 4] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 255, 255],
];

fn append_block(bytes: &mut Vec<u8>, format: BcFormat, variant: usize) {
    let color = COLORS[variant];
    let color_block = [color[0], color[1], 0, 0, 0, 0, 0, 0];
    let endpoint_block = |first| [first, 0, 0, 0, 0, 0, 0, 0];
    match format {
        BcFormat::Bc1 => bytes.extend(color_block),
        BcFormat::BcAlpha(format) => {
            bytes.extend(match format {
                BcAlphaFormat::Bc2 => [0xff; 8],
                BcAlphaFormat::Bc3 => endpoint_block(255),
            });
            bytes.extend(color_block);
        }
        BcFormat::BcChannel(format) => match format {
            BcChannelFormat::Bc4Unorm => bytes.extend(endpoint_block(PIXELS[variant][0])),
            BcChannelFormat::Bc5Unorm => {
                bytes.extend(endpoint_block(PIXELS[variant][0]));
                bytes.extend(endpoint_block(PIXELS[variant][1]));
            }
            BcChannelFormat::Bc4Snorm => bytes.extend(endpoint_block([129, 127, 0, 64][variant])),
            BcChannelFormat::Bc5Snorm => {
                bytes.extend(endpoint_block([129, 127, 0, 64][variant]));
                bytes.extend(endpoint_block([127, 129, 64, 0][variant]));
            }
        },
        _ => unreachable!("native readback fixture covers BC1–BC5"),
    }
}

fn expected_words(format: BcFormat, layer: usize, mip: usize) -> Vec<u32> {
    let (width, height) = EXTENTS[mip];
    let mut result = Vec::new();
    for y in 0..height {
        for x in 0..width {
            // The base image has a 3×2 physical block grid. Small levels
            // have a single block even when either texel extent is below 4.
            let variant = ((y / 4) * 3 + x / 4 + layer * 2 + mip) % 4;
            let rgba = match format {
                BcFormat::Bc1 | BcFormat::BcAlpha(_) => PIXELS[variant],
                BcFormat::BcChannel(BcChannelFormat::Bc4Unorm) => [PIXELS[variant][0], 0, 0, 255],
                BcFormat::BcChannel(BcChannelFormat::Bc5Unorm) => {
                    [PIXELS[variant][0], PIXELS[variant][1], 0, 255]
                }
                BcFormat::BcChannel(BcChannelFormat::Bc4Snorm) => {
                    [[129, 127, 0, 64][variant], 0, 0, 127]
                }
                BcFormat::BcChannel(BcChannelFormat::Bc5Snorm) => [
                    [129, 127, 0, 64][variant],
                    [127, 129, 64, 0][variant],
                    0,
                    127,
                ],
                _ => unreachable!("native readback fixture covers BC1–BC5"),
            };
            result.push(u32::from_le_bytes(rgba));
        }
    }
    result
}

fn read(
    gpu: &NativeCompute,
    upload: &PackedBcUpload<'_>,
    layer: usize,
    mip: u32,
) -> Result<Vec<u32>, MetalError> {
    match upload.format() {
        BcFormat::Bc1 => gpu.read_bc1_upload(&upload.as_bc1(layer, mip).unwrap(), 0, 0),
        BcFormat::BcAlpha(_) => {
            gpu.read_bc_alpha_upload(&upload.as_bc_alpha(layer, mip).unwrap(), 0, 0)
        }
        BcFormat::BcChannel(_) => {
            gpu.read_bc_channel_upload(&upload.as_bc_channel(layer, mip).unwrap(), 0, 0)
        }
        _ => unreachable!("native readback fixture covers BC1–BC5"),
    }
}

#[test]
fn packed_and_aligned_bc_uploads_return_known_pixels_on_metal() {
    let gpu = NativeCompute::new().expect("native Metal device and queue");
    for format in [
        BcFormat::Bc1,
        BcFormat::BcAlpha(BcAlphaFormat::Bc2),
        BcFormat::BcAlpha(BcAlphaFormat::Bc3),
        BcFormat::BcChannel(BcChannelFormat::Bc4Unorm),
        BcFormat::BcChannel(BcChannelFormat::Bc4Snorm),
        BcFormat::BcChannel(BcChannelFormat::Bc5Unorm),
        BcFormat::BcChannel(BcChannelFormat::Bc5Snorm),
    ] {
        let desc = BcTextureDesc {
            width: 9,
            height: 5,
            mips: 4,
            layers: 2,
            format,
        };
        let tight = BcTextureLayout::new(desc, BcCopyAlignment::TIGHT).unwrap();
        let aligned = BcTextureLayout::new(
            desc,
            BcCopyAlignment {
                row: 256,
                placement: 512,
            },
        )
        .unwrap();
        let mut source = Vec::new();
        for layer in 0..2 {
            for (mip, block_count) in BLOCK_COUNTS.into_iter().enumerate() {
                for n in 0..block_count {
                    append_block(&mut source, format, (n + layer * 2 + mip) % 4);
                }
            }
        }
        let tight_upload = PackedBcUpload::new(&tight, &source).unwrap();
        let mut staged = vec![0xff; aligned.byte_len()];
        tight_upload.repack_into(&aligned, &mut staged).unwrap();
        let aligned_upload = PackedBcUpload::new(&aligned, &staged).unwrap();

        for (name, upload) in [("tight", &tight_upload), ("aligned", &aligned_upload)] {
            for layer in 0..2 {
                for mip in 0..EXTENTS.len() {
                    let actual = read(&gpu, upload, layer, mip as u32).unwrap_or_else(|error| {
                        if error.is_capability_unavailable() {
                            panic!("CAPABILITY UNAVAILABLE: {error}");
                        }
                        panic!("IMPLEMENTATION FAILURE: {format:?} {name} {layer}/{mip}: {error}");
                    });
                    assert_eq!(
                        actual,
                        expected_words(format, layer, mip),
                        "{format:?} {name} layer {layer}, mip {mip}"
                    );
                }
            }
        }
    }
}
