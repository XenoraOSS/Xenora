// SPDX-License-Identifier: GPL-3.0-or-later

//! Linear copy footprints for independently authored block-compressed textures.
//!
//! For example, a 5×3 BC1 level has two 8-byte blocks forming one 16-byte row.
//! Its 2×1 mip takes a single 8-byte block. In a tight layout the levels occupy
//! 24 bytes; either can be borrowed directly from the same payload.
//!
//! ```
//! use xenora_texture::{BcCopyAlignment, BcFormat, BcTextureDesc, BcTextureLayout, PackedBcUpload};
//!
//! let layout = BcTextureLayout::new(
//!     BcTextureDesc { width: 5, height: 3, mips: 2, layers: 1, format: BcFormat::Bc1 },
//!     BcCopyAlignment::TIGHT,
//! )?;
//! let mut bytes = vec![0; layout.byte_len()];
//! layout.write_tight(&mut bytes, 0, 1, &[0x55; 8])?;
//! let upload = PackedBcUpload::new(&layout, &bytes)?;
//! assert_eq!(upload.subresource(0, 1)?.bytes, &[0x55; 8]);
//! # Ok::<(), xenora_texture::TextureError>(())
//! ```

use super::{
    BLOCK_BYTES, Bc1Subresource, Bc1Upload, Bc1UploadDesc, BcAlphaFormat, BcAlphaUpload,
    BcAlphaUploadDesc, BcChannelFormat, BcChannelUpload, BcChannelUploadDesc, TextureError,
    mip_blocks,
};

/// Block encoding carried by the layout. The layout handles bytes without decoding texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcFormat {
    Bc1,
    BcAlpha(BcAlphaFormat),
    BcChannel(BcChannelFormat),
    Bc6hUfloat,
    Bc6hSfloat,
    Bc7,
}

impl BcFormat {
    pub const fn block_bytes(self) -> usize {
        match self {
            Self::Bc1 => BLOCK_BYTES,
            Self::BcChannel(format) => format.block_bytes(),
            Self::BcAlpha(_) => BcAlphaUpload::BLOCK_BYTES,
            Self::Bc6hUfloat | Self::Bc6hSfloat | Self::Bc7 => 16,
        }
    }
}

/// Dimensions and encoding of a 2D texture or texture array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BcTextureDesc {
    pub width: usize,
    pub height: usize,
    pub mips: u32,
    pub layers: usize,
    pub format: BcFormat,
}

/// Row and subresource placement alignments, in bytes. Each must be a nonzero
/// power of two. Alignments smaller than a block are raised to its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BcCopyAlignment {
    pub row: usize,
    pub placement: usize,
}

impl BcCopyAlignment {
    pub const TIGHT: Self = Self {
        row: 1,
        placement: 1,
    };
}

/// Logical dimensions and physical storage for one mip in one layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BcFootprint {
    pub width: usize,
    pub height: usize,
    pub offset: usize,
    pub row_bytes: usize,
    pub row_pitch: usize,
    pub block_rows: usize,
    /// Full pitched rows, including the final row's padding.
    pub byte_len: usize,
}

/// Layer-major storage with one footprint per mip. Layers share the same
/// layout. Placement padding precedes each subresource as needed and there
/// is no trailing placement padding at the end of the final layer.
#[derive(Debug)]
pub struct BcTextureLayout {
    desc: BcTextureDesc,
    mips: Vec<BcFootprint>,
    layer_stride: usize,
    byte_len: usize,
}

