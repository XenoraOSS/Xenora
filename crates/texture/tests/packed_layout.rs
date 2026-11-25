// SPDX-License-Identifier: GPL-3.0-or-later
use xenora_texture::{PackedTextureLayout, TextureError, TextureFormat::*};

#[test]
fn odd_nonsquare_bc_chain_has_literal_offsets_and_complete_edge_blocks() {
    let layout = PackedTextureLayout::new(Bc1RgbaUnorm, 13, 9, 4).unwrap();
    assert_eq!(layout.format(), Bc1RgbaUnorm);
    let actual: Vec<_> = layout
        .levels()
        .iter()
        .map(|m| (m.width, m.height, m.offset, m.row_bytes, m.rows, m.byte_len))
        .collect();
    assert_eq!(
        actual,
        vec![
            (13, 9, 0, 32, 3, 96),
            (6, 4, 96, 16, 1, 16),
            (3, 2, 112, 8, 1, 8),
            (1, 1, 120, 8, 1, 8)
        ]
    );
    assert_eq!(layout.byte_len(), 128);
    let bc3 = PackedTextureLayout::new(Bc3RgbaUnorm, 13, 9, 4).unwrap();
    assert_eq!(bc3.format(), Bc3RgbaUnorm);
    assert_eq!(
        bc3.levels()
            .iter()
            .map(|m| (m.offset, m.row_bytes, m.byte_len))
            .collect::<Vec<_>>(),
        vec![(0, 64, 192), (192, 32, 32), (224, 16, 16), (240, 16, 16)]
    );
    assert_eq!(bc3.byte_len(), 256);
}

#[test]
fn small_compressed_levels_keep_full_blocks() {
    for (format, expected) in [(Bc1RgbaUnorm, [8, 8, 32]), (Bc3RgbaUnorm, [16, 16, 64])] {
        for (side, bytes) in [1, 4, 5].into_iter().zip(expected) {
            let layout = PackedTextureLayout::new(format, side, side, 1).unwrap();
            assert_eq!(layout.byte_len(), bytes);
        }
    }
}

#[test]
fn rgba8_mips_have_no_block_padding() {
    let layout = PackedTextureLayout::new(Rgba8Unorm, 5, 3, 3).unwrap();
    assert_eq!(layout.format(), Rgba8Unorm);
    assert_eq!(
        layout
            .levels()
            .iter()
            .map(|m| (m.width, m.height, m.offset, m.row_bytes, m.rows, m.byte_len))
            .collect::<Vec<_>>(),
        vec![
            (5, 3, 0, 20, 3, 60),
            (2, 1, 60, 8, 1, 8),
            (1, 1, 68, 4, 1, 4)
        ]
    );
    assert_eq!(layout.byte_len(), 72);
}

#[test]
fn invalid_dimensions_and_mips_reject() {
    for format in [Rgba8Unorm, Bc1RgbaUnorm, Bc3RgbaUnorm] {
        for (w, h, mips) in [
            (0, 1, 1),
            (1, 0, 1),
            (1, 1, 0),
            (1, 1, 2),
            (5, 3, 4),
            (1, 1, u32::MAX),
        ] {
            assert_eq!(
                PackedTextureLayout::new(format, w, h, mips),
                Err(TextureError::InvalidLayout)
            );
        }
    }
}

#[test]
fn overflow_in_rows_levels_or_chain_is_rejected() {
    for format in [Rgba8Unorm, Bc1RgbaUnorm, Bc3RgbaUnorm] {
        // Row bytes fit; the multiplication by row count must still reject.
        assert_eq!(
            PackedTextureLayout::new(format, 1, usize::MAX, 1),
            Err(TextureError::SizeOverflow)
        );
        assert_eq!(
            PackedTextureLayout::new(format, usize::MAX, usize::MAX, 1),
            Err(TextureError::SizeOverflow)
        );
    }
    assert_eq!(
        PackedTextureLayout::new(Rgba8Unorm, usize::MAX, 1, 1),
        Err(TextureError::SizeOverflow)
    );
    // Base level fits; adding the second mip must fail checked summation.
    assert_eq!(
        PackedTextureLayout::new(Rgba8Unorm, usize::MAX / 4, 1, 2),
        Err(TextureError::SizeOverflow)
    );
}

#[test]
fn r8_packed_odd_mips_and_checked_overflow() {
    let layout = PackedTextureLayout::new(R8Unorm, 5, 3, 3).unwrap();
    assert_eq!(layout.format(), R8Unorm);
    assert_eq!(
        layout
            .levels()
            .iter()
            .map(|m| (m.width, m.height, m.offset, m.row_bytes, m.rows, m.byte_len))
            .collect::<Vec<_>>(),
        vec![
            (5, 3, 0, 5, 3, 15),
            (2, 1, 15, 2, 1, 2),
            (1, 1, 17, 1, 1, 1)
        ]
    );
    assert_eq!(layout.byte_len(), 18);
    assert_eq!(
        PackedTextureLayout::new(R8Unorm, usize::MAX, 2, 1),
        Err(TextureError::SizeOverflow)
    );
    assert_eq!(
        PackedTextureLayout::new(R8Unorm, usize::MAX, 1, 2),
        Err(TextureError::SizeOverflow)
    );
    for (w, h, m) in [(0, 1, 1), (1, 0, 1), (1, 1, 0), (5, 3, 4)] {
        assert_eq!(
            PackedTextureLayout::new(R8Unorm, w, h, m),
            Err(TextureError::InvalidLayout)
        );
    }
}
