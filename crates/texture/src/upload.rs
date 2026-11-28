// SPDX-License-Identifier: GPL-3.0-or-later

use super::{BLOCK_BYTES, TextureError, mip_blocks};

/// A layer-major BC1 upload with one common pitch for every mip.
/// Each mip occupies exactly one slice, including any trailing padding.
#[derive(Clone, Copy, Debug)]
pub struct Bc1UploadDesc {
    pub width: usize,
    pub height: usize,
    pub mips: u32,
    pub layers: usize,
    pub format: u32,
    pub usage: u32,
    pub row_pitch: usize,
    pub slice_pitch: usize,
}

#[derive(Debug)]
pub struct Bc1Upload<'a> {
    bytes: &'a [u8],
    desc: Bc1UploadDesc,
}

#[derive(Debug)]
pub struct Bc1Subresource<'a> {
    pub bytes: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub row_pitch: usize,
}

impl<'a> Bc1Upload<'a> {
    pub fn new(bytes: &'a [u8], desc: Bc1UploadDesc) -> Result<Self, TextureError> {
        if desc.format != 1 || desc.usage != 1 {
            return Err(TextureError::InvalidLayout);
        }
        Self::layout(desc).validate(bytes.len(), BLOCK_BYTES)?;
        Ok(Self { bytes, desc })
    }

    pub fn subresource(&self, layer: usize, mip: u32) -> Result<Bc1Subresource<'a>, TextureError> {
        Self::layout(self.desc).subresource(self.bytes, layer, mip)
    }

    fn layout(desc: Bc1UploadDesc) -> PitchedLayout {
        PitchedLayout {
            width: desc.width,
            height: desc.height,
            mips: desc.mips,
            layers: desc.layers,
            row_pitch: desc.row_pitch,
            slice_pitch: desc.slice_pitch,
        }
    }
}

/// A block-compressed format whose 16-byte block holds an alpha half followed
/// by a BC1-style colour half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcAlphaFormat {
    /// Explicit 4-bit alpha per texel.
    Bc2,
    /// Interpolated alpha from two 8-bit endpoints and 3-bit indices.
    Bc3,
}

/// A layer-major BC2 or BC3 upload with the same pitch rules as
/// [`Bc1UploadDesc`], measured in 16-byte blocks.
#[derive(Clone, Copy, Debug)]
pub struct BcAlphaUploadDesc {
    pub width: usize,
    pub height: usize,
    pub mips: u32,
    pub layers: usize,
    pub format: BcAlphaFormat,
    pub row_pitch: usize,
    pub slice_pitch: usize,
}

#[derive(Debug)]
pub struct BcAlphaUpload<'a> {
    bytes: &'a [u8],
    desc: BcAlphaUploadDesc,
}

impl<'a> BcAlphaUpload<'a> {
    /// Size of one BC2 or BC3 block in bytes.
    pub const BLOCK_BYTES: usize = 16;

    pub fn new(bytes: &'a [u8], desc: BcAlphaUploadDesc) -> Result<Self, TextureError> {
        Self::layout(desc).validate(bytes.len(), Self::BLOCK_BYTES)?;
        Ok(Self { bytes, desc })
    }

    pub fn format(&self) -> BcAlphaFormat {
        self.desc.format
    }

    /// Selects one level; the returned rows hold 16-byte blocks.
    pub fn subresource(&self, layer: usize, mip: u32) -> Result<Bc1Subresource<'a>, TextureError> {
        Self::layout(self.desc).subresource(self.bytes, layer, mip)
    }

    fn layout(desc: BcAlphaUploadDesc) -> PitchedLayout {
        PitchedLayout {
            width: desc.width,
            height: desc.height,
            mips: desc.mips,
            layers: desc.layers,
            row_pitch: desc.row_pitch,
            slice_pitch: desc.slice_pitch,
        }
    }
}

/// A single- or dual-channel block-compressed format: BC4 stores one 8-byte
/// red block, BC5 a red block followed by a green block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcChannelFormat {
    Bc4Unorm,
    Bc4Snorm,
    Bc5Unorm,
    Bc5Snorm,
}

impl BcChannelFormat {
    /// Size of one block in bytes.
    pub const fn block_bytes(self) -> usize {
        match self {
            Self::Bc4Unorm | Self::Bc4Snorm => 8,
            Self::Bc5Unorm | Self::Bc5Snorm => 16,
        }
    }
}

