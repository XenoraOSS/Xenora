// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Apple Metal RGBA8 texture upload and pipeline binding foundation.
//!
//! Provides validated RGBA8 2D texture allocation, mip upload, and binding definitions
//! for compute kernels and future translated pixel shader stages.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLCommandQueue, MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion, MTLResource,
    MTLResourceOptions, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use xenora_texture::{SamplerError, TextureMetadata};

use crate::MetalError;
use crate::sampler::MetalSampler;

/// Bytes per RGBA8 texel.
pub const RGBA8_TEXEL_BYTES: usize = 4;

/// Upper limit on total pixels per texture to prevent bounded resource overflow.
pub const MAX_TEXTURE_PIXELS: usize = 16384 * 16384;

/// Subresource mip level data descriptor for RGBA8 upload.
#[derive(Debug, Clone, Copy)]
pub struct Rgba8MipLevel<'a> {
    pub level: u32,
    pub width: usize,
    pub height: usize,
    pub row_pitch: usize,
    pub bytes: &'a [u8],
}

impl<'a> Rgba8MipLevel<'a> {
    /// Create a tightly packed mip level descriptor.
    pub fn new_packed(level: u32, width: usize, height: usize, bytes: &'a [u8]) -> Self {
        let row_pitch = width * RGBA8_TEXEL_BYTES;
        Self {
            level,
            width,
            height,
            row_pitch,
            bytes,
        }
    }
}

/// Description of an RGBA8 2D texture to allocate on Metal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba8TextureDesc {
    pub width: usize,
    pub height: usize,
    pub mips: u32,
}

impl Rgba8TextureDesc {
    pub fn new(width: usize, height: usize, mips: u32) -> Result<Self, MetalError> {
        let meta = TextureMetadata::new(width, height, mips).map_err(|err: SamplerError| {
            MetalError::new("texture metadata validation", err.to_string())
        })?;

        let total_pixels = width
            .checked_mul(height)
            .ok_or_else(|| MetalError::new("texture dimensions", "pixel count overflow"))?;

        if total_pixels > MAX_TEXTURE_PIXELS {
            return Err(MetalError::new(
                "texture bounds",
                "texture exceeds maximum allowable pixels",
            ));
        }

        Ok(Self {
            width: meta.width,
            height: meta.height,
            mips: meta.mips,
        })
    }

    /// Compute expected dimensions for a given mip level.
    pub fn mip_extent(&self, level: u32) -> Result<(usize, usize), MetalError> {
        if level >= self.mips {
            return Err(MetalError::new(
                "mip level bounds",
                "mip level index is out of bounds",
            ));
        }
        let mip_w = (self.width >> level).max(1);
        let mip_h = (self.height >> level).max(1);
        Ok((mip_w, mip_h))
    }
}

/// Safe wrapper owning a native Metal RGBA8 2D texture.
#[derive(Debug, Clone)]
pub struct MetalRgba8Texture {
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    desc: Rgba8TextureDesc,
    /// Whether `replaceRegion` can write the texture directly.
    cpu_writable: bool,
}

impl MetalRgba8Texture {
    /// Allocate an empty native Metal RGBA8 2D texture with storage mode Shared and ShaderRead usage.
    pub fn allocate(
        device: &ProtocolObject<dyn MTLDevice>,
        desc: Rgba8TextureDesc,
    ) -> Result<Self, MetalError> {
        let desc = Rgba8TextureDesc::new(desc.width, desc.height, desc.mips)?;
        Self::allocate_with_storage(device, desc, MTLStorageMode::Shared)
    }

