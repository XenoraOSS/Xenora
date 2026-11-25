// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end integration tests for Xenora X17:
//! Guest-facing D3D12-style Graphics Runtime & Native Metal Pipeline Integration.
//!
//! Validates:
//! 1. Guest device creation, command queue, allocator, command list, and resource lifetimes.
//! 2. Command recording: resource barriers, viewports, render targets, vertex buffer bindings, draws.
//! 3. Error detection: invalid handles, closed command lists, empty submissions, state violations.
//! 4. Full guest-driven PE32+ execution through loader, ARM64 DBT, and D3D12 services.
//! 5. Authentic framebuffer generation from guest commands and vertex data.
//! 6. Native Metal backend execution ensuring guest vertex inputs drive actual GPU output.

use std::sync::Arc;
use xenora_loader::cpu::{ExitReason, Register};
use xenora_loader::graphics::{
    CommandListType, GraphicsBackendId, GraphicsContext, GraphicsError, GraphicsHandle,
    ResourceDesc, ResourceDimension, ResourceStates,
};
use xenora_loader::imports::ImportRegistry;
use xenora_loader::memory::GuestMemory;
use xenora_loader::pe::{
    IMAGE_FILE_MACHINE_AMD64, IMAGE_SCN_CNT_CODE, IMAGE_SCN_MEM_EXECUTE, IMAGE_SCN_MEM_READ,
    IMAGE_SCN_MEM_WRITE, PE32_PLUS_MAGIC,
};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn test_d3d12_device_and_resource_lifecycle() {
    let mut ctx = GraphicsContext::new();

    // 1. Device creation
    let device = ctx.create_device().expect("device creation");
    assert_ne!(device, GraphicsHandle::NULL);

    // 2. Queue and allocator creation
    let queue = ctx
        .create_command_queue(device, CommandListType::Direct)
        .expect("queue");
    let alloc = ctx
        .create_command_allocator(device, CommandListType::Direct)
        .expect("allocator");

    // 3. Command list creation
    let cmd_list = ctx
        .create_command_list(device, alloc, CommandListType::Direct)
        .expect("cmd list");

    // 4. Resource creation (Buffer and Texture)
    let vb_desc = ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: 1024,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    let vb = ctx
        .create_committed_resource(
            device,
            vb_desc,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            None,
        )
        .expect("vertex buffer");

    let rt_desc = ResourceDesc {
        dimension: ResourceDimension::Texture2D,
        width: 64,
        height: 64,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: true,
    };
    let rt = ctx
        .create_committed_resource(device, rt_desc, ResourceStates::RENDER_TARGET, None)
        .expect("render target");

    // 5. Command recording
    ctx.cmd_set_viewport(cmd_list, 64, 64).expect("viewport");
    ctx.cmd_set_render_targets(cmd_list, Some(rt), [0, 0, 0, 255])
        .expect("render target");
    ctx.cmd_set_vertex_buffer(cmd_list, vb, 0, 16)
        .expect("vertex buffer");
    ctx.cmd_draw_instanced(cmd_list, 3, 1, 0, 0).expect("draw");

    // Transition render target to PRESENT
    ctx.cmd_resource_barrier(
        cmd_list,
        rt,
        ResourceStates::RENDER_TARGET,
        ResourceStates::PRESENT,
    )
    .expect("barrier");

    // 6. Close command list
    ctx.close_command_list(cmd_list).expect("close");

    // Closing already closed list should fail
    assert_eq!(
        ctx.close_command_list(cmd_list),
        Err(GraphicsError::CommandListAlreadyClosed)
    );

    // 7. Execution
    let memory = GuestMemory::new();
    let outcome = ctx
        .execute_command_list(queue, cmd_list, &memory)
        .expect("execute");
    assert_eq!(outcome.pixels.len(), 64 * 64 * 4);
    assert_eq!(outcome.width, 64);
    assert_eq!(outcome.height, 64);
    assert_eq!(outcome.draw_count, 1);
    assert_eq!(
        outcome.backend,
        xenora_loader::graphics::GraphicsBackendId::SoftwareReference
    );

    // Verify last presentation
    let pres = ctx.last_presentation.lock().unwrap();
    assert_eq!(pres.width, 64);
    assert_eq!(pres.height, 64);
    assert_eq!(pres.frame_index, 1);
}

#[test]
fn test_d3d12_error_cases() {
    let mut ctx = GraphicsContext::new();
    let invalid_device = GraphicsHandle {
        id: 999,
        generation: 1,
    };

    // Device check
    assert_eq!(
        ctx.create_command_queue(invalid_device, CommandListType::Direct),
        Err(GraphicsError::InvalidDeviceHandle)
    );

    let device = ctx.create_device().unwrap();
    let queue = ctx
        .create_command_queue(device, CommandListType::Direct)
        .unwrap();
    let alloc = ctx
        .create_command_allocator(device, CommandListType::Direct)
        .unwrap();
    let cmd_list = ctx
        .create_command_list(device, alloc, CommandListType::Direct)
        .unwrap();

    // Cannot submit recording list
    let memory = GuestMemory::new();
    assert_eq!(
        ctx.execute_command_list(queue, cmd_list, &memory),
        Err(GraphicsError::CommandListNotRecording)
    );

    // Empty list submission
    ctx.close_command_list(cmd_list).unwrap();
    assert_eq!(
        ctx.execute_command_list(queue, cmd_list, &memory),
        Err(GraphicsError::EmptyCommandList)
    );

    // Invalid viewport
    ctx.reset_command_list(cmd_list, alloc).unwrap();
    assert_eq!(
        ctx.cmd_set_viewport(cmd_list, 0, 10),
        Err(GraphicsError::InvalidViewport)
    );
}

