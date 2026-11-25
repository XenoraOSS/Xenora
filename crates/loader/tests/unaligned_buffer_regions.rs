// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-only callbacks verify queued byte-region ownership; no GPU work occurs.
use std::sync::{Arc, Mutex};
use xenora_loader::graphics::*;
use xenora_loader::{GuestMemory, MemoryPermissions};

#[derive(Default)]
struct Completion {
    result: Option<GraphicsQueuedWorkOutput>,
    rejected: bool,
}
struct Ticket(Arc<Mutex<Completion>>);
impl GraphicsQueuedPendingWork for Ticket {
    fn poll(&mut self) -> Option<Result<GraphicsQueuedWorkOutput, GraphicsError>> {
        self.0.lock().unwrap().result.take().map(Ok)
    }
    fn wait(&mut self) -> Result<GraphicsQueuedWorkOutput, GraphicsError> {
        self.poll().expect("controlled CPU completion")
    }
    fn reject_dependency(&mut self, _: GraphicsQueuePoint) {
        self.0.lock().unwrap().rejected = true;
    }
}
#[derive(Default)]
struct Backend {
    requests: Vec<GraphicsQueuedWorkRequest>,
    tickets: Vec<Arc<Mutex<Completion>>>,
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
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 128, MemoryPermissions::READ_WRITE)
            .unwrap();
        let mut h = Self {
            ctx,
            memory,
            device,
            queues,
            allocators,
            lists,
            backend,
        };
        h.install(0);
        h
    }
    fn install(&mut self, mode: usize) {
        let captured = self.backend.clone();
        let callback = move |request| -> Result<Box<dyn GraphicsQueuedPendingWork>, GraphicsError> {
            let ticket = Arc::new(Mutex::new(Completion::default()));
            let mut backend = captured.lock().unwrap();
            backend.requests.push(request);
            backend.tickets.push(ticket.clone());
            Ok(Box::new(Ticket(ticket)))
        };
        match mode {
            0 => self.ctx.set_metal_unaligned_buffer_region_queued_backend(
                GraphicsTextureCapabilities::default(),
                callback,
            ),
            1 => self.ctx.set_metal_buffer_region_queued_backend(
                GraphicsTextureCapabilities::default(),
                callback,
            ),
            2 => self.ctx.set_metal_image_copy_queued_backend(
                GraphicsTextureCapabilities::default(),
                callback,
            ),
            _ => self
                .ctx
                .set_metal_copy_queued_backend(GraphicsTextureCapabilities::default(), callback),
        }
    }
    fn resource(&mut self, bytes: &[u8]) -> GraphicsHandle {
        let resource = self
            .ctx
            .create_committed_resource(
                self.device,
                ResourceDesc {
                    dimension: ResourceDimension::Buffer,
                    width: bytes.len() as u64,
                    height: 1,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: false,
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
        handle: GraphicsHandle,
        before: ResourceStates,
        after: ResourceStates,
    ) {
        self.ctx
            .cmd_resource_barrier(self.lists[slot], handle, before, after)
            .unwrap();
    }
    fn region(
        &mut self,
        slot: usize,
        source: GraphicsHandle,
        destination: GraphicsHandle,
        offsets: (usize, usize, usize),
        source_ready: bool,
    ) {
        if !source_ready {
            self.barrier(
                slot,
                source,
                ResourceStates::COPY_DEST,
                ResourceStates::COPY_SOURCE,
            );
        }
        self.ctx
            .cmd_copy_buffer_region(
                self.lists[slot],
                destination,
                offsets.1,
                source,
                offsets.0,
                offsets.2,
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
    fn submit(
        &mut self,
        slot: usize,
        waits: &[GraphicsQueuePoint],
    ) -> Result<GraphicsHandle, GraphicsError> {
        self.ctx
            .submit_command_list_after(self.queues[slot], self.lists[slot], waits, &self.memory)
    }
    fn complete(&self, index: usize, bytes: &[u8]) {
        self.backend.lock().unwrap().tickets[index]
            .lock()
            .unwrap()
            .result = Some(GraphicsQueuedWorkOutput::Copy {
            layout: GraphicsCopyLayout::Buffer {
                byte_len: bytes.len(),
            },
            bytes: bytes.to_vec(),
        });
    }
    fn reset(&mut self, slot: usize) {
        self.ctx
            .reset_command_list(self.lists[slot], self.allocators[slot])
            .unwrap();
    }
    fn read(&mut self, handle: GraphicsHandle) -> Vec<u8> {
        self.ctx
            .copy_resource_to_guest(handle, 0, 0x1000, 9, &mut self.memory)
            .unwrap();
        let mut bytes = vec![0; 9];
        self.memory.read_bytes(0x1000, &mut bytes).unwrap();
        bytes
    }
}
const SOURCE: &[u8] = &[17, 83, 149, 231, 9, 31, 47, 61, 79];
const PRIOR: &[u8] = &[101, 102, 103, 104, 105, 106, 107, 108, 109];

#[test]
fn cpu_byte_regions_literal_crossword_and_last_logical_byte() {
    for (offsets, expected) in [
        ((1, 3, 5), [101, 102, 103, 83, 149, 231, 9, 31, 109]),
        ((8, 8, 1), [101, 102, 103, 104, 105, 106, 107, 108, 79]),
        ((0, 0, 1), [17, 102, 103, 104, 105, 106, 107, 108, 109]),
        ((2, 1, 2), [101, 149, 231, 104, 105, 106, 107, 108, 109]),
        ((3, 2, 3), [101, 102, 231, 9, 31, 106, 107, 108, 109]),
    ] {
        let mut h = Harness::new();
        let source = h.resource(SOURCE);
        let destination = h.resource(PRIOR);
        h.region(0, source, destination, offsets, false);
        let receipt = h.submit(0, &[]).unwrap();
        assert_eq!(
            h.ctx.queued_copy_allocation_bytes(),
            88,
            "2D + 2S + 2D + 16 kernel parameters"
        );
        {
            let backend = h.backend.lock().unwrap();
            let GraphicsQueuedWorkRequest::BufferRegion(request) = &backend.requests[0] else {
                panic!("region")
            };
            assert_eq!(
                (
                    request.source_offset,
                    request.destination_offset,
                    request.byte_len
                ),
                offsets
            );
            let GraphicsCopyInput::Host(source) = &request.source else {
                panic!("host")
            };
            let GraphicsCopyInput::Host(prior) = &request.destination else {
                panic!("host")
            };
            assert_eq!(source.as_ref(), SOURCE);
            assert_eq!(prior.as_ref(), PRIOR);
        }
        h.complete(0, &expected);
        h.ctx.wait_submission(receipt).unwrap();
        h.ctx.release_submission(receipt).unwrap();
        assert_eq!(h.read(destination), expected);
        assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
        assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    }
}

#[test]
fn cpu_byte_region_old_installers_revoke_capability_without_dispatch() {
    for mode in 1..=3 {
        let mut h = Harness::new();
        let source = h.resource(SOURCE);
        let destination = h.resource(PRIOR);
        h.region(0, source, destination, (1, 3, 5), false);
        h.install(mode);
        let marker = h.ctx.create_device().unwrap();
        assert!(matches!(
            h.submit(0, &[]),
            Err(GraphicsError::NativeMetalRequired)
        ));
        assert_eq!(h.ctx.create_device().unwrap().id, marker.id + 1);
        assert!(h.backend.lock().unwrap().requests.is_empty());
        assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
        h.reset(0);
        if mode == 3 {
            h.barrier(
                0,
                source,
                ResourceStates::COPY_DEST,
                ResourceStates::COPY_SOURCE,
            );
            h.ctx
                .cmd_copy_resource(h.lists[0], destination, source)
                .unwrap();
            h.barrier(
                0,
                destination,
                ResourceStates::COPY_DEST,
                ResourceStates::COPY_SOURCE,
            );
            h.ctx.close_command_list(h.lists[0]).unwrap();
        } else {
            h.region(0, source, destination, (0, 4, 4), false);
        }
        let receipt = h.submit(0, &[]).unwrap();
        assert_eq!(h.ctx.submission_point(receipt).unwrap().value, 1);
        assert_eq!(
            h.ctx.queued_copy_allocation_bytes(),
            if mode == 3 { 48 } else { 72 }
        );
        let expected: &[u8] = if mode == 3 {
            SOURCE
        } else {
            &[101, 102, 103, 104, 17, 83, 149, 231, 109]
        };
        h.complete(0, expected);
        h.ctx.wait_submission(receipt).unwrap();
        h.ctx.release_submission(receipt).unwrap();
        assert_eq!(h.read(destination), expected);
    }
}

#[test]
fn cpu_byte_region_each_produced_input_orders_and_cached_failed_ancestor() {
    for prior_produced in [false, true] {
        for consumer_first in [false, true] {
            for fail in [false, true] {
                let mut h = Harness::new();
                let source = h.resource(SOURCE);
                let destination = h.resource(PRIOR);
                let seed_bytes = [21, 22, 23, 24, 25, 26, 27, 28, 29];
                let seed = h.resource(&seed_bytes);
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
                h.region(1, source, destination, (1, 3, 5), !prior_produced);
                assert!(h.submit(1, &[]).is_err());
                assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
                let b = h.submit(1, &[point]).unwrap();
                assert_eq!(h.ctx.queued_copy_allocation_bytes(), 112); // 48 producer + 64 mixed region.
                {
                    let backend = h.backend.lock().unwrap();
                    let GraphicsQueuedWorkRequest::BufferRegion(request) = &backend.requests[1]
                    else {
                        panic!("region")
                    };
                    let input = if prior_produced {
                        &request.destination
                    } else {
                        &request.source
                    };
                    let GraphicsCopyInput::Produced(version) = input else {
                        panic!("exact producer")
                    };
                    assert_eq!(
                        *version,
                        GraphicsProducedVersion {
                            point,
                            output: GraphicsProducedOutput::Copy(0)
                        }
                    );
                    assert_eq!(request.waits, [point]);
                }
                let expected: &[u8] = if prior_produced {
                    &[21, 22, 23, 83, 149, 231, 9, 31, 29]
                } else {
                    &[101, 102, 103, 22, 23, 24, 25, 26, 109]
                };
                h.complete(1, expected);
                h.complete(0, if fail { &[0] } else { &seed_bytes });
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
                        h.backend.lock().unwrap().tickets[1]
                            .lock()
                            .unwrap()
                            .rejected
                    );
                }
                h.ctx.release_submission(b).unwrap();
                h.ctx.release_submission(a).unwrap();
                assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
                if !fail {
                    assert_eq!(h.read(destination), expected);
                } else {
                    h.reset(0);
                    h.reset(1);
                    h.region(0, source, destination, (8, 8, 1), false);
                    let retry = h.submit(0, &[]).unwrap();
                    {
                        let backend = h.backend.lock().unwrap();
                        let GraphicsQueuedWorkRequest::BufferRegion(request) = &backend.requests[2]
                        else {
                            panic!("region")
                        };
                        let GraphicsCopyInput::Host(prior) = &request.destination else {
                            panic!("rolled back host")
                        };
                        assert_eq!(prior.as_ref(), PRIOR);
                    }
                    h.complete(2, &[101, 102, 103, 104, 105, 106, 107, 108, 79]);
                    h.ctx.wait_submission(retry).unwrap();
                    h.ctx.release_submission(retry).unwrap();
                }
                assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
            }
        }
    }
}

#[test]
fn cpu_byte_region_logical_bounds_alias_and_retirement() {
    let mut h = Harness::new();
    let source = h.resource(SOURCE);
    let destination = h.resource(PRIOR);
    for (src, dst, len) in [
        (9, 0, 1),
        (0, 9, 1),
        (usize::MAX, 0, 1),
        (0, usize::MAX, 1),
        (0, 0, 0),
    ] {
        assert!(
            h.ctx
                .cmd_copy_buffer_region(h.lists[0], destination, dst, source, src, len)
                .is_err()
        );
    }
    assert!(
        h.ctx
            .cmd_copy_buffer_region(h.lists[0], source, 1, source, 0, 1)
            .is_err()
    );
    h.region(0, source, destination, (8, 8, 1), false);
    let receipt = h.submit(0, &[]).unwrap();
    assert!(h.ctx.destroy_resource(destination).is_err());
    h.reset(0);
    h.ctx.destroy_resource(source).unwrap();
    h.ctx.destroy_resource(destination).unwrap();
    h.complete(0, &[101, 102, 103, 104, 105, 106, 107, 108, 79]);
    assert_eq!(
        h.ctx.wait_submission(receipt).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 88);
    h.ctx.release_submission(receipt).unwrap();
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
    assert!(h.ctx.destroy_resource(destination).is_err());
}