impl BcTextureLayout {
    pub fn new(desc: BcTextureDesc, alignment: BcCopyAlignment) -> Result<Self, TextureError> {
        let max_mips = usize::BITS - desc.width.max(desc.height).leading_zeros();
        if desc.width == 0
            || desc.height == 0
            || desc.layers == 0
            || desc.mips == 0
            || desc.mips > max_mips
            || !alignment.row.is_power_of_two()
            || !alignment.placement.is_power_of_two()
        {
            return Err(TextureError::InvalidLayout);
        }

        let block_bytes = desc.format.block_bytes();
        let row_alignment = alignment.row.max(block_bytes);
        let placement_alignment = alignment.placement.max(block_bytes);
        let mut mips = Vec::new();
        mips.try_reserve_exact(desc.mips as usize)
            .map_err(|_| TextureError::AllocationFailed)?;

        let mut layer_len: usize = 0;
        for mip in 0..desc.mips {
            let (blocks_x, block_rows) = mip_blocks(desc.width, desc.height, mip);
            let row_bytes = blocks_x
                .checked_mul(block_bytes)
                .ok_or(TextureError::SizeOverflow)?;
            let row_pitch = align_up(row_bytes, row_alignment)?;
            let offset = align_up(layer_len, placement_alignment)?;
            let byte_len = row_pitch
                .checked_mul(block_rows)
                .ok_or(TextureError::SizeOverflow)?;
            layer_len = offset
                .checked_add(byte_len)
                .ok_or(TextureError::SizeOverflow)?;
            mips.push(BcFootprint {
                width: (desc.width >> mip).max(1),
                height: (desc.height >> mip).max(1),
                offset,
                row_bytes,
                row_pitch,
                block_rows,
                byte_len,
            });
        }

        // A single layer never needs padding after its last mip.
        let layer_stride = if desc.layers == 1 {
            layer_len
        } else {
            align_up(layer_len, placement_alignment)?
        };
        let byte_len = layer_stride
            .checked_mul(desc.layers - 1)
            .and_then(|prefix| prefix.checked_add(layer_len))
            .ok_or(TextureError::SizeOverflow)?;
        Ok(Self {
            desc,
            mips,
            layer_stride,
            byte_len,
        })
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub fn footprint(&self, layer: usize, mip: u32) -> Result<BcFootprint, TextureError> {
        if layer >= self.desc.layers {
            return Err(TextureError::OutOfBounds);
        }
        let footprint = self
            .mips
            .get(mip as usize)
            .ok_or(TextureError::OutOfBounds)?;
        Ok(self.at_layer(*footprint, layer))
    }

    /// Writes tightly packed block rows to one subresource. Clears that
    /// subresource's row padding and leaves the rest of the payload alone.
    /// The destination must include exactly this layout's full byte span.
    pub fn write_tight(
        &self,
        dst: &mut [u8],
        layer: usize,
        mip: u32,
        src: &[u8],
    ) -> Result<(), TextureError> {
        let fp = self.footprint(layer, mip)?;
        if dst.len() != self.byte_len || src.len() != fp.row_bytes * fp.block_rows {
            return Err(TextureError::InvalidByteLength);
        }

        let dst = &mut dst[fp.offset..fp.offset + fp.byte_len];
        dst.fill(0);
        copy_rows(src, fp.row_bytes, dst, fp.row_pitch, fp.row_bytes);
        Ok(())
    }

    fn at_layer(&self, mut footprint: BcFootprint, layer: usize) -> BcFootprint {
        // Construction checked the span through the end of the final layer.
        footprint.offset += self.layer_stride * layer;
        footprint
    }
}

/// A validated, borrowed payload. Padding bytes in its input may be arbitrary.
#[derive(Debug)]
pub struct PackedBcUpload<'a> {
    layout: &'a BcTextureLayout,
    bytes: &'a [u8],
}

impl<'a> PackedBcUpload<'a> {
    pub fn new(layout: &'a BcTextureLayout, bytes: &'a [u8]) -> Result<Self, TextureError> {
        if bytes.len() != layout.byte_len {
            return Err(TextureError::InvalidByteLength);
        }
        Ok(Self { layout, bytes })
    }

    pub fn format(&self) -> BcFormat {
        self.layout.desc.format
    }

