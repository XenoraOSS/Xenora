// SPDX-License-Identifier: GPL-3.0-or-later

//! Reusable texture sampler descriptor and validation.
//!
//! Provides typed definitions and validation for texture samplers independent of
//! Metal or GPU-specific runtime types.

use std::fmt;

/// Filter mode for texture sampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    /// Nearest-neighbor point sampling.
    Nearest,
}

/// Address/wrap mode for texture sampling along a coordinate axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressMode {
    /// Clamp coordinates to the edge of the texture (`[0.0, 1.0]`).
    ClampToEdge,
}

/// Errors returned when constructing or validating a sampler descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplerError {
    /// The requested filter mode is not supported.
    UnsupportedFilterMode,
    /// The requested addressing mode is not supported.
    UnsupportedAddressMode,
    /// Minimum LOD is greater than maximum LOD.
    InvalidLodRange,
    /// Explicit mip selection or bounds exceed texture metadata.
    MipOutOfBounds,
    /// Texture metadata dimensions are invalid (e.g. zero width or height).
    InvalidTextureDimensions,
    /// Texture mip count is zero.
    InvalidTextureMipCount,
}

impl fmt::Display for SamplerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFilterMode => write!(f, "unsupported sampler filter mode"),
            Self::UnsupportedAddressMode => write!(f, "unsupported sampler address mode"),
            Self::InvalidLodRange => write!(f, "min_lod must not be greater than max_lod"),
            Self::MipOutOfBounds => write!(f, "selected mip or lod exceeds texture mip count"),
            Self::InvalidTextureDimensions => write!(f, "invalid texture dimensions"),
            Self::InvalidTextureMipCount => write!(f, "texture mip count must be at least 1"),
        }
    }
}

impl std::error::Error for SamplerError {}

/// Minimal metadata describing a target texture for sampler validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureMetadata {
    pub width: usize,
    pub height: usize,
    pub mips: u32,
}

impl TextureMetadata {
    pub fn new(width: usize, height: usize, mips: u32) -> Result<Self, SamplerError> {
        if width == 0 || height == 0 {
            return Err(SamplerError::InvalidTextureDimensions);
        }
        if mips == 0 {
            return Err(SamplerError::InvalidTextureMipCount);
        }
        let max_mips = usize::BITS - width.max(height).leading_zeros();
        if mips > max_mips {
            return Err(SamplerError::MipOutOfBounds);
        }
        Ok(Self {
            width,
            height,
            mips,
        })
    }
}

/// Reusable, typed sampler descriptor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerDesc {
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub mip_filter: FilterMode,
    pub address_u: AddressMode,
    pub address_v: AddressMode,
    pub min_lod: f32,
    pub max_lod: f32,
    pub explicit_mip: Option<u32>,
}

impl Default for SamplerDesc {
    fn default() -> Self {
        Self::nearest_clamp()
    }
}

impl SamplerDesc {
    /// Create a standard nearest-neighbor, clamp-to-edge sampler.
    pub const fn nearest_clamp() -> Self {
        Self {
            min_filter: FilterMode::Nearest,
            mag_filter: FilterMode::Nearest,
            mip_filter: FilterMode::Nearest,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            min_lod: 0.0,
            max_lod: f32::MAX,
            explicit_mip: None,
        }
    }

    /// Select one mip, overriding the validated LOD range when sampling.
    /// Texture binding still validates that this level exists.
    pub const fn with_explicit_mip(mut self, mip: u32) -> Self {
        self.explicit_mip = Some(mip);
        self
    }

    /// Builder for setting LOD range.
    pub const fn with_lod_range(mut self, min_lod: f32, max_lod: f32) -> Self {
        self.min_lod = min_lod;
        self.max_lod = max_lod;
        self
    }

    /// Validates the sampler descriptor on its own.
    pub fn validate(&self) -> Result<(), SamplerError> {
        if !self.min_lod.is_finite()
            || !self.max_lod.is_finite()
            || self.min_lod < 0.0
            || self.min_lod > self.max_lod
        {
            return Err(SamplerError::InvalidLodRange);
        }
        Ok(())
    }