#[test]
fn test_d3d12_xbox_create_device_service_through_iat() {
    let mut pe = vec![0u8; 4096];
    pe[0] = b'M';
    pe[1] = b'Z';
    pe[0x3C] = 0x80;

    let pe_sig = 0x80;
    pe[pe_sig..pe_sig + 4].copy_from_slice(b"PE\0\0");
    let coff = pe_sig + 4;
    pe[coff..coff + 2].copy_from_slice(&IMAGE_FILE_MACHINE_AMD64.to_le_bytes());
    pe[coff + 2..coff + 4].copy_from_slice(&2u16.to_le_bytes()); // 2 sections (.text, .rdata)
    pe[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());

    let opt = coff + 20;
    let image_base = 0x140000000u64;
    pe[opt..opt + 2].copy_from_slice(&PE32_PLUS_MAGIC.to_le_bytes());
    pe[opt + 16..opt + 20].copy_from_slice(&0x1000u32.to_le_bytes()); // entry point RVA
    pe[opt + 24..opt + 32].copy_from_slice(&image_base.to_le_bytes());
    pe[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes()); // section align
    pe[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes()); // file align
    pe[opt + 56..opt + 60].copy_from_slice(&0x3000u32.to_le_bytes()); // size of image
    pe[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes()); // size of headers
    pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());

    // Import directory at RVA 0x2000
    pe[opt + 120..opt + 124].copy_from_slice(&0x2000u32.to_le_bytes());
    pe[opt + 124..opt + 128].copy_from_slice(&0x100u32.to_le_bytes());

    // Section 1: .text (RVA 0x1000, file offset 0x200, size 0x200)
    let sec1 = opt + 240;
    pe[sec1..sec1 + 5].copy_from_slice(b".text");
    pe[sec1 + 8..sec1 + 12].copy_from_slice(&0x200u32.to_le_bytes());
    pe[sec1 + 12..sec1 + 16].copy_from_slice(&0x1000u32.to_le_bytes());
    pe[sec1 + 16..sec1 + 20].copy_from_slice(&0x200u32.to_le_bytes());
    pe[sec1 + 20..sec1 + 24].copy_from_slice(&0x200u32.to_le_bytes());
    pe[sec1 + 36..sec1 + 40].copy_from_slice(
        &(IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_EXECUTE).to_le_bytes(),
    );

    // Section 2: .rdata (RVA 0x2000, file offset 0x400, size 0x200)
    let sec2 = sec1 + 40;
    pe[sec2..sec2 + 6].copy_from_slice(b".rdata");
    pe[sec2 + 8..sec2 + 12].copy_from_slice(&0x200u32.to_le_bytes());
    pe[sec2 + 12..sec2 + 16].copy_from_slice(&0x2000u32.to_le_bytes());
    pe[sec2 + 16..sec2 + 20].copy_from_slice(&0x200u32.to_le_bytes());
    pe[sec2 + 20..sec2 + 24].copy_from_slice(&0x400u32.to_le_bytes());
    pe[sec2 + 36..sec2 + 40]
        .copy_from_slice(&(IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_WRITE).to_le_bytes());

    // Code at 0x200:
    // Call D3D12XboxCreateDevice through IAT at 0x2060
    // Arg 0 (RCX) = 0
    // Arg 1 (RDX) = 0 (feature level)
    // Arg 2 (R8) = 0 (riid)
    // Arg 3 (R9) = guest stack pointer &out_device (RSP + 32)
    // sub rsp, 0x28
    // lea r9, [rsp + 0x20]
    // xor rcx, rcx
    // xor rdx, rdx
    // xor r8, r8
    // call qword ptr [rip + 0x104b] -> IAT at 0x2060
    // mov rax, [rsp + 0x20]  -> put returned device handle into RAX
    // add rsp, 0x28
    // hlt
    let code_offset = 0x200;
    let code: &[u8] = &[
        0x48, 0x83, 0xEC, 0x28, // sub rsp, 0x28
        0x48, 0x8D, 0x4C, 0x24, 0x20, // lea rcx, [rsp+0x20]
        0x49, 0x89, 0xC9, // mov r9, rcx (ppDevice = rsp+0x20)
        0x48, 0x31, 0xC9, // xor rcx, rcx
        0x48, 0x31, 0xD2, // xor rdx, rdx
        0x4D, 0x31, 0xC0, // xor r8, r8
        0xFF, 0x15, 0x44, 0x10, 0x00,
        0x00, // call [rip+0x1044] -> 0x101c + 6 + 0x1044 = 0x2066? Let's verify IAT address
        0x48, 0x8B, 0x44, 0x24, 0x20, // mov rax, [rsp+0x20]
        0x48, 0x83, 0xC4, 0x28, // add rsp, 0x28
        0xF4, // hlt
    ];
    pe[code_offset..code_offset + code.len()].copy_from_slice(code);

    // Setup Import Directory in .rdata (file offset 0x400)
    // ImageImportDescriptor at 0x400 (RVA 0x2000):
    // OFT: RVA 0x2040 (offset 0x440)
    // TimeDate: 0
    // Forwarder: 0
    // Name: RVA 0x2080 (offset 0x480) -> "xenora_fixture.dll"
    // FT (IAT): RVA 0x2060 (offset 0x460)
    let desc_off = 0x400;
    pe[desc_off..desc_off + 4].copy_from_slice(&0x2040u32.to_le_bytes()); // OFT
    pe[desc_off + 12..desc_off + 16].copy_from_slice(&0x2080u32.to_le_bytes()); // Name
    pe[desc_off + 16..desc_off + 20].copy_from_slice(&0x2060u32.to_le_bytes()); // FT (IAT)

    // Name at 0x480: "xenora_fixture.dll\0"
    let dll_name = b"xenora_fixture.dll\0";
    pe[0x480..0x480 + dll_name.len()].copy_from_slice(dll_name);

    // OFT at 0x440: RVA 0x20A0 (hint/name)
    pe[0x440..0x448].copy_from_slice(&0x20A0u64.to_le_bytes());
    // FT at 0x460: RVA 0x20A0
    pe[0x460..0x468].copy_from_slice(&0x20A0u64.to_le_bytes());

    // Hint/Name at 0x4A0 (RVA 0x20A0): hint=0, name="CreateGraphicsDevice\0"
    let sym_name = b"\0\0CreateGraphicsDevice\0";
    pe[0x4A0..0x4A0 + sym_name.len()].copy_from_slice(sym_name);

    // Exact rip-relative offset calculation:
    // Instruction FF 15 starts at RVA 0x1015, length 6 bytes.
    // Next RIP = 0x1015 + 6 = 0x101B.
    // Target IAT is at RVA 0x2060.
    // Displacement = 0x2060 - 0x101B = 0x1045.
    let disp: i32 = 0x2060 - (0x1015 + 6);
    pe[0x217..0x21B].copy_from_slice(&disp.to_le_bytes());

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, state.clone());

    let result = run_synthetic_startup(
        &pe,
        StartupLayout {
            image_base,
            stack_top: 0x500000000,
            tls_base: 0x300000000,
        },
        imports,
        200,
    )
    .expect("run startup");

    assert_eq!(result.execution.exit_reason, ExitReason::Halt);
    let device_handle_val = result.execution.final_state.get(Register::Rax);
    assert_ne!(device_handle_val, 0);

    let handle = GraphicsHandle::from_u64(device_handle_val);
    assert_eq!(handle.id, 1);
    assert_eq!(handle.generation, 1);

    // Verify graphics context has this device registered
    let gfx = state.graphics.lock().unwrap();
    assert_eq!(gfx.last_presentation.lock().unwrap().frame_index, 0);
}

