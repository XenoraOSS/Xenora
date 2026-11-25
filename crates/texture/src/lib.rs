// SPDX-License-Identifier: GPL-3.0-or-later
//! Synthetic texture layout for original fixtures.
//!
//! Each array layer stores its mip levels consecutively. Each level stores
//! row-major 2×2 tiles of BC1 blocks. Edge tiles have full storage, including
//! padding blocks. This layout is defined by Xenora's fixtures.

#![forbid(unsafe_code)]

mod bc;
mod bc6h;
mod bc7;
mod footprint;
pub mod sampler;
mod upload;
pub use bc::{
    decode_bc2, decode_bc3, decode_bc4_snorm, decode_bc4_unorm, decode_bc5_snorm, decode_bc5_unorm,
};
pub use bc6h::{
    BC6H_SUPPORTED_MODES, Bc6hError, bc6h_mode, decode_bc6h_sfloat, decode_bc6h_ufloat,
};
pub use bc7::{BC7_SUPPORTED_MODES, bc7_mode, decode_bc7};
pub use footprint::{
    BcCopyAlignment, BcFootprint, BcFormat, BcTextureDesc, BcTextureLayout, PackedBcUpload,
};
pub use sampler::{AddressMode, FilterMode, SamplerDesc, SamplerError, TextureMetadata};
pub use upload::{
    Bc1Subresource, Bc1Upload, Bc1UploadDesc, BcAlphaFormat, BcAlphaUpload, BcAlphaUploadDesc,
    BcChannelFormat, BcChannelUpload, BcChannelUploadDesc,
};

const BLOCK_BYTES: usize = 8;
const TILE_BLOCKS: usize = 2;
const TILE_ROW_BYTES: usize = TILE_BLOCKS * BLOCK_BYTES;
const TILE_BYTES: usize = TILE_BLOCKS * TILE_ROW_BYTES;

#[derive(Debug, PartialEq, Eq)]
pub enum TextureError {
    InvalidLayout,
    SizeOverflow,
    OutOfBounds,
    InvalidByteLength,
    AllocationFailed,
}

#[derive(Debug)]
pub struct TiledBc1<'a> {
    bytes: &'a [u8],
    width: usize,
    height: usize,
    mips: u32,
    layers: usize,
    layer_bytes: usize,
}

impl<'a> TiledBc1<'a> {
    pub fn new(
        bytes: &'a [u8],
        width: usize,
        height: usize,
        mips: u32,
        layers: usize,
    ) -> Result<Self, TextureError> {
        let max_mips = usize::BITS - width.max(height).leading_zeros();
        if width == 0 || height == 0 || layers == 0 || mips == 0 || mips > max_mips {
            return Err(TextureError::InvalidLayout);
        }
        let layer_bytes = (0..mips).try_fold(0usize, |total, mip| {
            total
                .checked_add(mip_bytes(width, height, mip)?)
                .ok_or(TextureError::SizeOverflow)
        })?;
        let size = layer_bytes
            .checked_mul(layers)
            .ok_or(TextureError::SizeOverflow)?;
        if size != bytes.len() {
            return Err(TextureError::InvalidByteLength);
        }
        Ok(Self {
            bytes,
            width,
            height,
            mips,
            layers,
            layer_bytes,
        })
    }

    /// Returns the logical texel dimensions of an existing mip level.
    pub fn mip_extent(&self, mip: u32) -> Result<(usize, usize), TextureError> {
        if mip >= self.mips {
            return Err(TextureError::OutOfBounds);
        }
        Ok(((self.width >> mip).max(1), (self.height >> mip).max(1)))
    }

    /// Copies one subresource as tightly packed row-major BC1 block rows.
    /// Padded tile slots are excluded, including at partial texel blocks.
    pub fn linearize(&self, layer: usize, mip: u32) -> Result<Vec<u8>, TextureError> {
        if layer >= self.layers || mip >= self.mips {
            return Err(TextureError::OutOfBounds);
        }
        let (width, height) = mip_blocks(self.width, self.height, mip);
        let base = self.mip_base(layer, mip)?;
        let tile_row_stride = width.div_ceil(TILE_BLOCKS) * TILE_BYTES;
        let full_tiles = width / TILE_BLOCKS;
        let edge_bytes = (width % TILE_BLOCKS) * BLOCK_BYTES;
        let mut blocks = Vec::new();
        // Construction checked the larger, padded byte count.
        blocks
            .try_reserve_exact(width * height * BLOCK_BYTES)
            .map_err(|_| TextureError::AllocationFailed)?;
        // Construction requires the exact padded length, so each selected tile row is in bounds.
        for y in 0..height {
            let row_start =
                base + (y / TILE_BLOCKS) * tile_row_stride + (y % TILE_BLOCKS) * TILE_ROW_BYTES;
            for tile in 0..full_tiles {
                let start = row_start + tile * TILE_BYTES;
                blocks.extend_from_slice(&self.bytes[start..start + TILE_ROW_BYTES]);
            }
            if edge_bytes != 0 {
                let start = row_start + full_tiles * TILE_BYTES;
                blocks.extend_from_slice(&self.bytes[start..start + edge_bytes]);
            }
        }
        Ok(blocks)
    }

