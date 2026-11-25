// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-only image-transfer contract; callbacks below perform no GPU work.
use xenora_loader::graphics::*;
use xenora_loader::{GuestMemory, MemoryPermissions};

struct CpuTicket(Option<GraphicsQueuedWorkOutput>);
impl GraphicsQueuedPendingWork for CpuTicket {
    fn poll(&mut self) -> Option<Result<GraphicsQueuedWorkOutput, GraphicsError>> {
        self.0.take().map(Ok)
    }
    fn wait(&mut self) -> Result<GraphicsQueuedWorkOutput, GraphicsError> {
        self.poll().expect("CPU completion available")
    }
    fn reject_dependency(&mut self, _: GraphicsQueuePoint) {}
}

#[test]
fn cpu_image_copy_pitched_buffer_to_texture_literal_baseline() {
    let mut ctx = GraphicsContext::new();
    let device = ctx.create_device().unwrap();
    let queue = ctx
        .create_command_queue(device, CommandListType::Direct)
        .unwrap();
    let allocator = ctx
        .create_command_allocator(device, CommandListType::Direct)
        .unwrap();
    let list = ctx
        .create_command_list(device, allocator, CommandListType::Direct)
        .unwrap();
    let source = ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: 20,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::COPY_DEST,
            None,
        )
        .unwrap();
    let destination = ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 2,
                height: 2,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: true,
            },
            ResourceStates::COPY_DEST,
            None,
        )
        .unwrap();
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 128, MemoryPermissions::READ_WRITE)
        .unwrap();
    let source_bytes = [
        90, 91, 92, 93, 17, 83, 149, 231, 94, 95, 96, 97, 98, 99, 100, 101, 31, 47, 61, 79,
    ];
    let prior = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    memory.write_bytes(0x1000, &source_bytes).unwrap();
    ctx.copy_guest_to_resource(source, 0, 0x1000, 20, &memory)
        .unwrap();
    memory.write_bytes(0x1000, &prior).unwrap();
    ctx.copy_guest_to_resource(destination, 0, 0x1000, 16, &memory)
        .unwrap();
    ctx.cmd_resource_barrier(
        list,
        source,
        ResourceStates::COPY_DEST,
        ResourceStates::COPY_SOURCE,
    )
    .unwrap();
    ctx.cmd_copy_image(
        list,
        destination,
        GraphicsImageCopyLocation::Rgba8 { origin: (1, 0) },
        source,
        GraphicsImageCopyLocation::Buffer {
            offset: 4,
            bytes_per_row: 12,
        },
        (1, 2),
    )
    .unwrap();
    ctx.cmd_resource_barrier(
        list,
        destination,
        ResourceStates::COPY_DEST,
        ResourceStates::COPY_SOURCE,
    )
    .unwrap();
    ctx.close_command_list(list).unwrap();
    ctx.set_metal_image_copy_queued_backend(GraphicsTextureCapabilities::default(), move |work| {
        let GraphicsQueuedWorkRequest::ImageCopy(request) = work else {
            panic!("CPU image callback received other work")
        };
        assert_eq!(
            request.source_layout,
            GraphicsCopyLayout::Buffer { byte_len: 20 }
        );
        assert_eq!(
            request.destination_layout,
            GraphicsCopyLayout::Rgba8 { extent: (2, 2) }
        );
        assert_eq!(
            request.source_location,
            GraphicsImageCopyLocation::Buffer {
                offset: 4,
                bytes_per_row: 12
            }
        );
        assert_eq!(
            request.destination_location,
            GraphicsImageCopyLocation::Rgba8 { origin: (1, 0) }
        );
        assert_eq!(request.extent, (1, 2));
        assert!(request.waits.is_empty());
        assert_eq!(
            request.output,
            GraphicsProducedVersion {
                point: request.point,
                output: GraphicsProducedOutput::Copy(0)
            }
        );
        let GraphicsCopyInput::Host(source) = request.source else {
            panic!("host source")
        };
        let GraphicsCopyInput::Host(destination) = request.destination else {
            panic!("host prior")
        };
        assert_eq!(source.as_ref(), &source_bytes);
        assert_eq!(destination.as_ref(), &prior);
        Ok(Box::new(CpuTicket(Some(GraphicsQueuedWorkOutput::Copy {
            layout: GraphicsCopyLayout::Rgba8 { extent: (2, 2) },
            bytes: vec![1, 2, 3, 4, 17, 83, 149, 231, 9, 10, 11, 12, 31, 47, 61, 79],
        }))))
    });
    let receipt = ctx
        .submit_command_list(queue, list, &memory)
        .expect("valid pitched image copy must be admitted");
    assert_eq!(
        ctx.wait_submission(receipt).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert!(ctx.submission_outcome(receipt).unwrap().is_none());
    ctx.release_submission(receipt).unwrap();
    ctx.copy_resource_to_guest(destination, 0, 0x1000, 16, &mut memory)
        .unwrap();
    let mut actual = [0; 16];
    memory.read_bytes(0x1000, &mut actual).unwrap();
    assert_eq!(
        actual,
        [1, 2, 3, 4, 17, 83, 149, 231, 9, 10, 11, 12, 31, 47, 61, 79]
    );
    assert_eq!(ctx.queued_copy_allocation_bytes(), 0);
    assert_eq!(ctx.last_presentation.lock().unwrap().frame_index, 0);
}