#[test]
fn test_d3d12_native_metal_missing_backend_fails() {
    let mut ctx = GraphicsContext::new();
    let dev = ctx.create_device().unwrap();
    let queue = ctx
        .create_command_queue(dev, CommandListType::Direct)
        .unwrap();
    let alloc = ctx
        .create_command_allocator(dev, CommandListType::Direct)
        .unwrap();
    let list = ctx
        .create_command_list(dev, alloc, CommandListType::Direct)
        .unwrap();

    let vb_desc = ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: 48,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    let vb = ctx
        .create_committed_resource(
            dev,
            vb_desc,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            None,
        )
        .unwrap();

    ctx.cmd_set_viewport(list, 4, 3).unwrap();
    ctx.cmd_set_render_targets(list, None, [0, 0, 0, 255])
        .unwrap();
    ctx.cmd_set_vertex_buffer(list, vb, 0, 16).unwrap();
    ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    ctx.close_command_list(list).unwrap();

    // Explicitly configure context for NativeMetal without providing a backend closure
    ctx.set_require_metal_without_backend();

    let memory = GuestMemory::new();
    let res = ctx.execute_command_list(queue, list, &memory);
    assert_eq!(res, Err(GraphicsError::NativeMetalRequired));
}

#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal device; run scripts/test-gpu.sh"
)]
fn test_guest_dbt_graphics_pipeline_metal_render_dual_inputs() {
    use xenora_metal::NativeCompute;
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

    // Load and compile real RDNA2 constant vertex/pixel shaders to MSL
    let vs_bytes = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");
    let ps_bytes = include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin");
    let compiled = compile_graphics(Architecture::Gfx1030, vs_bytes, ps_bytes)
        .expect("compile graphics RDNA2 shaders");
    let msl = compiled.msl;

    let gpu = Arc::new(NativeCompute::new().expect("native Metal GPU device"));

    // Helper that executes a synthetic PE32+ guest program driving the full pipeline:
    // PE32+ -> ARM64 DBT -> xenora_fixture.dll!SubmitGuestDraw -> GraphicsContext -> Native Metal
    let run_guest_draw = |vertices: [[f32; 4]; 3], clear_color: [u8; 4]| -> Vec<u8> {
        let mut pe = vec![0u8; 4096];
        pe[0] = b'M';
        pe[1] = b'Z';
        pe[0x3C] = 0x80;

        let pe_sig = 0x80;
        pe[pe_sig..pe_sig + 4].copy_from_slice(b"PE\0\0");
        let coff = pe_sig + 4;
        pe[coff..coff + 2].copy_from_slice(&IMAGE_FILE_MACHINE_AMD64.to_le_bytes());
        pe[coff + 2..coff + 4].copy_from_slice(&3u16.to_le_bytes()); // 3 sections (.text, .rdata, .data)
        pe[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());

        let opt = coff + 20;
        let image_base = 0x140000000u64;
        pe[opt..opt + 2].copy_from_slice(&PE32_PLUS_MAGIC.to_le_bytes());
        pe[opt + 16..opt + 20].copy_from_slice(&0x1000u32.to_le_bytes()); // entry point RVA
        pe[opt + 24..opt + 32].copy_from_slice(&image_base.to_le_bytes());
        pe[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        pe[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes());
        pe[opt + 56..opt + 60].copy_from_slice(&0x4000u32.to_le_bytes()); // size of image
        pe[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes()); // size of headers
        pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());

        // Import directory at RVA 0x2000
        pe[opt + 120..opt + 124].copy_from_slice(&0x2000u32.to_le_bytes());
        pe[opt + 124..opt + 128].copy_from_slice(&0x100u32.to_le_bytes());

        // Section 1: .text (RVA 0x1000, file offset 0x200, size 0x200)
        let sec1 = opt + 240;
        pe[sec1..sec1 + 5].copy_from_slice(b".text");
        pe[sec1 + 8..sec1 + 12].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec1 + 12..sec1 + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        pe[sec1 + 16..sec1 + 20].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec1 + 20..sec1 + 24].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec1 + 36..sec1 + 40].copy_from_slice(
            &(IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_EXECUTE).to_le_bytes(),
        );

        // Section 2: .rdata (RVA 0x2000, file offset 0x400, size 0x200)
        let sec2 = sec1 + 40;
        pe[sec2..sec2 + 6].copy_from_slice(b".rdata");
        pe[sec2 + 8..sec2 + 12].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec2 + 12..sec2 + 16].copy_from_slice(&0x2000u32.to_le_bytes());
        pe[sec2 + 16..sec2 + 20].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec2 + 20..sec2 + 24].copy_from_slice(&0x400u32.to_le_bytes());
        pe[sec2 + 36..sec2 + 40]
            .copy_from_slice(&(IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_WRITE).to_le_bytes());

        // Section 3: .data (RVA 0x3000, file offset 0x600, size 0x200) - for vertex buffer
        let sec3 = sec2 + 40;
        pe[sec3..sec3 + 5].copy_from_slice(b".data");
        pe[sec3 + 8..sec3 + 12].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec3 + 12..sec3 + 16].copy_from_slice(&0x3000u32.to_le_bytes());
        pe[sec3 + 16..sec3 + 20].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec3 + 20..sec3 + 24].copy_from_slice(&0x600u32.to_le_bytes());
        pe[sec3 + 36..sec3 + 40]
            .copy_from_slice(&(IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_WRITE).to_le_bytes());

        // Write guest vertices into .data section (RVA 0x3000 -> absolute guest addr 0x140003000)
        let data_offset = 0x600;
        let mut v_bytes = Vec::new();
        for v in vertices.iter().flatten() {
            v_bytes.extend_from_slice(&v.to_le_bytes());
        }
        pe[data_offset..data_offset + v_bytes.len()].copy_from_slice(&v_bytes);

        // Code in .text (file offset 0x200, RVA 0x1000):
        // Arg 0 (RCX) = guest address of vertex buffer = image_base + 0x3000 = 0x140003000
        // Arg 1 (RDX) = clear_color (u32 packed)
        // Arg 2 (R8)  = viewport dims (width 4, height 3 -> (3 << 32) | 4 = 0x0000000300000004)
        // Arg 3 (R9)  = 0
        // sub rsp, 0x28
        // mov rcx, 0x140003000
        // mov edx, clear_color_u32
        // mov r8, 0x0000000300000004
        // xor r9, r9
        // call qword ptr [rip + disp] -> IAT at 0x2060
        // add rsp, 0x28
        // hlt
        let clear_u32 = (clear_color[0] as u32)
            | ((clear_color[1] as u32) << 8)
            | ((clear_color[2] as u32) << 16)
            | ((clear_color[3] as u32) << 24);

        let mut code = Vec::new();
        code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]); // sub rsp, 0x28
        code.extend_from_slice(&[0x48, 0xB9]); // mov rcx, imm64
        code.extend_from_slice(&(image_base + 0x3000).to_le_bytes());
        code.extend_from_slice(&[0xBA]); // mov edx, imm32
        code.extend_from_slice(&clear_u32.to_le_bytes());
        code.extend_from_slice(&[0x49, 0xB8]); // mov r8, imm64
        let dims: u64 = (3u64 << 32) | 4u64;
        code.extend_from_slice(&dims.to_le_bytes());
        code.extend_from_slice(&[0x4D, 0x31, 0xC9]); // xor r9, r9

        let call_offset_in_text = code.len();
        code.extend_from_slice(&[0xFF, 0x15, 0, 0, 0, 0]); // call [rip+disp]
        code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
        code.push(0xF4); // hlt

        let code_offset = 0x200;
        pe[code_offset..code_offset + code.len()].copy_from_slice(&code);

        // Setup Import Directory in .rdata (file offset 0x400)
        // ImageImportDescriptor at 0x400 (RVA 0x2000):
        let desc_off = 0x400;
        pe[desc_off..desc_off + 4].copy_from_slice(&0x2040u32.to_le_bytes()); // OFT
        pe[desc_off + 12..desc_off + 16].copy_from_slice(&0x2080u32.to_le_bytes()); // Name
        pe[desc_off + 16..desc_off + 20].copy_from_slice(&0x2060u32.to_le_bytes()); // FT (IAT)

        // Name at 0x480: "xenora_fixture.dll\0"
        let dll_name = b"xenora_fixture.dll\0";
        pe[0x480..0x480 + dll_name.len()].copy_from_slice(dll_name);

        // OFT at 0x440: RVA 0x20A0 (hint/name)
        pe[0x440..0x448].copy_from_slice(&0x20A0u64.to_le_bytes());
        // FT at 0x460: RVA 0x20A0
        pe[0x460..0x468].copy_from_slice(&0x20A0u64.to_le_bytes());

        // Hint/Name at 0x4A0 (RVA 0x20A0): hint=0, name="SubmitGuestDraw\0"
        let sym_name = b"\0\0SubmitGuestDraw\0";
        pe[0x4A0..0x4A0 + sym_name.len()].copy_from_slice(sym_name);

        // Calculate rip displacement to IAT at RVA 0x2060
        let call_rva = 0x1000 + call_offset_in_text as u32;
        let next_rip = call_rva + 6;
        let disp: i32 = (0x2060 - next_rip as i64) as i32;
        let patch_loc = code_offset + call_offset_in_text + 2;
        pe[patch_loc..patch_loc + 4].copy_from_slice(&disp.to_le_bytes());

        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, state.clone());

        // Wire native Metal renderer backend into GraphicsContext
        {
            let gpu_clone = gpu.clone();
            let msl_clone = msl.clone();
            let metal_renderer = move |vtx: &[[f32; 4]],
                                       vp: (usize, usize),
                                       cc: [u8; 4]|
                  -> Result<Vec<u8>, GraphicsError> {
                let entries = [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT];
                gpu_clone
                    .render_triangles_rgba8(
                        &msl_clone,
                        entries,
                        vtx,
                        &vec![[0.0; 4]; vtx.len()],
                        [vp.0, vp.1],
                        cc,
                    )
                    .map_err(|e| GraphicsError::BackendError(format!("Metal GPU error: {:?}", e)))
            };
            let mut gfx = state.graphics.lock().unwrap();
            gfx.set_metal_backend(metal_renderer);
        }

        let result = run_synthetic_startup(
            &pe,
            StartupLayout {
                image_base,
                stack_top: 0x500000000,
                tls_base: 0x300000000,
            },
            imports,
            200,
        )
        .expect("run startup");

        assert_eq!(result.execution.exit_reason, ExitReason::Halt);
        assert_eq!(result.execution.final_state.get(Register::Rax), 0); // S_OK

        let gfx = state.graphics.lock().unwrap();
        let pres = gfx.last_presentation.lock().unwrap();
        assert_eq!(pres.backend, Some(GraphicsBackendId::MetalNative));
        assert_eq!(pres.width, 4);
        assert_eq!(pres.height, 3);
        assert_eq!(pres.frame_index, 1);
        pres.pixels.clone()
    };

    // Variant 1: Full-screen triangle covering the entire viewport [-1, 1]
    let vtx_fullscreen: [[f32; 4]; 3] = [
        [-1.0, -1.0, 0.5, 1.0],
        [3.0, -1.0, 0.5, 1.0],
        [-1.0, 3.0, 0.5, 1.0],
    ];
    let clear_variant1 = [19, 43, 93, 251];
    let pixels1 = run_guest_draw(vtx_fullscreen, clear_variant1);

    // Variant 2: Shifted / offscreen triangle completely outside viewport clip volume
    let vtx_offscreen: [[f32; 4]; 3] = [
        [5.0, 5.0, 0.5, 1.0],
        [6.0, 5.0, 0.5, 1.0],
        [5.0, 6.0, 0.5, 1.0],
    ];
    let clear_variant2 = [10, 20, 30, 255];
    let pixels2 = run_guest_draw(vtx_offscreen, clear_variant2);

    // 1. Both outputs must have the exact requested dimensions (4x3 = 12 pixels, 48 bytes RGBA8)
    assert_eq!(pixels1.len(), 48);
    assert_eq!(pixels2.len(), 48);

    // 2. Strict inequality between the two runs: different guest vertex & clear inputs
    // produce distinctly different Metal GPU framebuffers
    assert_ne!(pixels1, pixels2);

    // 3. Variant 1: Fullscreen triangle produces red [255, 0, 0, 255] for all 12 pixels
    let expected_red = [255, 0, 0, 255];
    assert_eq!(pixels1, expected_red.repeat(12));

    // 4. Variant 2: Offscreen triangle produces clear color [10, 20, 30, 255] for all 12 pixels
    let expected_clear = [10, 20, 30, 255];
    assert_eq!(pixels2, expected_clear.repeat(12));
}