    /// Validates the sampler descriptor against specific texture metadata.
    pub fn validate_against_texture(&self, texture: &TextureMetadata) -> Result<(), SamplerError> {
        self.validate()?;

        if texture.width == 0 || texture.height == 0 {
            return Err(SamplerError::InvalidTextureDimensions);
        }
        if texture.mips == 0 {
            return Err(SamplerError::InvalidTextureMipCount);
        }

        if let Some(explicit_mip) = self.explicit_mip
            && explicit_mip >= texture.mips
        {
            return Err(SamplerError::MipOutOfBounds);
        }

        if self.min_lod >= texture.mips as f32 {
            return Err(SamplerError::MipOutOfBounds);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_nearest_clamp_descriptor() {
        let sampler = SamplerDesc::nearest_clamp();
        assert_eq!(sampler.validate(), Ok(()));
        assert_eq!(sampler.min_filter, FilterMode::Nearest);
        assert_eq!(sampler.mag_filter, FilterMode::Nearest);
        assert_eq!(sampler.address_u, AddressMode::ClampToEdge);
        assert_eq!(sampler.address_v, AddressMode::ClampToEdge);
    }

    #[test]
    fn test_valid_mip_selection() {
        let meta = TextureMetadata::new(16, 16, 4).unwrap();
        let sampler = SamplerDesc::nearest_clamp().with_explicit_mip(2);
        assert_eq!(sampler.validate_against_texture(&meta), Ok(()));

        let sampler_zero = SamplerDesc::nearest_clamp().with_explicit_mip(0);
        assert_eq!(sampler_zero.validate_against_texture(&meta), Ok(()));

        let sampler_last = SamplerDesc::nearest_clamp().with_explicit_mip(3);
        assert_eq!(sampler_last.validate_against_texture(&meta), Ok(()));
    }

    #[test]
    fn test_invalid_mip_selection() {
        let meta = TextureMetadata::new(16, 16, 4).unwrap();
        let sampler = SamplerDesc::nearest_clamp().with_explicit_mip(4);
        assert_eq!(
            sampler.validate_against_texture(&meta),
            Err(SamplerError::MipOutOfBounds)
        );

        let sampler_large = SamplerDesc::nearest_clamp().with_explicit_mip(100);
        assert_eq!(
            sampler_large.validate_against_texture(&meta),
            Err(SamplerError::MipOutOfBounds)
        );
    }

    #[test]
    fn test_invalid_dimensions() {
        assert_eq!(
            TextureMetadata::new(0, 16, 1),
            Err(SamplerError::InvalidTextureDimensions)
        );
        assert_eq!(
            TextureMetadata::new(16, 0, 1),
            Err(SamplerError::InvalidTextureDimensions)
        );
        assert_eq!(
            TextureMetadata::new(16, 16, 0),
            Err(SamplerError::InvalidTextureMipCount)
        );
        // 16x16 has max 5 mips (16, 8, 4, 2, 1)
        assert_eq!(
            TextureMetadata::new(16, 16, 6),
            Err(SamplerError::MipOutOfBounds)
        );
    }

    #[test]
    fn test_malformed_lod_range() {
        let invalid_order = SamplerDesc::nearest_clamp().with_lod_range(3.0, 1.0);
        assert_eq!(invalid_order.validate(), Err(SamplerError::InvalidLodRange));

        let negative_lod = SamplerDesc::nearest_clamp().with_lod_range(-1.0, 2.0);
        assert_eq!(negative_lod.validate(), Err(SamplerError::InvalidLodRange));

        let nan_lod = SamplerDesc::nearest_clamp().with_lod_range(f32::NAN, 2.0);
        assert_eq!(nan_lod.validate(), Err(SamplerError::InvalidLodRange));
    }

    #[test]
    fn test_lod_exceeds_texture_mips() {
        let meta = TextureMetadata::new(8, 8, 3).unwrap();
        let high_lod = SamplerDesc::nearest_clamp().with_lod_range(3.0, 5.0);
        assert_eq!(
            high_lod.validate_against_texture(&meta),
            Err(SamplerError::MipOutOfBounds)
        );
    }

    #[test]
    fn test_non_power_of_two_metadata() {
        // 3x3 has max 2 mips (floor(log2(3)) + 1 = 2)
        let meta = TextureMetadata::new(3, 3, 2).unwrap();
        assert_eq!(meta.mips, 2);
        assert_eq!(
            TextureMetadata::new(3, 3, 3),
            Err(SamplerError::MipOutOfBounds)
        );
    }
}