use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Completion {
    result: Option<GraphicsQueuedWorkOutput>,
    rejected: Vec<GraphicsQueuePoint>,
}
struct Controlled(Arc<Mutex<Completion>>);
impl GraphicsQueuedPendingWork for Controlled {
    fn poll(&mut self) -> Option<Result<GraphicsQueuedWorkOutput, GraphicsError>> {
        self.0.lock().unwrap().result.take().map(Ok)
    }
    fn wait(&mut self) -> Result<GraphicsQueuedWorkOutput, GraphicsError> {
        self.poll().expect("controlled CPU completion")
    }
    fn reject_dependency(&mut self, point: GraphicsQueuePoint) {
        self.0.lock().unwrap().rejected.push(point);
    }
}
#[derive(Default)]
struct Backend {
    requests: Vec<GraphicsQueuedWorkRequest>,
    completions: Vec<Arc<Mutex<Completion>>>,
}
struct Harness {
    ctx: GraphicsContext,
    memory: GuestMemory,
    device: GraphicsHandle,
    queues: [GraphicsHandle; 2],
    allocators: [GraphicsHandle; 2],
    lists: [GraphicsHandle; 2],
    backend: Arc<Mutex<Backend>>,
}
impl Harness {
    fn new() -> Self {
        let mut ctx = GraphicsContext::new();
        let device = ctx.create_device().unwrap();
        let queues = std::array::from_fn(|_| {
            ctx.create_command_queue(device, CommandListType::Direct)
                .unwrap()
        });
        let allocators = std::array::from_fn(|_| {
            ctx.create_command_allocator(device, CommandListType::Direct)
                .unwrap()
        });
        let lists = std::array::from_fn(|i| {
            ctx.create_command_list(device, allocators[i], CommandListType::Direct)
                .unwrap()
        });
        let backend = Arc::new(Mutex::new(Backend::default()));
        let captured = backend.clone();
        ctx.set_metal_image_copy_queued_backend(
            GraphicsTextureCapabilities::default(),
            move |request| {
                let completion = Arc::new(Mutex::new(Completion::default()));
                let mut backend = captured.lock().unwrap();
                backend.requests.push(request);
                backend.completions.push(completion.clone());
                Ok(Box::new(Controlled(completion)))
            },
        );
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 4096, MemoryPermissions::READ_WRITE)
            .unwrap();
        Self {
            ctx,
            memory,
            device,
            queues,
            allocators,
            lists,
            backend,
        }
    }
    fn resource(&mut self, texture: bool, bytes: &[u8]) -> GraphicsHandle {
        let resource = self
            .ctx
            .create_committed_resource(
                self.device,
                ResourceDesc {
                    dimension: if texture {
                        ResourceDimension::Texture2D
                    } else {
                        ResourceDimension::Buffer
                    },
                    width: if texture { 2 } else { bytes.len() as u64 },
                    height: 1,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: texture,
                },
                ResourceStates::COPY_DEST,
                None,
            )
            .unwrap();
        self.memory.write_bytes(0x1000, bytes).unwrap();
        self.ctx
            .copy_guest_to_resource(resource, 0, 0x1000, bytes.len(), &self.memory)
            .unwrap();
        resource
    }
    fn barrier(
        &mut self,
        slot: usize,
        resource: GraphicsHandle,
        before: ResourceStates,
        after: ResourceStates,
    ) {
        self.ctx
            .cmd_resource_barrier(self.lists[slot], resource, before, after)
            .unwrap();
    }
    fn submit(
        &mut self,
        slot: usize,
        waits: &[GraphicsQueuePoint],
    ) -> Result<GraphicsHandle, GraphicsError> {
        self.ctx
            .submit_command_list_after(self.queues[slot], self.lists[slot], waits, &self.memory)
    }
    fn complete(&self, index: usize, output: GraphicsQueuedWorkOutput) {
        self.backend.lock().unwrap().completions[index]
            .lock()
            .unwrap()
            .result = Some(output);
    }
    fn reset(&mut self, slot: usize) {
        self.ctx
            .reset_command_list(self.lists[slot], self.allocators[slot])
            .unwrap();
    }
    fn read(&mut self, resource: GraphicsHandle, len: usize) -> Vec<u8> {
        self.ctx
            .copy_resource_to_guest(resource, 0, 0x1000, len, &mut self.memory)
            .unwrap();
        let mut bytes = vec![0; len];
        self.memory.read_bytes(0x1000, &mut bytes).unwrap();
        bytes
    }
    fn image(
        &mut self,
        slot: usize,
        direction: usize,
        source: GraphicsHandle,
        destination: GraphicsHandle,
        source_state: ResourceStates,
    ) {
        if source_state != ResourceStates::COPY_SOURCE {
            self.barrier(slot, source, source_state, ResourceStates::COPY_SOURCE);
        }
        self.ctx
            .cmd_copy_image(
                self.lists[slot],
                destination,
                location(direction != 1),
                source,
                location(direction != 0),
                (1, 1),
            )
            .unwrap();
        self.barrier(
            slot,
            destination,
            ResourceStates::COPY_DEST,
            ResourceStates::COPY_SOURCE,
        );
        self.ctx.close_command_list(self.lists[slot]).unwrap();
    }
}
fn location(texture: bool) -> GraphicsImageCopyLocation {
    if texture {
        GraphicsImageCopyLocation::Rgba8 { origin: (1, 0) }
    } else {
        GraphicsImageCopyLocation::Buffer {
            offset: 4,
            bytes_per_row: 4,
        }
    }
}
fn layout(texture: bool) -> GraphicsCopyLayout {
    if texture {
        GraphicsCopyLayout::Rgba8 { extent: (2, 1) }
    } else {
        GraphicsCopyLayout::Buffer { byte_len: 9 }
    }
}
fn output(texture: bool, bytes: &[u8]) -> GraphicsQueuedWorkOutput {
    GraphicsQueuedWorkOutput::Copy {
        layout: layout(texture),
        bytes: bytes.to_vec(),
    }
}
const BUFFER: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 99];
const COLOR: &[u8] = &[17, 83, 149, 231, 31, 47, 61, 79];

