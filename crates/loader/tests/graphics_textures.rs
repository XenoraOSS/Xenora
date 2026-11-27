// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest draws that sample a texture bound through D3D12-style descriptor
//! heaps, a root signature and descriptor tables (#303).
//!
//! Every fixture is synthetic. The expected images are written out by hand
//! from the texel data and the draw geometry, so they depend on neither the
//! software reference nor the native Metal path.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use xenora_loader::graphics::{
    CommandListType, D3D12_TEXTURE_ADDRESS_MODE_CLAMP, DXGI_FORMAT_BC1_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DescriptorRange, DescriptorRangeType, ExecutionOutcome,
    GraphicsBackendId, GraphicsContext, GraphicsError, GraphicsHandle, ResourceDesc,
    ResourceDimension, ResourceStates, RootParameter, RootSignatureDesc, SamplerDesc,
    ShaderResourceViewDesc, ShaderVisibility, SrvDimension, TextureFormat,
};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const CBV_SRV_UAV: u32 = 0;
const SAMPLER: u32 = 1;
const VB_ADDR: u64 = 0x10_0000;
const INDEX_ADDR: u64 = 0x18_0000;
const R16_UINT: u32 = 57;
const R32_UINT: u32 = 42;
const TEX_ADDR: u64 = 0x20_0000;
const STRIDE: usize = 32;

/// Texel `(x, y)` of the 4×4 texture A is `[64x + 16, 64y + 16, 200, 255]`.
fn texture_a() -> Vec<u8> {
    (0..16u8)
        .flat_map(|i| [(i % 4) * 64 + 16, (i / 4) * 64 + 16, 200, 255])
        .collect()
}

/// Texel `(x, y)` of the 4×4 texture B is `[7, 11x + 3, 13y + 5, 255]`.
fn texture_b() -> Vec<u8> {
    (0..16u8)
        .flat_map(|i| [7, (i % 4) * 11 + 3, (i / 4) * 13 + 5, 255])
        .collect()
}

/// An 8×8 BC1 texture of four row-major blocks. Each block selects only its
/// two endpoints (selector 0 or 1), whose RGB565 words expand exactly to 0
/// or 255 per channel. Interpolated palette entries are left out: Apple's
/// hardware decoder rounds them differently from the spec formula.
/// Each block uses selector 1 where `x == 0 || y == 3` within the block.
const BC1_BLOCKS: [[u8; 8]; 4] = [
    // Endpoint 0 red (0xF800), endpoint 1 blue (0x001F).
    [0x00, 0xF8, 0x1F, 0x00, 0x01, 0x01, 0x01, 0x55],
    // Green (0x07E0), black (0x0000).
    [0xE0, 0x07, 0x00, 0x00, 0x01, 0x01, 0x01, 0x55],
    // White (0xFFFF), magenta (0xF81F).
    [0xFF, 0xFF, 0x1F, 0xF8, 0x01, 0x01, 0x01, 0x55],
    // Yellow (0xFFE0), cyan (0x07FF).
    [0xE0, 0xFF, 0xFF, 0x07, 0x01, 0x01, 0x01, 0x55],
];

/// The BC1 texture decoded by hand, row-major RGBA8.
fn bc1_expected() -> Vec<u8> {
    let endpoints = [
        [[255, 0, 0, 255], [0, 0, 255, 255]],
        [[0, 255, 0, 255], [0, 0, 0, 255]],
        [[255, 255, 255, 255], [255, 0, 255, 255]],
        [[255, 255, 0, 255], [0, 255, 255, 255]],
    ];
    let mut texels = Vec::new();
    for y in 0..8 {
        for x in 0..8 {
            let block = (y / 4) * 2 + x / 4;
            let selector = usize::from(x % 4 == 0 || y % 4 == 3);
            texels.extend(endpoints[block][selector]);
        }
    }
    texels
}

/// A fullscreen triangle: position float4, then texture coordinate float4.
/// Over an N×N viewport the pixel centre `(x + 0.5, y + 0.5)` gets
/// `uv = ((x + 0.5) / N, (y + 0.5) / N)`, so pixel `(x, y)` of an N×N
/// texture samples texel `(x, y)` and the image equals the texture.
const VERTICES: [[f32; 8]; 3] = [
    [-1.0, -1.0, 0.5, 1.0, 0.0, 1.0, 0.0, 1.0],
    [3.0, -1.0, 0.5, 1.0, 2.0, 1.0, 0.0, 1.0],
    [-1.0, 3.0, 0.5, 1.0, 0.0, -1.0, 0.0, 1.0],
];

/// The same triangle with `uv' = 2·uv − 0.6`: pixel centres sample
/// u (and v) = −0.35, 0.15, 0.65, 1.15, off texel centres and outside
/// [0, 1] on both sides. Point sampling with clamping reads texels
/// 0, 0, 2, 3 along each axis; wrapping or linear filtering would not.
const CLAMP_VERTICES: [[f32; 8]; 3] = [
    [-1.0, -1.0, 0.5, 1.0, -0.6, 1.4, 0.0, 1.0],
    [3.0, -1.0, 0.5, 1.0, 3.4, 1.4, 0.0, 1.0],
    [-1.0, 3.0, 0.5, 1.0, -0.6, -2.6, 0.0, 1.0],
];
const CLAMPED_TEXEL: [usize; 4] = [0, 0, 2, 3];

fn pixel_table(range_type: DescriptorRangeType) -> RootParameter {
    RootParameter::DescriptorTable {
        ranges: vec![DescriptorRange {
            range_type,
            num_descriptors: 1,
            base_shader_register: 0,
            register_space: 0,
            offset_in_descriptors_from_table_start: 0,
        }],
        visibility: ShaderVisibility::Pixel,
    }
}

fn root_signature_desc() -> RootSignatureDesc {
    RootSignatureDesc {
        parameters: vec![
            pixel_table(DescriptorRangeType::Srv),
            pixel_table(DescriptorRangeType::Sampler),
        ],
        static_sampler_count: 0,
    }
}

