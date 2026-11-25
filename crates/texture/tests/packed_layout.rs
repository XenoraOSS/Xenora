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

#[test]
fn array_rgba8_literal_slice_major_mip_footprints() {
    let layout = xenora_texture::PackedTextureArrayLayout::new(Rgba8Unorm, 5, 3, 3, 2).unwrap();
    assert_eq!(layout.layers(), 2);
    let chain: &PackedTextureLayout = layout.chain();
    assert_eq!(chain.byte_len(), 72);
    assert_eq!(chain.format(), Rgba8Unorm);
    assert_eq!(layout.byte_len(), 144);
    let actual: Vec<_> = (0..2)
        .flat_map(|slice| {
            let layout = &layout;
            (0..3).map(move |mip| {
                let f = layout.footprint(slice, mip).unwrap();
                (f.width, f.height, f.offset, f.row_bytes, f.rows, f.byte_len)
            })
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            (5, 3, 0, 20, 3, 60),
            (2, 1, 60, 8, 1, 8),
            (1, 1, 68, 4, 1, 4),
            (5, 3, 72, 20, 3, 60),
            (2, 1, 132, 8, 1, 8),
            (1, 1, 140, 4, 1, 4),
        ]
    );
}

#[test]
fn array_rejects_zero_layers_and_out_of_range_subresources() {
    assert!(xenora_texture::PackedTextureArrayLayout::new(Rgba8Unorm, 5, 3, 3, 0).is_err());
    let layout = xenora_texture::PackedTextureArrayLayout::new(Rgba8Unorm, 5, 3, 3, 2).unwrap();
    for (slice, mip) in [(2, 0), (0, 3), (1, 3), (u32::MAX, 0), (0, u32::MAX)] {
        assert!(
            layout.footprint(slice, mip).is_err(),
            "slice={slice} mip={mip}"
        );
    }
    assert_eq!(layout.footprint(1, 2).unwrap().offset, 140);
}

#[test]
fn array_total_overflow_rejects_even_when_single_chain_fits() {
    let width = usize::MAX / 4;
    let chain = PackedTextureLayout::new(Rgba8Unorm, width, 1, 1).unwrap();
    assert_eq!(chain.byte_len(), usize::MAX - 3);
    assert!(matches!(
        xenora_texture::PackedTextureArrayLayout::new(Rgba8Unorm, width, 1, 1, 2),
        Err(TextureError::SizeOverflow)
    ));
    let one = xenora_texture::PackedTextureArrayLayout::new(Rgba8Unorm, width, 1, 1, 1).unwrap();
    assert_eq!(one.byte_len(), usize::MAX - 3);
}

#[test]
fn ten_format_storage_blocks_are_literal_and_role_independent() {
    for (format, extent, bytes) in [
        (R8Unorm, (1, 1), 1),
        (Rg8Unorm, (1, 1), 2),
        (Rgba8Unorm, (1, 1), 4),
        (Bgra8Unorm, (1, 1), 4),
        (Rgba16Float, (1, 1), 8),
        (R32Float, (1, 1), 4),
        (R32Uint, (1, 1), 4),
        (Depth32Float, (1, 1), 4),
        (Bc1RgbaUnorm, (4, 4), 8),
        (Bc3RgbaUnorm, (4, 4), 16),
    ] {
        assert_eq!(format.block_extent(), extent, "{format:?}");
        assert_eq!(format.block_bytes(), bytes, "{format:?}");
    }
}