/// A layer-major BC4 or BC5 upload with the same pitch rules as
/// [`Bc1UploadDesc`], measured in the format's block size.
#[derive(Clone, Copy, Debug)]
pub struct BcChannelUploadDesc {
    pub width: usize,
    pub height: usize,
    pub mips: u32,
    pub layers: usize,
    pub format: BcChannelFormat,
    pub row_pitch: usize,
    pub slice_pitch: usize,
}

#[derive(Debug)]
pub struct BcChannelUpload<'a> {
    bytes: &'a [u8],
    desc: BcChannelUploadDesc,
}

impl<'a> BcChannelUpload<'a> {
    pub fn new(bytes: &'a [u8], desc: BcChannelUploadDesc) -> Result<Self, TextureError> {
        Self::layout(desc).validate(bytes.len(), desc.format.block_bytes())?;
        Ok(Self { bytes, desc })
    }

    pub fn format(&self) -> BcChannelFormat {
        self.desc.format
    }

    /// Selects one level; the returned rows hold blocks of this format.
    pub fn subresource(&self, layer: usize, mip: u32) -> Result<Bc1Subresource<'a>, TextureError> {
        Self::layout(self.desc).subresource(self.bytes, layer, mip)
    }

    fn layout(desc: BcChannelUploadDesc) -> PitchedLayout {
        PitchedLayout {
            width: desc.width,
            height: desc.height,
            mips: desc.mips,
            layers: desc.layers,
            row_pitch: desc.row_pitch,
            slice_pitch: desc.slice_pitch,
        }
    }
}

/// A block-compressed format of the BPTC family, whose 16-byte blocks select
/// a mode in their low bits: BC6H for half-float RGB, BC7 for RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BptcFormat {
    /// BC6H with unsigned half-float endpoints (UF16).
    Bc6hUfloat,
    /// BC6H with signed half-float endpoints (SF16).
    Bc6hSfloat,
    /// BC7 with unsigned normalized channels.
    Bc7Unorm,
}

/// A layer-major BC6H or BC7 upload with the same pitch rules as
/// [`Bc1UploadDesc`], measured in 16-byte blocks.
#[derive(Clone, Copy, Debug)]
pub struct BptcUploadDesc {
    pub width: usize,
    pub height: usize,
    pub mips: u32,
    pub layers: usize,
    pub format: BptcFormat,
    pub row_pitch: usize,
    pub slice_pitch: usize,
}

/// Validated pitched BC6H or BC7 bytes. Construction checks the layout only:
/// block contents, reserved modes included, pass through unchanged.
#[derive(Debug)]
pub struct BptcUpload<'a> {
    bytes: &'a [u8],
    desc: BptcUploadDesc,
}

impl<'a> BptcUpload<'a> {
    /// Size of one BC6H or BC7 block in bytes.
    pub const BLOCK_BYTES: usize = 16;

    pub fn new(bytes: &'a [u8], desc: BptcUploadDesc) -> Result<Self, TextureError> {
        Self::layout(desc).validate(bytes.len(), Self::BLOCK_BYTES)?;
        Ok(Self { bytes, desc })
    }

    pub fn format(&self) -> BptcFormat {
        self.desc.format
    }

    /// Selects one level; the returned rows hold 16-byte blocks.
    pub fn subresource(&self, layer: usize, mip: u32) -> Result<Bc1Subresource<'a>, TextureError> {
        Self::layout(self.desc).subresource(self.bytes, layer, mip)
    }

    fn layout(desc: BptcUploadDesc) -> PitchedLayout {
        PitchedLayout {
            width: desc.width,
            height: desc.height,
            mips: desc.mips,
            layers: desc.layers,
            row_pitch: desc.row_pitch,
            slice_pitch: desc.slice_pitch,
        }
    }
}

/// Format-independent pitch rules shared by the pitched uploads.
struct PitchedLayout {
    width: usize,
    height: usize,
    mips: u32,
    layers: usize,
    row_pitch: usize,
    slice_pitch: usize,
}