#[test]
fn cpu_image_each_direction_each_produced_role_orders_failure_and_exact_layouts() {
    for direction in 0..3 {
        for prior_produced in [false, true] {
            for consumer_first in [false, true] {
                for fail in [false, true] {
                    let mut h = Harness::new();
                    let source_texture = direction != 0;
                    let destination_texture = direction != 1;
                    let source =
                        h.resource(source_texture, if source_texture { COLOR } else { BUFFER });
                    let destination = h.resource(
                        destination_texture,
                        if destination_texture { COLOR } else { BUFFER },
                    );
                    let changed_texture = if prior_produced {
                        destination_texture
                    } else {
                        source_texture
                    };
                    let seed_bytes: &[u8] = if changed_texture {
                        &[101, 102, 103, 104, 105, 106, 107, 108]
                    } else {
                        &[101, 102, 103, 104, 105, 106, 107, 108, 109]
                    };
                    let seed = h.resource(changed_texture, seed_bytes);
                    let changed = if prior_produced { destination } else { source };
                    h.barrier(
                        0,
                        seed,
                        ResourceStates::COPY_DEST,
                        ResourceStates::COPY_SOURCE,
                    );
                    h.ctx.cmd_copy_resource(h.lists[0], changed, seed).unwrap();
                    if !prior_produced {
                        h.barrier(
                            0,
                            changed,
                            ResourceStates::COPY_DEST,
                            ResourceStates::COPY_SOURCE,
                        );
                    }
                    h.ctx.close_command_list(h.lists[0]).unwrap();
                    let a = h.submit(0, &[]).unwrap();
                    let point = h.ctx.submission_point(a).unwrap();
                    h.image(
                        1,
                        direction,
                        source,
                        destination,
                        if prior_produced {
                            ResourceStates::COPY_DEST
                        } else {
                            ResourceStates::COPY_SOURCE
                        },
                    );
                    assert!(h.submit(1, &[]).is_err());
                    assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
                    let producer_charge = h.ctx.queued_copy_allocation_bytes();
                    let b = h.submit(1, &[point]).unwrap();
                    let expected_charge = match (direction, prior_produced) {
                        (0, false) => 24,
                        (0, true) => 40,
                        (1, false) => 48,
                        (1, true) => 32,
                        _ => 24,
                    };
                    assert_eq!(
                        h.ctx.queued_copy_allocation_bytes() - producer_charge,
                        expected_charge
                    );
                    {
                        let backend = h.backend.lock().unwrap();
                        let GraphicsQueuedWorkRequest::ImageCopy(request) = &backend.requests[1]
                        else {
                            panic!("image")
                        };
                        assert_eq!(request.source_layout, layout(source_texture));
                        assert_eq!(request.destination_layout, layout(destination_texture));
                        assert_eq!(request.waits, [point]);
                        let (produced, host) = if prior_produced {
                            (&request.destination, &request.source)
                        } else {
                            (&request.source, &request.destination)
                        };
                        let GraphicsCopyInput::Produced(version) = produced else {
                            panic!("exact version")
                        };
                        assert_eq!(
                            *version,
                            GraphicsProducedVersion {
                                point,
                                output: GraphicsProducedOutput::Copy(0)
                            }
                        );
                        let GraphicsCopyInput::Host(bytes) = host else {
                            panic!("host other input")
                        };
                        let host_texture = if prior_produced {
                            source_texture
                        } else {
                            destination_texture
                        };
                        assert_eq!(bytes.as_ref(), if host_texture { COLOR } else { BUFFER });
                    }
                    // Literal expected outputs, independently selected for each direction/role.
                    let expected: &[u8] = match (direction, prior_produced) {
                        (0, false) | (2, false) => &[17, 83, 149, 231, 105, 106, 107, 108],
                        (0, true) => &[101, 102, 103, 104, 5, 6, 7, 8],
                        (1, false) => &[1, 2, 3, 4, 105, 106, 107, 108, 99],
                        (1, true) => &[101, 102, 103, 104, 31, 47, 61, 79, 109],
                        _ => &[101, 102, 103, 104, 31, 47, 61, 79],
                    };
                    h.complete(1, output(destination_texture, expected));
                    h.complete(
                        0,
                        output(changed_texture, if fail { &[0] } else { seed_bytes }),
                    );
                    if !consumer_first {
                        h.ctx.wait_submission(a).unwrap();
                    }
                    assert_eq!(
                        h.ctx.wait_submission(b).unwrap(),
                        if fail {
                            GraphicsSubmissionStatus::Failed
                        } else {
                            GraphicsSubmissionStatus::Complete
                        }
                    );
                    h.ctx.wait_submission(a).unwrap();
                    if fail {
                        assert!(
                            !h.backend.lock().unwrap().completions[1]
                                .lock()
                                .unwrap()
                                .rejected
                                .is_empty()
                        );
                    }
                    h.ctx.release_submission(b).unwrap();
                    h.ctx.release_submission(a).unwrap();
                    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
                    if !fail {
                        assert_eq!(h.read(destination, expected.len()), expected);
                    } else {
                        h.reset(0);
                        h.reset(1);
                        h.image(0, direction, source, destination, ResourceStates::COPY_DEST);
                        let retry = h.submit(0, &[]).unwrap();
                        let backend = h.backend.lock().unwrap();
                        let GraphicsQueuedWorkRequest::ImageCopy(request) = &backend.requests[2]
                        else {
                            panic!("image")
                        };
                        let GraphicsCopyInput::Host(bytes) = &request.destination else {
                            panic!("rollback host")
                        };
                        assert_eq!(
                            bytes.as_ref(),
                            if destination_texture { COLOR } else { BUFFER }
                        );
                        drop(backend);
                        h.complete(
                            2,
                            output(
                                destination_texture,
                                if destination_texture { COLOR } else { BUFFER },
                            ),
                        );
                        h.ctx.wait_submission(retry).unwrap();
                        h.ctx.release_submission(retry).unwrap();
                    }
                    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
                }
            }
        }
    }
}