#[test]
fn ten_format_odd_mips_and_two_layers_have_literal_full_footprints() {
    // Each tuple is (width, height, absolute offset, row bytes, block rows, bytes).
    // These tables do not call block/layout helpers to construct expected values.
    let r8 = [
        (5, 3, 0, 5, 3, 15),
        (2, 1, 15, 2, 1, 2),
        (1, 1, 17, 1, 1, 1),
        (5, 3, 18, 5, 3, 15),
        (2, 1, 33, 2, 1, 2),
        (1, 1, 35, 1, 1, 1),
    ];
    let rg8 = [
        (5, 3, 0, 10, 3, 30),
        (2, 1, 30, 4, 1, 4),
        (1, 1, 34, 2, 1, 2),
        (5, 3, 36, 10, 3, 30),
        (2, 1, 66, 4, 1, 4),
        (1, 1, 70, 2, 1, 2),
    ];
    let four = [
        (5, 3, 0, 20, 3, 60),
        (2, 1, 60, 8, 1, 8),
        (1, 1, 68, 4, 1, 4),
        (5, 3, 72, 20, 3, 60),
        (2, 1, 132, 8, 1, 8),
        (1, 1, 140, 4, 1, 4),
    ];
    let half4 = [
        (5, 3, 0, 40, 3, 120),
        (2, 1, 120, 16, 1, 16),
        (1, 1, 136, 8, 1, 8),
        (5, 3, 144, 40, 3, 120),
        (2, 1, 264, 16, 1, 16),
        (1, 1, 280, 8, 1, 8),
    ];
    let bc1 = [
        (5, 3, 0, 16, 1, 16),
        (2, 1, 16, 8, 1, 8),
        (1, 1, 24, 8, 1, 8),
        (5, 3, 32, 16, 1, 16),
        (2, 1, 48, 8, 1, 8),
        (1, 1, 56, 8, 1, 8),
    ];
    let bc3 = [
        (5, 3, 0, 32, 1, 32),
        (2, 1, 32, 16, 1, 16),
        (1, 1, 48, 16, 1, 16),
        (5, 3, 64, 32, 1, 32),
        (2, 1, 96, 16, 1, 16),
        (1, 1, 112, 16, 1, 16),
    ];
    for (format, chain_bytes, array_bytes, expected) in [
        (R8Unorm, 18, 36, r8),
        (Rg8Unorm, 36, 72, rg8),
        (Rgba8Unorm, 72, 144, four),
        (Bgra8Unorm, 72, 144, four),
        (Rgba16Float, 144, 288, half4),
        (R32Float, 72, 144, four),
        (R32Uint, 72, 144, four),
        (Depth32Float, 72, 144, four),
        (Bc1RgbaUnorm, 32, 64, bc1),
        (Bc3RgbaUnorm, 64, 128, bc3),
    ] {
        let chain = PackedTextureLayout::new(format, 5, 3, 3).unwrap();
        assert_eq!(chain.format(), format);
        assert_eq!(chain.byte_len(), chain_bytes, "{format:?}");
        let actual: Vec<_> = chain
            .levels()
            .iter()
            .map(|f| (f.width, f.height, f.offset, f.row_bytes, f.rows, f.byte_len))
            .collect();
        assert_eq!(actual, expected[..3], "{format:?} chain");
        let array = xenora_texture::PackedTextureArrayLayout::new(format, 5, 3, 3, 2).unwrap();
        assert_eq!(array.layers(), 2);
        assert_eq!(array.chain(), &chain);
        assert_eq!(array.byte_len(), array_bytes, "{format:?}");
        for (index, expected) in expected.into_iter().enumerate() {
            let f = array
                .footprint((index / 3) as u32, (index % 3) as u32)
                .unwrap();
            assert_eq!(
                (f.width, f.height, f.offset, f.row_bytes, f.rows, f.byte_len),
                expected,
                "{format:?} subresource {index}"
            );
        }
    }
}

#[test]
fn ten_format_checked_rows_mip_sum_and_array_total_overflow() {
    for (format, width, bytes) in [
        (R8Unorm, usize::MAX, usize::MAX),
        (Rg8Unorm, usize::MAX / 2, usize::MAX - 1),
        (Rgba8Unorm, usize::MAX / 4, usize::MAX - 3),
        (Bgra8Unorm, usize::MAX / 4, usize::MAX - 3),
        (Rgba16Float, usize::MAX / 8, usize::MAX - 7),
        (R32Float, usize::MAX / 4, usize::MAX - 3),
        (R32Uint, usize::MAX / 4, usize::MAX - 3),
        (Depth32Float, usize::MAX / 4, usize::MAX - 3),
        (Bc1RgbaUnorm, usize::MAX / 2 - 3, usize::MAX - 7),
        (Bc3RgbaUnorm, usize::MAX / 4 - 3, usize::MAX - 15),
    ] {
        let chain = PackedTextureLayout::new(format, width, 1, 1).unwrap();
        assert_eq!(chain.byte_len(), bytes, "{format:?} representable boundary");
        assert_eq!(
            PackedTextureLayout::new(format, width, 1, 2),
            Err(TextureError::SizeOverflow),
            "{format:?} mip sum"
        );
        assert_eq!(
            PackedTextureLayout::new(format, width, usize::MAX, 1),
            Err(TextureError::SizeOverflow),
            "{format:?} rows"
        );
        assert_eq!(
            xenora_texture::PackedTextureArrayLayout::new(format, width, 1, 1, 2),
            Err(TextureError::SizeOverflow),
            "{format:?} array sum"
        );
        let one = xenora_texture::PackedTextureArrayLayout::new(format, width, 1, 1, 1).unwrap();
        assert_eq!(one.byte_len(), bytes);
        assert_eq!(one.footprint(0, 0).unwrap().byte_len, bytes);
    }
}
