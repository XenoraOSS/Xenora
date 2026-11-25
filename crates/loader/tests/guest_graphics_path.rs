// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #412: the guest graphics command path, end to end.
//!
//! A guest PE32+ program is built from source by
//! `tests/compat_fixtures/build-guest-graphics.sh`, loaded by the real startup
//! path, and driven through the guest graphics services. The program renders
//! two frames, reads the pixels back into its own memory and reports through a
//! `.gate` section.
//!
//! The expected pixels are literals in this file. They are not computed from
//! the rasteriser or the renderer under test: the constant colour is what
//! `fixtures/rdna2/x9-constant-ps.bin` paints and what the software reference
//! rasteriser paints for a covered pixel, and the clear colour is what the
//! program asks the backend to clear to.
//!
//! The build script and the guest program are built by other workers. This
//! test fails, and does not skip, when they are missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

/// The load address of the guest image. It differs from the preferred base,
/// so the image's relocations matter.
const IMAGE_BASE: u64 = 0x2_5000_0000;
const STACK_TOP: u64 = 0x2_0001_0000;
const TLS_BASE: u64 = 0x3_0000_0000;
/// The instruction budget for a complete run.
const BUDGET: usize = 5_000_000;

/// The magic the program writes into its result block.
const MAGIC: u32 = 0x6772_6170;
/// The number of structural checks the program makes.
const CHECKS: u32 = 7;
/// The render target is eight by eight pixels.
const WIDTH: u32 = 8;
const HEIGHT: u32 = 8;
/// The bytes of one frame: 8 * 8 pixels of four bytes each.
const FRAME_BYTES: usize = 256;
/// The colour the constant pixel shader paints for a covered pixel.
const FRAME0_PIXEL: [u8; 4] = [255, 0, 0, 255];
/// The colour the program clears frame one to. Its triangle covers no pixel.
const FRAME1_PIXEL: [u8; 4] = [10, 20, 30, 255];

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// The repository root.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

/// Build every variant once and return the directory that holds them.
///
/// The build script writes into `artifacts/guest-graphics` by default.
fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let root = repo_root();
        let script = root.join("tests/compat_fixtures/build-guest-graphics.sh");
        let build = Command::new("sh")
            .arg(&script)
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-guest-graphics.sh");
        assert!(
            build.status.success(),
            "the guest graphics program did not build; it needs clang and an LLD:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        find_build_dir(&root)
    })
}

/// The directory the build script wrote the program into.
fn find_build_dir(root: &Path) -> PathBuf {
    let wanted = "guest-graphics-O0.exe";
    let primary = root.join("artifacts/guest-graphics");
    if primary.join(wanted).is_file() {
        return primary;
    }
    // The script may name its output directory differently; look for the
    // program one level below the artifacts directory.
    let artifacts = root.join("artifacts");
    if let Ok(entries) = std::fs::read_dir(&artifacts) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate.join(wanted).is_file() {
                return candidate;
            }
        }
    }
    panic!(
        "the build script did not produce {wanted} in {} or in a directory below it",
        artifacts.display()
    );
}

// ---------------------------------------------------------------------------
// Running the guest
// ---------------------------------------------------------------------------

struct Outcome {
    result: StartupResult,
    state: Arc<ServicesState>,
}

/// Run one built variant through the real startup path with the standard
/// services, loaded away from its preferred base so its relocations matter.
fn run(name: &str, budget: usize) -> Result<Outcome, String> {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    run_with_state(state, name, budget)
}

