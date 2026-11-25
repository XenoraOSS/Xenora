// SPDX-License-Identifier: GPL-3.0-or-later
//! Canonical tightly packed storage for persistent sampled textures.
use crate::TextureError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Rgba8Unorm,
    R8Unorm,
    Rg8Unorm,
    Bgra8Unorm,
    Rgba16Float,
    R32Float,
    R32Uint,
    Depth32Float,
    Bc1RgbaUnorm,
    Bc3RgbaUnorm,
}

impl TextureFormat {
    /// Storage-block dimensions in texels, independent of supported GPU roles.
    pub const fn block_extent(self) -> (usize, usize) {
        match self {
            Self::Bc1RgbaUnorm | Self::Bc3RgbaUnorm => (4, 4),
            _ => (1, 1),
        }
    }

    /// Bytes occupied by one complete storage block; copies preserve these bytes.
    pub const fn block_bytes(self) -> usize {
        match self {
            Self::R8Unorm => 1,
            Self::Rg8Unorm => 2,
            Self::Rgba8Unorm
            | Self::Bgra8Unorm
            | Self::R32Float
            | Self::R32Uint
            | Self::Depth32Float => 4,
            Self::Rgba16Float | Self::Bc1RgbaUnorm => 8,
            Self::Bc3RgbaUnorm => 16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureMipFootprint {
    pub width: usize,
    pub height: usize,
    pub offset: usize,
    pub row_bytes: usize,
    pub rows: usize,
    pub byte_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedTextureLayout {
    format: TextureFormat,
    levels: Vec<TextureMipFootprint>,
    byte_len: usize,
}

impl PackedTextureLayout {
    /// Computes one tightly packed 2D mip chain without allocating texel storage.
    /// BC edge blocks occupy complete blocks, including the final 1x1 level.
    /// Device dimension and resource-byte budgets are enforced by callers.
    pub fn new(
        format: TextureFormat,
        width: usize,
        height: usize,
        mips: u32,
    ) -> Result<Self, TextureError> {
        let max_mips = usize::BITS - width.max(height).leading_zeros();
        if width == 0 || height == 0 || mips == 0 || mips > max_mips {
            return Err(TextureError::InvalidLayout);
        }
        // Validate all arithmetic before allocating even the bounded metadata table.
        let byte_len = (0..mips).try_fold(0usize, |offset, level| {
            let mip = footprint(
                format,
                (width >> level).max(1),
                (height >> level).max(1),
                offset,
            )?;
            offset
                .checked_add(mip.byte_len)
                .ok_or(TextureError::SizeOverflow)
        })?;
        let mut levels = Vec::new();
        levels
            .try_reserve_exact(mips as usize)
            .map_err(|_| TextureError::AllocationFailed)?;
        let mut offset = 0;
        for level in 0..mips {
            let mip = footprint(
                format,
                (width >> level).max(1),
                (height >> level).max(1),
                offset,
            )?;
            offset += mip.byte_len; // The complete sum was checked above.
            levels.push(mip);
        }
        Ok(Self {
            format,
            levels,
            byte_len,
        })
    }
    pub fn format(&self) -> TextureFormat {
        self.format
    }
    pub fn levels(&self) -> &[TextureMipFootprint] {
        &self.levels
    }
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }
}

/// Canonical layer-major storage: each layer contains a complete packed mip chain.
/// This describes transfer bytes, not a native GPU texture's physical allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedTextureArrayLayout {
    chain: PackedTextureLayout,
    layers: u32,
    byte_len: usize,
}

impl PackedTextureArrayLayout {
    /// Device layer limits and resource budgets remain the caller's responsibility.
    pub fn new(
        format: TextureFormat,
        width: usize,
        height: usize,
        mips: u32,
        layers: u32,
    ) -> Result<Self, TextureError> {
        if layers == 0 {
            return Err(TextureError::InvalidLayout);
        }
        let chain = PackedTextureLayout::new(format, width, height, mips)?;
        let byte_len = chain
            .byte_len()
            .checked_mul(layers as usize)
            .ok_or(TextureError::SizeOverflow)?;
        Ok(Self {
            chain,
            layers,
            byte_len,
        })
    }

    pub fn chain(&self) -> &PackedTextureLayout {
        &self.chain
    }

    pub fn layers(&self) -> u32 {
        self.layers
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Return a mip footprint with an absolute byte offset in the full array.
    pub fn footprint(&self, slice: u32, mip: u32) -> Result<TextureMipFootprint, TextureError> {
        if slice >= self.layers {
            return Err(TextureError::OutOfBounds);
        }
        let mut footprint = *self
            .chain
            .levels()
            .get(mip as usize)
            .ok_or(TextureError::OutOfBounds)?;
        // Construction checked the larger full-array byte count.
        footprint.offset += slice as usize * self.chain.byte_len();
        Ok(footprint)
    }
}

fn footprint(
    format: TextureFormat,
    width: usize,
    height: usize,
    offset: usize,
) -> Result<TextureMipFootprint, TextureError> {
    // BC1/BC3 use 4x4 texel blocks of 8/16 bytes respectively:
    // https://learn.microsoft.com/en-us/windows/win32/direct3d11/texture-block-compression-in-direct3d-11
    let (block_width, block_height) = format.block_extent();
    let columns = width.div_ceil(block_width);
    let rows = height.div_ceil(block_height);
    let unit_bytes = format.block_bytes();
    let row_bytes = columns
        .checked_mul(unit_bytes)
        .ok_or(TextureError::SizeOverflow)?;
    let byte_len = row_bytes
        .checked_mul(rows)
        .ok_or(TextureError::SizeOverflow)?;
    Ok(TextureMipFootprint {
        width,
        height,
        offset,
        row_bytes,
        rows,
        byte_len,
    })
}