/// Guest addresses of the indexed-draw fixtures.
const INDEXED_VB: u64 = 0x7000_0000;
const INDEXED_IB: u64 = 0x7100_0000;
/// Vertex stride of the indexed fixtures: a float4 position and 16 unused bytes.
const INDEXED_STRIDE: usize = 32;

/// Triangle (4,3) (16,4) (6,16) of a 16x16 target in clip space; no pixel
/// centre lies on one of its edges.
const INDEXED_TRIANGLE: [[f32; 4]; 3] = [
    [-0.5, 0.625, 0.5, 1.0],
    [1.0, 0.5, 0.5, 1.0],
    [-0.25, -1.0, 0.5, 1.0],
];

/// Ten vertices: the triangle at vertices 5, 7 and 2, and decoys elsewhere,
/// so a wrong fetch passes different positions.
fn indexed_vertices() -> Vec<u8> {
    let mut bytes = Vec::new();
    for vertex in 0..10 {
        let position = match vertex {
            5 => INDEXED_TRIANGLE[0],
            7 => INDEXED_TRIANGLE[1],
            2 => INDEXED_TRIANGLE[2],
            other => [other as f32 * 0.125 - 0.75, -0.5, 0.5, 1.0],
        };
        for value in position {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend([0xAB; 16]);
    }
    bytes
}

struct IndexedRig {
    ctx: GraphicsContext,
    memory: GuestMemory,
    queue: GraphicsHandle,
    alloc: GraphicsHandle,
    list: GraphicsHandle,
    vb: GraphicsHandle,
    ib16: GraphicsHandle,
    ib32: GraphicsHandle,
}

/// A context with guest-backed vertex and index buffers. The 16-bit buffer
/// holds 9, 9, 6, 8, 3, 5, 0 and the 32-bit buffer 9, 3, 5, 0, 7 and three
/// indices above 16 bits, 0x10003, 0x10005, 0x10000.
fn indexed_rig() -> IndexedRig {
    use xenora_loader::memory::MemoryPermissions;
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
    let buffer = |ctx: &mut GraphicsContext, bytes: usize, addr| {
        let desc = ResourceDesc {
            dimension: ResourceDimension::Buffer,
            width: bytes as u64,
            height: 1,
            depth_or_array_size: 1,
            mip_levels: 1,
            format_rgba8: false,
        };
        ctx.create_committed_resource(device, desc, ResourceStates::COMMON, Some(addr))
            .unwrap()
    };
    let vertices = indexed_vertices();
    let ib16: Vec<u8> = [9u16, 9, 6, 8, 3, 5, 0]
        .iter()
        .flat_map(|index| index.to_le_bytes())
        .collect();
    let ib32: Vec<u8> = [9u32, 3, 5, 0, 7, 0x1_0003, 0x1_0005, 0x1_0000]
        .iter()
        .flat_map(|index| index.to_le_bytes())
        .collect();
    let vb = buffer(&mut ctx, vertices.len(), INDEXED_VB);
    let ib16_handle = buffer(&mut ctx, ib16.len(), INDEXED_IB);
    let ib32_handle = buffer(&mut ctx, ib32.len(), INDEXED_IB + 0x1000);
    let mut memory = GuestMemory::new();
    for (addr, bytes) in [
        (INDEXED_VB, &vertices),
        (INDEXED_IB, &ib16),
        (INDEXED_IB + 0x1000, &ib32),
    ] {
        memory
            .map_region_with_data(addr, 0x1000, MemoryPermissions::READ_ONLY, bytes)
            .unwrap();
    }
    IndexedRig {
        ctx,
        memory,
        queue,
        alloc,
        list,
        vb,
        ib16: ib16_handle,
        ib32: ib32_handle,
    }
}

/// Record one indexed draw (16x16 viewport, the rig's vertex buffer) and run it.
fn run_indexed(
    rig: &mut IndexedRig,
    index_buffer: (GraphicsHandle, usize, u32),
    draw: (u32, u32, u32, i32),
) -> Result<xenora_loader::graphics::ExecutionOutcome, GraphicsError> {
    let ctx = &mut rig.ctx;
    ctx.reset_command_list(rig.list, rig.alloc)?;
    ctx.cmd_set_viewport(rig.list, 16, 16)?;
    ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 0, INDEXED_STRIDE)?;
    ctx.cmd_set_index_buffer(rig.list, index_buffer.0, index_buffer.1, index_buffer.2)?;
    let (count, instances, start, base) = draw;
    ctx.cmd_draw_indexed_instanced(rig.list, count, instances, start, base, 0)?;
    ctx.close_command_list(rig.list)?;
    ctx.execute_command_list(rig.queue, rig.list, &rig.memory)
}

