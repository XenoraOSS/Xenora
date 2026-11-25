// SPDX-License-Identifier: GPL-3.0-or-later

use xenora_texture::{
    Bc1Upload, Bc1UploadDesc, BcAlphaFormat, BcChannelFormat, BcCopyAlignment, BcFootprint,
    BcFormat, BcTextureDesc, BcTextureLayout, PackedBcUpload, TextureError,
};

const DESC: BcTextureDesc = BcTextureDesc {
    width: 13,
    height: 9,
    mips: 4,
    layers: 2,
    format: BcFormat::Bc1,
};

const ALIGNED: BcCopyAlignment = BcCopyAlignment {
    row: 32,
    placement: 128,
};

#[test]
fn hand_calculated_footprints_cover_partial_blocks_and_layer_padding() {
    let tight = BcTextureLayout::new(DESC, BcCopyAlignment::TIGHT).unwrap();
    let aligned = BcTextureLayout::new(DESC, ALIGNED).unwrap();
    // 13×9 has four blocks across and three down. 6×4 takes two blocks.
    // Both 3×2 and 1×1 take one block each.
    let levels = [(13, 9, 32, 3), (6, 4, 16, 1), (3, 2, 8, 1), (1, 1, 8, 1)];
    for (layout, offsets, stride, pitches) in [
        (&tight, [0, 96, 112, 120], 128, [32, 16, 8, 8]),
        (&aligned, [0, 128, 256, 384], 512, [32; 4]),
    ] {
        for layer in 0..2 {
            for (mip, (width, height, row_bytes, block_rows)) in levels.into_iter().enumerate() {
                let row_pitch = pitches[mip];
                assert_eq!(
                    layout.footprint(layer, mip as u32),
                    Ok(BcFootprint {
                        width,
                        height,
                        offset: stride * layer + offsets[mip],
                        row_bytes,
                        row_pitch,
                        block_rows,
                        byte_len: row_pitch * block_rows,
                    })
                );
            }
        }
    }
    assert_eq!(tight.byte_len(), 256);
    assert_eq!(aligned.byte_len(), 928);
    assert_eq!(aligned.footprint(2, 0), Err(TextureError::OutOfBounds));
    assert_eq!(aligned.footprint(0, 4), Err(TextureError::OutOfBounds));
}

#[test]
fn compact_chain_uses_block_rows_instead_of_reserving_base_size_for_every_mip() {
    let desc = BcTextureDesc {
        width: 1024,
        height: 1024,
        mips: 11,
        layers: 1,
        ..DESC
    };
    let layout = BcTextureLayout::new(desc, BcCopyAlignment::TIGHT).unwrap();
    assert_eq!(layout.byte_len(), 699_064);
    assert_eq!(layout.footprint(0, 10).unwrap().offset, 699_056);
    let compact = vec![0; layout.byte_len()];
    PackedBcUpload::new(&layout, &compact).unwrap();
    let fixed_slice = Bc1UploadDesc {
        width: 1024,
        height: 1024,
        mips: 11,
        layers: 1,
        format: 1,
        usage: 1,
        row_pitch: 2048,
        slice_pitch: 524_288,
    };
    assert_eq!(
        Bc1Upload::new(&compact, fixed_slice).unwrap_err(),
        TextureError::InvalidByteLength
    );
    let fixed = vec![0; 5_767_168];
    let fixed_upload = Bc1Upload::new(&fixed, fixed_slice).unwrap();
    assert_eq!(fixed_upload.subresource(0, 10).unwrap().bytes.len(), 2048);
    let aligned = BcTextureLayout::new(
        desc,
        BcCopyAlignment {
            row: 256,
            placement: 512,
        },
    )
    .unwrap();
    assert_eq!(aligned.byte_len(), 705_280);
    assert_eq!(aligned.footprint(0, 10).unwrap().offset, 705_024);
}

#[test]
fn selected_write_clears_its_row_padding_and_preserves_neighbors() {
    let layout = BcTextureLayout::new(BcTextureDesc { width: 9, ..DESC }, ALIGNED).unwrap();
    let src: Vec<u8> = (1..=72).collect(); // Three 24-byte rows.
    let mut dst = vec![0xcc; layout.byte_len()];
    layout.write_tight(&mut dst, 1, 0, &src).unwrap();
    let mut expected = vec![0xcc; dst.len()];
    expected[512..608].fill(0);
    expected[512..536].copy_from_slice(&src[..24]);
    expected[544..568].copy_from_slice(&src[24..48]);
    expected[576..600].copy_from_slice(&src[48..]);
    assert_eq!(dst, expected);
    let view = PackedBcUpload::new(&layout, &dst).unwrap();
    let level = view.subresource(1, 0).unwrap();
    assert_eq!(level.bytes, &dst[512..608]);
    assert!(std::ptr::eq(level.bytes.as_ptr(), dst[512..].as_ptr()));
    assert_eq!((level.width, level.height, level.row_pitch), (9, 9, 32));
    let legacy = view.as_bc1(1, 0).unwrap().subresource(0, 0).unwrap();
    assert!(std::ptr::eq(legacy.bytes.as_ptr(), level.bytes.as_ptr()));
    assert_eq!(legacy.bytes.len(), 96);
}

