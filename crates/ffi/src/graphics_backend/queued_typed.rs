// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use xenora_loader::graphics::GraphicsTypedTextureDesc;
use xenora_metal::{
    QueuedTypedImageCopy, TypedCopyInput, TypedImageCopyLocation, TypedTextureDesc,
};

pub(super) fn native_desc(d: GraphicsTypedTextureDesc) -> TypedTextureDesc {
    TypedTextureDesc {
        format: d.format,
        extent: [d.extent.0, d.extent.1],
        mip_levels: d.mip_levels,
        array_layers: d.array_layers,
    }
}
pub(super) fn loader_desc(d: TypedTextureDesc) -> GraphicsTypedTextureDesc {
    GraphicsTypedTextureDesc {
        format: d.format,
        extent: (d.extent[0], d.extent[1]),
        mip_levels: d.mip_levels,
        array_layers: d.array_layers,
    }
}
fn byte_len(d: TypedTextureDesc) -> Result<usize, GraphicsError> {
    PackedTextureArrayLayout::new(
        d.format,
        d.extent[0],
        d.extent[1],
        d.mip_levels,
        d.array_layers.unwrap_or(1),
    )
    .map(|l| l.byte_len())
    .map_err(|_| GraphicsError::InvalidDescriptor)
}
fn location(l: GraphicsImageCopyLocation) -> Result<TypedImageCopyLocation, GraphicsError> {
    match l {
        GraphicsImageCopyLocation::Texture {
            mip,
            slice,
            origin: (x, y),
        } => Ok(TypedImageCopyLocation::Texture {
            mip,
            slice,
            origin: [x, y],
        }),
        GraphicsImageCopyLocation::Buffer {
            offset,
            bytes_per_row,
        } => Ok(TypedImageCopyLocation::Buffer {
            offset,
            bytes_per_row,
        }),
        _ => Err(GraphicsError::InvalidDescriptor),
    }
}
fn depth_bytes(
    input: &GraphicsCopyInput,
    layout: GraphicsCopyLayout,
) -> Result<Option<Vec<u8>>, GraphicsError> {
    let GraphicsCopyInput::HostDepth(snapshot) = input else {
        return Ok(None);
    };
    let GraphicsCopyLayout::TypedTexture(desc) = layout else {
        return Err(GraphicsError::InvalidDescriptor);
    };
    if desc.format != TextureFormat::Depth32Float {
        return Err(GraphicsError::InvalidDescriptor);
    }
    let bytes = if let Some(packed) = snapshot.downcast_ref::<GraphicsTypedDepthSnapshot>() {
        if packed.desc != desc {
            return Err(GraphicsError::InvalidDescriptor);
        }
        packed.bytes.to_vec()
    } else {
        if desc.array_layers.is_some() || desc.mip_levels != 1 {
            return Err(GraphicsError::InvalidDescriptor);
        }
        snapshot
            .downcast_ref::<DepthSnapshot>()
            .ok_or(GraphicsError::InvalidDescriptor)?
            .to_packed_bytes()
    };
    if bytes.len() != byte_len(native_desc(desc))? {
        return Err(GraphicsError::InvalidDescriptor);
    }
    Ok(Some(bytes))
}
impl State {
    fn typed_input<'a>(
        &self,
        input: &'a GraphicsCopyInput,
        layout: GraphicsCopyLayout,
        waits: &[GraphicsQueuePoint],
        depth: Option<&'a [u8]>,
    ) -> Result<TypedCopyInput<'a>, GraphicsError> {
        if let GraphicsCopyLayout::Buffer { byte_len } = layout {
            return match self.region_buffer_input(input, byte_len, waits)? {
                QueuedCopyInput::HostBuffer(b) => Ok(TypedCopyInput::HostBuffer(b)),
                QueuedCopyInput::ProducedBuffer(b) => Ok(TypedCopyInput::ProducedBuffer(b)),
                _ => Err(GraphicsError::InvalidDescriptor),
            };
        }
        let GraphicsCopyLayout::TypedTexture(desc) = layout else {
            return Err(GraphicsError::InvalidDescriptor);
        };
        let native = native_desc(desc);
        match input {
            GraphicsCopyInput::Host(bytes) => {
                if bytes.len() != byte_len(native)? {
                    return Err(GraphicsError::InvalidDescriptor);
                }
                Ok(TypedCopyInput::HostTexture {
                    desc: native,
                    bytes,
                })
            }
            GraphicsCopyInput::HostDepth(_) => Ok(TypedCopyInput::HostTexture {
                desc: native,
                bytes: depth.ok_or(GraphicsError::InvalidDescriptor)?,
            }),
            GraphicsCopyInput::Produced(version) => {
                if !waits.contains(&version.point) {
                    return Err(GraphicsError::InvalidDescriptor);
                }
                let output = produced_typed_output(&self.entries, *version)?;
                if output.desc() != native {
                    return Err(GraphicsError::InvalidDescriptor);
                }
                Ok(TypedCopyInput::ProducedTexture(output))
            }
        }
    }
    pub(super) fn submit_native_typed_copy(
        &mut self,
        queue: RenderQueue,
        request: &GraphicsQueuedImageCopyRequest,
        dependencies: &[RenderPoint],
    ) -> Result<RenderPoint, GraphicsError> {
        let source_depth = depth_bytes(&request.source, request.source_layout)?;
        let destination_depth = depth_bytes(&request.destination, request.destination_layout)?;
        let source = self.typed_input(
            &request.source,
            request.source_layout,
            &request.waits,
            source_depth.as_deref(),
        )?;
        let destination = self.typed_input(
            &request.destination,
            request.destination_layout,
            &request.waits,
            destination_depth.as_deref(),
        )?;
        self.cohort
            .submit_typed_image_copy(
                queue,
                QueuedTypedImageCopy {
                    source,
                    destination,
                    source_location: location(request.source_location)?,
                    destination_location: location(request.destination_location)?,
                    extent: [request.extent.0, request.extent.1],
                },
                dependencies,
            )
            .map_err(metal_error)
    }
}