    /// Borrows the selected block rows, including their row padding.
    pub fn subresource(&self, layer: usize, mip: u32) -> Result<Bc1Subresource<'a>, TextureError> {
        let fp = self.layout.footprint(layer, mip)?;
        Ok(Bc1Subresource {
            bytes: &self.bytes[fp.offset..fp.offset + fp.byte_len],
            width: fp.width,
            height: fp.height,
            row_pitch: fp.row_pitch,
        })
    }

    /// Copies every logical block to an equivalent layout and zeroes its padding.
    /// The source and destination must have the same dimensions, mip and layer
    /// counts, and block encoding. A rejected copy leaves the destination intact.
    pub fn repack_into(
        &self,
        layout: &BcTextureLayout,
        dst: &mut [u8],
    ) -> Result<(), TextureError> {
        if self.layout.desc != layout.desc {
            return Err(TextureError::InvalidLayout);
        }
        if dst.len() != layout.byte_len {
            return Err(TextureError::InvalidByteLength);
        }

        dst.fill(0);
        for layer in 0..layout.desc.layers {
            for (&source, &target) in self.layout.mips.iter().zip(&layout.mips) {
                let source = self.layout.at_layer(source, layer);
                let target = layout.at_layer(target, layer);
                copy_rows(
                    &self.bytes[source.offset..source.offset + source.byte_len],
                    source.row_pitch,
                    &mut dst[target.offset..target.offset + target.byte_len],
                    target.row_pitch,
                    source.row_bytes,
                );
            }
        }
        Ok(())
    }

    /// Adapts one BC1 subresource to the existing single-upload reader, without copying.
    pub fn as_bc1(&self, layer: usize, mip: u32) -> Result<Bc1Upload<'a>, TextureError> {
        if self.layout.desc.format != BcFormat::Bc1 {
            return Err(TextureError::InvalidLayout);
        }
        let view = self.subresource(layer, mip)?;
        Bc1Upload::new(
            view.bytes,
            Bc1UploadDesc {
                width: view.width,
                height: view.height,
                mips: 1,
                layers: 1,
                format: 1,
                usage: 1,
                row_pitch: view.row_pitch,
                slice_pitch: view.bytes.len(),
            },
        )
    }

    /// Adapts one BC2 or BC3 subresource to the existing reader, without copying.
    pub fn as_bc_alpha(&self, layer: usize, mip: u32) -> Result<BcAlphaUpload<'a>, TextureError> {
        let BcFormat::BcAlpha(format) = self.layout.desc.format else {
            return Err(TextureError::InvalidLayout);
        };
        let view = self.subresource(layer, mip)?;
        BcAlphaUpload::new(
            view.bytes,
            BcAlphaUploadDesc {
                width: view.width,
                height: view.height,
                mips: 1,
                layers: 1,
                format,
                row_pitch: view.row_pitch,
                slice_pitch: view.bytes.len(),
            },
        )
    }

    /// Adapts one BC4 or BC5 subresource to the existing reader, without copying.
    pub fn as_bc_channel(
        &self,
        layer: usize,
        mip: u32,
    ) -> Result<BcChannelUpload<'a>, TextureError> {
        let BcFormat::BcChannel(format) = self.layout.desc.format else {
            return Err(TextureError::InvalidLayout);
        };
        let view = self.subresource(layer, mip)?;
        BcChannelUpload::new(
            view.bytes,
            BcChannelUploadDesc {
                width: view.width,
                height: view.height,
                mips: 1,
                layers: 1,
                format,
                row_pitch: view.row_pitch,
                slice_pitch: view.bytes.len(),
            },
        )
    }
}

fn align_up(value: usize, alignment: usize) -> Result<usize, TextureError> {
    value
        .checked_next_multiple_of(alignment)
        .ok_or(TextureError::SizeOverflow)
}

fn copy_rows(src: &[u8], src_pitch: usize, dst: &mut [u8], dst_pitch: usize, row_bytes: usize) {
    for (src_row, dst_row) in src
        .chunks_exact(src_pitch)
        .zip(dst.chunks_exact_mut(dst_pitch))
    {
        dst_row[..row_bytes].copy_from_slice(&src_row[..row_bytes]);
    }
}
