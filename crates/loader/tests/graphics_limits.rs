// SPDX-License-Identifier: GPL-3.0-or-later

//! CPU-only guest graphics boundary regressions. The callback does no GPU work.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use xenora_loader::cpu::CpuState;
use xenora_loader::graphics::{
    CommandListType, GraphicsContext, GraphicsError, ResourceDesc, ResourceDimension,
    ResourceStates,
};
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::{ImportRegistry, Win64Args};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{GuestMemory, MemoryPermissions};

#[test]
fn overflowing_vertex_binding_is_rejected_without_recording_a_reference() {
    for backing in [None, Some(0x3000)] {
        let mut graphics = GraphicsContext::new();
        let device = graphics.create_device().unwrap();
        let allocator = graphics
            .create_command_allocator(device, CommandListType::Direct)
            .unwrap();
        let list = graphics
            .create_command_list(device, allocator, CommandListType::Direct)
            .unwrap();
        for (offset, stride) in [(usize::MAX, 16), (16, usize::MAX)] {
            let resource = graphics
                .create_committed_resource(
                    device,
                    ResourceDesc {
                        dimension: ResourceDimension::Buffer,
                        width: 48,
                        height: 1,
                        depth_or_array_size: 1,
                        mip_levels: 1,
                        format_rgba8: false,
                    },
                    ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                    backing,
                )
                .unwrap();
            assert_eq!(
                graphics.cmd_set_vertex_buffer(list, resource, offset, stride),
                Err(GraphicsError::InvalidDescriptor)
            );
            // A rejected command must not retain the resource in this recording list.
            graphics.destroy_resource(resource).unwrap();
        }
        graphics.close_command_list(list).unwrap();
    }
}

const DESCRIPTOR: u64 = 0x1000;
const OUTPUT: u64 = 0x2000;
const VERTICES: u64 = 0x3000;
const SENTINEL: u64 = 0x1234_5678_9abc_def0;
const E_FAIL: u64 = 0x8000_4005;

fn try_invoke(
    registry: &ImportRegistry,
    memory: &mut GuestMemory,
    name: &str,
    args: [u64; 4],
) -> Result<u64, xenora_loader::ExitReason> {
    let service = registry
        .resolve("xenora_fixture.dll", &ImportName::Name(name.into()))
        .unwrap();
    (service.handler)(
        &Win64Args {
            rcx: args[0],
            rdx: args[1],
            r8: args[2],
            r9: args[3],
            rsp: 0,
        },
        &mut CpuState::new(0, 0),
        memory,
    )
}

fn invoke(registry: &ImportRegistry, memory: &mut GuestMemory, name: &str, args: [u64; 4]) -> u64 {
    try_invoke(registry, memory, name, args).unwrap()
}

fn create(registry: &ImportRegistry, memory: &mut GuestMemory, versioned: bool) -> u64 {
    if versioned {
        invoke(
            registry,
            memory,
            "GraphicsCallV1",
            [DESCRIPTOR, 128, OUTPUT, 0],
        )
    } else {
        invoke(registry, memory, "CreateGraphicsDevice", [0, 0, 0, OUTPUT])
    }
}

#[test]
fn registered_graphics_imports_share_the_object_cap_and_reserve_whole_draws() {
    // 4091 permits the five-object draw; 4092 must reject before allocating anything.
    for initial_objects in [4091, 4092] {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        state
            .graphics
            .lock()
            .unwrap()
            .set_metal_backend(move |_, (w, h), clear| {
                // CPU-only fake adapter: no native Metal or GPU execution.
                callback_calls.fetch_add(1, Ordering::SeqCst);
                Ok(clear.repeat(w * h))
            });
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, state.clone());
        let mut memory = GuestMemory::new();
        for (address, size) in [(DESCRIPTOR, 128), (OUTPUT, 8), (VERTICES, 48)] {
            memory
                .map_region(address, size, MemoryPermissions::READ_WRITE)
                .unwrap();
        }
        let mut descriptor = [0u8; 128];
        descriptor[..8].copy_from_slice(&1u64.to_le_bytes());
        descriptor[8..16].copy_from_slice(&1u64.to_le_bytes());
        memory.write_bytes(DESCRIPTOR, &descriptor).unwrap();
        let vertices: [f32; 12] = [-1., -1., 0., 1., 1., -1., 0., 1., 0., 1., 0., 1.];
        let bytes: Vec<_> = vertices.iter().flat_map(|v| v.to_le_bytes()).collect();
        memory.write_bytes(VERTICES, &bytes).unwrap();
        for index in 0..initial_objects {
            assert_eq!(create(&registry, &mut memory, index % 2 == 0), 0);
        }
        memory.write_u64(OUTPUT, SENTINEL).unwrap();
        let draw_args = [VERTICES, 0xff03_0201, (3u64 << 32) | 4, OUTPUT];
        let result = invoke(&registry, &mut memory, "SubmitGuestDraw", draw_args);
        let expected_frames = if initial_objects == 4091 {
            assert_eq!(result, 0);
            assert_eq!(memory.read_u64(OUTPUT).unwrap(), 48);
            1
        } else {
            assert_eq!(result, E_FAIL);
            assert_eq!(memory.read_u64(OUTPUT).unwrap(), SENTINEL);
            // All four remaining slots must still be available after the failed draw.
            for index in 0..4 {
                assert_eq!(create(&registry, &mut memory, index % 2 == 0), 0);
            }
            0
        };
        assert_eq!(calls.load(Ordering::SeqCst), expected_frames);
        memory.write_u64(OUTPUT, SENTINEL).unwrap();
        assert_eq!(create(&registry, &mut memory, true), 2);
        assert_eq!(create(&registry, &mut memory, false), E_FAIL);
        assert_eq!(
            invoke(&registry, &mut memory, "SubmitGuestDraw", draw_args),
            E_FAIL
        );
        assert_eq!(memory.read_u64(OUTPUT).unwrap(), SENTINEL);
        assert_eq!(calls.load(Ordering::SeqCst), expected_frames);
        let graphics = state.graphics.lock().unwrap();
        let presentation = graphics.last_presentation.lock().unwrap();
        assert_eq!(presentation.frame_index, expected_frames as u64);
        if expected_frames == 1 {
            assert_eq!(presentation.pixels, [1, 2, 3, 255].repeat(12));
        } else {
            assert!(presentation.pixels.is_empty());
        }
    }
}