#[test]
fn repack_ignores_dirty_source_padding_and_zeroes_both_kinds_of_target_padding() {
    let tight = BcTextureLayout::new(DESC, BcCopyAlignment::TIGHT).unwrap();
    let aligned = BcTextureLayout::new(DESC, ALIGNED).unwrap();
    let tight_bytes: Vec<u8> = (0..=255).collect();
    // Independent byte map from the paper footprints above.
    let spans = [
        (0, 0, 96),
        (96, 128, 16),
        (112, 256, 8),
        (120, 384, 8),
        (128, 512, 96),
        (224, 640, 16),
        (240, 768, 8),
        (248, 896, 8),
    ];
    let mut dirty_aligned = vec![0xab; 928];
    let mut clean_aligned = vec![0; 928];
    for (src, dst, len) in spans {
        dirty_aligned[dst..dst + len].copy_from_slice(&tight_bytes[src..src + len]);
        clean_aligned[dst..dst + len].copy_from_slice(&tight_bytes[src..src + len]);
    }

    let input = PackedBcUpload::new(&aligned, &dirty_aligned).unwrap();
    let mut compact = vec![0xcc; tight.byte_len()];
    input.repack_into(&tight, &mut compact).unwrap();
    assert_eq!(compact, tight_bytes);

    let mut output = vec![0xcc; aligned.byte_len()];
    input.repack_into(&aligned, &mut output).unwrap();
    assert_eq!(output, clean_aligned);
    output.fill(0xcc);
    PackedBcUpload::new(&tight, &compact)
        .unwrap()
        .repack_into(&aligned, &mut output)
        .unwrap();
    assert_eq!(output, clean_aligned);
}

#[test]
fn validates_sizes_before_writing_and_requires_the_same_encoding() {
    let layout = BcTextureLayout::new(DESC, ALIGNED).unwrap();
    let mut bytes = vec![0xab; layout.byte_len()];
    assert_eq!(
        layout.write_tight(&mut bytes, 0, 3, &[0; 7]),
        Err(TextureError::InvalidByteLength)
    );
    assert_eq!(
        layout.write_tight(&mut bytes, 0, 3, &[0; 9]),
        Err(TextureError::InvalidByteLength)
    );
    assert_eq!(
        layout.write_tight(&mut bytes[..927], 0, 3, &[0; 8]),
        Err(TextureError::InvalidByteLength)
    );
    assert_eq!(
        layout.write_tight(&mut bytes, 2, 3, &[0; 8]),
        Err(TextureError::OutOfBounds)
    );
    assert!(bytes.iter().all(|byte| *byte == 0xab));

    for len in [0, 927, 929] {
        assert_eq!(
            PackedBcUpload::new(&layout, &vec![0; len]).unwrap_err(),
            TextureError::InvalidByteLength
        );
    }
    let upload = PackedBcUpload::new(&layout, &bytes).unwrap();
    for incompatible in [
        BcTextureDesc {
            format: BcFormat::BcChannel(BcChannelFormat::Bc4Unorm),
            ..DESC
        },
        BcTextureDesc { width: 14, ..DESC },
        BcTextureDesc { height: 10, ..DESC },
        BcTextureDesc { mips: 3, ..DESC },
        BcTextureDesc { layers: 1, ..DESC },
    ] {
        let target = BcTextureLayout::new(incompatible, ALIGNED).unwrap();
        let mut dst = vec![0xcc; target.byte_len()];
        assert_eq!(
            upload.repack_into(&target, &mut dst),
            Err(TextureError::InvalidLayout)
        );
        assert!(dst.iter().all(|byte| *byte == 0xcc));
    }
    let mut short = vec![0xcc; 927];
    assert_eq!(
        upload.repack_into(&layout, &mut short),
        Err(TextureError::InvalidByteLength)
    );
    assert!(short.iter().all(|byte| *byte == 0xcc));
    assert_eq!(
        upload.as_bc_alpha(0, 0).unwrap_err(),
        TextureError::InvalidLayout
    );
    assert_eq!(
        upload.as_bc_channel(0, 0).unwrap_err(),
        TextureError::InvalidLayout
    );
    assert_eq!(upload.as_bc1(0, 4).unwrap_err(), TextureError::OutOfBounds);
}