fn srv_desc(format: u32) -> ShaderResourceViewDesc {
    ShaderResourceViewDesc {
        format,
        view_dimension: SrvDimension::Texture2D,
        most_detailed_mip: 0,
        mip_levels: 1,
    }
}

struct Rig {
    ctx: GraphicsContext,
    memory: GuestMemory,
    device: GraphicsHandle,
    queue: GraphicsHandle,
    alloc: GraphicsHandle,
    list: GraphicsHandle,
    vb: GraphicsHandle,
    srv_heap: GraphicsHandle,
    sampler_heap: GraphicsHandle,
    root_signature: GraphicsHandle,
    /// Sampler table at root parameter 0, SRV table at 1.
    swapped_root_signature: GraphicsHandle,
}

/// SRV heap slots: 0 texture A (4×4), 1 texture B (4×4), 2 the 8×8 BC1
/// texture, 3 empty.
/// Sampler heap slots: 0 and 2 point/clamp, 1 empty. Every supported
/// sampler has the same state, so slots differ only in being filled.
fn rig() -> Rig {
    let mut ctx = GraphicsContext::new();
    let device = ctx.create_device().unwrap();
    let queue = ctx
        .create_command_queue(device, CommandListType::Direct)
        .unwrap();
    let alloc = ctx
        .create_command_allocator(device, CommandListType::Direct)
        .unwrap();
    let list = ctx
        .create_command_list(device, alloc, CommandListType::Direct)
        .unwrap();
    // Records 0–2 are VERTICES and 3–5 CLAMP_VERTICES.
    let vertex_bytes: Vec<u8> = VERTICES
        .as_flattened()
        .iter()
        .chain(CLAMP_VERTICES.as_flattened())
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let vb = ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: vertex_bytes.len() as u64,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(VB_ADDR),
        )
        .unwrap();
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(VB_ADDR, 0x1000, MemoryPermissions::READ_ONLY, &vertex_bytes)
        .unwrap();

    let textures = [
        (DXGI_FORMAT_R8G8B8A8_UNORM, texture_a()),
        (DXGI_FORMAT_R8G8B8A8_UNORM, texture_b()),
        (DXGI_FORMAT_BC1_UNORM, BC1_BLOCKS.as_flattened().to_vec()),
    ];
    let srv_heap = ctx
        .create_descriptor_heap(device, CBV_SRV_UAV, 4, true)
        .unwrap();
    let srv_start = ctx.cpu_descriptor_handle_for_heap_start(srv_heap).unwrap();
    for (slot, (format, bytes)) in (0u32..).zip(&textures) {
        let addr = TEX_ADDR + u64::from(slot) * 0x1000;
        memory
            .map_region_with_data(addr, 0x1000, MemoryPermissions::READ_ONLY, bytes)
            .unwrap();
        let size = if *format == DXGI_FORMAT_BC1_UNORM {
            8
        } else {
            4
        };
        let texture = ctx
            .create_committed_texture2d(
                device,
                size,
                size,
                *format,
                ResourceStates::COMMON,
                Some(addr),
            )
            .unwrap();
        ctx.create_shader_resource_view(
            texture,
            &srv_desc(*format),
            srv_start.offset(slot).unwrap(),
        )
        .unwrap();
    }
    let sampler_heap = ctx
        .create_descriptor_heap(device, SAMPLER, 3, true)
        .unwrap();
    let sampler_start = ctx
        .cpu_descriptor_handle_for_heap_start(sampler_heap)
        .unwrap();
    for slot in [0, 2] {
        ctx.create_sampler(
            &SamplerDesc::point_clamp(),
            sampler_start.offset(slot).unwrap(),
        )
        .unwrap();
    }
    let root_signature = ctx
        .create_root_signature(device, &root_signature_desc())
        .unwrap();
    let swapped_root_signature = ctx
        .create_root_signature(
            device,
            &RootSignatureDesc {
                parameters: vec![
                    pixel_table(DescriptorRangeType::Sampler),
                    pixel_table(DescriptorRangeType::Srv),
                ],
                static_sampler_count: 0,
            },
        )
        .unwrap();
    Rig {
        ctx,
        memory,
        device,
        queue,
        alloc,
        list,
        vb,
        srv_heap,
        sampler_heap,
        root_signature,
        swapped_root_signature,
    }
}

/// What one textured draw binds; `None` leaves that table unset.
#[derive(Clone, Copy)]
struct Binding {
    srv_slot: Option<u32>,
    sampler_slot: Option<u32>,
    heaps_bound: (bool, bool),
    stride: usize,
    viewport: u32,
    first_vertex: u32,
    swapped: bool,
    /// Vertex count, instance count, start instance.
    draw: (u32, u32, u32),
}

const SLOT_0: Binding = Binding {
    srv_slot: Some(0),
    sampler_slot: Some(0),
    heaps_bound: (true, true),
    stride: STRIDE,
    viewport: 4,
    first_vertex: 0,
    swapped: false,
    draw: (3, 1, 0),
};

/// Slot 2 holds the 8×8 BC1 texture and is drawn into an 8×8 viewport.
fn srv(slot: u32) -> Binding {
    Binding {
        srv_slot: Some(slot),
        viewport: if slot == 2 { 8 } else { 4 },
        ..SLOT_0
    }
}

