# Texture format and layout inventory

This page records which texture formats and memory layouts Xenora handles today, and at which stage. It also records which gaps are worth closing and which are deferred. It is an inventory of the repository at the time of writing, built from the code and tests it links. It is not a support claim: a format listed here works only for the original synthetic fixtures named, on desktop specifications, and nothing here says anything about Xbox hardware or commercial titles.

## Stages

A format can be handled at one stage and missing at the next. The columns below keep them apart:

- **Container**: recognising a file or package format that holds textures (DDS, KTX and similar).
- **CPU decode**: turning one compressed block into texels on the CPU, used as an independent reference.
- **Layout**: converting between a stored memory layout and the row-major layout Metal expects.
- **Native**: uploading to a Metal texture of that pixel format and reading it back on the GPU.
- **Sampling**: a translated RDNA 2 image instruction reading the texture inside a shader.
- **Guest**: a guest program reaching the path through Xenora's D3D12-style graphics services.

"—" means not implemented. "Rejected" means the input is refused with an error rather than handled.

## Formats

| Format | Container | CPU decode | Layout | Native | Sampling | Guest |
| --- | --- | --- | --- | --- | --- | --- |
| RGBA8 unorm | — | not needed | row-major mips (`crates/metal/src/texture_pipeline.rs`) | upload and render target (`render_rgba8*`, `crates/metal/tests/native/texture_sampling.rs`) | `image_sample_lz`, 2D, level 0 (`crates/isa/src/lib.rs`, `crates/metal/tests/native/translated_render.rs`) | one-level 2D SRV, `DXGI_FORMAT_R8G8B8A8_UNORM` (`crates/loader/src/graphics.rs`, `crates/loader/tests/graphics_textures.rs`) |
| BC1 | — | `decode_bc1` (`crates/texture/src/lib.rs`) | synthetic tiled layout and pitched uploads (`TiledBc1`, `Bc1Upload`) | `read_tiled_bc1`, `read_bc1_upload` (`crates/metal/tests/native/texture.rs`) | sampled by the translated `image_sample_lz` pixel stage (`render_rgba8_sampled_bc1`, `crates/metal/tests/native/translated_bc1.rs`) | one-level 2D SRV, `DXGI_FORMAT_BC1_UNORM` (same files as RGBA8) |
| BC2, BC3 | — | `decode_bc2`, `decode_bc3` (`crates/texture/src/bc.rs`, `crates/texture/tests/bc_formats.rs`) | pitched upload (`BcAlphaUpload`) | `read_bc_alpha_upload` (`crates/metal/tests/native/texture_bc_alpha.rs`) | — | — |
| BC4, BC5 (unorm, snorm) | — | `decode_bc4_*`, `decode_bc5_*` (`crates/texture/src/bc.rs`, `crates/texture/tests/bc_formats.rs`) | pitched upload (`BcChannelUpload`) | `read_bc_channel_upload` (`crates/metal/tests/native/texture_bc_channel.rs`) | — | — |
| BC6H (ufloat, sfloat) | — | single-subset modes only; two-subset modes rejected (`crates/texture/src/bc6h.rs`, `crates/texture/tests/bc6h.rs`) | — | — | — | — |
| BC7 unorm | — | all eight modes (`crates/texture/src/bc7.rs`, `crates/texture/tests/bc7.rs`) | — | — | — | — |
| BC7 sRGB | — | same bytes as BC7; the sRGB transfer function is outside the decoder | — | — | — | — |
| Depth32Float | — | not needed | render attachment | `render_depth` with depth readback (`crates/metal/tests/native/depth.rs`) | — | — |

Sampler state is limited to nearest filtering and clamp-to-edge addressing (`crates/texture/src/sampler.rs`). Other filter or address modes are rejected.

Native BC paths require `supportsBCTextureCompression`. A device without it fails as a capability error (`crates/metal/src/compute.rs`), separately from implementation failures.

Known source differences, recorded where they are implemented:

