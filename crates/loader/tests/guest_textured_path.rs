// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #434: a guest PE32+ program binds its own texture and draws a
//! textured triangle, end to end. Issue #559 also covers indexed draws.
//!
//! A guest PE32+ program is built from source by
//! `tests/compat_fixtures/build-guest-textured.sh`, loaded by the real startup
//! path, and driven through the guest graphics services. The program creates a
//! 2x2 `R8G8B8A8` texture, a descriptor heap, a view, a sampler and a root
//! signature, binds them, draws, reads the pixels back into its own memory and
//! reports through a `.gate` section.
//!
//! The expected pixels are literals in this file. They are not computed from
//! the rasteriser, the renderer or any other function under test. Sequential
//! draws sample column 0 (red). Indexed draws select a different triangle
//! that samples column 1 (blue). Each expected frame has one uniform color.
//!
//! A failed source build fails the test.

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
const MAGIC: u32 = 0x6772_7478;
/// The number of structural checks the program makes.
const CHECKS: u32 = 6;
/// The render target is eight by eight pixels.
const WIDTH: u32 = 8;
const HEIGHT: u32 = 8;
/// The bytes of one frame: 8 * 8 pixels of four bytes each.
const FRAME_BYTES: usize = 256;

/// The frame the program must draw: 64 pixels of `R8G8B8A8` red.
///
/// The guest's texture is two by two and its column 0 is red, column 1 is
/// blue. Every vertex carries the texture coordinate `(0.25, 0.5)`, and the
/// repository's sampling rule is `texel = floor(clamp(coord, 0, 1) * size)`,
/// so `u = 0.25` over a width of two selects column 0, which is red. Both
/// rows of that column are red, so the frame does not depend on the row the
/// sampler picks.
const EXPECTED_FRAME: [u8; FRAME_BYTES] = [
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
];

/// Both indexed draws resolve the same three blue vertices. With a
/// constant u = 0.75 and a two-column texture, every pixel picks column 1.
const INDEXED_PIXEL: [u8; 4] = [0, 0, 255, 255];
const INDEXED_GUESTS: [&str; 2] = ["guest-textured-index16-O1", "guest-textured-index32-O2"];

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
/// The build script writes into `artifacts/guest-textured` by default.
fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let root = repo_root();
        let script = root.join("tests/compat_fixtures/build-guest-textured.sh");
        let build = Command::new("sh")
            .arg(&script)
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-guest-textured.sh");
        assert!(
            build.status.success(),
            "the guest textured program did not build; it needs clang and an LLD:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        find_build_dir(&root)
    })
}

