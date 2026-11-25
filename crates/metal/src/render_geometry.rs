// SPDX-License-Identifier: GPL-3.0-or-later
//! Guarded GPU-only byte gathering for the existing packed float4 vertex ABI.
use super::*;
use crate::{GeometryAttribute, GeometryBuffer, GeometryIndexFormat, QueuedGeometry};
use objc2_metal::MTLResourceOptions;

pub(crate) struct Validation {
    buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    _lease: Rc<ColorLease>,
}
impl Validation {
    pub(crate) fn succeeded(&self) -> bool {
        // SAFETY: Callers only read after command completion. This initialized
        // shared four-byte buffer has one atomic GPU writer domain and no CPU writes.
        unsafe { self.buffer.contents().cast::<u32>().as_ptr().read() == 0 }
    }
}
pub(crate) struct Gather {
    inputs: [Retained<ProtocolObject<dyn MTLBuffer>>; 3],
    pub(crate) positions: Retained<ProtocolObject<dyn MTLBuffer>>,
    pub(crate) colors: Retained<ProtocolObject<dyn MTLBuffer>>,
    parameters: [u32; 16],
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    pub(crate) status: Rc<Validation>,
    _lease: Rc<ColorLease>,
}
impl Gather {
    pub(super) fn allocation_lease(&self) -> Rc<ColorLease> {
        self._lease.clone()
    }
    pub(crate) fn count(&self) -> usize {
        self.parameters[10] as usize
    }
    pub(crate) fn encode(
        &self,
        command: &ProtocolObject<dyn MTLCommandBuffer>,
    ) -> Result<(), MetalError> {
        let encoder = command
            .computeCommandEncoder()
            .ok_or_else(|| error("geometry encoder allocation failed"))?;
        encoder.setComputePipelineState(&self.pipeline);
        // SAFETY: Every input and output is an owned same-device tracked buffer.
        // Parameters are copied now, and the kernel checks logical bounds before reads.
        unsafe {
            for (slot, input) in self.inputs.iter().enumerate() {
                encoder.setBuffer_offset_atIndex(Some(input), 0, slot);
            }
            encoder.setBuffer_offset_atIndex(Some(&self.positions), 0, 3);
            encoder.setBuffer_offset_atIndex(Some(&self.colors), 0, 4);
            encoder.setBuffer_offset_atIndex(Some(&self.status.buffer), 0, 5);
            encoder.setBytes_length_atIndex(
                NonNull::from(&self.parameters).cast::<c_void>(),
                64,
                6,
            );
        }
        let width = self.pipeline.maxTotalThreadsPerThreadgroup().min(64);
        encoder.dispatchThreadgroups_threadsPerThreadgroup(
            size(self.count().div_ceil(width)),
            size(width),
        );
        encoder.endEncoding();
        Ok(())
    }
}

type PreparedGeometry = (
    Vec<Option<Rc<Gather>>>,
    Option<Rc<Validation>>,
    Vec<RenderBufferOutput>,
);