/// Record and run: the binding's viewport, heaps, root signature, the two
/// tables (SRV at root parameter 0 and sampler at 1, or swapped).
fn record_binding(rig: &mut Rig, binding: Binding) -> Result<(), GraphicsError> {
    let ctx = &mut rig.ctx;
    ctx.reset_command_list(rig.list, rig.alloc)?;
    let size = binding.viewport as usize;
    ctx.cmd_set_viewport(rig.list, size, size)?;
    ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 0, binding.stride)?;
    let mut heaps = Vec::new();
    if binding.heaps_bound.0 {
        heaps.push(rig.srv_heap);
    }
    if binding.heaps_bound.1 {
        heaps.push(rig.sampler_heap);
    }
    ctx.cmd_set_descriptor_heaps(rig.list, &heaps)?;
    let (root_signature, srv_parameter, sampler_parameter) = if binding.swapped {
        (rig.swapped_root_signature, 1, 0)
    } else {
        (rig.root_signature, 0, 1)
    };
    ctx.cmd_set_graphics_root_signature(rig.list, root_signature)?;
    if let Some(slot) = binding.srv_slot {
        let base = ctx.gpu_descriptor_handle_for_heap_start(rig.srv_heap)?;
        let base = base.offset(slot).unwrap();
        ctx.cmd_set_graphics_root_descriptor_table(rig.list, srv_parameter, base)?;
    }
    if let Some(slot) = binding.sampler_slot {
        let base = ctx.gpu_descriptor_handle_for_heap_start(rig.sampler_heap)?;
        let base = base.offset(slot).unwrap();
        ctx.cmd_set_graphics_root_descriptor_table(rig.list, sampler_parameter, base)?;
    }
    Ok(())
}

fn run(rig: &mut Rig, binding: Binding) -> Result<ExecutionOutcome, GraphicsError> {
    record_binding(rig, binding)?;
    let ctx = &mut rig.ctx;
    let (vertices, instances, start_instance) = binding.draw;
    ctx.cmd_draw_instanced(
        rig.list,
        vertices,
        instances,
        binding.first_vertex,
        start_instance,
    )?;
    ctx.close_command_list(rig.list)?;
    ctx.execute_command_list(rig.queue, rig.list, &rig.memory)
}

#[test]
fn software_textured_draw_reproduces_rgba8_texels() {
    let mut rig = rig();
    let outcome = run(&mut rig, SLOT_0).expect("textured draw");
    assert_eq!(outcome.backend, GraphicsBackendId::SoftwareReference);
    assert_eq!(outcome.draw_count, 1);
    assert_eq!((outcome.width, outcome.height), (4, 4));
    assert_eq!(outcome.pixels, texture_a());
}

/// The draw reads the descriptor the table names: slot 1 gives texture B,
/// which differs from texture A in every texel.
#[test]
fn descriptor_table_offset_selects_the_texture() {
    let mut rig = rig();
    let outcome = run(&mut rig, srv(1)).expect("textured draw");
    assert_eq!(outcome.pixels, texture_b());
    for (a, b) in texture_a().chunks(4).zip(texture_b().chunks(4)) {
        assert_ne!(a, b);
    }
}

#[test]
fn software_textured_draw_decodes_bc1() {
    let mut rig = rig();
    let outcome = run(&mut rig, srv(2)).expect("BC1 textured draw");
    assert_eq!((outcome.width, outcome.height), (8, 8));
    assert_eq!(outcome.pixels, bc1_expected());
    // The shared BC1 decoder agrees with the hand-decoded blocks.
    let expected = bc1_expected();
    for (index, block) in BC1_BLOCKS.into_iter().enumerate() {
        let (bx, by) = (index % 2 * 4, index / 2 * 4);
        for (i, texel) in xenora_texture::decode_bc1(block).iter().enumerate() {
            let at = ((by + i / 4) * 8 + bx + i % 4) * 4;
            assert_eq!(texel, &expected[at..at + 4], "block {index} texel {i}");
        }
    }
}