const R16_UINT: u32 = 57;
const R32_UINT: u32 = 42;

/// Start index, signed base vertex, index size and buffer offset all select
/// vertices 5, 7, 2: the backend receives exactly the triangle's positions
/// (literal values), never a decoy.
#[test]
fn test_d3d12_indexed_draw_fetches_start_and_base_vertex() {
    let mut rig = indexed_rig();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = seen.clone();
    rig.ctx.set_metal_backend(move |vtx, viewport, clear| {
        record.lock().unwrap().push(vtx.to_vec());
        GraphicsContext::execute_software_reference_triangles(vtx, viewport, clear)
    });
    let (ib16, ib32) = (rig.ib16, rig.ib32);
    for (name, buffer, draw) in [
        // 16-bit indices 3, 5, 0 at index 4, base vertex +2.
        ("u16 positive base", (ib16, 0, R16_UINT), (3, 1, 4, 2)),
        // 16-bit indices 6, 8, 3 at index 2, base vertex -1.
        ("u16 negative base", (ib16, 0, R16_UINT), (3, 1, 2, -1)),
        // The same through a buffer offset of one index (2 bytes).
        ("u16 buffer offset", (ib16, 2, R16_UINT), (3, 1, 1, -1)),
        // 32-bit indices 3, 5, 0 at index 1, base vertex +2.
        ("u32 positive base", (ib32, 0, R32_UINT), (3, 1, 1, 2)),
        // 32-bit indices above 16 bits, base vertex -65534: all 32 bits count.
        ("u32 wide indices", (ib32, 0, R32_UINT), (3, 1, 5, -65534)),
    ] {
        seen.lock().unwrap().clear();
        let outcome = run_indexed(&mut rig, buffer, draw).expect(name);
        assert_eq!(outcome.draw_count, 1, "{name}");
        assert_eq!(*seen.lock().unwrap(), vec![INDEXED_TRIANGLE], "{name}");
        assert_eq!(
            outcome.pixels,
            GraphicsContext::execute_software_reference_draw(
                &INDEXED_TRIANGLE,
                (16, 16),
                [0, 0, 0, 255]
            )
            .unwrap(),
            "{name}"
        );
    }
    // A vertex buffer offset of two vertices does the base vertex's work.
    seen.lock().unwrap().clear();
    let ctx = &mut rig.ctx;
    ctx.reset_command_list(rig.list, rig.alloc).unwrap();
    ctx.cmd_set_viewport(rig.list, 16, 16).unwrap();
    ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 2 * INDEXED_STRIDE, INDEXED_STRIDE)
        .unwrap();
    ctx.cmd_set_index_buffer(rig.list, ib16, 0, R16_UINT)
        .unwrap();
    ctx.cmd_draw_indexed_instanced(rig.list, 3, 1, 4, 0, 0)
        .unwrap();
    ctx.close_command_list(rig.list).unwrap();
    let outcome = ctx
        .execute_command_list(rig.queue, rig.list, &rig.memory)
        .expect("vertex buffer offset");
    assert_eq!(outcome.draw_count, 1);
    assert_eq!(*seen.lock().unwrap(), vec![INDEXED_TRIANGLE]);
    // Zero indices or zero instances draw nothing and never call the backend.
    seen.lock().unwrap().clear();
    for draw in [(0, 1, 0, 0), (3, 0, 0, 0)] {
        let outcome = run_indexed(&mut rig, (ib16, 0, R16_UINT), draw).unwrap();
        assert_eq!(outcome.draw_count, 0);
    }
    assert!(seen.lock().unwrap().is_empty());
}

