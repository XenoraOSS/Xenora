// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Apple Metal sampler state abstraction.
//!
//! Provides safe creation and ownership of native Metal `MTLSamplerState` objects
//! configured from validated [`SamplerDesc`].

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLSamplerAddressMode, MTLSamplerDescriptor, MTLSamplerMinMagFilter,
    MTLSamplerMipFilter, MTLSamplerState,
};
use xenora_texture::{AddressMode, FilterMode, SamplerDesc, SamplerError};

use crate::MetalError;

/// Safe wrapper owning a native Metal sampler state object.
#[derive(Debug, Clone)]
pub struct MetalSampler {
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    desc: SamplerDesc,
}

impl MetalSampler {
    /// Create a native Metal sampler from a [`SamplerDesc`] on the provided Metal device.
    ///
    /// # Errors
    ///
    /// Returns [`MetalError`] if descriptor validation fails or if Metal fails
    /// to create the sampler state.
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        desc: SamplerDesc,
    ) -> Result<Self, MetalError> {
        desc.validate()
            .map_err(|err: SamplerError| MetalError::new("sampler validation", err.to_string()))?;

        autoreleasepool(|_| {
            let descriptor = MTLSamplerDescriptor::new();

            let min_filter = match desc.min_filter {
                FilterMode::Nearest => MTLSamplerMinMagFilter::Nearest,
            };
            let mag_filter = match desc.mag_filter {
                FilterMode::Nearest => MTLSamplerMinMagFilter::Nearest,
            };
            let mip_filter = match desc.mip_filter {
                FilterMode::Nearest => MTLSamplerMipFilter::Nearest,
            };

            let s_address = match desc.address_u {
                AddressMode::ClampToEdge => MTLSamplerAddressMode::ClampToEdge,
            };
            let t_address = match desc.address_v {
                AddressMode::ClampToEdge => MTLSamplerAddressMode::ClampToEdge,
            };

            descriptor.setMinFilter(min_filter);
            descriptor.setMagFilter(mag_filter);
            descriptor.setMipFilter(mip_filter);
            descriptor.setSAddressMode(s_address);
            descriptor.setTAddressMode(t_address);
            descriptor.setRAddressMode(MTLSamplerAddressMode::ClampToEdge);

            // Normalized coordinates: normalized UV sampling in [0.0, 1.0].
            descriptor.setNormalizedCoordinates(true);

            // Explicit selection pins the sampler to that mip, including for
            // translated sample_lz instructions that request level zero.
            let (min_lod, max_lod) = desc
                .explicit_mip
                .map(|mip| (mip as f32, mip as f32))
                .unwrap_or((desc.min_lod, desc.max_lod));
            descriptor.setLodMinClamp(min_lod);
            descriptor.setLodMaxClamp(max_lod);

            let sampler = device
                .newSamplerStateWithDescriptor(&descriptor)
                .ok_or_else(|| {
                    MetalError::new(
                        "newSamplerStateWithDescriptor",
                        "failed to create Metal sampler state",
                    )
                })?;

            Ok(Self { sampler, desc })
        })
    }

    /// Access the underlying native Metal sampler state protocol object.
    #[inline]
    pub fn raw(&self) -> &ProtocolObject<dyn MTLSamplerState> {
        &self.sampler
    }

    pub(crate) fn device_id(&self) -> u64 {
        self.sampler.device().registryID()
    }

    pub(crate) fn same_object(&self, other: &Self) -> bool {
        std::ptr::eq(self.raw(), other.raw())
    }

    /// Access the source sampler descriptor.
    #[inline]
    pub fn desc(&self) -> &SamplerDesc {
        &self.desc
    }
}
