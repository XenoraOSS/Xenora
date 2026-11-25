// SPDX-License-Identifier: GPL-3.0-or-later
//! Typed attachments share the existing geometry, depth and submission machinery.
use super::*;
use xenora_texture::TextureFormat;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TypedColorClear {
    Float([f64; 4]),
    Uint([u32; 4]),
}
impl TypedColorClear {
    pub(super) fn components(self) -> [f64; 4] {
        match self {
            Self::Float(value) => value,
            Self::Uint(value) => value.map(f64::from),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypedColorAttachment {
    pub format: TextureFormat,
    pub clear: TypedColorClear,
    /// Apply the draw's blend state to this slot. Integer slots must disable it.
    pub blend_enabled: bool,
}
impl TypedColorAttachment {
    pub(super) fn pixel_format(self) -> Result<MTLPixelFormat, MetalError> {
        Ok(match self.format {
            TextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
            TextureFormat::Rg8Unorm => MTLPixelFormat::RG8Unorm,
            TextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
            TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
            TextureFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
            TextureFormat::R32Float => MTLPixelFormat::R32Float,
            TextureFormat::R32Uint => MTLPixelFormat::R32Uint,
            _ => {
                return Err(MetalError::new(
                    "typed target",
                    "format is not a color attachment",
                ));
            }
        })
    }
    pub(super) fn validate(self) -> Result<(), MetalError> {
        self.pixel_format()?;
        match (self.format, self.clear) {
            (TextureFormat::R32Uint, TypedColorClear::Uint(_)) if !self.blend_enabled => Ok(()),
            (TextureFormat::R32Uint, _) => Err(MetalError::new(
                "typed target",
                "integer attachment requires integer clear and disabled blending",
            )),
            (_, TypedColorClear::Float(value)) if value.iter().all(|v| v.is_finite()) => Ok(()),
            _ => Err(MetalError::new(
                "typed target",
                "floating attachment requires a finite floating clear",
            )),
        }
    }
}

/// Attachment formats remain fixed throughout the pass; outputs are packed bytes.
pub struct TypedTriangleTargetsPass<'a> {
    pub draws: &'a [TriangleDraw<'a>],
    pub states: &'a [TriangleState],
    pub extent: [usize; 2],
    pub colors: &'a [TypedColorAttachment],
    pub clear_depth: Option<f32>,
}
impl NativeCompute {
    #[allow(clippy::too_many_arguments)]
    pub fn submit_typed_triangle_targets<'a>(
        &self,
        shaders: &[TriangleShader<'_>],
        pass: &TypedTriangleTargetsPass<'_>,
        bindings: &[Vec<crate::SampledBinding>],
        depth: Option<(&'a mut MetalDepthTarget, DepthLoad)>,
        color_loads: &[Option<&[u8]>],
        uniforms: &[DrawUniforms],
    ) -> Result<PendingRender<'a>, MetalError> {
        if !(1..=8).contains(&pass.colors.len())
            || pass.draws.len() > MAX_RENDER_DRAWS
            || bindings.len() != pass.draws.len()
            || bindings.iter().any(|b| b.len() > 4)
            || (depth.is_some() && pass.clear_depth.is_some())
        {
            return Err(MetalError::new(
                "typed targets",
                "invalid pass/binding/depth shape",
            ));
        }
        for color in pass.colors {
            color.validate()?;
        }
        let normalized = bindings
            .iter()
            .map(|draw| {
                draw.iter()
                    .map(crate::SampledBinding::normalize)
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let clears = vec![[0; 4]; pass.colors.len()];
        self.submit_targets_typed_internal(
            "",
            ["", ""],
            pass.draws,
            pass.extent,
            &clears,
            None,
            Some((pass.states, pass.clear_depth)),
            None,
            depth,
            Some(color_loads),
            Some((shaders, &normalized)),
            None,
            Some(uniforms),
            Some(pass.colors),
        )
    }
}