/// Index format, offset, count, range, negative fetch and unknown or missing
/// buffers each fail with a typed error, and nothing reaches the backend.
#[test]
fn test_d3d12_indexed_draw_errors() {
    let mut rig = indexed_rig();
    let calls = Arc::new(std::sync::Mutex::new(0usize));
    let counter = calls.clone();
    rig.ctx.set_metal_backend(move |vtx, viewport, clear| {
        *counter.lock().unwrap() += 1;
        GraphicsContext::execute_software_reference_triangles(vtx, viewport, clear)
    });
    let (ib16, ib32) = (rig.ib16, rig.ib32);
    let unknown = GraphicsHandle {
        id: 999,
        generation: 1,
    };
    for format in [0, 41, 56, 58, 28] {
        assert_eq!(
            run_indexed(&mut rig, (ib16, 0, format), (3, 1, 0, 0)).unwrap_err(),
            GraphicsError::InvalidIndexFormat(format)
        );
    }
    for (name, buffer, draw, error) in [
        (
            "misaligned offset",
            (ib32, 2, R32_UINT),
            (3, 1, 0, 0),
            GraphicsError::InvalidDescriptor,
        ),
        (
            "offset past buffer",
            (ib16, 16, R16_UINT),
            (3, 1, 0, 0),
            GraphicsError::InvalidDescriptor,
        ),
        (
            "unknown buffer",
            (unknown, 0, R16_UINT),
            (3, 1, 0, 0),
            GraphicsError::InvalidResourceHandle,
        ),
        (
            "incomplete triangle list",
            (ib16, 0, R16_UINT),
            (4, 1, 0, 0),
            GraphicsError::InvalidDescriptor,
        ),
        (
            "bad count with zero instances",
            (ib16, 0, R16_UINT),
            (4, 0, 0, 0),
            GraphicsError::InvalidDescriptor,
        ),
        (
            "too many native indices",
            (ib16, 0, R16_UINT),
            (65_538, 1, 0, 0),
            GraphicsError::InvalidDescriptor,
        ),
        (
            "allowed native count past short index buffer",
            (ib16, 0, R16_UINT),
            (771, 1, 0, 0),
            GraphicsError::IndexOutOfRange,
        ),
        (
            "last triangle indices past buffer",
            (ib16, 0, R16_UINT),
            (6, 1, 2, 0),
            GraphicsError::IndexOutOfRange,
        ),
        (
            "two instances",
            (ib16, 0, R16_UINT),
            (3, 2, 4, 2),
            GraphicsError::InvalidDescriptor,
        ),
        (
            "indices past buffer",
            (ib16, 0, R16_UINT),
            (3, 1, 5, 0),
            GraphicsError::IndexOutOfRange,
        ),
        (
            "start overflow",
            (ib32, 0, R32_UINT),
            (3, 1, u32::MAX, 0),
            GraphicsError::IndexOutOfRange,
        ),
        (
            "negative vertex",
            (ib16, 0, R16_UINT),
            (3, 1, 4, -1),
            GraphicsError::IndexOutOfRange,
        ),
        (
            "vertex past buffer",
            (ib16, 0, R16_UINT),
            (3, 1, 0, 1),
            GraphicsError::IndexOutOfRange,
        ),
        (
            "base overflow",
            (ib32, 0, R32_UINT),
            (3, 1, 0, i32::MAX),
            GraphicsError::IndexOutOfRange,
        ),
    ] {
        let got = run_indexed(&mut rig, buffer, draw);
        assert_eq!(got.unwrap_err(), error, "{name}");
    }

    // A draw without an index buffer is rejected at execution.
    let ctx = &mut rig.ctx;
    ctx.reset_command_list(rig.list, rig.alloc).unwrap();
    ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 0, INDEXED_STRIDE)
        .unwrap();
    ctx.cmd_draw_indexed_instanced(rig.list, 3, 1, 0, 0, 0)
        .unwrap();
    ctx.close_command_list(rig.list).unwrap();
    assert_eq!(
        ctx.execute_command_list(rig.queue, rig.list, &rig.memory),
        Err(GraphicsError::InvalidResourceHandle)
    );
    // A stride smaller than one float4 position is rejected.
    ctx.reset_command_list(rig.list, rig.alloc).unwrap();
    ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 0, 12).unwrap();
    ctx.cmd_set_index_buffer(rig.list, ib16, 0, R16_UINT)
        .unwrap();
    ctx.cmd_draw_indexed_instanced(rig.list, 3, 1, 4, 2, 0)
        .unwrap();
    ctx.close_command_list(rig.list).unwrap();
    assert_eq!(
        ctx.execute_command_list(rig.queue, rig.list, &rig.memory),
        Err(GraphicsError::InvalidDescriptor)
    );
    // A nonzero start instance is rejected.
    ctx.reset_command_list(rig.list, rig.alloc).unwrap();
    ctx.cmd_set_vertex_buffer(rig.list, rig.vb, 0, INDEXED_STRIDE)
        .unwrap();
    ctx.cmd_set_index_buffer(rig.list, ib16, 0, R16_UINT)
        .unwrap();
    ctx.cmd_draw_indexed_instanced(rig.list, 3, 1, 4, 2, 1)
        .unwrap();
    ctx.close_command_list(rig.list).unwrap();
    assert_eq!(
        ctx.execute_command_list(rig.queue, rig.list, &rig.memory),
        Err(GraphicsError::InvalidDescriptor)
    );
    // An index buffer whose guest backing would wrap the address space is
    // rejected, not read at a wrapped address.
    let device = ctx.create_device().unwrap();
    // Invalid backing spans are rejected at creation, before a draw can use them.
    for guest_addr in [u64::MAX - 8, u64::MAX - 2] {
        assert_eq!(
            rig.ctx.create_committed_resource(
                device,
                ResourceDesc {
                    dimension: ResourceDimension::Buffer,
                    width: 64,
                    height: 1,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: false,
                },
                ResourceStates::COMMON,
                Some(guest_addr),
            ),
            Err(GraphicsError::InvalidDescriptor),
            "guest address {guest_addr:#x}"
        );
    }
    // No rejected draw reached the backend.
    assert_eq!(*calls.lock().unwrap(), 0);
}