- **BC6H:** the Khronos text and Microsoft's sample code disagree on whether a negative value that rounds to zero keeps its sign (`-0.0` against `0.0`). `bc6h.rs` follows Khronos.
- **BC1:** Apple's hardware rounds BC1's interpolated palette entries differently from the spec-exact decoder (171/84 against 170/85, see `CHANGELOG.md`). The sampled BC1 fixtures therefore use only endpoint colours.

## Layouts

| Layout | State | Where |
| --- | --- | --- |
| Row-major RGBA8 mips | implemented | `crates/metal/src/texture_pipeline.rs` |
| Pitched, layer-major BC uploads (one pitch for every mip, padded slices) | implemented for BC1–BC5 | `crates/texture/src/upload.rs` |
| Xenora's synthetic 2×2-tile BC1 layout | implemented; defined by Xenora's fixtures, not by any console | `crates/texture/src/lib.rs` |
| Per-subresource mip footprints for block-compressed chains | in progress: #391, PR #394 | — |
| Desktop RDNA 2 surface tiling (swizzle modes) | unknown; not researched in this repository | — |
| Console texture memory layouts | unknown; no public source reviewed | — |

## Public specifications

These are the primary sources the implementations above cite. The format rows already name them in their source files.

- Khronos Data Format Specification 1.3: the S3TC section (BC1–BC3), the RGTC section (BC4, BC5), section 20.1 "BC7" and section 20.2 "BC6H".
- Microsoft Direct3D documentation: "Texture Block Compression in Direct3D 11", "Block Compression (Direct3D 10)", "BC6H Format" and "BC7 Format". These are used for the format definitions and DXGI format codes, not for console behaviour.
- Apple Metal documentation: `MTLPixelFormat` and `MTLDevice.supportsBCTextureCompression`, for the native pixel formats and the capability check.
- AMD "RDNA 2" Instruction Set Architecture reference guide (GPUOpen): MIMG instructions such as `image_sample_lz`, and the image resource and sampler descriptor formats. These cover desktop gfx1030 only. [Xbox GPU evidence](xbox-gpu.md) records why desktop statements do not transfer to the console without separate evidence.

## Unknowns

These are research questions, not missing features:

- **Console texture memory layouts and tiling.** No public specification has been reviewed. Desktop RDNA 2 tiling would be a separate research note and would still not establish console behaviour.
- **Console resource descriptor formats.** Unknown. See [Xbox GPU evidence](xbox-gpu.md), which lists descriptor layout among the unresolved items.
- **Asset and audio containers.** Xenora parses none. The runtime's audio path accepts only raw PCM (`XENORA_AUDIO_FORMAT_PCM_S16LE` and `PCM_F32LE` in `crates/ffi/include/xenora.h`). No original workload has shown a need for a container or a compressed audio format.

## Follow-ups

Ranked by how close each is to existing tested paths. The first item is already tracked; the others are proposals, so do not start them without an issue and a claim.

1. **BC6H two-subset modes and native BC6H/BC7.** Tracked in #236 (CPU decode, claimed) and #237 (native upload and readback, waiting for maintainer approval). Do not duplicate them.
2. **Translated sampling of BC2–BC5.**
   - Fixture: original blocks with hand-chosen endpoints and indices, sampled by the existing `image_sample_lz` pixel stage.
   - Expected values: derived by hand from the Khronos tables. The `bc.rs` decoders must not produce them, so that the decoder and the reference stay independent.
   - Owners: Metal and Compiler.
3. **Additional sampler state** (linear filtering, wrap and mirror addressing).
   - Fixture: a small original RGBA8 texture with distinct texels.
   - Expected values: from the public Direct3D and Metal sampling equations, computed in exact fractions.
   - Owners: Metal and Compiler.
4. **Desktop RDNA 2 image resource descriptor decoding.** Needed before guest-supplied descriptors can select formats.
   - Fixture: original descriptor words for each supported format.
   - Expected values: from the AMD RDNA 2 ISA guide's descriptor tables.
   - Owner: ISA.
   - This does not cover console descriptors.

Deferred, with no demonstrated need:

- BC7 sRGB
- DDS and KTX containers
- ASTC and ETC (not part of the desktop Direct3D 12 BC set this target uses)
- any compressed audio format
- console tiling, until a public source exists