/// The directory the build script wrote the program into.
fn find_build_dir(root: &Path) -> PathBuf {
    let wanted = "guest-textured-O0.exe";
    let primary = root.join("artifacts/guest-textured");
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
    frame_len: u32,
    frame_ptr: u64,
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
    let word = |offset: u64| {
        memory
            .read_u32(at + offset)
            .expect("the result block is readable")
    };
    ResultBlock {
        magic: word(0),
        checks: word(4),
        width: word(8),
        height: word(12),
        backend: word(16),
        draw_count: word(20),
        frame_len: word(24),
        // The pointer is a 64-bit field, so it starts at offset 32.
        frame_ptr: memory
            .read_u64(at + 32)
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

/// The guest binds its own texture and draws a textured triangle, at every
/// optimisation level, on the software reference backend.
#[test]
fn the_guest_textured_path_draws_the_sampled_texture_on_the_software_backend() {
    let original = [
        "guest-textured-O0",
        "guest-textured-O1",
        "guest-textured-O2",
        "guest-textured-Os",
    ];
    for name in original.into_iter().chain(INDEXED_GUESTS) {
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
        assert_eq!(block.frame_len, FRAME_BYTES as u32, "{name}: frame bytes");

        // The pixels, read out of the guest's own memory: the colour the
        // sampler picked out of the guest's texture, not the clear colour.
        let frame = frame_bytes(&outcome, block.frame_ptr);
        assert_expected_frame(name, &frame);
    }
}

fn assert_expected_frame(name: &str, frame: &[u8]) {
    if INDEXED_GUESTS.contains(&name) {
        assert_eq!(
            frame,
            INDEXED_PIXEL.repeat(FRAME_BYTES / INDEXED_PIXEL.len()),
            "{name}: the indexed triangle must sample blue"
        );
    } else {
        assert_eq!(
            frame, EXPECTED_FRAME,
            "{name}: the triangle must sample red"
        );
    }
}

/// A program that imports something nobody provides is refused before its
/// entry point runs.
#[test]
fn an_import_nobody_provides_is_refused_before_entry() {
    let error = run("guest-textured-missing", BUDGET)
        .err()
        .expect("the image is refused");
    assert!(
        error.to_lowercase().contains("import"),
        "unexpected refusal: {error}"
    );
}

// ---------------------------------------------------------------------------
// The native Metal path
// ---------------------------------------------------------------------------

/// The same program, on a native Metal backend.
///
/// The frame must hold exactly the same pixels as on the software reference
/// path: the Metal backend samples the guest's texture with the translated
/// `image_sample_lz` stage and the nearest clamp sampler.
#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal device; run scripts/test-gpu.sh"
)]
fn the_guest_textured_path_draws_the_sampled_texture_on_native_metal() {
    use xenora_loader::graphics::{GraphicsError, TextureFormat};
    use xenora_metal::NativeCompute;
    use xenora_metal::sampler::MetalSampler;
    use xenora_metal::texture_pipeline::{MetalRgba8Texture, Rgba8MipLevel, Rgba8TextureDesc};
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

    let msl = compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
        &image_sample_lz_ps(),
    )
    .expect("compile translated image_sample_lz shader")
    .msl;
    let gpu = Arc::new(NativeCompute::new().expect("native Metal GPU device"));
    let entries = [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT];

    for name in std::iter::once("guest-textured-O1").chain(INDEXED_GUESTS) {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        {
            let backend = Arc::clone(&gpu);
            let msl = msl.clone();
            let mut graphics = state.graphics.lock().expect("the graphics context lock");
            graphics.set_metal_textured_backend(move |draw| {
                assert_eq!(
                    draw.texture.format,
                    TextureFormat::Rgba8Unorm,
                    "the guest's texture format"
                );
                let metal_error = |error: xenora_metal::MetalError| {
                    GraphicsError::BackendError(format!("Metal GPU error: {error:?}"))
                };
                let sampler = MetalSampler::new(
                    backend.device(),
                    xenora_texture::SamplerDesc::nearest_clamp(),
                )
                .map_err(metal_error)?;
                let width = draw.texture.width as usize;
                let height = draw.texture.height as usize;
                let desc = Rgba8TextureDesc::new(width, height, 1).map_err(metal_error)?;
                let level = Rgba8MipLevel::new_packed(0, width, height, draw.texture.bytes);
                let texture = MetalRgba8Texture::upload(backend.device(), desc, &[level])
                    .map_err(metal_error)?;
                backend
                    .render_rgba8_sampled(
                        &msl,
                        entries,
                        &draw.positions,
                        &draw.texcoords,
                        [draw.viewport.0, draw.viewport.1],
                        draw.clear_color,
                        Some((&texture, &sampler)),
                    )
                    .map_err(metal_error)
            });
        }

        let outcome =
            run_with_state(state, name, BUDGET).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            outcome.result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{name}: the guest did not end itself"
        );
        assert_eq!(outcome.state.exit_status(), Some(0), "{name}");

        let block = result_block(&outcome);
        assert_eq!(block.magic, MAGIC, "result magic");
        assert_eq!(block.checks, CHECKS, "structural checks");
        assert_eq!(block.width, WIDTH, "render target width");
        assert_eq!(block.height, HEIGHT, "render target height");
        assert_eq!(block.backend, 1, "the guest did not see the Metal backend");
        assert_eq!(block.draw_count, 1, "draw count");
        assert_eq!(block.frame_len, FRAME_BYTES as u32, "frame bytes");

        let frame = frame_bytes(&outcome, block.frame_ptr);
        assert_expected_frame(name, &frame);
    }
    println!(
        "device={} guest-bound texture sampled through the translated image_sample_lz stage",
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
