# AMD FidelityFX licensing and native Metal research boundaries

This note records a bounded reading of the public AMD FidelityFX SDK at
upstream `main` commit `7539ae8b6568ad29ed78a753404351daf3b5cca7`
(`GPUOpen-LibrariesAndSDKs/FidelityFX-SDK`). It is a compatibility research
note, not an implementation report and not legal advice. No AMD source file or
shader has been copied into Xenora for this note.

## License finding

The SDK license is file-specific rather than a single uniform repository-wide
permission:

- The default terms in `docs/license.md` apply to files not explicitly listed.
  They permit installation, reproduction, copying and distribution **in binary
  form only**, and prohibit reverse engineering, decompilation and
  disassembly. Those default terms are not a basis for copying or adapting
  source into Xenora.
- The same document then enumerates 943 paths and states that those files are
  subject to MIT-style permission. A mechanical parse of the pinned revision
  counts 72 listed paths below `Kits\FidelityFX\upscalers`, 97 below
  `Kits\FidelityFX\framegeneration`, 52 technique-documentation paths and 26
  upscaler sample paths. This count is evidence about the document text only;
  it is not a legal conclusion that every file in those directories is covered.
- Because the list names individual paths, a future adaptation must check the
  exact file being considered, preserve its copyright and permission notice,
  record the upstream commit and document modifications. A directory-level
  inference is not sufficient.
- Signed binaries, generated shader blobs, build scripts and documentation may
  have different practical use even when listed. Listing does not establish
  that generated artifacts are reproducible inputs or that binary-only support
  files may be adapted as source.
- If any file is absent from the explicit list, or if a dependency pulls in a
  separately licensed component, it is outside the adaptation surface until a
  maintainer reviews that exact license.

The safest implementation posture for Xenora is therefore independent design
from the public behavioral documentation, with source-level borrowing only as a
later per-file exception after license review. This distinction is material:
publicly visible source is not automatically MIT-licensed source.

## Public temporal-upscaling contract

The public temporal super-resolution documentation describes an algorithm that
uses current and previous frame data. The host interface exposes these
principal dispatch inputs:

- render-resolution color and depth;
- two-dimensional motion vectors, normally at render resolution unless a
  display-resolution motion-vector flag is selected;
- an optional 1x1 exposure texture or automatic exposure;
- optional reactive and transparency/composition masks;
- a presentation-resolution output;
- jitter offset, motion-vector scale, render and output dimensions;
- sharpening enablement and sharpness;
- frame time delta, pre-exposure, camera near/far values;
- a reset flag for discontinuous camera motion.

Important input semantics documented upstream include:

- render-resolution inputs other than motion vectors are normally rendered
  with sub-pixel jitter; motion vectors are normally unjittered unless the
  documented jitter-cancellation mode is selected;
- motion vectors describe current-pixel to previous-pixel motion in pixel
  screen space, with `motionVectorScale` converting other source spaces;
- depth inversion and infinite-far-plane configuration are explicit creation
  flags rather than inferred implementation details;
- a reactive mask reduces reliance on temporal history for alpha-blended or
  otherwise non-depth/motion-vector representing content;
- a transparency/composition mask affects history protection and lock behavior;
- the reset flag invalidates temporal reuse after a camera jump cut and may
  clear additional internal resources.

These are behavioral requirements for any independent implementation. They do
not by themselves establish bit-exact AMD FSR behavior.

## Metal compatibility boundaries

A native Metal implementation can represent the lifecycle as compute passes in
the same command buffer as surrounding rendering, with explicit ownership of
input and history textures. The required host-side state is larger than a
single dispatch:

1. immutable context configuration: maximum render/output sizes, flags,
   depth interpretation, HDR/color interpretation and sharpening policy;
2. externally supplied per-frame resources: color, depth, motion vectors,
   optional exposure/reactivity/transparency inputs and output;
3. implementation-owned temporal state: history color/depth-derived data,
   exposure or luminance statistics, confidence/reactivity intermediates and
   generation-dependent scratch textures;
4. synchronization ordering: the producing render work must complete before
   upscaling reads its inputs, and upscaling writes must complete before the
   output is consumed;
5. reset and resize behavior: camera discontinuities must invalidate temporal
   state, while dimension changes should recreate or resize generation-owned
   resources transactionally rather than mixing incompatible history.

Metal does not use DirectX resource states or Vulkan image layouts. The
translation work is therefore not a state-name mapping. Xenora must instead
prove command-buffer ordering, texture usage declarations, hazard scope and
ownership lifetime. Runtime capability detection is required for SIMD-group,
memory-layout, texture-format and performance behaviors across Apple GPU
generations.

## Independent fixture and oracle requirements

Before any implementation claim, Xenora needs original fixtures that do not
depend on AMD sample assets or proprietary game content:

- synthetic low-resolution color, depth and motion-vector sequences generated
  by Xenora test code;
- deterministic translations and camera jump cuts with expected history reset;
- coverage cases for missing reactive/transparency inputs and automatic versus
  supplied exposure;
- negative controls for mismatched dimensions, invalid jitter, invalid depth
  configuration and stale output reuse;
- CPU reference generation of the fixture input fields, with Metal execution
  compared against independently documented expectations rather than against
  the same implementation that generated the golden output;
- repeatability checks across command-buffer boundaries and resource lifetime
  boundaries.

A correct fixture can validate scheduling, lifetime and semantic contracts
without proving visual equivalence to AMD FSR. Visual-quality or performance
equivalence requires a separately designed test protocol and must not be
claimed from smoke tests.

## Relation to existing Xenora work

Issue #658 documents independent MetalFX temporal-upscaling lessons from a
Vulkan-on-Metal driver experiment. That is useful context for command-buffer
placement and synchronization, but MetalFX is a different proprietary host API
and cannot substitute for AMD FSR behavioral compatibility. Active RDNA,
shader and renderer campaigns own the implementation files; this note does not
change `crates/metal`, `crates/shader`, `crates/loader/src/graphics*` or the
fixture runner.

## Unsupported or unresolved

The following remain unsupported or unresolved by this research note:

- no AMD SDK source or shader has been copied or adapted;
- no FSR1, FSR2, FSR3, FSR4, frame-generation or optical-flow implementation
  is claimed;
- no performance, visual-quality or bit-exact compatibility claim is made;
- proprietary console-specific upscaling interfaces remain unknown and outside
  scope;
- Vulkan and MoltenVK remain prohibited; SPIR-V may exist only as a compiler
  intermediate representation and does not authorize a Vulkan runtime;
- any use of explicitly MIT-listed AMD files requires a later exact-file
  license review and attribution record.

## Public sources

- AMD FidelityFX SDK repository:
  `https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK`
- pinned upstream revision used for this note:
  `7539ae8b6568ad29ed78a753404351daf3b5cca7`
- `docs/license.md` at that revision;
- `Kits/FidelityFX/docs/techniques/super-resolution-temporal.md` at that revision;
- `Kits/FidelityFX/docs/techniques/super-resolution-upscaler.md` at that revision;
- public host-interface declarations in `Kits/FidelityFX/upscalers/include/ffx_upscale.h`
  at that revision, read for interface semantics only.