impl PitchedLayout {
    fn validate(&self, len: usize, block_bytes: usize) -> Result<(), TextureError> {
        let max_mips = usize::BITS - self.width.max(self.height).leading_zeros();
        if self.width == 0
            || self.height == 0
            || self.layers == 0
            || self.mips == 0
            || self.mips > max_mips
        {
            return Err(TextureError::InvalidLayout);
        }
        let (blocks_x, blocks_y) = mip_blocks(self.width, self.height, 0);
        let tight_row = blocks_x
            .checked_mul(block_bytes)
            .ok_or(TextureError::SizeOverflow)?;
        if self.row_pitch < tight_row || !self.row_pitch.is_multiple_of(block_bytes) {
            return Err(TextureError::InvalidLayout);
        }
        let minimum_slice = self
            .row_pitch
            .checked_mul(blocks_y)
            .ok_or(TextureError::SizeOverflow)?;
        if self.slice_pitch < minimum_slice || !self.slice_pitch.is_multiple_of(block_bytes) {
            return Err(TextureError::InvalidLayout);
        }
        let size = self
            .slice_pitch
            .checked_mul(self.mips as usize)
            .and_then(|size| size.checked_mul(self.layers))
            .ok_or(TextureError::SizeOverflow)?;
        if len != size {
            return Err(TextureError::InvalidByteLength);
        }
        Ok(())
    }

