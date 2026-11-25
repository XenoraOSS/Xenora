// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit resource roles; legacy backend registration never opts in implicitly.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphicsResourceRoleCapabilities {
    pub sampled_formats: u16,
    pub color_formats: u16,
}
impl GraphicsResourceRoleCapabilities {
    pub(super) fn sampled(self, format: TextureFormat) -> bool {
        self.sampled_formats & (1 << typed_copy::format_code(format)) != 0
    }
    pub(super) fn color(self, format: TextureFormat) -> bool {
        self.color_formats & (1 << typed_copy::format_code(format)) != 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GraphicsTypedColorClear {
    Float([f64; 4]),
    Uint([u32; 4]),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphicsTypedColorAttachment {
    pub format: TextureFormat,
    pub clear: GraphicsTypedColorClear,
    pub blend_enabled: bool,
}
impl GraphicsTypedColorAttachment {
    pub(super) fn validate(self) -> Result<(), GraphicsError> {
        match (self.format, self.clear) {
            (TextureFormat::R32Uint, GraphicsTypedColorClear::Uint(_)) if !self.blend_enabled => {
                Ok(())
            }
            (
                TextureFormat::R8Unorm
                | TextureFormat::Rg8Unorm
                | TextureFormat::Rgba8Unorm
                | TextureFormat::Bgra8Unorm
                | TextureFormat::Rgba16Float
                | TextureFormat::R32Float,
                GraphicsTypedColorClear::Float(v),
            ) if v.iter().all(|c| c.is_finite()) => Ok(()),
            _ => Err(GraphicsError::InvalidDescriptor),
        }
    }
}
impl GraphicsContext {
    /// Call after installing a backend that implements these exact role masks.
    pub fn enable_resource_roles(
        &mut self,
        caps: GraphicsResourceRoleCapabilities,
    ) -> Result<(), GraphicsError> {
        // Color formats exclude compressed and depth; masks use stable guest codes.
        if caps.sampled_formats & !0x3ff != 0 || caps.color_formats & !0x1f9 != 0 {
            return Err(GraphicsError::InvalidDescriptor);
        }
        self.resource_roles = caps;
        Ok(())
    }
    pub(crate) fn sampled_resource_desc(
        &self,
        handle: GraphicsHandle,
    ) -> Result<GraphicsTypedTextureDesc, GraphicsError> {
        if let Some(depth) = self.depth_resources.get(&handle) {
            return Ok(depth.typed_desc);
        }
        let r = self
            .resources
            .get(&handle)
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        if r.desc.dimension != ResourceDimension::Texture2D || r.guest_backing_addr.is_some() {
            return Err(GraphicsError::InvalidDescriptor);
        }
        Ok(GraphicsTypedTextureDesc {
            format: r.texture_format.ok_or(GraphicsError::InvalidDescriptor)?,
            extent: (r.desc.width as usize, r.desc.height as usize),
            mip_levels: u32::from(r.desc.mip_levels),
            array_layers: r
                .is_texture_array
                .then_some(u32::from(r.desc.depth_or_array_size)),
        })
    }
    pub(super) fn sampled_resource_bytes(
        &self,
        handle: GraphicsHandle,
    ) -> Result<Arc<[u8]>, GraphicsError> {
        if let Some(r) = self.resources.get(&handle) {
            return Ok(Arc::from(r.data.as_slice()));
        }
        let depth = self
            .depth_resources
            .get(&handle)
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        let packed = depth
            .snapshot
            .as_ref()
            .and_then(|s| s.downcast_ref::<GraphicsTypedDepthSnapshot>())
            .ok_or(GraphicsError::ResourceLifetimeViolation)?;
        if packed.desc != depth.typed_desc || packed.bytes.len() != depth.typed_desc.byte_len()? {
            return Err(GraphicsError::InvalidDescriptor);
        }
        Ok(packed.bytes.clone())
    }
}