    /// Reads a block by its logical block coordinates, excluding tile padding.
    pub fn block(
        &self,
        layer: usize,
        mip: u32,
        x: usize,
        y: usize,
    ) -> Result<[u8; BLOCK_BYTES], TextureError> {
        if layer >= self.layers || mip >= self.mips {
            return Err(TextureError::OutOfBounds);
        }
        let (width, height) = mip_blocks(self.width, self.height, mip);
        if x >= width || y >= height {
            return Err(TextureError::OutOfBounds);
        }
        let base = self.mip_base(layer, mip)?;
        // Construction checked the full padded size for every level and layer.
        let tile = (y / TILE_BLOCKS) * width.div_ceil(TILE_BLOCKS) + x / TILE_BLOCKS;
        let within_tile = (y % TILE_BLOCKS) * TILE_BLOCKS + x % TILE_BLOCKS;
        let offset = base + tile * TILE_BYTES + within_tile * BLOCK_BYTES;
        self.bytes
            .get(offset..offset + BLOCK_BYTES)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(TextureError::InvalidByteLength)
    }

    fn mip_base(&self, layer: usize, mip: u32) -> Result<usize, TextureError> {
        let mut base = layer * self.layer_bytes;
        for prior in 0..mip {
            base += mip_bytes(self.width, self.height, prior)?;
        }
        Ok(base)
    }
}

fn mip_blocks(width: usize, height: usize, mip: u32) -> (usize, usize) {
    (
        (width >> mip).max(1).div_ceil(4),
        (height >> mip).max(1).div_ceil(4),
    )
}

fn mip_bytes(width: usize, height: usize, mip: u32) -> Result<usize, TextureError> {
    let (width, height) = mip_blocks(width, height, mip);
    width
        .div_ceil(TILE_BLOCKS)
        .checked_mul(height.div_ceil(TILE_BLOCKS))
        .and_then(|tiles| tiles.checked_mul(TILE_BYTES))
        .ok_or(TextureError::SizeOverflow)
}

/// Expands a BC1 block to 16 row-major RGBA8 texels using integer interpolation.
/// See Microsoft's Direct3D 10 documentation for the BC1 palette and indices.
pub fn decode_bc1(block: [u8; BLOCK_BYTES]) -> [[u8; 4]; 16] {
    let [a, b, c, d, i, j, k, l] = block;
    let first = u16::from_le_bytes([a, b]);
    let second = u16::from_le_bytes([c, d]);
    let mut palette = [rgb565(first), rgb565(second), [0; 4], [0; 4]];
    let opaque = first > second;
    for channel in 0..3 {
        let a = u16::from(palette[0][channel]);
        let b = u16::from(palette[1][channel]);
        if opaque {
            palette[2][channel] = ((2 * a + b) / 3) as u8;
            palette[3][channel] = ((a + 2 * b) / 3) as u8;
        } else {
            palette[2][channel] = ((a + b) / 2) as u8;
        }
    }
    palette[2][3] = 255;
    if opaque {
        palette[3][3] = 255;
    }
    let indices = u32::from_le_bytes([i, j, k, l]);
    std::array::from_fn(|texel| palette[((indices >> (2 * texel)) & 3) as usize])
}