#[test]
fn unsupported_objects_fail_at_creation() {
    let mut rig = rig();
    let ctx = &mut rig.ctx;
    let device = rig.device;
    let unsupported = |result: Result<(), GraphicsError>, case: &str| {
        assert!(
            matches!(result, Err(GraphicsError::Unsupported(_))),
            "{case}: {result:?}"
        );
    };

    // Descriptor heaps: RTV and DSV types, CPU-only heaps, capacity.
    for (heap_type, count, visible, case) in [
        (2, 4, true, "RTV heap"),
        (3, 4, true, "DSV heap"),
        (CBV_SRV_UAV, 4, false, "non-shader-visible heap"),
        (CBV_SRV_UAV, 0, true, "empty heap"),
        (SAMPLER, 65, true, "heap over capacity"),
    ] {
        unsupported(
            ctx.create_descriptor_heap(device, heap_type, count, visible)
                .map(drop),
            case,
        );
    }

    // Textures: other formats and BC1 sizes that are not whole blocks.
    unsupported(
        ctx.create_committed_texture2d(device, 4, 4, 2, ResourceStates::COMMON, None)
            .map(drop),
        "R32G32B32A32_FLOAT texture",
    );
    unsupported(
        ctx.create_committed_texture2d(
            device,
            6,
            6,
            DXGI_FORMAT_BC1_UNORM,
            ResourceStates::COMMON,
            None,
        )
        .map(drop),
        "6x6 BC1 texture",
    );
    unsupported(
        ctx.create_committed_texture2d(
            device,
            65,
            4,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            ResourceStates::COMMON,
            None,
        )
        .map(drop),
        "texture over the size limit",
    );

    // Shader resource views.
    let texture = ctx
        .create_committed_texture2d(
            device,
            4,
            4,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            ResourceStates::COMMON,
            None,
        )
        .unwrap();
    let dest = ctx
        .cpu_descriptor_handle_for_heap_start(rig.srv_heap)
        .unwrap();
    let base = srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM);
    for (desc, case) in [
        (
            srv_desc(DXGI_FORMAT_BC1_UNORM),
            "format differs from resource",
        ),
        (srv_desc(10), "R16G16B16A16_FLOAT view"),
        (
            ShaderResourceViewDesc {
                view_dimension: SrvDimension::Texture3D,
                ..base
            },
            "Texture3D view",
        ),
        (
            ShaderResourceViewDesc {
                mip_levels: 2,
                ..base
            },
            "two-level view",
        ),
    ] {
        unsupported(ctx.create_shader_resource_view(texture, &desc, dest), case);
    }

    // Samplers.
    let dest = ctx
        .cpu_descriptor_handle_for_heap_start(rig.sampler_heap)
        .unwrap();
    let point = SamplerDesc::point_clamp();
    for (desc, case) in [
        (
            SamplerDesc {
                filter: 0x15,
                ..point
            },
            "linear filter",
        ),
        (
            SamplerDesc {
                address_v: 1,
                ..point
            },
            "wrap addressing",
        ),
        (
            SamplerDesc {
                mip_lod_bias: 0.5,
                ..point
            },
            "LOD bias",
        ),
    ] {
        unsupported(ctx.create_sampler(&desc, dest), case);
    }
    assert_eq!(point.address_u, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);

    // Root signatures.
    let table = |range_type, visibility| RootParameter::DescriptorTable {
        ranges: vec![DescriptorRange {
            range_type,
            num_descriptors: 1,
            base_shader_register: 0,
            register_space: 0,
            offset_in_descriptors_from_table_start: 0,
        }],
        visibility,
    };
    let srv_table = || pixel_table(DescriptorRangeType::Srv);
    let sampler_table = || pixel_table(DescriptorRangeType::Sampler);
    let two_ranges = RootParameter::DescriptorTable {
        ranges: vec![
            DescriptorRange {
                range_type: DescriptorRangeType::Srv,
                num_descriptors: 1,
                base_shader_register: 0,
                register_space: 0,
                offset_in_descriptors_from_table_start: 0,
            };
            2
        ],
        visibility: ShaderVisibility::Pixel,
    };
    let wide_range = RootParameter::DescriptorTable {
        ranges: vec![DescriptorRange {
            range_type: DescriptorRangeType::Srv,
            num_descriptors: 2,
            base_shader_register: 0,
            register_space: 0,
            offset_in_descriptors_from_table_start: 0,
        }],
        visibility: ShaderVisibility::Pixel,
    };
    for (parameters, static_sampler_count, case) in [
        (
            vec![srv_table(), sampler_table(), srv_table()],
            0,
            "three tables",
        ),
        (vec![srv_table(), srv_table()], 0, "two SRV tables"),
        (
            vec![
                table(DescriptorRangeType::Uav, ShaderVisibility::Pixel),
                sampler_table(),
            ],
            0,
            "UAV range",
        ),
        (
            vec![
                table(DescriptorRangeType::Cbv, ShaderVisibility::Pixel),
                sampler_table(),
            ],
            0,
            "CBV range",
        ),
        (
            vec![
                table(DescriptorRangeType::Srv, ShaderVisibility::Vertex),
                sampler_table(),
            ],
            0,
            "vertex-stage table",
        ),
        (
            vec![
                srv_table(),
                table(DescriptorRangeType::Sampler, ShaderVisibility::All),
            ],
            0,
            "all-stage table",
        ),
        (
            vec![
                RootParameter::Constants32Bit {
                    visibility: ShaderVisibility::Pixel,
                },
                sampler_table(),
            ],
            0,
            "root constants",
        ),
        (
            vec![two_ranges, sampler_table()],
            0,
            "two ranges in a table",
        ),
        (vec![wide_range, sampler_table()], 0, "two-descriptor range"),
        (vec![srv_table(), sampler_table()], 1, "static sampler"),
        (vec![srv_table()], 0, "SRV table only"),
    ] {
        let desc = RootSignatureDesc {
            parameters,
            static_sampler_count,
        };
        unsupported(ctx.create_root_signature(device, &desc).map(drop), case);
    }
    // The supported shape also works with its tables in the other order.
    let swapped = RootSignatureDesc {
        parameters: vec![sampler_table(), srv_table()],
        static_sampler_count: 0,
    };
    ctx.create_root_signature(device, &swapped).unwrap();
}

/// Bindings that resolve to nothing, or to the wrong kind of descriptor,
/// fail at execution before the backend is called.
#[test]
fn bad_bindings_fail_and_never_draw() {
    let mut rig = rig();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    rig.ctx.set_metal_textured_backend(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(vec![0; 64])
    });
    for (binding, case) in [
        (
            Binding {
                srv_slot: None,
                ..SLOT_0
            },
            "SRV table unset",
        ),
        (
            Binding {
                sampler_slot: None,
                ..SLOT_0
            },
            "sampler table unset",
        ),
        (srv(3), "empty SRV slot"),
        (srv(4), "SRV slot past the heap"),
        (
            Binding {
                sampler_slot: Some(1),
                ..SLOT_0
            },
            "empty sampler slot",
        ),
        (
            Binding {
                heaps_bound: (true, false),
                ..SLOT_0
            },
            "sampler heap not bound",
        ),
        (
            Binding {
                heaps_bound: (false, true),
                ..SLOT_0
            },
            "SRV heap not bound",
        ),
        (
            Binding {
                stride: 16,
                ..SLOT_0
            },
            "vertex stride without a texture coordinate",
        ),
    ] {
        let result = run(&mut rig, binding);
        assert_eq!(result, Err(GraphicsError::InvalidDescriptor), "{case}");
    }

    // A table whose base lies in the other heap, or in the CPU handle space.
    let ctx = &mut rig.ctx;
    let sampler_base = ctx
        .gpu_descriptor_handle_for_heap_start(rig.sampler_heap)
        .unwrap();
    let srv_base = ctx
        .gpu_descriptor_handle_for_heap_start(rig.srv_heap)
        .unwrap();
    let cpu_base = ctx
        .cpu_descriptor_handle_for_heap_start(rig.srv_heap)
        .unwrap();
    let misaligned = xenora_loader::graphics::GpuDescriptorHandle(srv_base.0 + 4);
    for (srv_table, case) in [
        (sampler_base, "SRV table in the sampler heap"),
        (
            xenora_loader::graphics::GpuDescriptorHandle(cpu_base.0),
            "CPU handle as table base",
        ),
        (misaligned, "misaligned table base"),
    ] {
        ctx.reset_command_list(rig.list, rig.alloc).unwrap();
        ctx.cmd_set_viewport(rig.list, 4, 4).unwrap();
        ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 0, STRIDE)
            .unwrap();
        ctx.cmd_set_descriptor_heaps(rig.list, &[rig.srv_heap, rig.sampler_heap])
            .unwrap();
        ctx.cmd_set_graphics_root_signature(rig.list, rig.root_signature)
            .unwrap();
        ctx.cmd_set_graphics_root_descriptor_table(rig.list, 0, srv_table)
            .unwrap();
        ctx.cmd_set_graphics_root_descriptor_table(rig.list, 1, sampler_base)
            .unwrap();
        ctx.cmd_draw_instanced(rig.list, 3, 1, 0, 0).unwrap();
        ctx.close_command_list(rig.list).unwrap();
        let result = ctx.execute_command_list(rig.queue, rig.list, &rig.memory);
        assert_eq!(result, Err(GraphicsError::InvalidDescriptor), "{case}");
    }

    // Two heaps of one type in one SetDescriptorHeaps call.
    ctx.reset_command_list(rig.list, rig.alloc).unwrap();
    assert_eq!(
        ctx.cmd_set_descriptor_heaps(rig.list, &[rig.srv_heap, rig.srv_heap]),
        Err(GraphicsError::InvalidDescriptor)
    );

    assert_eq!(calls.load(Ordering::SeqCst), 0, "the backend never drew");
    // The same rig draws once its binding is valid.
    run(&mut rig, SLOT_0).expect("valid binding draws");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Off-centre and out-of-range coordinates: point sampling with clamping