#[test]
fn failed_legacy_calls_preserve_capacity_and_prior_presentation() {
    use xenora_loader::ExitReason;

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = calls.clone();
    state
        .graphics
        .lock()
        .unwrap()
        .set_metal_backend(move |_, (w, h), clear| {
            // CPU-only: first submission seeds presentation; subsequent calls fail.
            if callback_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(clear.repeat(w * h))
            } else {
                Err(GraphicsError::BackendError("injected CPU failure".into()))
            }
        });
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state.clone());
    let mut memory = GuestMemory::new();
    for (address, size) in [(DESCRIPTOR, 128), (OUTPUT, 8), (VERTICES, 48), (0x4000, 4)] {
        memory
            .map_region(address, size, MemoryPermissions::READ_WRITE)
            .unwrap();
    }
    memory
        .map_region(0x4004, 4, MemoryPermissions::READ_ONLY)
        .unwrap();
    memory
        .map_region(0x5000, 8, MemoryPermissions::READ_ONLY)
        .unwrap();
    let mut descriptor = [0u8; 128];
    descriptor[..8].copy_from_slice(&1u64.to_le_bytes());
    descriptor[8..16].copy_from_slice(&1u64.to_le_bytes());
    memory.write_bytes(DESCRIPTOR, &descriptor).unwrap();
    let vertices: [f32; 12] = [-1., -1., 0., 1., 1., -1., 0., 1., 0., 1., 0., 1.];
    let bytes: Vec<_> = vertices.iter().flat_map(|v| v.to_le_bytes()).collect();
    memory.write_bytes(VERTICES, &bytes).unwrap();
    let draw = [VERTICES, 0xff03_0201, (3u64 << 32) | 4, OUTPUT];
    assert_eq!(invoke(&registry, &mut memory, "SubmitGuestDraw", draw), 0);
    memory.write_u64(OUTPUT, SENTINEL).unwrap();

    // Unmapped, short, mixed-permission adjacent-region, read-only and overflowing output spans.
    for output in [0x6000, OUTPUT + 1, 0x4000, 0x5000, u64::MAX - 3] {
        assert!(matches!(
            try_invoke(
                &registry,
                &mut memory,
                "CreateGraphicsDevice",
                [0, 0, 0, output]
            ),
            Err(ExitReason::MemoryFault(_))
        ));
        assert!(matches!(
            try_invoke(
                &registry,
                &mut memory,
                "SubmitGuestDraw",
                [draw[0], draw[1], draw[2], output]
            ),
            Err(ExitReason::MemoryFault(_))
        ));
    }
    for vertices in [u64::MAX - 31, VERTICES + 1, 0x6000] {
        assert!(matches!(
            try_invoke(
                &registry,
                &mut memory,
                "SubmitGuestDraw",
                [vertices, draw[1], draw[2], OUTPUT]
            ),
            Err(ExitReason::MemoryFault(_))
        ));
    }
    assert_eq!(
        invoke(
            &registry,
            &mut memory,
            "SubmitGuestDraw",
            [VERTICES, draw[1], (3u64 << 32) | 4097, OUTPUT]
        ),
        0x80070057
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "preflight must precede the backend"
    );
    for _ in 0..2 {
        assert_eq!(
            try_invoke(&registry, &mut memory, "SubmitGuestDraw", draw),
            Err(ExitReason::IllegalInstruction(0xff))
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(memory.read_u64(OUTPUT).unwrap(), SENTINEL);
    {
        let graphics = state.graphics.lock().unwrap();
        let presentation = graphics.last_presentation.lock().unwrap();
        assert_eq!(presentation.frame_index, 1);
        assert_eq!(presentation.pixels, [1, 2, 3, 255].repeat(12));
    }
    // Only the five objects from the successful seed draw remain live.
    for index in 0..4091 {
        assert_eq!(create(&registry, &mut memory, index % 2 == 0), 0);
    }
    assert_eq!(create(&registry, &mut memory, true), 2);
    assert_eq!(create(&registry, &mut memory, false), E_FAIL);
}

#[test]
fn legacy_device_output_accepts_adjacent_writable_regions() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let mut memory = GuestMemory::new();
    memory
        .map_region(OUTPUT, 4, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory
        .map_region(OUTPUT + 4, 4, MemoryPermissions::READ_WRITE)
        .unwrap();
    assert_eq!(
        invoke(
            &registry,
            &mut memory,
            "CreateGraphicsDevice",
            [0, 0, 0, OUTPUT]
        ),
        0
    );
    assert_ne!(memory.read_u64(OUTPUT).unwrap(), 0);
}