#[test]
fn cpu_image_bad_full_outputs_preserve_prior_bytes_and_states() {
    for fault in 0..3 {
        let mut h = Harness::new();
        let source = h.resource(false, BUFFER);
        let destination = h.resource(true, COLOR);
        h.image(0, 0, source, destination, ResourceStates::COPY_DEST);
        let receipt = h.submit(0, &[]).unwrap();
        h.complete(
            0,
            match fault {
                0 => GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
                    colors: vec![vec![0; 8]],
                    depth: None,
                }),
                1 => GraphicsQueuedWorkOutput::Copy {
                    layout: GraphicsCopyLayout::Buffer { byte_len: 8 },
                    bytes: vec![0; 8],
                },
                _ => output(true, &[0; 7]),
            },
        );
        assert_eq!(
            h.ctx.wait_submission(receipt).unwrap(),
            GraphicsSubmissionStatus::Failed
        );
        h.ctx.release_submission(receipt).unwrap();
        h.reset(0);
        h.image(0, 0, source, destination, ResourceStates::COPY_DEST);
        let retry = h.submit(0, &[]).unwrap();
        {
            let backend = h.backend.lock().unwrap();
            let GraphicsQueuedWorkRequest::ImageCopy(request) = &backend.requests[1] else {
                panic!("image")
            };
            let GraphicsCopyInput::Host(bytes) = &request.destination else {
                panic!("host")
            };
            assert_eq!(bytes.as_ref(), COLOR);
        }
        h.complete(1, output(true, &[17, 83, 149, 231, 5, 6, 7, 8]));
        h.ctx.wait_submission(retry).unwrap();
        h.ctx.release_submission(retry).unwrap();
        assert_eq!(h.read(destination, 8), [17, 83, 149, 231, 5, 6, 7, 8]);
        assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    }
}