/// gives pixel `(x, y)` texel `(CLAMPED_TEXEL[x], CLAMPED_TEXEL[y])`. The
/// draw starts at vertex record 3.
#[test]
fn point_clamp_sampling_off_centre_and_outside_unit_range() {
    let mut rig = rig();
    let outcome = run(
        &mut rig,
        Binding {
            first_vertex: 3,
            ..SLOT_0
        },
    )
    .expect("textured draw");
    assert_eq!(outcome.pixels, clamped_pixels());
    assert_ne!(outcome.pixels, texture_a());
}

fn clamped_pixels() -> Vec<u8> {
    let texture = texture_a();
    (0..16)
        .flat_map(|i| {
            let at = (CLAMPED_TEXEL[i / 4] * 4 + CLAMPED_TEXEL[i % 4]) * 4;
            <[u8; 4]>::try_from(&texture[at..at + 4]).unwrap()
        })
        .collect()
}

fn index_buffer(rig: &mut Rig, format: u32) -> (GraphicsHandle, usize, u32) {
    // Offset skips the first index and the draw start skips the second.
    // With the supplied signed bases, either form resolves to [5, 3, 4].
    let (bytes, width): (Vec<u8>, usize) = if format == R16_UINT {
        (
            [0u16, 0, 6, 4, 5]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect(),
            2,
        )
    } else {
        (
            [0u32, 0, 4, 2, 3]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
            4,
        )
    };
    rig.memory
        .map_region_with_data(INDEX_ADDR, 0x1000, MemoryPermissions::READ_ONLY, &bytes)
        .unwrap();
    let resource = rig
        .ctx
        .create_committed_resource(
            rig.device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: bytes.len() as u64,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::COMMON,
            Some(INDEX_ADDR),
        )
        .unwrap();
    (resource, width, format)
}

fn run_indexed(
    rig: &mut Rig,
    index: Option<(GraphicsHandle, usize, u32)>,
    (count, instances, start, base): (u32, u32, u32, i32),
) -> Result<ExecutionOutcome, GraphicsError> {
    record_binding(rig, SLOT_0)?;
    let ctx = &mut rig.ctx;
    if let Some((resource, offset, format)) = index {
        ctx.cmd_set_index_buffer(rig.list, resource, offset, format)?;
    }
    ctx.cmd_draw_indexed_instanced(rig.list, count, instances, start, base, 0)?;
    ctx.close_command_list(rig.list)?;
    ctx.execute_command_list(rig.queue, rig.list, &rig.memory)
}

#[test]
fn flat_indexed_draw_checks_the_vertex_binding_before_reading_indices() {
    let mut rig = rig();
    let (resource, offset, format) = index_buffer(&mut rig, R16_UINT);
    for (stride, expected) in [
        (None, GraphicsError::InvalidResourceHandle),
        (Some(8), GraphicsError::InvalidDescriptor),
    ] {
        rig.ctx.reset_command_list(rig.list, rig.alloc).unwrap();
        rig.ctx
            .cmd_set_index_buffer(rig.list, resource, offset, format)
            .unwrap();
        if let Some(stride) = stride {
            rig.ctx
                .cmd_set_vertex_buffer(rig.list, rig.vb, 0, stride)
                .unwrap();
        }
        rig.ctx
            .cmd_draw_indexed_instanced(rig.list, 3, 1, u32::MAX, 0, 0)
            .unwrap();
        rig.ctx.close_command_list(rig.list).unwrap();
        assert_eq!(
            rig.ctx
                .execute_command_list(rig.queue, rig.list, &rig.memory),
            Err(expected)
        );
    }
}

#[test]
fn indexed_inputs_require_buffer_resources() {
    let mut rig = rig();
    let texture = rig
        .ctx
        .create_committed_texture2d(
            rig.device,
            4,
            4,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            ResourceStates::COMMON,
            None,
        )
        .unwrap();
    let bindings = [
        rig.ctx.cmd_set_vertex_buffer(rig.list, texture, 0, STRIDE),
        rig.ctx.cmd_set_index_buffer(rig.list, texture, 0, R16_UINT),
    ];
    assert_eq!(
        bindings,
        [
            Err(GraphicsError::InvalidDescriptor),
            Err(GraphicsError::InvalidDescriptor)
        ]
    );

    let index = index_buffer(&mut rig, R16_UINT);
    let valid = run_indexed(&mut rig, Some(index), (3, 1, 1, -1)).expect("valid buffer draw");
    assert_eq!(valid.pixels, clamped_pixels());
}