impl RenderCohort {
    pub(crate) fn prepare_geometry(
        &self,
        queue: usize,
        inputs: &[Option<QueuedGeometry<'_>>],
    ) -> Result<PreparedGeometry, MetalError> {
        if inputs.iter().all(Option::is_none) {
            return Ok((inputs.iter().map(|_| None).collect(), None, Vec::new()));
        }
        let gpu = &self.queues[queue];
        let length = |input: &GeometryBuffer<'_>| -> Result<usize, MetalError> {
            let len = match input {
                GeometryBuffer::Host(bytes) => bytes.len(),
                GeometryBuffer::Produced(output) => {
                    self.validate_point(&output.point)?;
                    if output.buffer.device().registryID() != gpu.device.registryID() {
                        return Err(error("geometry buffer device mismatch"));
                    }
                    output.byte_len
                }
            };
            if len == 0 || len > 64 * 1024 * 1024 {
                return Err(error("geometry buffer length"));
            }
            Ok(len)
        };
        let attr = |input: &GeometryAttribute<'_>| -> Result<usize, MetalError> {
            let len = length(&input.buffer)?;
            if input.stride < 16
                || input.stride > u32::MAX as usize
                || input.offset.checked_add(16).is_none_or(|end| end > len)
            {
                return Err(error("geometry attribute range"));
            }
            Ok(len)
        };
        // Validate every descriptor and reserve all payload before allocating uploads.
        let mut reservations = Vec::new();
        let mut total = 0usize;
        for input in inputs {
            let Some(input) = input else {
                reservations.push(None);
                continue;
            };
            total = total
                .checked_add(input.count as usize)
                .filter(|n| *n <= MAX_RENDER_VERTICES)
                .ok_or_else(|| error("geometry vertex limit"))?;
            if input.count == 0
                || !input.count.is_multiple_of(3)
                || (input.indices.is_none() && input.base_vertex != 0)
            {
                return Err(error("geometry draw descriptor"));
            }
            attr(&input.positions)?;
            if let Some(colors) = &input.colors {
                attr(colors)?;
            }
            if let Some(index) = &input.indices {
                let len = length(&index.buffer)?;
                let size = match index.format {
                    GeometryIndexFormat::R16Uint => 2,
                    GeometryIndexFormat::R32Uint => 4,
                };
                (input.start as usize)
                    .checked_add(input.count as usize)
                    .and_then(|n| n.checked_mul(size))
                    .and_then(|n| n.checked_add(index.offset))
                    .filter(|end| *end <= len)
                    .ok_or_else(|| error("geometry index range"))?;
            } else {
                for attribute in std::iter::once(&input.positions).chain(input.colors.iter()) {
                    (input.start as usize)
                        .checked_add(input.count as usize - 1)
                        .and_then(|n| n.checked_mul(attribute.stride))
                        .and_then(|n| n.checked_add(attribute.offset))
                        .and_then(|n| n.checked_add(16))
                        .filter(|end| *end <= length(&attribute.buffer).unwrap())
                        .ok_or_else(|| error("geometry linear range"))?;
                }
            }
            let mut bytes = input.count as usize * 32 + 128;
            for buffer in std::iter::once(&input.positions.buffer)
                .chain(input.colors.as_ref().map(|a| &a.buffer))
                .chain(input.indices.as_ref().map(|a| &a.buffer))
            {
                if let GeometryBuffer::Host(data) = buffer {
                    bytes = bytes
                        .checked_add(data.len().div_ceil(4) * 12)
                        .ok_or_else(|| error("geometry budget overflow"))?;
                }
            }
            reservations.push(Some(ColorLease::reserve(&self.color_bytes, bytes)?));
        }
        let status_lease = ColorLease::reserve(&self.color_bytes, 4)?;
        let status = Rc::new(Validation {
            buffer: gpu.new_buffer(&[0])?,
            _lease: status_lease,
        });
        let options = compile_options()?;
        let library = gpu.compile_library(include_str!("geometry_gather.metal"), &options)?;
        let function = find_function(&library, "gather_geometry")?;
        let pipeline = gpu.compute_pipeline(&function)?;
        verify_shader_validation(&pipeline)?;
        let mut produced = Vec::new();
        let mut upload = |input: &GeometryBuffer<'_>| -> Result<Retained<ProtocolObject<dyn MTLBuffer>>, MetalError> {
            match input {
                GeometryBuffer::Produced(output) => { produced.push(output.clone()); Ok(output.buffer.clone()) }
                GeometryBuffer::Host(bytes) => {
                    let mut words=vec![0u32; bytes.len().div_ceil(4)];
                    for (i,byte) in bytes.iter().enumerate() { words[i/4] |= u32::from(*byte)<<((i%4)*8); }
                    gpu.new_buffer(&words)
                }
            }
        };
        let mut result = Vec::new();
        for (input, lease) in inputs.iter().zip(reservations) {
            let Some(input) = input else {
                result.push(None);
                continue;
            };
            let positions = upload(&input.positions.buffer)?;
            let colors = input
                .colors
                .as_ref()
                .map(|a| upload(&a.buffer))
                .transpose()?
                .unwrap_or_else(|| positions.clone());
            let indices = input
                .indices
                .as_ref()
                .map(|a| upload(&a.buffer))
                .transpose()?
                .unwrap_or_else(|| positions.clone());
            let mut p = [0u32; 16];
            p[0] = length(&input.positions.buffer)? as u32;
            p[1] = input.positions.offset as u32;
            p[2] = input.positions.stride as u32;
            if let Some(a) = &input.colors {
                p[3] = length(&a.buffer)? as u32;
                p[4] = a.offset as u32;
                p[5] = a.stride as u32;
                p[12] = 1;
            }
            if let Some(a) = &input.indices {
                p[6] = length(&a.buffer)? as u32;
                p[7] = a.offset as u32;
                p[8] = match a.format {
                    GeometryIndexFormat::R16Uint => 2,
                    GeometryIndexFormat::R32Uint => 4,
                };
                p[13] = 1;
            }
            p[9] = input.start;
            p[10] = input.count;
            p[11] = input.base_vertex as u32;
            let allocate = || {
                gpu.device
                    .newBufferWithLength_options(
                        input.count as usize * 16,
                        MTLResourceOptions::StorageModeShared,
                    )
                    .ok_or_else(|| error("packed geometry allocation failed"))
            };
            result.push(Some(Rc::new(Gather {
                inputs: [positions, colors, indices],
                positions: allocate()?,
                colors: allocate()?,
                parameters: p,
                pipeline: pipeline.clone(),
                status: status.clone(),
                _lease: lease.unwrap(),
            })));
        }
        Ok((result, Some(status), produced))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
    fn native_geometry_real_submission_point_retains_allocation_after_collect() {
        let gpu = NativeCompute::new().unwrap();
        let mut cohort = gpu.new_render_cohort().unwrap();
        let budget = cohort.color_bytes.clone();
        let positions = [
            [-1.0f32, -1.0, 0.0, 1.0],
            [3.0, -1.0, 0.0, 1.0],
            [-1.0, 3.0, 0.0, 1.0],
        ];
        let bytes: Vec<u8> = positions
            .into_iter()
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect();
        let geometry = [Some(QueuedGeometry {
            positions: GeometryAttribute {
                buffer: GeometryBuffer::Host(&bytes),
                offset: 0,
                stride: 16,
            },
            colors: None,
            indices: None,
            start: 0,
            count: 3,
            base_vertex: 0,
        })];
        let source = "#include <metal_stdlib>\nusing namespace metal;\nvertex float4 vs(uint id [[vertex_id]], device const packed_float4* p [[buffer(0)]]) { return float4(p[id]); }\nfragment float4 ps() { return float4(1,0,0,1); }";
        let shaders = [TriangleShader {
            source,
            entries: ["vs", "ps"],
        }];
        let draws = [crate::TriangleDraw {
            vertices: &[],
            colors: &[],
            viewport: [1, 1],
        }];
        let states = [crate::TriangleState::default()];
        let clears = [[0; 4]];
        let pass = crate::TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [1, 1],
            clears: &clears,
            clear_depth: None,
        };
        let point = cohort
            .submit_with_geometry(
                RenderQueue::First,
                &shaders,
                &pass,
                &[vec![]],
                None,
                &[QueuedColorLoad::Clear],
                &[],
                &[DrawUniforms::default()],
                &geometry,
            )
            .unwrap();
        assert_eq!(budget.get(), 508);
        assert_eq!(
            cohort.collect(&point).unwrap().colors,
            vec![vec![255, 0, 0, 255]]
        );
        cohort.reinitialize().unwrap();
        assert_eq!(
            budget.get(),
            508,
            "returned Point must retain production gather scratch after collect and history cleanup"
        );
        drop(point);
        assert_eq!(budget.get(), 0);
    }

    #[test]
    #[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
    fn native_geometry_point_retains_scratch_and_checks_status_before_success() {
        let gpu = NativeCompute::new().unwrap();
        let cohort = gpu.new_render_cohort().unwrap();
        let budget = cohort.color_bytes.clone();
        let bytes = [0u8; 48];
        let inputs = [Some(QueuedGeometry {
            positions: GeometryAttribute {
                buffer: GeometryBuffer::Host(&bytes),
                offset: 0,
                stride: 16,
            },
            colors: None,
            indices: None,
            start: 0,
            count: 3,
            base_vertex: 0,
        })];
        let (gathers, status, sources) = cohort.prepare_geometry(0, &inputs).unwrap();
        assert!(sources.is_empty());
        // Packed positions/colors96, parameter array+setBytes128, host3*48,
        // and separately owned validation status4.
        assert_eq!(budget.get(), 372);
        let command = gpu.queue.commandBuffer().unwrap();
        command.commit();
        command.waitUntilCompleted();
        let point = Rc::new(Point {
            generation: cohort.generation.clone(),
            queue: 0,
            value: 1,
            command,
            event: event(&gpu).unwrap(),
            dependencies: RefCell::new(Vec::new()),
            terminal: Cell::new(None),
            validation: status.clone(),
            _leases: gathers
                .iter()
                .flatten()
                .map(|g| g.allocation_lease())
                .collect(),
            depth_lease: None,
        });
        drop(gathers);
        drop(status);
        assert_eq!(
            budget.get(),
            372,
            "point owns scratch after pending geometry is collected"
        );
        assert_eq!(RenderCohort::poll_point(&point).unwrap(), Some(true));
        drop(point);
        assert_eq!(budget.get(), 0);

        let aliased = [Some(QueuedGeometry {
            positions: GeometryAttribute {
                buffer: GeometryBuffer::Host(&bytes),
                offset: 0,
                stride: 16,
            },
            colors: Some(GeometryAttribute {
                buffer: GeometryBuffer::Host(&bytes),
                offset: 0,
                stride: 16,
            }),
            indices: None,
            start: 0,
            count: 3,
            base_vertex: 0,
        })];
        let (gathers, status, _) = cohort.prepare_geometry(0, &aliased).unwrap();
        assert_eq!(
            budget.get(),
            516,
            "aliased host slices upload and charge each occurrence"
        );
        assert!(!std::ptr::eq(
            &*gathers[0].as_ref().unwrap().inputs[0],
            &*gathers[0].as_ref().unwrap().inputs[1]
        ));
        drop(gathers);
        drop(status);
        assert_eq!(budget.get(), 0);

        // Controlled terminal status, not a simulated real GPU execution fault.
        let validation = Rc::new(Validation {
            buffer: gpu.new_buffer(&[1]).unwrap(),
            _lease: ColorLease::reserve(&budget, 4).unwrap(),
        });
        let command = gpu.queue.commandBuffer().unwrap();
        command.commit();
        command.waitUntilCompleted();
        let failed = Rc::new(Point {
            generation: cohort.generation.clone(),
            queue: 0,
            value: 2,
            command: command.clone(),
            event: event(&gpu).unwrap(),
            dependencies: RefCell::new(Vec::new()),
            terminal: Cell::new(None),
            validation: Some(validation),
            _leases: Vec::new(),
            depth_lease: None,
        });
        let child = Point {
            generation: cohort.generation.clone(),
            queue: 1,
            value: 1,
            command,
            event: event(&gpu).unwrap(),
            dependencies: RefCell::new(vec![failed.clone()]),
            terminal: Cell::new(None),
            validation: None,
            _leases: Vec::new(),
            depth_lease: None,
        };
        assert_eq!(RenderCohort::poll_point(&child).unwrap(), Some(false));
        assert_eq!(child.terminal.get(), None);
        assert_eq!(failed.terminal.get(), None);
        assert_eq!(child.dependencies.borrow().len(), 1);
        assert!(!failed.wait_terminal());
        drop(child);
        drop(failed);
        assert_eq!(budget.get(), 0);
    }
}