    fn allocate_with_storage(
        device: &ProtocolObject<dyn MTLDevice>,
        desc: Rgba8TextureDesc,
        storage: MTLStorageMode,
    ) -> Result<Self, MetalError> {
        let options = if storage == MTLStorageMode::Private {
            MTLResourceOptions::StorageModePrivate
        } else {
            MTLResourceOptions::StorageModeShared
        };
        autoreleasepool(|_| {
            // SAFETY: Validated non-zero dimensions within MAX_TEXTURE_PIXELS and RGBA8Unorm format.
            let descriptor = unsafe {
                MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                    MTLPixelFormat::RGBA8Unorm,
                    desc.width,
                    desc.height,
                    desc.mips > 1,
                )
            };
            if desc.mips > 1 {
                // SAFETY: desc.mips is validated by TextureMetadata to not exceed max_mip_levels.
                unsafe {
                    descriptor.setMipmapLevelCount(desc.mips as usize);
                }
            }
            descriptor.setStorageMode(storage);
            descriptor.setResourceOptions(options);
            descriptor.setUsage(MTLTextureUsage::ShaderRead);

            let texture = device
                .newTextureWithDescriptor(&descriptor)
                .ok_or_else(|| {
                    MetalError::new(
                        "newTextureWithDescriptor",
                        "failed to allocate RGBA8 Metal texture",
                    )
                })?;

            Ok(Self {
                texture,
                desc,
                cpu_writable: storage == MTLStorageMode::Shared,
            })
        })
    }

    /// Upload a single validated mip level into this texture.
    ///
    /// Only Shared-storage textures from `allocate` or `upload` accept CPU writes.
    pub fn upload_mip(&self, mip: &Rgba8MipLevel<'_>) -> Result<(), MetalError> {
        if !self.cpu_writable {
            return Err(MetalError::new(
                "texture storage",
                "Private-storage textures require staged uploads",
            ));
        }
        validate_mip(&self.desc, mip)?;

        autoreleasepool(|_| {
            let region = MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: mip.width,
                    height: mip.height,
                    depth: 1,
                },
            };

            let ptr = NonNull::new(mip.bytes.as_ptr() as *mut c_void).ok_or_else(|| {
                MetalError::new("pointer", "null data pointer for texture upload")
            })?;

            // SAFETY: The bounds check verifies that mip.bytes has at least
            // `mip.row_pitch * mip.height` bytes, the region size matches mip dimensions,
            // the mip level is within the texture's allocated mip count, and the pointer is valid.
            unsafe {
                self.texture
                    .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                        region,
                        mip.level as usize,
                        ptr,
                        mip.row_pitch,
                    );
            }

            Ok(())
        })
    }

    /// Allocate and populate an RGBA8 2D texture from a set of mip levels.
    pub fn upload(
        device: &ProtocolObject<dyn MTLDevice>,
        desc: Rgba8TextureDesc,
        mips: &[Rgba8MipLevel<'_>],
    ) -> Result<Self, MetalError> {
        let texture = Self::allocate(device, desc)?;
        for mip in mips {
            texture.upload_mip(mip)?;
        }
        Ok(texture)
    }

    /// Allocate a Private-storage RGBA8 texture and fill every mip level through
    /// a reused Shared staging buffer that never exceeds `budget`.
    ///
    /// The plan from [`plan_staged_upload`] is built, and every input checked,
    /// before any Metal object is created. Each submission copies its rows into
    /// the staging buffer, blits them into the texture and waits until the
    /// command buffer has completed. Only a `Completed` status lets the buffer be
    /// reused; any other status is returned as an error.
    pub fn upload_staged(
        device: &ProtocolObject<dyn MTLDevice>,
        queue: &ProtocolObject<dyn MTLCommandQueue>,
        desc: Rgba8TextureDesc,
        mips: &[Rgba8MipLevel<'_>],
        budget: StagingBudget,
    ) -> Result<(Self, StagedUploadReport), MetalError> {
        let plan = plan_staged_upload(&desc, mips, budget)?;
        Self::check_staging_device(device, queue)?;
        let texture = Self::allocate_with_storage(device, desc, MTLStorageMode::Private)?;
        let report = texture.execute_staged(device, queue, mips, &plan)?;
        Ok((texture, report))
    }

    /// Replace only the supplied mip levels in an existing RGBA8 texture.
    ///
    /// Distinct input levels are validated together before any write. Copies
    /// may share a blit and use the same staging buffer across submissions.
    /// This returns after the last command completes. Omitted levels keep
    /// their contents. If Metal fails after an earlier command completed, its
    /// writes are retained. The caller must order any accesses through aliased
    /// handles or other queues around this update.
    pub fn update_staged(
        &mut self,
        queue: &ProtocolObject<dyn MTLCommandQueue>,
        mips: &[Rgba8MipLevel<'_>],
        budget: StagingBudget,
    ) -> Result<StagedUploadReport, MetalError> {
        let plan = plan_staged(&self.desc, mips, budget, false)?;
        let device = self.texture.device();
        Self::check_staging_device(&device, queue)?;
        self.execute_staged(&device, queue, mips, &plan)
    }

    fn check_staging_device(
        device: &ProtocolObject<dyn MTLDevice>,
        queue: &ProtocolObject<dyn MTLCommandQueue>,
    ) -> Result<(), MetalError> {
        if !std::ptr::eq(device, &*queue.device()) {
            return Err(MetalError::new(
                "staged upload",
                "the queue uses a different Metal device",
            ));
        }
        Ok(())
    }

    fn execute_staged(
        &self,
        device: &ProtocolObject<dyn MTLDevice>,
        queue: &ProtocolObject<dyn MTLCommandQueue>,
        mips: &[Rgba8MipLevel<'_>],
        plan: &[StagedSubmission],
    ) -> Result<StagedUploadReport, MetalError> {
        let staging_len = plan.iter().map(|s| s.staging_bytes).max().unwrap_or(0);
        let staging = device
            .newBufferWithLength_options(staging_len, MTLResourceOptions::StorageModeShared)
            .ok_or_else(|| MetalError::new("newBufferWithLength", "staging allocation failed"))?;
        let mut copied = 0;
        for submission in plan {
            for copy in &submission.copies {
                let mip = mips
                    .iter()
                    .find(|mip| mip.level == copy.level)
                    .ok_or_else(|| MetalError::new("staged upload", "planned level missing"))?;
                let tight = mip.width * RGBA8_TEXEL_BYTES;
                for row in 0..copy.rows {
                    let src = &mip.bytes[(copy.first_row + row) * mip.row_pitch..][..tight];
                    let offset = copy.staging_offset + row * tight;
                    // SAFETY: The plan keeps offset + tight within staging_len, the
                    // buffer is Shared and CPU-visible, and the previous submission
                    // has completed, so the GPU no longer reads these bytes.
                    unsafe {
                        let dst = staging.contents().cast::<u8>().as_ptr().add(offset);
                        std::ptr::copy_nonoverlapping(src.as_ptr(), dst, tight);
                    }
                }
                copied += copy.rows * tight;
            }
            autoreleasepool(|_| self.encode_staged(queue, &staging, submission))?;
        }
        Ok(StagedUploadReport {
            submissions: plan.len(),
            peak_staging_bytes: staging_len,
            bytes_copied: copied,
        })
    }

    fn encode_staged(
        &self,
        queue: &ProtocolObject<dyn MTLCommandQueue>,
        staging: &ProtocolObject<dyn MTLBuffer>,
        submission: &StagedSubmission,
    ) -> Result<(), MetalError> {
        let command = queue
            .commandBuffer()
            .ok_or_else(|| MetalError::new("commandBuffer", "command buffer creation failed"))?;
        let blit = command
            .blitCommandEncoder()
            .ok_or_else(|| MetalError::new("blitCommandEncoder", "blit encoder creation failed"))?;
        for copy in &submission.copies {
            let (width, _) = self.desc.mip_extent(copy.level)?;
            let pitch = width * RGBA8_TEXEL_BYTES;
            // SAFETY: The plan validated the level, the row range within the mip and
            // the staging range within the buffer. The command buffer retains both
            // resources, and the caller waits for completion before reusing staging.
            unsafe {
                blit.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                    staging,
                    copy.staging_offset,
                    pitch,
                    pitch * copy.rows,
                    MTLSize { width, height: copy.rows, depth: 1 },
                    &self.texture,
                    0,
                    copy.level as usize,
                    MTLOrigin { x: 0, y: copy.first_row, z: 0 },
                );
            }
        }
        blit.endEncoding();
        command.commit();
        command.waitUntilCompleted();
        check_completed(&command)
    }

    /// Copy one mip level back into tightly packed RGBA8 bytes through a blit.
    ///
    /// Works for Shared and Private textures and shares no code with the upload
    /// paths, so tests can compare its output with independently built pixels.
    pub fn read_back(
        &self,
        device: &ProtocolObject<dyn MTLDevice>,
        queue: &ProtocolObject<dyn MTLCommandQueue>,
        level: u32,
    ) -> Result<Vec<u8>, MetalError> {
        let (width, height) = self.desc.mip_extent(level)?;
        let pitch = width * RGBA8_TEXEL_BYTES;
        let len = pitch * height;
        autoreleasepool(|_| {
            let buffer = device
                .newBufferWithLength_options(len, MTLResourceOptions::StorageModeShared)
                .ok_or_else(|| {
                    MetalError::new("newBufferWithLength", "readback allocation failed")
                })?;
            let command = queue.commandBuffer().ok_or_else(|| {
                MetalError::new("commandBuffer", "command buffer creation failed")
            })?;
            let blit = command.blitCommandEncoder().ok_or_else(|| {
                MetalError::new("blitCommandEncoder", "blit encoder creation failed")
            })?;
            // SAFETY: The level is in range, the source region is the whole mip and
            // the destination buffer holds exactly pitch * height bytes.
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &self.texture,
                    0,
                    level as usize,
                    MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width, height, depth: 1 },
                    &buffer,
                    0,
                    pitch,
                    len,
                );
            }
            blit.endEncoding();
            command.commit();
            command.waitUntilCompleted();
            check_completed(&command)?;
            // SAFETY: The command buffer completed, so the Shared buffer holds len
            // initialized bytes and the GPU no longer writes it.
            let bytes =
                unsafe { std::slice::from_raw_parts(buffer.contents().cast::<u8>().as_ptr(), len) };
            Ok(bytes.to_vec())
        })
    }

    /// Access the underlying native Metal texture protocol object.
    #[inline]
    pub fn raw(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.texture
    }

    /// Access the texture descriptor.
    #[inline]
    pub fn desc(&self) -> &Rgba8TextureDesc {
        &self.desc
    }
}