/// Run one built variant against a state the caller has already set up.
fn run_with_state(state: Arc<ServicesState>, name: &str, budget: usize) -> Result<Outcome, String> {
    let bytes =
        std::fs::read(built().join(format!("{name}.exe"))).expect("read the built guest program");
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: IMAGE_BASE,
        stack_top: STACK_TOP,
        tls_base: TLS_BASE,
    };
    match run_synthetic_startup(&bytes, layout, registry, budget) {
        Ok(result) => Ok(Outcome { result, state }),
        Err(error) => {
            // Nothing may have run or been left behind.
            assert_eq!(state.exit_status(), None, "{name}");
            assert_eq!(state.open_handle_count(), 0, "{name}");
            Err(error.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// The result block
// ---------------------------------------------------------------------------

/// What the program reports in its `.gate` section.
#[derive(Clone, Copy, Debug)]
struct ResultBlock {
    magic: u32,
    checks: u32,
    width: u32,
    height: u32,
    backend: u32,
    draw_count: u32,
    frame0_len: u32,
    frame1_len: u32,
    frame0_ptr: u64,
    frame1_ptr: u64,
}

/// Read the result block out of the mapped image.
fn result_block(outcome: &Outcome) -> ResultBlock {
    let image = &outcome.result.image;
    let section = image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".gate")
        .expect("a .gate section");
    let at = IMAGE_BASE + u64::from(section.virtual_address);
    let memory = &image.memory;
    ResultBlock {
        magic: memory.read_u32(at).expect("the result block is readable"),
        checks: memory
            .read_u32(at + 4)
            .expect("the result block is readable"),
        width: memory
            .read_u32(at + 8)
            .expect("the result block is readable"),
        height: memory
            .read_u32(at + 12)
            .expect("the result block is readable"),
        backend: memory
            .read_u32(at + 16)
            .expect("the result block is readable"),
        draw_count: memory
            .read_u32(at + 20)
            .expect("the result block is readable"),
        frame0_len: memory
            .read_u32(at + 24)
            .expect("the result block is readable"),
        frame1_len: memory
            .read_u32(at + 28)
            .expect("the result block is readable"),
        frame0_ptr: memory
            .read_u64(at + 32)
            .expect("the result block is readable"),
        frame1_ptr: memory
            .read_u64(at + 40)
            .expect("the result block is readable"),
    }
}

/// The bytes of one frame, read from the guest's own memory.
fn frame_bytes(outcome: &Outcome, at: u64) -> Vec<u8> {
    let mut bytes = vec![0u8; FRAME_BYTES];
    outcome
        .result
        .image
        .memory
        .read_bytes(at, &mut bytes)
        .expect("the frame is readable from guest memory");
    bytes
}

/// The pixels a frame of one colour is.
fn solid_frame(pixel: [u8; 4]) -> Vec<u8> {
    pixel.repeat(64)
}

// ---------------------------------------------------------------------------
// What the built program declares, read from the program itself
// ---------------------------------------------------------------------------

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(
        bytes[at..at + 2]
            .try_into()
            .expect("two bytes in the program"),
    )
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(
        bytes[at..at + 4]
            .try_into()
            .expect("four bytes in the program"),
    )
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(
        bytes[at..at + 8]
            .try_into()
            .expect("eight bytes in the program"),
    )
}

/// A section of the program: virtual address, virtual size, raw pointer and
/// raw size.
type Section = (u32, u32, u32, u32);

/// The sections of a PE32+ program.
fn sections(bytes: &[u8]) -> Vec<Section> {
    let pe = u32_at(bytes, 0x3C) as usize;
    let coff = pe + 4;
    let count = u16_at(bytes, coff + 2) as usize;
    let size_of_optional = u16_at(bytes, coff + 16) as usize;
    let headers = pe + 24 + size_of_optional;
    (0..count)
        .map(|index| {
            let at = headers + index * 40;
            (
                u32_at(bytes, at + 12),
                u32_at(bytes, at + 8),
                u32_at(bytes, at + 20),
                u32_at(bytes, at + 16),
            )
        })
        .collect()
}

/// The offset of a directory entry in the optional header, if it is there.
fn directory(bytes: &[u8], index: usize) -> Option<(u32, u32)> {
    let pe = u32_at(bytes, 0x3C) as usize;
    let opt = pe + 24;
    let count = u32_at(bytes, opt + 108) as usize;
    if index >= count {
        return None;
    }
    let at = opt + 112 + index * 8;
    let rva = u32_at(bytes, at);
    let size = u32_at(bytes, at + 4);
    if size == 0 {
        return None;
    }
    Some((rva, size))
}

/// The file offset of an address in the program.
fn file_offset(sections: &[Section], rva: u32) -> Option<usize> {
    for &(virtual_address, virtual_size, raw_pointer, raw_size) in sections {
        let span = virtual_size.max(raw_size);
        if rva >= virtual_address && rva < virtual_address + span {
            let offset = raw_pointer + (rva - virtual_address);
            return Some(offset as usize);
        }
    }
    None
}

/// The number of imported functions the program declares.
///
/// This is read from the program's own import directory, so the loader's
/// report is checked against the program and not against a constant.
fn declared_imports(bytes: &[u8]) -> usize {
    let sections = sections(bytes);
    let Some((rva, _)) = directory(bytes, 1) else {
        return 0;
    };
    let Some(at) = file_offset(&sections, rva) else {
        panic!("the import directory is inside the program");
    };
    let mut count = 0;
    let mut descriptor = at;
    loop {
        let lookup = u32_at(bytes, descriptor);
        let address = u32_at(bytes, descriptor + 12);
        let first_thunk = u32_at(bytes, descriptor + 16);
        if lookup == 0 && address == 0 && first_thunk == 0 {
            break;
        }
        let thunks = if lookup != 0 { lookup } else { first_thunk };
        let mut thunk = file_offset(&sections, thunks).expect("a thunk array in the program");
        while u64_at(bytes, thunk) != 0 {
            count += 1;
            thunk += 8;
        }
        descriptor += 20;
    }
    count
}

/// The number of DIR64 relocations the program declares.
///
/// This is read from the program's own base relocation directory, so the
/// loader's report is checked against the program and not against a constant.
fn declared_dir64(bytes: &[u8]) -> usize {
    let sections = sections(bytes);
    let Some((rva, size)) = directory(bytes, 5) else {
        return 0;
    };
    let Some(at) = file_offset(&sections, rva) else {
        panic!("the relocation directory is inside the program");
    };
    let end = at + size as usize;
    let mut count = 0;
    let mut block = at;
    while block + 8 <= end {
        let block_size = u32_at(bytes, block + 4) as usize;
        if block_size < 8 {
            break;
        }
        let mut entry = block + 8;
        while entry + 4 <= block + block_size {
            let kind = u32_at(bytes, entry) >> 12;
            if kind == 10 {
                count += 1;
            }
            entry += 4;
        }
        block += block_size;
    }
    count
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn the_guest_graphics_path_renders_two_frames_on_the_software_backend() {
    for name in [
        "guest-graphics-O0",
        "guest-graphics-O1",
        "guest-graphics-O2",
        "guest-graphics-Os",
    ] {
        let outcome = run(name, BUDGET).unwrap_or_else(|error| panic!("{name}: {error}"));
        let result = &outcome.result;
        let state = &outcome.state;

        // It was loaded, relocated and bound by the real startup path. The
        // expected numbers are read from the built program itself.
        let program = std::fs::read(built().join(format!("{name}.exe")))
            .expect("read the built guest program");
        let imports = declared_imports(&program);
        assert!(imports > 0, "{name}: the program declares no import");
        assert_eq!(result.bound_imports, imports, "{name}: bound imports");
        assert_eq!(
            result.relocations.applied_dir64,
            declared_dir64(&program),
            "{name}: applied DIR64 relocations"
        );
        println!(
            "{name}: {imports} bound imports, {} applied DIR64 relocations",
            result.relocations.applied_dir64
        );

        // It ended itself, with status zero: every structural check passed.
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{name}"
        );
        assert_eq!(state.exit_status(), Some(0), "{name}");

        // It ran as translated ARM64 code and called the guest services.
        let metrics = &result.dbt_metrics;
        assert!(metrics.blocks_compiled > 0, "{name}: no block compiled");
        assert!(
            metrics.native_bytes_emitted > 0,
            "{name}: no native code emitted"
        );
        assert!(metrics.service_calls > 0, "{name}: no service call");

        // The result block it left behind.
        let block = result_block(&outcome);
        assert_eq!(block.magic, MAGIC, "{name}: result magic");
        assert_eq!(block.checks, CHECKS, "{name}: structural checks");
        assert_eq!(block.width, WIDTH, "{name}: render target width");
        assert_eq!(block.height, HEIGHT, "{name}: render target height");
        assert_eq!(block.backend, 0, "{name}: software reference backend");
        assert_eq!(block.draw_count, 1, "{name}: draw count");
        assert_eq!(
            block.frame0_len, FRAME_BYTES as u32,
            "{name}: frame 0 bytes"
        );
        assert_eq!(
            block.frame1_len, FRAME_BYTES as u32,
            "{name}: frame 1 bytes"
        );

        // The pixels, read out of the guest's own memory.
        let frame0 = frame_bytes(&outcome, block.frame0_ptr);
        let frame1 = frame_bytes(&outcome, block.frame1_ptr);
        assert_eq!(
            frame0,
            solid_frame(FRAME0_PIXEL),
            "{name}: frame 0 is the colour the constant shader paints"
        );
        assert_eq!(
            frame1,
            solid_frame(FRAME1_PIXEL),
            "{name}: frame 1 is the clear colour"
        );
        assert_ne!(frame0, frame1, "{name}: the two frames are the same");
    }
}

#[test]
fn an_import_nobody_provides_is_refused_before_entry() {
    let error = run("guest-graphics-missing", BUDGET)
        .err()
        .expect("the image is refused");
    assert!(
        error.starts_with("unresolved import dependency"),
        "unexpected refusal: {error}"
    );
}

#[test]
fn a_service_rejects_a_call_it_cannot_honour() {
    // The control program calls the graphics services wrongly: a zero-width
    // viewport. The service must answer with a failure HRESULT, so the
    // program ends with a status that is neither zero nor the number of a
    // structural check.
    let outcome = run("guest-graphics-bad", BUDGET)
        .unwrap_or_else(|error| panic!("guest-graphics-bad: {error}"));
    let status = outcome.state.exit_status().expect("the program exits");
    assert_ne!(status, 0, "the invalid call was accepted");
    assert!(
        !(1..=CHECKS).contains(&status),
        "the program exited with the number of a structural check ({status}), so the invalid \
         call was accepted"
    );
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(status),
        "the guest did not end itself"
    );
}