    fn subresource<'a>(
        &self,
        bytes: &'a [u8],
        layer: usize,
        mip: u32,
    ) -> Result<Bc1Subresource<'a>, TextureError> {
        if layer >= self.layers || mip >= self.mips {
            return Err(TextureError::OutOfBounds);
        }
        // Construction proved that the complete array fits in the source span.
        let offset = (layer * self.mips as usize + mip as usize) * self.slice_pitch;
        let width = (self.width >> mip).max(1);
        let height = (self.height >> mip).max(1);
        let rows = height.div_ceil(4);
        Ok(Bc1Subresource {
            bytes: &bytes[offset..offset + rows * self.row_pitch],
            width,
            height,
            row_pitch: self.row_pitch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESC: Bc1UploadDesc = Bc1UploadDesc {
        width: 5,
        height: 5,
        mips: 2,
        layers: 2,
        format: 1,
        usage: 1,
        row_pitch: 24,
        slice_pitch: 56,
    };

    #[test]
    fn selects_mips_and_layers_with_partial_blocks_and_padding() {
        let mut bytes = [0; 224];
        bytes[144..152].fill(0x9a); // Layer 1, mip 0, row 1, block 1.
        bytes[168..176].copy_from_slice(&[0, 0, 255, 255, 255, 255, 255, 255]);
        let upload = Bc1Upload::new(&bytes, DESC).unwrap();
        let base = upload.subresource(1, 0).unwrap();
        assert_eq!((base.width, base.height, base.row_pitch), (5, 5, 24));
        assert_eq!(base.bytes.len(), 48);
        assert_eq!(base.bytes[32..40], [0x9a; 8]);
        let small = upload.subresource(1, 1).unwrap();
        assert_eq!((small.width, small.height, small.bytes.len()), (2, 2, 24));
        assert_eq!(
            crate::decode_bc1(small.bytes[..8].try_into().unwrap()),
            [[0; 4]; 16]
        );
        assert_eq!(upload.subresource(0, 1).unwrap().bytes, [0; 24]);
        assert_eq!(
            upload.subresource(0, 2).unwrap_err(),
            TextureError::OutOfBounds
        );
        assert_eq!(
            upload.subresource(2, 0).unwrap_err(),
            TextureError::OutOfBounds
        );
    }

    #[test]
    fn rejects_invalid_uploads_before_indexing() {
        let bytes = [0; 224];
        for desc in [
            Bc1UploadDesc { format: 2, ..DESC },
            Bc1UploadDesc { usage: 2, ..DESC },
            Bc1UploadDesc { mips: 4, ..DESC },
            Bc1UploadDesc { layers: 0, ..DESC },
            Bc1UploadDesc {
                row_pitch: 8,
                ..DESC
            },
            Bc1UploadDesc {
                row_pitch: 17,
                ..DESC
            },
            Bc1UploadDesc {
                slice_pitch: 40,
                ..DESC
            },
            Bc1UploadDesc {
                slice_pitch: 49,
                ..DESC
            },
        ] {
            assert_eq!(
                Bc1Upload::new(&bytes, desc).unwrap_err(),
                TextureError::InvalidLayout
            );
        }
        assert_eq!(
            Bc1Upload::new(&bytes[..223], DESC).unwrap_err(),
            TextureError::InvalidByteLength
        );
        for desc in [
            Bc1UploadDesc {
                width: usize::MAX,
                ..DESC
            },
            Bc1UploadDesc {
                row_pitch: usize::MAX - 7,
                ..DESC
            },
            Bc1UploadDesc {
                layers: usize::MAX,
                ..DESC
            },
        ] {
            assert_eq!(
                Bc1Upload::new(&bytes, desc).unwrap_err(),
                TextureError::SizeOverflow
            );
        }
    }

    const ALPHA_DESC: BcAlphaUploadDesc = BcAlphaUploadDesc {
        width: 5,
        height: 5,
        mips: 2,
        layers: 2,
        format: BcAlphaFormat::Bc3,
        row_pitch: 48,
        slice_pitch: 112,
    };

    #[test]
    fn selects_bc_alpha_subresources_in_sixteen_byte_blocks() {
        let mut bytes = [0; 448];
        // Layer 1 starts at 224; its base row 1, block 1 is at 224 + 48 + 16.
        bytes[288..304].fill(0x5c);
        bytes[336] = 0xa7; // Layer 1, mip 1, block 0.
        let upload = BcAlphaUpload::new(&bytes, ALPHA_DESC).unwrap();
        assert_eq!(upload.format(), BcAlphaFormat::Bc3);
        let base = upload.subresource(1, 0).unwrap();
        assert_eq!((base.width, base.height, base.row_pitch), (5, 5, 48));
        assert_eq!(base.bytes.len(), 96);
        assert_eq!(base.bytes[64..80], [0x5c; 16]);
        let small = upload.subresource(1, 1).unwrap();
        assert_eq!((small.width, small.height, small.bytes.len()), (2, 2, 48));
        assert_eq!(small.bytes[0], 0xa7);
        assert_eq!(
            upload.subresource(2, 0).unwrap_err(),
            TextureError::OutOfBounds
        );
        assert_eq!(
            upload.subresource(0, 2).unwrap_err(),
            TextureError::OutOfBounds
        );
    }

    #[test]
    fn rejects_bc1_pitches_for_sixteen_byte_blocks() {
        let bytes = [0; 448];
        // Each pitch is a valid BC1 multiple of 8 but not of 16, or is
        // shorter than two 16-byte blocks.
        for desc in [
            BcAlphaUploadDesc {
                row_pitch: 24,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                row_pitch: 40,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                slice_pitch: 104,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                slice_pitch: 80,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                mips: 4,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                width: 0,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                height: 0,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                layers: 0,
                ..ALPHA_DESC
            },
            BcAlphaUploadDesc {
                mips: 0,
                ..ALPHA_DESC
            },
        ] {
            assert_eq!(
                BcAlphaUpload::new(&bytes, desc).unwrap_err(),
                TextureError::InvalidLayout
            );
        }
        // Short, oversized, and one block too few or too many for the layout.
        let long = [0; 449];
        for bytes in [&bytes[..447], &long[..], &bytes[..432], &[0; 464][..]] {
            assert_eq!(
                BcAlphaUpload::new(bytes, ALPHA_DESC).unwrap_err(),
                TextureError::InvalidByteLength
            );
        }
        assert_eq!(
            BcAlphaUpload::new(
                &bytes,
                BcAlphaUploadDesc {
                    width: usize::MAX,
                    ..ALPHA_DESC
                }
            )
            .unwrap_err(),
            TextureError::SizeOverflow
        );
    }

    #[test]
    fn sizes_bc4_and_bc5_pitches_by_their_block() {
        let desc = BcChannelUploadDesc {
            width: 5,
            height: 5,
            mips: 2,
            layers: 2,
            format: BcChannelFormat::Bc4Unorm,
            row_pitch: 24,
            slice_pitch: 56,
        };
        let mut bytes = [0; 224];
        bytes[144] = 0x3c; // Layer 1, mip 0, row 1, block 1.
        let upload = BcChannelUpload::new(&bytes, desc).unwrap();
        assert_eq!(upload.format(), BcChannelFormat::Bc4Unorm);
        let base = upload.subresource(1, 0).unwrap();
        assert_eq!((base.bytes.len(), base.bytes[32]), (48, 0x3c));
        // A BC4 pitch holds only half a BC5 row, and 24 is not a 16-byte multiple.
        for format in [BcChannelFormat::Bc5Unorm, BcChannelFormat::Bc5Snorm] {
            assert_eq!(
                BcChannelUpload::new(&bytes, BcChannelUploadDesc { format, ..desc }).unwrap_err(),
                TextureError::InvalidLayout
            );
        }
        let bc5 = BcChannelUploadDesc {
            format: BcChannelFormat::Bc5Snorm,
            row_pitch: 48,
            slice_pitch: 112,
            ..desc
        };
        let bytes = [0; 448];
        assert_eq!(
            BcChannelUpload::new(&bytes, bc5)
                .unwrap()
                .subresource(1, 1)
                .unwrap()
                .bytes
                .len(),
            48
        );
        // Short, oversized, and one block too few or too many for the layout.
        for len in [447, 449, 432, 464] {
            assert_eq!(
                BcChannelUpload::new(&vec![0; len], bc5).unwrap_err(),
                TextureError::InvalidByteLength
            );
        }
        for desc in [
            BcChannelUploadDesc { width: 0, ..bc5 },
            BcChannelUploadDesc { height: 0, ..bc5 },
            BcChannelUploadDesc { layers: 0, ..bc5 },
            BcChannelUploadDesc { mips: 0, ..bc5 },
            BcChannelUploadDesc { mips: 4, ..bc5 },
        ] {
            assert_eq!(
                BcChannelUpload::new(&bytes, desc).unwrap_err(),
                TextureError::InvalidLayout
            );
        }
    }

    const BPTC_DESC: BptcUploadDesc = BptcUploadDesc {
        width: 5,
        height: 5,
        mips: 2,
        layers: 2,
        format: BptcFormat::Bc7Unorm,
        row_pitch: 48,
        slice_pitch: 112,
    };

    #[test]
    fn selects_bptc_subresources_in_sixteen_byte_blocks() {
        let mut bytes = [0; 448];
        // Layer 1 starts at 2 * 112; its mip 0 row 1 block 1 is 48 + 16 further.
        bytes[288..304].fill(0x40);
        // Layer 0, mip 1 starts one slice in.
        bytes[112] = 0x03;
        for format in [
            BptcFormat::Bc6hUfloat,
            BptcFormat::Bc6hSfloat,
            BptcFormat::Bc7Unorm,
        ] {
            let upload = BptcUpload::new(
                &bytes,
                BptcUploadDesc {
                    format,
                    ..BPTC_DESC
                },
            )
            .unwrap();
            assert_eq!(upload.format(), format);
            let base = upload.subresource(1, 0).unwrap();
            assert_eq!((base.width, base.height, base.row_pitch), (5, 5, 48));
            assert_eq!(base.bytes.len(), 96);
            assert_eq!(base.bytes[64..80], [0x40; 16]);
            let small = upload.subresource(0, 1).unwrap();
            assert_eq!((small.width, small.height, small.bytes.len()), (2, 2, 48));
            assert_eq!(small.bytes[0], 0x03);
            for (layer, mip) in [(2, 0), (0, 2)] {
                assert_eq!(
                    upload.subresource(layer, mip).unwrap_err(),
                    TextureError::OutOfBounds
                );
            }
        }
    }

    #[test]
    fn rejects_invalid_bptc_uploads_before_indexing() {
        let bytes = [0; 448];
        for desc in [
            // An eight-byte pitch holds half a block; 40 is not a multiple of 16.
            BptcUploadDesc {
                row_pitch: 24,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                row_pitch: 40,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                slice_pitch: 80,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                slice_pitch: 104,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                width: 0,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                height: 0,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                layers: 0,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                mips: 0,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                mips: 4,
                ..BPTC_DESC
            },
        ] {
            assert_eq!(
                BptcUpload::new(&bytes, desc).unwrap_err(),
                TextureError::InvalidLayout
            );
        }
        // Short, oversized, and one block too few or too many for the layout.
        for len in [447, 449, 432, 464] {
            assert_eq!(
                BptcUpload::new(&vec![0; len], BPTC_DESC).unwrap_err(),
                TextureError::InvalidByteLength
            );
        }
        for desc in [
            BptcUploadDesc {
                width: usize::MAX,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                row_pitch: usize::MAX - 15,
                ..BPTC_DESC
            },
            BptcUploadDesc {
                layers: usize::MAX,
                ..BPTC_DESC
            },
        ] {
            assert_eq!(
                BptcUpload::new(&bytes, desc).unwrap_err(),
                TextureError::SizeOverflow
            );
        }
    }
}