/// Upper bound on the staging bytes one staged upload holds at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingBudget {
    max_bytes: usize,
}

impl StagingBudget {
    /// A budget of `max_bytes`, which must be a non-zero multiple of the RGBA8
    /// texel size so every staged row starts on a texel boundary.
    pub fn new(max_bytes: usize) -> Result<Self, MetalError> {
        if max_bytes == 0 || !max_bytes.is_multiple_of(RGBA8_TEXEL_BYTES) {
            return Err(MetalError::new(
                "staging budget",
                format!("budget {max_bytes} must be a non-zero multiple of {RGBA8_TEXEL_BYTES}"),
            ));
        }
        Ok(Self { max_bytes })
    }

    /// The largest staging allocation this budget allows.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

/// One blit of whole rows of one mip level from the staging buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagedCopy {
    pub level: u32,
    pub first_row: usize,
    pub rows: usize,
    /// Byte offset of the first row in the staging buffer. Rows are packed tightly.
    pub staging_offset: usize,
}

/// The copies that share one staging fill and one command buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedSubmission {
    pub copies: Vec<StagedCopy>,
    /// Staging bytes this submission uses, never more than the budget.
    pub staging_bytes: usize,
}

/// What a staged upload did, measured from the executed plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagedUploadReport {
    /// Command buffers committed and completed.
    pub submissions: usize,
    /// Size of the one staging buffer, the most staging memory held at once.
    pub peak_staging_bytes: usize,
    /// Texel bytes copied into the texture.
    pub bytes_copied: usize,
}