#[test]
fn a_step_budget_that_is_too_small_is_not_an_exit() {
    // The program needs a few hundred steps to finish, so this budget stops
    // it long before the end.
    const SMALL: usize = 100;
    let outcome = run("guest-graphics-O1", SMALL)
        .unwrap_or_else(|error| panic!("guest-graphics-O1: {error}"));
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::StepLimitExceeded
    );
    assert_eq!(outcome.state.exit_status(), None);
    // The budget really was too small: the same program runs for more steps
    // than the small budget allows.
    let full = run("guest-graphics-O1", BUDGET)
        .unwrap_or_else(|error| panic!("guest-graphics-O1: {error}"));
    assert!(
        full.result.dbt_metrics.instructions_executed > SMALL,
        "the program finished in {} steps, so a budget of {SMALL} was not small",
        full.result.dbt_metrics.instructions_executed
    );
}

// ---------------------------------------------------------------------------
// The native Metal path
// ---------------------------------------------------------------------------

/// The same program, on a native Metal backend.
///
/// The frames must hold exactly the same pixels as on the software reference
/// path: the constant shader paints the covered pixels, and the software
/// rasteriser agrees with it.
#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal device; run scripts/test-gpu.sh"
)]
fn the_guest_graphics_path_renders_two_frames_on_native_metal() {
    use xenora_loader::graphics::GraphicsError;
    use xenora_metal::NativeCompute;
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

    let vs_bytes = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");
    let ps_bytes = include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin");
    let msl = compile_graphics(Architecture::Gfx1030, vs_bytes, ps_bytes)
        .expect("compile the RDNA2 constant shaders")
        .msl;
    let gpu = Arc::new(NativeCompute::new().expect("native Metal GPU device"));

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    {
        let backend = Arc::clone(&gpu);
        let msl = msl.clone();
        let mut graphics = state.graphics.lock().expect("the graphics context lock");
        graphics.set_metal_backend(
            move |vtx: &[[f32; 4]; 3], vp: (usize, usize), cc: [u8; 4]| {
                backend
                    .render_rgba8(
                        &msl,
                        [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                        vtx,
                        &[[0.0; 4]; 3],
                        [vp.0, vp.1],
                        cc,
                    )
                    .map_err(|error| {
                        GraphicsError::BackendError(format!("Metal GPU error: {error:?}"))
                    })
            },
        );
    }

    let outcome = run_with_state(state, "guest-graphics-O1", BUDGET)
        .unwrap_or_else(|error| panic!("guest-graphics-O1: {error}"));
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "the guest did not end itself"
    );

    let block = result_block(&outcome);
    assert_eq!(block.magic, MAGIC, "result magic");
    assert_eq!(block.checks, CHECKS, "structural checks");
    assert_eq!(block.width, WIDTH, "render target width");
    assert_eq!(block.height, HEIGHT, "render target height");
    assert_eq!(block.backend, 1, "the guest did not see the Metal backend");
    assert_eq!(block.draw_count, 1, "draw count");
    assert_eq!(block.frame0_len, FRAME_BYTES as u32, "frame 0 bytes");
    assert_eq!(block.frame1_len, FRAME_BYTES as u32, "frame 1 bytes");

    let frame0 = frame_bytes(&outcome, block.frame0_ptr);
    let frame1 = frame_bytes(&outcome, block.frame1_ptr);
    assert_eq!(frame0, solid_frame(FRAME0_PIXEL), "frame 0 on Metal");
    assert_eq!(frame1, solid_frame(FRAME1_PIXEL), "frame 1 on Metal");
    assert_ne!(frame0, frame1, "the two frames are the same");
}