#[test]
fn textured_indexed_draw_fetches_positions_and_uvs_at_the_resolved_indices() {
    for (format, base) in [(R16_UINT, -1), (R32_UINT, 1)] {
        let mut rig = rig();
        let index = index_buffer(&mut rig, format);
        let output =
            run_indexed(&mut rig, Some(index), (3, 1, 1, base)).expect("textured indexed draw");
        assert_eq!(output.backend, GraphicsBackendId::SoftwareReference);
        assert_eq!(output.draw_count, 1);
        assert_eq!((output.width, output.height), (4, 4));
        assert_eq!(output.pixels, clamped_pixels());
        assert_ne!(output.pixels, texture_a());
    }
}

#[test]
fn textured_indexed_draw_checks_ranges_and_skips_zero_draws() {
    let mut rig = rig();
    let index = index_buffer(&mut rig, R16_UINT);
    for (view, draw, expected) in [
        (None, (3, 1, 1, -1), GraphicsError::InvalidResourceHandle),
        (Some(index), (3, 1, 2, -1), GraphicsError::IndexOutOfRange),
        (Some(index), (3, 1, 1, -7), GraphicsError::IndexOutOfRange),
        (Some(index), (3, 1, 1, 0), GraphicsError::IndexOutOfRange),
    ] {
        assert_eq!(run_indexed(&mut rig, view, draw).unwrap_err(), expected);
    }
    for (count, instances) in [(0, 1), (3, 0)] {
        let output = run_indexed(&mut rig, None, (count, instances, u32::MAX, i32::MIN))
            .expect("zero draw requires no index buffer");
        assert_eq!(output.draw_count, 0);
    }
    let output = run_indexed(&mut rig, Some(index), (3, 1, 1, -1)).expect("draw after rejection");
    assert_eq!(output.pixels, clamped_pixels());
}

/// The root signature's parameter order decides which table is which, and
/// a nonzero sampler slot resolves.
#[test]
fn swapped_root_parameters_and_second_sampler_slot() {
    let mut rig = rig();
    let outcome = run(
        &mut rig,
        Binding {
            swapped: true,
            sampler_slot: Some(2),
            ..srv(1)
        },
    )
    .expect("textured draw");
    assert_eq!(outcome.pixels, texture_b());
    // Binding the SRV table at the sampler's parameter fails.
    let mut wrong = self::rig();
    let ctx = &mut wrong.ctx;
    ctx.reset_command_list(wrong.list, wrong.alloc).unwrap();
    ctx.cmd_set_descriptor_heaps(wrong.list, &[wrong.srv_heap, wrong.sampler_heap])
        .unwrap();
    ctx.cmd_set_graphics_root_signature(wrong.list, wrong.swapped_root_signature)
        .unwrap();
    let base = ctx
        .gpu_descriptor_handle_for_heap_start(wrong.srv_heap)
        .unwrap();
    ctx.cmd_set_graphics_root_descriptor_table(wrong.list, 0, base)
        .unwrap();
    ctx.close_command_list(wrong.list).unwrap();
    assert_eq!(
        ctx.execute_command_list(wrong.queue, wrong.list, &wrong.memory),
        Err(GraphicsError::InvalidDescriptor)
    );
}

#[derive(Clone)]
enum Step {
    Heaps(Vec<GraphicsHandle>),
    Root(GraphicsHandle),
    Tables,
    Draw,
}

fn record(rig: &mut Rig, steps: &[Step]) -> Result<ExecutionOutcome, GraphicsError> {
    let ctx = &mut rig.ctx;
    ctx.reset_command_list(rig.list, rig.alloc)?;
    ctx.cmd_set_viewport(rig.list, 4, 4)?;
    ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 0, STRIDE)?;
    for step in steps {
        match step {
            Step::Heaps(heaps) => ctx.cmd_set_descriptor_heaps(rig.list, heaps)?,
            Step::Root(root) => ctx.cmd_set_graphics_root_signature(rig.list, *root)?,
            Step::Tables => {
                let srv = ctx.gpu_descriptor_handle_for_heap_start(rig.srv_heap)?;
                let sampler = ctx.gpu_descriptor_handle_for_heap_start(rig.sampler_heap)?;
                ctx.cmd_set_graphics_root_descriptor_table(rig.list, 0, srv)?;
                ctx.cmd_set_graphics_root_descriptor_table(rig.list, 1, sampler)?;
            }
            Step::Draw => ctx.cmd_draw_instanced(rig.list, 3, 1, 0, 0)?,
        }
    }
    ctx.close_command_list(rig.list)?;
    ctx.execute_command_list(rig.queue, rig.list, &rig.memory)
}

/// Changing heaps or the root signature invalidates bound tables; setting
/// the same heaps or the same root signature again keeps them.
#[test]
fn heap_and_root_signature_changes_invalidate_tables() {
    let mut rig = rig();
    let other_srv_heap = rig
        .ctx
        .create_descriptor_heap(rig.device, CBV_SRV_UAV, 4, true)
        .unwrap();
    let same_layout = rig
        .ctx
        .create_root_signature(rig.device, &root_signature_desc())
        .unwrap();
    let heaps = Step::Heaps(vec![rig.srv_heap, rig.sampler_heap]);
    let other = Step::Heaps(vec![other_srv_heap, rig.sampler_heap]);
    let root = Step::Root(rig.root_signature);
    let bound = [heaps.clone(), root.clone(), Step::Tables];
    for (extra, expected, case) in [
        (vec![], Ok(()), "plain"),
        (vec![heaps.clone()], Ok(()), "same heaps again"),
        (vec![root.clone()], Ok(()), "same root signature again"),
        (
            vec![other, heaps.clone()],
            Err(GraphicsError::InvalidDescriptor),
            "heaps changed and restored",
        ),
        (
            vec![Step::Root(same_layout)],
            Err(GraphicsError::InvalidDescriptor),
            "another root signature with the same layout",
        ),
    ] {
        let mut steps = bound.to_vec();
        steps.extend(extra);
        steps.push(Step::Draw);
        let result = record(&mut rig, &steps);
        match expected {
            Ok(()) => assert_eq!(result.expect(case).pixels, texture_a(), "{case}"),
            Err(error) => assert_eq!(result, Err(error), "{case}"),
        }
    }
}