#[test]
fn rejects_invalid_descriptors_and_overflowing_footprints() {
    for desc in [
        BcTextureDesc { width: 0, ..DESC },
        BcTextureDesc { height: 0, ..DESC },
        BcTextureDesc { mips: 0, ..DESC },
        BcTextureDesc { mips: 5, ..DESC },
        BcTextureDesc { layers: 0, ..DESC },
    ] {
        assert_eq!(
            BcTextureLayout::new(desc, ALIGNED).unwrap_err(),
            TextureError::InvalidLayout
        );
    }
    for (row, placement) in [(0, 1), (1, 0), (3, 1), (1, 3)] {
        assert_eq!(
            BcTextureLayout::new(DESC, BcCopyAlignment { row, placement }).unwrap_err(),
            TextureError::InvalidLayout
        );
    }
    for desc in [
        BcTextureDesc {
            width: usize::MAX,
            ..DESC
        },
        BcTextureDesc {
            height: usize::MAX,
            ..DESC
        },
        BcTextureDesc {
            layers: usize::MAX,
            ..DESC
        },
    ] {
        assert_eq!(
            BcTextureLayout::new(desc, ALIGNED).unwrap_err(),
            TextureError::SizeOverflow
        );
    }
    let highest_bit = 1 << (usize::BITS - 1);
    for alignment in [
        BcCopyAlignment {
            row: highest_bit,
            placement: 1,
        },
        BcCopyAlignment {
            row: 1,
            placement: highest_bit,
        },
    ] {
        assert_eq!(
            BcTextureLayout::new(DESC, alignment).unwrap_err(),
            TextureError::SizeOverflow
        );
    }
    // Even an unusually large placement alignment needs no trailing space
    // after a sole subresource.
    let one = BcTextureDesc {
        mips: 1,
        layers: 1,
        ..DESC
    };
    let layout = BcTextureLayout::new(
        one,
        BcCopyAlignment {
            row: 1,
            placement: highest_bit,
        },
    )
    .unwrap();
    assert_eq!(layout.byte_len(), 96);
}

#[test]
fn all_block_sizes_match_a_byte_by_byte_layout_oracle() {
    let formats = [
        (BcFormat::Bc1, 8),
        (BcFormat::BcAlpha(BcAlphaFormat::Bc2), 16),
        (BcFormat::BcAlpha(BcAlphaFormat::Bc3), 16),
        (BcFormat::BcChannel(BcChannelFormat::Bc4Unorm), 8),
        (BcFormat::BcChannel(BcChannelFormat::Bc4Snorm), 8),
        (BcFormat::BcChannel(BcChannelFormat::Bc5Unorm), 16),
        (BcFormat::BcChannel(BcChannelFormat::Bc5Snorm), 16),
        (BcFormat::Bc6hUfloat, 16),
        (BcFormat::Bc6hSfloat, 16),
        (BcFormat::Bc7, 16),
    ];
    for (format, block_bytes) in formats {
        for (width, height) in [
            (1_usize, 1_usize),
            (1, 37),
            (37, 1),
            (4, 4),
            (5, 9),
            (17, 13),
        ] {
            for (row, placement) in [(1, 1), (32, 16), (16, 128), (64, 512)] {
                let mips = usize::BITS - width.max(height).leading_zeros();
                let desc = BcTextureDesc {
                    width,
                    height,
                    mips,
                    layers: 3,
                    format,
                };
                let layout =
                    BcTextureLayout::new(desc, BcCopyAlignment { row, placement }).unwrap();
                let tight = BcTextureLayout::new(desc, BcCopyAlignment::TIGHT).unwrap();
                let mut expected = Vec::new();
                let mut source = Vec::new();
                for layer in 0..3 {
                    let (mut w, mut h) = (width, height);
                    for mip in 0..mips {
                        while expected.len() % placement != 0 {
                            expected.push(0);
                        }
                        let start = expected.len();
                        let mut tight_level = Vec::new();
                        for y in (0..h).step_by(4) {
                            let row_start = expected.len();
                            for x in (0..w).step_by(4) {
                                for i in 0..block_bytes {
                                    let byte =
                                        (1 + layer * 51 + mip as usize * 7 + x + 3 * y + i) as u8;
                                    expected.push(byte);
                                    tight_level.push(byte);
                                }
                            }
                            while (expected.len() - row_start) % row != 0 {
                                expected.push(0);
                            }
                        }
                        let fp = layout.footprint(layer, mip).unwrap();
                        assert_eq!(
                            (fp.offset, fp.offset + fp.byte_len),
                            (start, expected.len())
                        );
                        source.extend(tight_level);
                        (w, h) = ((w / 2).max(1), (h / 2).max(1));
                    }
                }
                let upload = PackedBcUpload::new(&tight, &source).unwrap();
                let mut bytes = vec![0xff; layout.byte_len()];
                upload.repack_into(&layout, &mut bytes).unwrap();
                assert_eq!(
                    bytes, expected,
                    "{format:?}, {width}×{height}, {row}/{placement}"
                );
            }
        }
    }
}