#[test]
fn cpu_image_wrong_destination_state_and_multiple_commands_reject_before_dispatch() {
    for wrong_state in [false, true] {
        let mut h = Harness::new();
        let source = h.resource(false, BUFFER);
        let destination = h.resource(true, COLOR);
        h.barrier(
            0,
            source,
            ResourceStates::COPY_DEST,
            ResourceStates::COPY_SOURCE,
        );
        if wrong_state {
            h.barrier(
                0,
                destination,
                ResourceStates::COPY_DEST,
                ResourceStates::COPY_SOURCE,
            );
        }
        h.ctx
            .cmd_copy_image(
                h.lists[0],
                destination,
                location(true),
                source,
                location(false),
                (1, 1),
            )
            .unwrap();
        if !wrong_state {
            h.ctx
                .cmd_copy_image(
                    h.lists[0],
                    destination,
                    location(true),
                    source,
                    location(false),
                    (1, 1),
                )
                .unwrap();
        }
        h.ctx.close_command_list(h.lists[0]).unwrap();
        assert!(h.submit(0, &[]).is_err());
        assert!(h.backend.lock().unwrap().requests.is_empty());
        assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
    }
}

#[test]
fn cpu_image_retired_backings_keep_payload_until_release_without_resurrection() {
    let mut h = Harness::new();
    let source = h.resource(true, COLOR);
    let destination = h.resource(false, BUFFER);
    h.image(0, 1, source, destination, ResourceStates::COPY_DEST);
    let receipt = h.submit(0, &[]).unwrap();
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 56);
    assert!(h.ctx.destroy_resource(destination).is_err());
    h.reset(0);
    h.ctx.destroy_resource(source).unwrap();
    h.ctx.destroy_resource(destination).unwrap();
    h.complete(0, output(false, &[1, 2, 3, 4, 31, 47, 61, 79, 99]));
    assert_eq!(
        h.ctx.wait_submission(receipt).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 56);
    h.ctx.release_submission(receipt).unwrap();
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
    assert!(h.ctx.destroy_resource(destination).is_err());
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
}

