// SPDX-License-Identifier: GPL-3.0-or-later
//! Canonical tightly packed storage for persistent sampled textures.
use crate::TextureError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Rgba8Unorm,
    Bc1RgbaUnorm,
    Bc3RgbaUnorm,
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

fn footprint(
    format: TextureFormat,
    width: usize,
    height: usize,
    offset: usize,
) -> Result<TextureMipFootprint, TextureError> {
    // BC1/BC3 use 4x4 texel blocks of 8/16 bytes respectively:
    // https://learn.microsoft.com/en-us/windows/win32/direct3d11/texture-block-compression-in-direct3d-11
    let (columns, rows, unit_bytes) = match format {
        TextureFormat::Rgba8Unorm => (width, height, 4),
        TextureFormat::Bc1RgbaUnorm => (width.div_ceil(4), height.div_ceil(4), 8),
        TextureFormat::Bc3RgbaUnorm => (width.div_ceil(4), height.div_ceil(4), 16),
    };
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