fn rgb565(color: u16) -> [u8; 4] {
    let r = ((color >> 11) & 31) as u8;
    let g = ((color >> 5) & 63) as u8;
    let b = (color & 31) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_edges_and_mips_across_layers() {
        // 9×5 has 3×2 logical blocks in two tiles, followed by a 4×2
        // mip in one tile. Repeating byte values identify physical slots.
        let bytes: Vec<_> = (0..24).flat_map(|marker| [marker; 8]).collect();
        let tex = TiledBc1::new(&bytes, 9, 5, 2, 2).unwrap();
        assert_eq!(tex.block(0, 0, 1, 1), Ok([3; 8]));
        assert_eq!(tex.block(0, 0, 2, 1), Ok([6; 8]));
        assert_eq!(tex.block(0, 1, 0, 0), Ok([8; 8]));
        assert_eq!(tex.block(1, 0, 0, 0), Ok([12; 8]));
        assert_eq!(tex.block(1, 1, 0, 0), Ok([20; 8]));
        assert_eq!(tex.block(0, 0, 3, 0), Err(TextureError::OutOfBounds));
        assert_eq!(tex.block(0, 2, 0, 0), Err(TextureError::OutOfBounds));
        assert_eq!(tex.block(2, 0, 0, 0), Err(TextureError::OutOfBounds));
        assert_eq!(tex.mip_extent(1), Ok((4, 2)));
        let logical_blocks = [[12; 8], [13; 8], [16; 8], [14; 8], [15; 8], [18; 8]];
        assert_eq!(tex.linearize(1, 0).unwrap(), logical_blocks.as_flattened());
        assert_eq!(tex.linearize(1, 1).unwrap(), [20; 8]);
        assert_eq!(tex.linearize(2, 0), Err(TextureError::OutOfBounds));
        assert_eq!(tex.linearize(0, 2), Err(TextureError::OutOfBounds));
    }

    #[test]
    fn rejects_truncation_invalid_mips_and_overflow() {
        assert_eq!(
            TiledBc1::new(&[0; 31], 4, 4, 1, 1).unwrap_err(),
            TextureError::InvalidByteLength
        );
        assert_eq!(
            TiledBc1::new(&[], 1, 1, 2, 1).unwrap_err(),
            TextureError::InvalidLayout
        );
        assert_eq!(
            TiledBc1::new(&[], usize::MAX, usize::MAX, 1, 1).unwrap_err(),
            TextureError::SizeOverflow
        );
    }

    #[test]
    fn advances_to_the_next_tile_row() {
        let bytes: Vec<_> = (0..16).flat_map(|marker| [marker; 8]).collect();
        let texture = TiledBc1::new(&bytes, 9, 9, 1, 1).unwrap();
        assert_eq!(texture.block(0, 0, 0, 2), Ok([8; 8]));
        assert_eq!(texture.block(0, 0, 2, 2), Ok([12; 8]));
    }

    #[test]
    fn linearizes_odd_tile_edges_in_each_layer() {
        // A 21×21 texture uses 9, 4 and 1 tiles across the three mips.
        // Mip 1 is 10×10 texels, so only nine of its sixteen slots are logical.
        let block = |slot: u8| [slot, 1, 2, 3, 4, 5, 6, !slot];
        let bytes: Vec<_> = (0..112).flat_map(block).collect();
        let texture = TiledBc1::new(&bytes, 21, 21, 3, 2).unwrap();
        for (layer, first_slot) in [(0, 36), (1, 92)] {
            let expected: Vec<_> = [0, 1, 4, 2, 3, 6, 8, 9, 12]
                .into_iter()
                .flat_map(|slot| block(first_slot + slot))
                .collect();
            assert_eq!(texture.linearize(layer, 1).unwrap(), expected);
            let last_mip: Vec<_> = (16..20).flat_map(|slot| block(first_slot + slot)).collect();
            assert_eq!(texture.linearize(layer, 2).unwrap(), last_mip);
        }
    }

    #[test]
    fn decodes_opaque_and_transparent_bc1_palettes() {
        let opaque = decode_bc1([0, 0xf8, 31, 0, 0xe4, 0xe4, 0xe4, 0xe4]);
        let transparent = decode_bc1([31, 0, 0, 0xf8, 0xe4, 0xe4, 0xe4, 0xe4]);
        assert_eq!(
            opaque.as_slice(),
            [
                [255, 0, 0, 255],
                [0, 0, 255, 255],
                [170, 0, 85, 255],
                [85, 0, 170, 255]
            ]
            .repeat(4)
        );
        assert_eq!(
            transparent.as_slice(),
            [
                [0, 0, 255, 255],
                [255, 0, 0, 255],
                [127, 0, 127, 255],
                [0; 4]
            ]
            .repeat(4)
        );
    }

    #[test]
    fn decodes_a_block_from_a_selected_layer_and_mip() {
        let mut bytes = [0; 128];
        // A 5×5 layer has one 32-byte tile per mip. Layer 1, mip 1
        // starts at byte 96. Only that block is filled with opaque green.
        bytes[96..104].copy_from_slice(&[224, 7, 0, 0, 0, 0, 0, 0]);
        let texture = TiledBc1::new(&bytes, 5, 5, 2, 2).unwrap();
        assert_eq!(
            decode_bc1(texture.block(1, 1, 0, 0).unwrap()),
            [[0, 255, 0, 255]; 16]
        );
    }
}