#[test]
fn cpu_image_downgrade_revokes_image_but_keeps_buffer_regions() {
    let mut h = Harness::new();
    let source = h.resource(false, BUFFER);
    let texture = h.resource(true, COLOR);
    h.image(0, 0, source, texture, ResourceStates::COPY_DEST);
    h.ctx
        .set_metal_buffer_region_queued_backend(GraphicsTextureCapabilities::default(), |work| {
            let GraphicsQueuedWorkRequest::BufferRegion(_) = work else {
                panic!("image dispatched to old installer")
            };
            Ok(Box::new(CpuTicket(Some(output(
                false,
                &[1, 2, 3, 4, 1, 2, 3, 4, 99],
            )))))
        });
    assert!(matches!(
        h.submit(0, &[]),
        Err(GraphicsError::NativeMetalRequired)
    ));
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
    h.reset(0);
    let destination = h.resource(false, BUFFER);
    h.barrier(
        0,
        source,
        ResourceStates::COPY_DEST,
        ResourceStates::COPY_SOURCE,
    );
    h.ctx
        .cmd_copy_buffer_region(h.lists[0], destination, 4, source, 0, 4)
        .unwrap();
    h.barrier(
        0,
        destination,
        ResourceStates::COPY_DEST,
        ResourceStates::COPY_SOURCE,
    );
    h.ctx.close_command_list(h.lists[0]).unwrap();
    let receipt = h.submit(0, &[]).unwrap();
    assert_eq!(h.ctx.submission_point(receipt).unwrap().value, 1);
    h.ctx.wait_submission(receipt).unwrap();
    h.ctx.release_submission(receipt).unwrap();
    assert_eq!(h.read(destination, 9), [1, 2, 3, 4, 1, 2, 3, 4, 99]);
}

#[test]
fn cpu_image_render_color_version_is_valid_for_source_or_prior_destination() {
    for prior_produced in [false, true] {
        let mut h = Harness::new();
        let texture = h.resource(true, COLOR);
        let buffer = h.resource(false, BUFFER);
        h.barrier(
            0,
            texture,
            ResourceStates::COPY_DEST,
            ResourceStates::RENDER_TARGET,
        );
        h.ctx
            .cmd_set_color_targets(h.lists[0], &[texture], &[[101, 102, 103, 104]])
            .unwrap();
        h.barrier(
            0,
            texture,
            ResourceStates::RENDER_TARGET,
            if prior_produced {
                ResourceStates::COPY_DEST
            } else {
                ResourceStates::COPY_SOURCE
            },
        );
        h.ctx.close_command_list(h.lists[0]).unwrap();
        let a = h.submit(0, &[]).unwrap();
        let point = h.ctx.submission_point(a).unwrap();
        if prior_produced {
            h.image(1, 0, buffer, texture, ResourceStates::COPY_DEST);
        } else {
            h.image(1, 1, texture, buffer, ResourceStates::COPY_SOURCE);
        }
        let b = h.submit(1, &[point]).unwrap();
        {
            let backend = h.backend.lock().unwrap();
            let GraphicsQueuedWorkRequest::ImageCopy(request) = &backend.requests[1] else {
                panic!("image")
            };
            let input = if prior_produced {
                &request.destination
            } else {
                &request.source
            };
            let GraphicsCopyInput::Produced(version) = input else {
                panic!("produced color")
            };
            assert_eq!(
                *version,
                GraphicsProducedVersion {
                    point,
                    output: GraphicsProducedOutput::Color(0)
                }
            );
        }
        h.complete(
            0,
            GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
                colors: vec![vec![101, 102, 103, 104, 101, 102, 103, 104]],
                depth: None,
            }),
        );
        let expected: &[u8] = if prior_produced {
            &[101, 102, 103, 104, 5, 6, 7, 8]
        } else {
            &[1, 2, 3, 4, 101, 102, 103, 104, 99]
        };
        h.complete(1, output(prior_produced, expected));
        assert_eq!(
            h.ctx.wait_submission(b).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        h.ctx.release_submission(b).unwrap();
        h.ctx.release_submission(a).unwrap();
        assert_eq!(
            h.read(
                if prior_produced { texture } else { buffer },
                expected.len()
            ),
            expected
        );
        assert_eq!(
            h.ctx.last_presentation.lock().unwrap().frame_index,
            1,
            "only producer render presents"
        );
    }
}
