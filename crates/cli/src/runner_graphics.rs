// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared graphics adapters for hosted PE workers.

use std::error::Error;
use std::sync::Arc;

use xenora_loader::ServicesState;
use xenora_loader::graphics::{FenceOperation, GraphicsBackendId, GraphicsError, NativeFence};
use xenora_metal::NativeCompute;
use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

#[derive(Clone, Copy)]
pub enum Backend {
    Reference,
    Metal,
}

impl Backend {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "reference" => Some(Self::Reference),
            "metal" => Some(Self::Metal),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Metal => "metal",
        }
    }

    pub fn id(self) -> GraphicsBackendId {
        match self {
            Self::Reference => GraphicsBackendId::SoftwareReference,
            Self::Metal => GraphicsBackendId::MetalNative,
        }
    }
}

pub fn compile_rgba_shader() -> Result<String, Box<dyn Error>> {
    Ok(compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
    )?
    .msl)
}

pub fn install_metal(state: &ServicesState) -> Result<(), Box<dyn Error>> {
    let msl = compile_rgba_shader()?;
    let gpu = Arc::new(NativeCompute::new()?);
    let renderer = Arc::clone(&gpu);
    let mut graphics = state
        .graphics
        .lock()
        .map_err(|_| "the hosted graphics context is poisoned")?;
    graphics.set_native_fence_factory(move |initial| {
        let fence = gpu.new_render_fence(initial).map_err(backend_error)?;
        let adapter: NativeFence = Box::new(move |operation| {
            match operation {
                FenceOperation::CompletedValue => fence.completed_value(),
                FenceOperation::Signal(value) => fence.signal(value),
                FenceOperation::QueueWait { value, timeout } => fence.queue_wait(value, timeout),
                FenceOperation::HostWait { value, timeout } => fence.host_wait(value, timeout),
            }
            .map_err(backend_error)
        });
        Ok(adapter)
    })?;
    graphics.set_metal_backend(move |vertices, (width, height), clear| {
        renderer
            .render_rgba8(
                &msl,
                [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                vertices,
                &[[0.0; 4]; 3],
                [width, height],
                clear,
            )
            .map_err(backend_error)
    });
    Ok(())
}

fn backend_error(error: impl std::fmt::Display) -> GraphicsError {
    GraphicsError::BackendError(error.to_string())
}

pub fn trace(state: &ServicesState) {
    let Ok(graphics) = state.graphics.lock() else {
        return;
    };
    let presentation = Arc::clone(&graphics.last_presentation);
    drop(graphics);
    let Ok(frame) = presentation.lock() else {
        return;
    };
    let backend = match frame.backend {
        Some(GraphicsBackendId::SoftwareReference) => Backend::Reference.name(),
        Some(GraphicsBackendId::MetalNative) => Backend::Metal.name(),
        None => "none",
    };
    eprintln!(
        "trace graphics backend={backend} frames={} last_draws={} width={} height={}",
        frame.frame_index, frame.draw_count, frame.width, frame.height,
    );
}