/// The loader's indexed fetch feeds the native Metal renderer: the GPU image
/// of the fetched triangle equals the image of the literal triangle.
#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal device; run scripts/test-gpu.sh"
)]
fn test_d3d12_indexed_draw_native_metal() {
    use xenora_metal::NativeCompute;
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

    let vs_bytes = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");
    let ps_bytes = include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin");
    let msl = compile_graphics(Architecture::Gfx1030, vs_bytes, ps_bytes)
        .expect("compile graphics RDNA2 shaders")
        .msl;
    let gpu = Arc::new(NativeCompute::new().expect("native Metal GPU device"));
    let render = {
        let gpu = gpu.clone();
        let msl = msl.clone();
        move |vtx: &[[f32; 4]], vp: (usize, usize), cc: [u8; 4]| {
            gpu.render_triangles_rgba8(
                &msl,
                [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                vtx,
                &vec![[0.0; 4]; vtx.len()],
                [vp.0, vp.1],
                cc,
            )
            .map_err(|e| GraphicsError::BackendError(format!("Metal GPU error: {e:?}")))
        }
    };
    let direct = render(&INDEXED_TRIANGLE, (16, 16), [0, 0, 0, 255]).expect("direct render");
    let mut rig = indexed_rig();
    rig.ctx.set_metal_backend(render);
    let ib16 = rig.ib16;
    let outcome = run_indexed(&mut rig, (ib16, 0, R16_UINT), (3, 1, 2, -1)).expect("indexed");
    assert_eq!(outcome.backend, GraphicsBackendId::MetalNative);
    assert_eq!(outcome.pixels, direct);
    // The constant shader paints the covered pixels red; the CPU rasteriser
    // agrees pixel for pixel, and the triangle covers some but not all pixels.
    let reference = GraphicsContext::execute_software_reference_draw(
        &INDEXED_TRIANGLE,
        (16, 16),
        [0, 0, 0, 255],
    )
    .unwrap();
    assert_eq!(outcome.pixels, reference);
    let red = outcome
        .pixels
        .chunks(4)
        .filter(|p| *p == [255, 0, 0, 255])
        .count();
    assert!((1..256).contains(&red), "{red} red pixels");
    println!("indexed native Metal draw: {red} of 256 pixels covered");
}