/// Split a full RGBA8 upload into budget-sized submissions of whole rows.
///
/// Every level `0..desc.mips` must appear exactly once, because a Private
/// texture has no defined contents for a level nobody writes. Each mip is
/// checked like [`MetalRgba8Texture::upload_mip`] checks it. The budget must hold
/// at least one row of the widest level. Rows are packed in level order, and a
/// submission closes when the next row would exceed the budget.
pub fn plan_staged_upload(
    desc: &Rgba8TextureDesc,
    mips: &[Rgba8MipLevel<'_>],
    budget: StagingBudget,
) -> Result<Vec<StagedSubmission>, MetalError> {
    let desc = Rgba8TextureDesc::new(desc.width, desc.height, desc.mips)?;
    plan_staged(&desc, mips, budget, true)
}

fn plan_staged(
    desc: &Rgba8TextureDesc,
    mips: &[Rgba8MipLevel<'_>],
    budget: StagingBudget,
    fill_all: bool,
) -> Result<Vec<StagedSubmission>, MetalError> {
    if !fill_all && mips.is_empty() {
        return Err(MetalError::new("staged upload", "no mip levels to update"));
    }
    let mut by_level: Vec<Option<&Rgba8MipLevel<'_>>> = vec![None; desc.mips as usize];
    for mip in mips {
        validate_mip(desc, mip)?;
        let slot = &mut by_level[mip.level as usize];
        if slot.is_some() {
            return Err(MetalError::new(
                "staged upload",
                format!("mip level {} appears more than once", mip.level),
            ));
        }
        *slot = Some(mip);
    }
    let mut plan = Vec::new();
    let mut current = StagedSubmission {
        copies: Vec::new(),
        staging_bytes: 0,
    };
    for (level, mip) in by_level.iter().enumerate() {
        let Some(mip) = mip else {
            if fill_all {
                return Err(MetalError::new(
                    "staged upload",
                    format!("mip level {level} is missing"),
                ));
            }
            continue;
        };
        let pitch = mip.width * RGBA8_TEXEL_BYTES;
        if pitch > budget.max_bytes {
            return Err(MetalError::new(
                "staging budget",
                format!(
                    "budget {} is smaller than one {pitch}-byte row of mip level {level}",
                    budget.max_bytes
                ),
            ));
        }
        let mut row = 0;
        while row < mip.height {
            let fit = (budget.max_bytes - current.staging_bytes) / pitch;
            if fit == 0 {
                // Unreachable after the row check above, but an empty submission
                // that cannot take one row must never loop.
                if current.copies.is_empty() {
                    return Err(MetalError::new(
                        "staging budget",
                        "a row does not fit an empty staging buffer",
                    ));
                }
                plan.push(std::mem::replace(
                    &mut current,
                    StagedSubmission {
                        copies: Vec::new(),
                        staging_bytes: 0,
                    },
                ));
                continue;
            }
            let rows = fit.min(mip.height - row);
            current.copies.push(StagedCopy {
                level: mip.level,
                first_row: row,
                rows,
                staging_offset: current.staging_bytes,
            });
            current.staging_bytes += rows * pitch;
            row += rows;
        }
    }
    if !current.copies.is_empty() {
        plan.push(current);
    }
    Ok(plan)
}

fn check_completed(command: &ProtocolObject<dyn MTLCommandBuffer>) -> Result<(), MetalError> {
    let status = command.status();
    if status != MTLCommandBufferStatus::Completed {
        let detail = command.error().map_or_else(
            || format!("unexpected command status {status:?}"),
            |error| error.localizedDescription().to_string(),
        );
        return Err(MetalError::new("waitUntilCompleted", detail));
    }
    Ok(())
}

/// Check one mip level against the texture description and return its tight row pitch.
fn validate_mip(desc: &Rgba8TextureDesc, mip: &Rgba8MipLevel<'_>) -> Result<usize, MetalError> {
    let (expected_w, expected_h) = desc.mip_extent(mip.level)?;
    if mip.width != expected_w || mip.height != expected_h {
        return Err(MetalError::new(
            "mip dimensions",
            format!(
                "mip level {} expected {}x{}, got {}x{}",
                mip.level, expected_w, expected_h, mip.width, mip.height
            ),
        ));
    }

    let tight_pitch = mip
        .width
        .checked_mul(RGBA8_TEXEL_BYTES)
        .ok_or_else(|| MetalError::new("row pitch", "row pitch overflow"))?;
    if mip.row_pitch < tight_pitch {
        return Err(MetalError::new(
            "row pitch",
            format!(
                "row pitch {} smaller than tight pitch {}",
                mip.row_pitch, tight_pitch
            ),
        ));
    }

    let min_bytes = mip
        .row_pitch
        .checked_mul(mip.height)
        .ok_or_else(|| MetalError::new("mip bytes", "byte length calculation overflow"))?;
    if mip.bytes.len() < min_bytes {
        return Err(MetalError::new(
            "mip buffer bounds",
            format!(
                "buffer length {} is less than required {}",
                mip.bytes.len(),
                min_bytes
            ),
        ));
    }
    Ok(tight_pitch)
}

/// Resource binding indices and lifetime conventions for sampling pipelines.
///
/// In the Xenora fixture ABI and translated shader conventions:
/// - Metal textures are bound to `[[texture(slot)]]`.
/// - Metal samplers are bound to `[[sampler(slot)]]`.
///
/// Under future RDNA2-to-Metal shader translation:
/// - An image resource descriptor (`T#`) maps to a `texture2d` binding.
/// - A sampler descriptor (`S#`) maps to a `sampler` binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingSlot {
    pub index: u32,
}

impl BindingSlot {
    pub const fn new(index: u32) -> Self {
        Self { index }
    }
}

/// A validated texture-sampler pair ready for pipeline binding.
///
/// This struct holds strong references to the native Metal texture and sampler,
/// verifying that LOD ranges and explicit mip levels are consistent before
/// submitting draw or compute work.
#[derive(Debug, Clone)]
pub struct BoundSampledTexture {
    texture: MetalRgba8Texture,
    sampler: MetalSampler,
    texture_slot: BindingSlot,
    sampler_slot: BindingSlot,
}

impl BoundSampledTexture {
    /// Bind a texture and sampler together to specified shader slots.
    ///
    /// # Errors
    ///
    /// Returns [`MetalError`] if the sampler's explicit mip or LOD constraints
    /// are out of bounds for the texture's mip chain.
    pub fn bind(
        texture: MetalRgba8Texture,
        sampler: MetalSampler,
        texture_slot: BindingSlot,
        sampler_slot: BindingSlot,
    ) -> Result<Self, MetalError> {
        let meta = TextureMetadata::new(
            texture.desc().width,
            texture.desc().height,
            texture.desc().mips,
        )
        .map_err(|err: SamplerError| MetalError::new("texture metadata", err.to_string()))?;

        sampler
            .desc()
            .validate_against_texture(&meta)
            .map_err(|err: SamplerError| {
                MetalError::new("sampler texture compatibility", err.to_string())
            })?;

        Ok(Self {
            texture,
            sampler,
            texture_slot,
            sampler_slot,
        })
    }

    #[inline]
    pub fn texture(&self) -> &MetalRgba8Texture {
        &self.texture
    }

    #[inline]
    pub fn sampler(&self) -> &MetalSampler {
        &self.sampler
    }

    #[inline]
    pub fn texture_slot(&self) -> BindingSlot {
        self.texture_slot
    }

    #[inline]
    pub fn sampler_slot(&self) -> BindingSlot {
        self.sampler_slot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(level: u32, first_row: usize, rows: usize, staging_offset: usize) -> StagedCopy {
        StagedCopy {
            level,
            first_row,
            rows,
            staging_offset,
        }
    }

    fn levels<'a>(desc: &Rgba8TextureDesc, bytes: &'a [u8]) -> Vec<Rgba8MipLevel<'a>> {
        (0..desc.mips)
            .map(|level| {
                let (w, h) = desc.mip_extent(level).unwrap();
                Rgba8MipLevel::new_packed(level, w, h, &bytes[..w * h * RGBA8_TEXEL_BYTES])
            })
            .collect()
    }

    fn plan(desc: Rgba8TextureDesc, budget: usize) -> Vec<StagedSubmission> {
        let bytes = vec![0; desc.width * desc.height * RGBA8_TEXEL_BYTES];
        plan_staged_upload(
            &desc,
            &levels(&desc, &bytes),
            StagingBudget::new(budget).unwrap(),
        )
        .unwrap()
    }

    fn error(desc: Rgba8TextureDesc, mips: &[Rgba8MipLevel<'_>], budget: usize) -> String {
        plan_staged_upload(&desc, mips, StagingBudget::new(budget).unwrap())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn budget_must_be_a_non_zero_texel_multiple() {
        assert!(StagingBudget::new(0).is_err());
        assert!(StagingBudget::new(6).is_err());
        assert_eq!(StagingBudget::new(8).unwrap().max_bytes(), 8);
    }

    #[test]
    fn rows_split_into_budget_sized_submissions() {
        // 8 texels × 4 bytes = 32 bytes per row; a 64-byte budget holds two rows.
        let desc = Rgba8TextureDesc::new(8, 4, 1).unwrap();
        let plan = plan(desc, 64);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].copies, [copy(0, 0, 2, 0)]);
        assert_eq!(plan[1].copies, [copy(0, 2, 2, 0)]);
        assert!(plan.iter().all(|s| s.staging_bytes == 64));
    }

    #[test]
    fn one_submission_packs_every_level_when_it_fits() {
        // Levels 8×4, 4×2, 2×1: 128 + 32 + 8 = 168 bytes.
        let desc = Rgba8TextureDesc::new(8, 4, 3).unwrap();
        let plan = plan(desc, 168);
        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan[0].copies,
            [copy(0, 0, 4, 0), copy(1, 0, 2, 128), copy(2, 0, 1, 160)]
        );
        assert_eq!(plan[0].staging_bytes, 168);
    }

    #[test]
    fn a_submission_closes_when_the_next_row_does_not_fit() {
        // A 40-byte budget holds one 32-byte row of level 0, then two 16-byte rows of
        // level 1 and the single 8-byte row of level 2 together.
        let desc = Rgba8TextureDesc::new(8, 4, 3).unwrap();
        let plan = plan(desc, 40);
        assert_eq!(plan.len(), 5);
        for (row, submission) in plan[..4].iter().enumerate() {
            assert_eq!(submission.copies, [copy(0, row, 1, 0)]);
            assert_eq!(submission.staging_bytes, 32);
        }
        assert_eq!(plan[4].copies, [copy(1, 0, 2, 0), copy(2, 0, 1, 32)]);
        assert_eq!(plan[4].staging_bytes, 40);
    }

    #[test]
    fn every_row_is_planned_once_within_budget() {
        for (w, h, mips) in [(1, 1, 1), (37, 19, 6), (64, 3, 7), (5, 128, 8)] {
            let desc = Rgba8TextureDesc::new(w, h, mips).unwrap();
            let widest = w * RGBA8_TEXEL_BYTES;
            for budget in [
                widest,
                widest + 4,
                3 * widest,
                w * h * RGBA8_TEXEL_BYTES * 2,
            ] {
                let mut next_row = vec![0; mips as usize];
                for submission in plan(desc, budget) {
                    assert!(submission.staging_bytes <= budget);
                    let mut offset = 0;
                    for c in &submission.copies {
                        let (mw, _) = desc.mip_extent(c.level).unwrap();
                        assert_eq!(c.first_row, next_row[c.level as usize]);
                        assert_eq!(c.staging_offset, offset);
                        assert!(c.rows > 0);
                        offset += c.rows * mw * RGBA8_TEXEL_BYTES;
                        next_row[c.level as usize] += c.rows;
                    }
                    assert_eq!(offset, submission.staging_bytes);
                }
                for level in 0..mips {
                    assert_eq!(next_row[level as usize], desc.mip_extent(level).unwrap().1);
                }
            }
        }
    }

    #[test]
    fn rejects_partial_and_malformed_uploads_before_planning() {
        let desc = Rgba8TextureDesc::new(8, 4, 2).unwrap();
        let bytes = vec![0; 8 * 4 * RGBA8_TEXEL_BYTES];
        let mips = levels(&desc, &bytes);

        let missing = error(desc, &mips[..1], 1024);
        assert!(missing.contains("mip level 1 is missing"), "{missing}");

        let duplicate = error(desc, &[mips[0], mips[1], mips[1]], 1024);
        assert!(duplicate.contains("more than once"), "{duplicate}");

        let out_of_range = Rgba8MipLevel::new_packed(2, 2, 1, &bytes[..8]);
        let range = error(desc, &[mips[0], mips[1], out_of_range], 1024);
        assert!(range.contains("out of bounds"), "{range}");

        let wrong_size = Rgba8MipLevel::new_packed(1, 4, 1, &bytes[..16]);
        assert!(error(desc, &[mips[0], wrong_size], 1024).contains("expected 4x2"));

        let mut short_pitch = mips[0];
        short_pitch.row_pitch = 28;
        assert!(error(desc, &[short_pitch, mips[1]], 1024).contains("row pitch"));

        let short_bytes = Rgba8MipLevel::new_packed(0, 8, 4, &bytes[..127]);
        assert!(error(desc, &[short_bytes, mips[1]], 1024).contains("buffer length"));

        let small = error(desc, &mips, 28);
        assert!(small.contains("smaller than one 32-byte row"), "{small}");
    }
}