/// Zero vertices or instances draw nothing; any other shape than one
/// instance of three vertices is unsupported. Neither reaches the backend.
#[test]
fn textured_draw_shapes() {
    let mut rig = rig();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    rig.ctx.set_metal_textured_backend(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(vec![0; 64])
    });
    for draw in [(3, 0, 0), (0, 1, 0)] {
        let outcome = run(&mut rig, Binding { draw, ..SLOT_0 }).unwrap();
        assert_eq!(outcome.draw_count, 0, "{draw:?}");
    }
    for draw in [(6, 1, 0), (3, 2, 0), (3, 1, 1)] {
        let result = run(&mut rig, Binding { draw, ..SLOT_0 });
        assert!(
            matches!(result, Err(GraphicsError::Unsupported(_))),
            "{draw:?}: {result:?}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Vertex and texture reads stay inside their resources even when the
/// guest backing maps more bytes.
#[test]
fn reads_stay_inside_resources() {
    let mut rig = rig();
    rig.vb = rig
        .ctx
        .create_committed_resource(
            rig.device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: 2 * STRIDE as u64,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(VB_ADDR),
        )
        .unwrap();
    assert_eq!(run(&mut rig, SLOT_0), Err(GraphicsError::IndexOutOfRange));
}

/// Array textures cannot be viewed, and a texture in a state that excludes
/// pixel-shader reads cannot be sampled.
#[test]
fn texture_shape_and_state_are_checked() {
    let mut rig = rig();
    let ctx = &mut rig.ctx;
    let dest = ctx
        .cpu_descriptor_handle_for_heap_start(rig.srv_heap)
        .unwrap()
        .offset(3)
        .unwrap();
    let array = ctx
        .create_committed_resource(
            rig.device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 4,
                height: 4,
                depth_or_array_size: 2,
                mip_levels: 1,
                format_rgba8: true,
            },
            ResourceStates::COMMON,
            None,
        )
        .unwrap();
    assert!(matches!(
        ctx.create_shader_resource_view(array, &srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM), dest),
        Err(GraphicsError::Unsupported(_))
    ));
    for (state, ok) in [
        (ResourceStates::COPY_DEST, false),
        (
            ResourceStates(ResourceStates::COPY_DEST.0 | ResourceStates::PIXEL_SHADER_RESOURCE.0),
            false,
        ),
        (
            ResourceStates(
                ResourceStates::NON_PIXEL_SHADER_RESOURCE.0
                    | ResourceStates::PIXEL_SHADER_RESOURCE.0,
            ),
            true,
        ),
        (ResourceStates::RENDER_TARGET, false),
        (ResourceStates::PIXEL_SHADER_RESOURCE, true),
    ] {
        let ctx = &mut rig.ctx;
        let texture = ctx
            .create_committed_texture2d(
                rig.device,
                4,
                4,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                state,
                Some(TEX_ADDR),
            )
            .unwrap();
        ctx.create_shader_resource_view(texture, &srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM), dest)
            .unwrap();
        let result = run(&mut rig, srv(3));
        if ok {
            assert_eq!(result.unwrap().pixels, texture_a(), "{state:?}");
        } else {
            assert_eq!(
                result,
                Err(GraphicsError::ResourceLifetimeViolation),
                "{state:?}"
            );
        }
    }
}

/// The public software reference rejects viewports it cannot allocate.
#[test]
fn software_reference_rejects_oversized_viewports() {
    let texels = texture_a();
    for viewport in [(usize::MAX, 2), (65, 64), (0, 4)] {
        let draw = xenora_loader::graphics::TexturedDraw {
            positions: VERTICES.map(|v| [v[0], v[1], v[2], v[3]]),
            texcoords: VERTICES.map(|v| [v[4], v[5], v[6], v[7]]),
            viewport,
            clear_color: [0, 0, 0, 255],
            texture: xenora_loader::graphics::TextureView {
                format: TextureFormat::Rgba8Unorm,
                width: 4,
                height: 4,
                bytes: &texels,
            },
            sampler: SamplerDesc::point_clamp(),
        };
        assert_eq!(
            GraphicsContext::execute_software_reference_textured_draw(&draw),
            Err(GraphicsError::InvalidViewport),
            "{viewport:?}"
        );
    }
}

/// Handle arithmetic cannot carry into the heap ID.
#[test]
fn descriptor_handle_offsets_stay_in_their_heap() {
    let rig = rig();
    let start = rig
        .ctx
        .cpu_descriptor_handle_for_heap_start(rig.srv_heap)
        .unwrap();
    assert_eq!(start.offset(1 << 27), None);
    let last = start.offset((1 << 27) - 1).unwrap();
    assert_eq!(last.0 >> 32, start.0 >> 32);
    let gpu = rig
        .ctx
        .gpu_descriptor_handle_for_heap_start(rig.srv_heap)
        .unwrap();
    assert_eq!(gpu.offset(1 << 27), None);
}

#[test]
fn textured_draw_requires_native_backend_when_configured() {
    let mut rig = rig();
    rig.ctx.set_require_metal_without_backend();
    assert_eq!(
        run(&mut rig, SLOT_0),
        Err(GraphicsError::NativeMetalRequired)
    );
}

/// The backend receives the bound texture's level, the sampler and the
/// guest's per-vertex positions and texture coordinates.
#[test]
fn backend_receives_the_bound_texture_and_vertices() {
    let mut rig = rig();
    let seen = Arc::new(std::sync::Mutex::new(None));
    let sink = seen.clone();
    rig.ctx.set_metal_textured_backend(move |draw| {
        *sink.lock().unwrap() = Some((
            draw.texture.format,
            (draw.texture.width, draw.texture.height),
            draw.texture.bytes.to_vec(),
            draw.positions,
            draw.texcoords,
            draw.sampler,
        ));
        GraphicsContext::execute_software_reference_textured_draw(draw)
    });
    let outcome = run(&mut rig, srv(2)).unwrap();
    assert_eq!(outcome.backend, GraphicsBackendId::MetalNative);
    let (format, extent, bytes, positions, texcoords, sampler) =
        seen.lock().unwrap().take().unwrap();
    assert_eq!(format, TextureFormat::Bc1Unorm);
    assert_eq!(extent, (8, 8));
    assert_eq!(bytes, BC1_BLOCKS.as_flattened());
    assert_eq!(positions, VERTICES.map(|v| [v[0], v[1], v[2], v[3]]));
    assert_eq!(texcoords, VERTICES.map(|v| [v[4], v[5], v[6], v[7]]));
    assert_eq!(sampler, SamplerDesc::point_clamp());
}

/// The translated RDNA2 `image_sample_lz` pixel stage samples the bound
/// texture on Metal. Native, software and hand-derived images agree.
#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal device; run scripts/test-gpu.sh"
)]
fn native_metal_textured_draw_matches_references() {
    use xenora_metal::NativeCompute;
    use xenora_metal::sampler::MetalSampler;
    use xenora_metal::texture_pipeline::{MetalRgba8Texture, Rgba8MipLevel, Rgba8TextureDesc};
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};
    use xenora_texture::{Bc1Upload, Bc1UploadDesc};

    let msl = compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
        &image_sample_lz_ps(),
    )
    .expect("compile translated image_sample_lz shader")
    .msl;
    let gpu = Arc::new(NativeCompute::new().expect("native Metal GPU device"));
    let entries = [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT];
    let backend = gpu.clone();
    let metal_error = |e: xenora_metal::MetalError| GraphicsError::BackendError(format!("{e:?}"));
    let mut rig = rig();
    rig.ctx.set_metal_textured_backend(move |draw| {
        assert_eq!(draw.sampler, SamplerDesc::point_clamp());
        let sampler = MetalSampler::new(
            backend.device(),
            xenora_texture::SamplerDesc::nearest_clamp(),
        )
        .map_err(metal_error)?;
        let (width, height) = (draw.texture.width as usize, draw.texture.height as usize);
        let extent = [draw.viewport.0, draw.viewport.1];
        match draw.texture.format {
            TextureFormat::Rgba8Unorm => {
                let desc = Rgba8TextureDesc::new(width, height, 1).map_err(metal_error)?;
                let level = Rgba8MipLevel::new_packed(0, width, height, draw.texture.bytes);
                let texture = MetalRgba8Texture::upload(backend.device(), desc, &[level])
                    .map_err(metal_error)?;
                backend.render_rgba8_sampled(
                    &msl,
                    entries,
                    &draw.positions,
                    &draw.texcoords,
                    extent,
                    draw.clear_color,
                    Some((&texture, &sampler)),
                )
            }
            TextureFormat::Bc1Unorm => {
                let row_pitch = width.div_ceil(4) * 8;
                let desc = Bc1UploadDesc {
                    width,
                    height,
                    mips: 1,
                    layers: 1,
                    format: 1,
                    usage: 1,
                    row_pitch,
                    slice_pitch: row_pitch * height.div_ceil(4),
                };
                let upload = Bc1Upload::new(draw.texture.bytes, desc)
                    .map_err(|e| GraphicsError::BackendError(format!("{e:?}")))?;
                backend.render_rgba8_sampled_bc1(
                    &msl,
                    entries,
                    &draw.positions,
                    &draw.texcoords,
                    extent,
                    draw.clear_color,
                    &upload,
                    &sampler,
                )
            }
        }
        .map_err(metal_error)
    });

    let mut software = self::rig();
    for (slot, expected, name) in [
        (0, texture_a(), "RGBA8 texture A"),
        (1, texture_b(), "RGBA8 texture B"),
        (2, bc1_expected(), "BC1 texture"),
    ] {
        let native = run(&mut rig, srv(slot)).expect("native textured draw");
        assert_eq!(native.backend, GraphicsBackendId::MetalNative);
        assert_eq!(native.pixels, expected, "{name}: native vs hand-derived");
        let reference = run(&mut software, srv(slot)).expect("software textured draw");
        assert_eq!(
            native.pixels, reference.pixels,
            "{name}: native vs software"
        );
    }
    // Off-centre and out-of-range coordinates clamp on Metal as in software.
    let clamped = Binding {
        first_vertex: 3,
        ..SLOT_0
    };
    let native = run(&mut rig, clamped).expect("native clamped draw");
    let reference = run(&mut software, clamped).expect("software clamped draw");
    assert_eq!(
        native.pixels, reference.pixels,
        "clamped: native vs software"
    );
    assert_ne!(native.pixels, texture_a());

    let index = index_buffer(&mut rig, R32_UINT);
    let indexed =
        run_indexed(&mut rig, Some(index), (3, 1, 1, 1)).expect("native textured indexed draw");
    assert_eq!(indexed.backend, GraphicsBackendId::MetalNative);
    assert_eq!(indexed.draw_count, 1);
    assert_eq!(
        indexed.pixels,
        clamped_pixels(),
        "indexed: native vs hand-derived"
    );
    assert_eq!(
        indexed.pixels, reference.pixels,
        "indexed: native vs software"
    );
    println!(
        "device={} guest-bound textures: RGBA8 slots 0 and 1 (16 of 16 pixels) and BC1 slot 2 (64 of 64) match",
        gpu.device_name()
    );
}

/// Translated gfx1030 pixel stage: interpolate parameter 0's `xy`, then
/// `image_sample_lz v[4:7], v[2:3], s[0:7], s[8:11] dmask:0xf dim:2D` and
/// export the sample. Same bytes as the Metal crate's translated-render test.
fn image_sample_lz_ps() -> Vec<u8> {
    [
        &[0x00, 0x00, 0x08, 0xc8][..],
        &[0x01, 0x00, 0x09, 0xc8],
        &[0x00, 0x01, 0x0c, 0xc8],
        &[0x01, 0x01, 0x0d, 0xc8],
        &[0x08, 0x0f, 0x9c, 0xf0, 0x02, 0x04, 0x40, 0x00],
        &[0x0f, 0x18, 0x00, 0xf8, 0x04, 0x05, 0x06, 0x07],
        &[0x00, 0x00, 0x81, 0xbf],
    ]
    .concat()
}
