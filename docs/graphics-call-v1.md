# Original guest graphics service, version 1

`xenora_fixture.dll!GraphicsCallV1` is an original synthetic research interface.
It is not a Windows, Direct3D or Xbox ABI and is not evidence of console execution.
Its shader input is original gfx1030 machine code in the supported compiler subset;
it does not accept MSL source or protected game content.

## Calling convention and record

Use the loader's Windows x64 calling convention:

- RCX: address of a 128-byte descriptor.
- RDX: descriptor byte count, exactly 128.
- R8: address of an eight-byte writable result slot, required for every operation.
- R9: reserved, exactly zero.

The descriptor contains sixteen little-endian u64 words. Word 0 is version 1;
word 1 is an opcode from the table below. Words 2–15 contain arguments in table
order. Every unused argument word must be zero. Pointers and handles occupy a full
word. Handles retain their generation in the high32 bits and ID in the low32 bits.
Handles are opaque; guests must use returned values, not invent IDs.

Fields described as u32, u16, booleans or f32 bits require zero upper bits.
Booleans must be exactly 0 or 1. A float is its IEEE754 binary32 bit pattern in the
low32 bits; finite/range validation depends on its operation. The one signed field,
indexed base vertex, is a sign-extended i32 stored as an i64 bit pattern in a u64 word.
No native host pointer is exposed.

RAX returns 0 for success, 1 for an invalid envelope/version/opcode/reserved word,
or 2 for invalid graphics arguments, state, unsupported behavior or backend failure.
On success the result slot receives the value listed for that operation below. On failure the result slot remains unchanged. Guest
memory faults stop execution with the loader's MemoryFault rather than returning a
status. Allocation failure is graphics status 2. The service does not set GetLastError.

The complete result span is checked for write permission before any mutation.
Descriptor and variable-size payload reads precede their associated mutation.
Result/input aliasing is permitted after inputs have been consumed, but Readback
rejects overlap between its destination span and the result slot so both outputs
remain meaningful. Each call is one operation; successful earlier setup calls are
not undone by a later failed call. Execute retains the loader's atomic color/depth,
barrier, presentation and history behavior.

## Operations

All list/queue types are Direct. Resources created here are host-backed; uploads
and readbacks explicitly transfer bytes from/to guest memory. State values use the
loader ResourceStates bits. Unsupported formats and layouts fail.

| Opcode | Operation | Arguments after opcode | Successful result |
| --- | --- | --- | --- |
| 1 | CreateDevice | none | device handle |
| 2 | CreateQueue | device | queue handle |
| 3 | CreateAllocator | device | allocator handle |
| 4 | CreateList | device, allocator | recording list handle |
| 5 | CreateBuffer | device, byte size, initial state u32 | resource handle |
| 6 | CreateTexture | device, width, height u32, mip count u16, initial state u32 | RGBA8 resource handle |
| 7 | CreateDepth | device, width, height | uninitialized DEPTH_WRITE handle |
| 8 | CreatePipeline | device, vertex address, vertex byte count, pixel address, pixel byte count | pipeline handle |
| 9 | Upload | resource, resource byte offset, guest address, byte count | 0 |
| 10 | Readback | resource, resource byte offset, guest address, byte count | 0 |
| 11 | ResetList | list, allocator | 0 |
| 12 | CloseList | list | 0 |
| 13 | Barrier | list, resource, before state u32, after state u32 | 0 |
| 14 | Pipeline | list, pipeline (0 unbinds) | 0 |
| 15 | ColorTargets | list, count, record address | 0 |
| 16 | DepthTarget | list, depth (0 unbinds), mode, clear bits | 0 |
| 17 | Viewport | list, width, height | 0 |
| 18 | VertexBuffer | list, resource, byte offset, stride | 0 |
| 19 | VertexColors | list, resource, byte offset, stride | 0 |
| 20 | IndexBuffer | list, resource, byte offset, format u32 | 0 |
| 21 | SampledTexture (slot zero only) | list, texture (0 unbinds), explicit mip u32, min LOD bits, max LOD bits | 0 |
| 22 | DrawState | list, compare, write boolean, blend, RGBA mask, origin x, origin y, scissor boolean, scissor x, y, width, height | 0 |
| 23 | Draw | list, vertex count u32, instance count u32, start vertex u32, start instance u32 | 0 |
| 24 | DrawIndexed | list, index count u32, instance count u32, start index u32, signed base vertex, start instance u32 | 0 |
| 25 | Execute | queue, closed list | actual draw count |
| 26 | DestroyResource | buffer/color/depth resource | 0 |
| 27 | DestroyPipeline | pipeline | 0 |
| 28 | TransientDepthClear | list, clear bits | 0 |
| 29 | ColorTargetsWithLoads | list, count, record address | 0 |
| 30 | Submit | queue, closed list | submission handle |
| 31 | Poll | submission | 0 pending, 1 complete, 2 failed |
| 32 | Wait | submission | 1 complete, 2 failed |
| 33 | ReleaseSubmission | terminal submission | 0 |
| 34 | SampledBindings | list, address of 80-byte binding payload | 0 |
| 35 | SubmitAfter | queue, closed list, wait-table address, wait count (0–16; backend limit applies) | submission handle |
| 36 | SubmissionPoint | submission | queue point value u64 |
| 37 | CreateTextureView | device, backing resource, base mip u32, mip count u32, format | view handle |
| 38 | DestroyTextureView | view | 0 |
| 39 | SampledViews | list, address of 80-byte view-binding payload | 0 |
| 40 | CreateTypedTexture | device, width, height u32, mip count u32, storage format, initial state u32 | resource handle |
| 41 | ShaderUniforms | list, address of 32-byte input, present boolean | 0 |
| 42 | CopyResource | list, destination resource, source resource | 0 |
| 43 | CopyBufferRegion | list, destination, destination byte offset, source, source byte offset, byte count | 0 |
| 44 | CopyImage | list, destination, destination kind, destination A, destination B, source, source kind, source A, source B, width, height | 0 |
| 45 | CreateChildTextureView | device, parent view, relative base mip u32, mip count u32, format | view handle |
| 46 | CreateSampler | device, explicit mip u32, min LOD bits u32, max LOD bits u32 | sampler handle |
| 47 | DestroySampler | sampler handle | 0 |
| 48 | SampledViewHandles | list, address of 32-byte view/sampler-handle payload | 0 |
| 49 | CreateGuestBackedBuffer | device, byte length, initial state u32, guest address | resource handle |

CreatePipeline reads nonempty, four-byte-aligned shader blobs of at most 1024 bytes
each. Decoding and translation must succeed before a pipeline handle is allocated.
Texture mip bytes are packed consecutively from mip0, with each axis halved and
clamped to one per level. Upload requires COPY_DEST; Readback requires COPY_SOURCE.

ColorTargets count is 1–8. Its payload has exactly two u64 words per target: the
handle and a clear RGBA8 value in low32 bits (R in least significant byte). The
clear word's upper32 bits must be zero. The loader validates distinct compatible
targets and their render states; these bindings select mip0 for output.

ColorTargetsWithLoads also takes 1–8 targets, with three u64 words per record:
handle, mode, clear RGBA8. Mode 0 clears (upper32 clear bits must be zero); mode 1
loads prior committed base-level bytes and requires the clear word to be zero.
New host-backed RGBA8 textures start with zero bytes, which may be loaded. Loads
use the same target states, extent, reference lifetime and atomic commit rules as
clears. Different submissions can select different pipelines over preserved colors.

DepthTarget mode 0 is Clear with finite clear in [0,1]; mode 1 is Load and requires
clear bits zero and a previously initialized resource. Unbinding cannot request
Load. TransientDepthClear cannot be combined with a retained depth binding.

Index format 42 is R32_UINT and 57 is R16_UINT. Vertex/color inputs are float4,
with stride at least16. Only bounded triangle lists and the existing single-instance
subset are supported. Indexed color gathering follows the same index/base as positions.

SampledTexture (opcode 21) changes only texture slot 0 and sampler slot 0.
A zero texture handle clears both slot-zero entries; slot 1 remains unchanged.
Use SampledBindings (opcode 34) to replace both independent tables atomically.
Its payload is ten little-endian u64 words (80 bytes):

| Payload word | Field |
| --- | --- |
| 0 | Texture slot 0 handle; zero unbinds |
| 1 | Texture slot 1 handle; zero unbinds |
| 2 | Sampler slot 0 present: exactly 0 or 1 |
| 3 | Sampler slot 0 explicit mip, low32 u32 |
| 4 | Sampler slot 0 minimum LOD, low32 f32 bits |
| 5 | Sampler slot 0 maximum LOD, low32 f32 bits |
| 6 | Sampler slot 1 present: exactly 0 or 1 |
| 7 | Sampler slot 1 explicit mip, low32 u32 |
| 8 | Sampler slot 1 minimum LOD, low32 f32 bits |
| 9 | Sampler slot 1 maximum LOD, low32 f32 bits |

Absent samplers require all three following fields to be zero. Present samplers
require zero upper32 bits in those fields. Explicit mip `0xffffffff` means no
explicit selection; otherwise the level must exist for every texture sampled
with that sampler. Both LOD fields must be finite and satisfy the sampler range
contract. Explicit selection pins native LOD clamps to that mip; it does not
create a texture view. Filtering is nearest and addressing is clamp-to-edge.

Shader descriptors use a synthetic mapping: resource SGPR bases 0/12 map to
texture slots 0/1 (`s[0:7]` / `s[12:19]`); sampler bases 8/20 map to sampler
slots 0/1 (`s[8:11]` / `s[20:23]`). All four cross-pairs are supported. This is
not a general descriptor-heap or Xbox descriptor ABI.

At each actual draw, bound texture and sampler presence must exactly match the
selected pipeline's required-slot masks: missing and extra bindings reject.
LOD compatibility is checked separately for every pair used by that shader,
not just equal-numbered slots. Textures must be supported host-backed Texture2D resources in
PIXEL_SHADER_RESOURCE state and cannot also be color outputs in the same draw.
Reset clears both tables. Switching to an unsampled pipeline requires clearing
all sampled entries; opcode 21 alone cannot clear a previously bound slot 1.

### Persistent typed sampled textures

CreateTypedTexture (opcode 40) adds explicit storage formats without changing
CreateTexture (opcode 6), which remains RGBA8. Storage-format codes are exactly
0 = `Rgba8Unorm`, 1 = `Bc1RgbaUnorm`, and 2 = `Bc3RgbaUnorm`. These are separate
from the view-format codes below. Unknown codes and nonzero unused arguments reject.
Width and height must be nonzero and at most 4,096. Height, mip count and initial
state require zero upper32 bits. Mip count is nonzero and cannot exceed the full
chain obtained by halving each dimension to one. BC creation accepts COMMON,
COPY_DEST, COPY_SOURCE or PIXEL_SHADER_RESOURCE as its initial state.

Backing storage is a tightly packed mip chain, ordered from mip 0 with no gaps or
row padding. For each mip, halve width and height and clamp each axis to one:

| Storage | Bytes per row | Stored rows |
| --- | --- | --- |
| RGBA8 | width × 4 | height |
| BC1 | ceil(width / 4) × 8 | ceil(height / 4) |
| BC3 | ceil(width / 4) × 16 | ceil(height / 4) |

Each mip occupies row bytes × stored rows; its offset is the sum of preceding mip
lengths. Edge blocks occupy complete blocks even for a 1×1 tail. For example, a
5×5 texture with three mips stores 4 + 1 + 1 blocks: 48 bytes for BC1 or 96 for BC3.
All footprint arithmetic is checked before allocation. Creation zero-initializes the
backing and preserves the 64 MiB per-resource and 256 MiB context allocation limits.
Compressed backing bytes, rather than an expanded RGBA image, count toward storage;
views share that backing charge. Existing object limits also apply.

Upload (opcode 9) and Readback (opcode 10) transfer the exact raw backing bytes at
checked byte offsets. They do not decode, encode, reorder, or add padding to BC
blocks. Guests must pack pitched source data into the canonical layout above.
Transfers require COPY_DEST/COPY_SOURCE respectively and retain complete memory
preflight, range checking and pinned-resource exclusion. Native sampling uploads the
original compressed blocks; there is no CPU decompression fallback.

Creating host storage does not establish GPU support. Typed backend installers
advertise BC1 and BC3 capabilities independently, obtained by the native adapter from
its actual device. Existing backend installers advertise RGBA8 only. An actual draw
using unsupported BC storage rejects before backend dispatch, including through a
view; it cannot silently reinterpret the bytes as RGBA8. Native allocation also
validates support. Binding and view creation may precede this draw-time check.

BC resources are sampled/transfer resources, not color attachments. Both explicit
color-target bindings and the legacy target path reject BC attachments, including
clear-only passes. Render targets and queued Produced outputs remain RGBA8. A queued
request may combine a compressed Host snapshot with a dependency-backed Produced
RGBA8 input; the compressed snapshot retains its explicit format and original bytes.
Snapshots preserve prior submissions across later uploads. Reset, destruction,
retirement and whole-backing hazard rules apply identically to compressed resources;
destroying a pending handle does not free storage still required by admitted work.

### Sampled texture views

CreateTextureView (opcode 37) creates an immutable, shader-read-only view of an
existing host-backed typed Texture2D resource. View-format codes are:

| Code | View interpretation | Required backing storage |
| --- | --- | --- |
| 0 | Rgba8Unorm | RGBA8 |
| 1 | Rgba8UnormSrgb | RGBA8 |
| 2 | Bc1RgbaUnorm | BC1 |
| 3 | Bc1RgbaUnormSrgb | BC1 |
| 4 | Bc3RgbaUnorm | BC3 |
| 5 | Bc3RgbaUnormSrgb | BC3 |

Other codes and cross-family casts reject, including RGBA8-to-BC and BC1-to-BC3.
Base mip
and mip count require zero upper32 bits. Mip count must be nonzero, and checked
`base_mip + mip_count` must not exceed the backing resource's mip count. The backing
must be a single-slice 2D texture; buffers, depth resources, guest-backed storage,
and view handles used as opcode 37 backings reject. Views cannot be used as writable
attachments. Creation does not copy, crop, convert, or allocate a second pixel backing.

CreateChildTextureView (opcode 45) uses the same argument ordering as opcode 37,
with a parent view handle in place of the backing resource and a base mip relative
to that parent. Its five arguments are `[device, parent_view, relative_base_mip,
mip_count, format]`. Both mip fields require zero upper32 bits; count must be
nonzero and checked `relative_base_mip + mip_count` must fit the parent range.
The child stores the root backing identity and the checked absolute base mip,
`parent.base_mip + relative_base_mip`. Format codes and storage-family restrictions
are the same as opcode 37; the requested format does not convert backing bytes.

The child has its own handle and lifetime. Destroying an otherwise destroyable
parent does not invalidate the child, which independently retains the root backing.
Existing object budgets, binding/retirement rules and whole-resource alias checks
still apply. Opcode 38 destroys children and opcode 39 binds them like other views.
This adds read-only mip-range nesting, not texture arrays or writable views.
Six focused CPU checks, two original guest tests across three adapters and all
nine repository gates passed. See the [validation guide](testing/local-graphics-checkpoint.md).

SampledViews (opcode 39) uses the same ten-u64, 80-byte little-endian layout as
SampledBindings above, except words 0 and 1 contain **view handles**, not resource
handles. Zero unbinds a slot. Sampler-present flags must be exactly 0 or 1; absent
samplers require their mip/min/max fields to be zero. Present sampler fields retain
the upper-bit, finite-value and range requirements above. The entire payload and
all handles/samplers are validated before recording one atomic replacement of both
tables. An invalid later entry records nothing and retains no earlier entry.
The writable result span is preflighted before recording; the result may alias the
payload after its complete contents have been consumed. Unused descriptor words
remain zero. The command shares the existing 4,096-command recording limit.

Mip indices and LOD bounds are relative to the view. For a view with base mip 1 and
count 2, view mip 0 names backing mip 1 and view mip 1 names backing mip 2; an explicit
view mip 2 rejects. View dimensions are the backing dimensions shifted by base mip,
with each axis clamped to one. Sampler compatibility is checked against this view
extent and mip count for every used texture/sampler pair. The native view shares the
backing allocation and restricts its visible mip range; it is not a sampler LOD clamp
or a cropped upload. Linear and sRGB views can share the same bytes. sRGB sampling
decodes RGB to linear values while retaining the alpha interpretation; render outputs
remain linear RGBA8.

Opcodes 21 and 34 continue to bind whole resources. Opcode 21 replaces or clears only
slot 0, including any prior view in that slot, and leaves slot 1 unchanged. Opcode 34
replaces both slots with whole-resource bindings; opcode 39 replaces both with view
bindings. Samplers remain independent. Existing pipeline required-slot masks apply.
A view-aware host backend must be installed: legacy and multislot-only callbacks
reject actual draws using views before dispatch, rather than ignoring their metadata.
BC views additionally require the corresponding typed-format capability.

Views count against the shared 4,096-object limit; backing bytes are charged once.
DestroyTextureView (opcode 38) rejects while any recorded command refers to the view,
including superseded commands and commands in closed, executed or failed lists.
Every referring list must reset first. DestroyResource rejects while any live view
refers to that backing. After reset, a pending submission may retain its resolved
view and backing even after the guest destroys the view and backing handles. Such
handles stay invalid, and in-flight object/storage charges remain until the required
references retire. View destruction does not modify backing pixels.

State, transfer pinning and alias checks use the backing resource identity, not the
view handle. Different views of one resource cannot bypass sampled/output alias
rejection or pending-transfer exclusion. The initial policy is whole-resource hazard
exclusion, even for nonoverlapping mip ranges; subresource concurrency is not implied.
Transfers still address backing-resource bytes through Upload/Readback, not view handles.

DrawState compare codes: 0 disabled, 1 Never, 2 Less, 3 Equal, 4 LessEqual,
5 Greater, 6 NotEqual, 7 GreaterEqual, 8 Always. Depth writes require a compare.
Blend codes: 0 Replace, 1 Alpha, 2 Additive. Color-mask bits 0–3 select R/G/B/A;
other bits fail. When the scissor boolean is zero, all four rectangle words must
be zero. Otherwise a zero-area scissor suppresses rasterization. State validation
and blend equations follow the loader interface.

## Bounds and lifetime

This service permits at most4096 graphics objects, including submission receipts and
retired in-flight objects, and at most4096 recorded commands per
list. Existing stricter pipeline counts, resource budgets and draw/target limits
also apply. Native passes accept 65,535 expanded vertices, 256 nonempty draws and
1,048,576 target pixels with dimensions up to 4,096. Software execution remains bounded
to 768 vertices and 4,096 pixels. These are synthetic interface limits, not Metal hardware limits.
Reset releases recorded references. Every referring list must reset before a
resource or pipeline can be destroyed, even after successful or failed execution.
Queue/allocator/list/device lifetime currently ends with the owning context; reuse
those objects across frames. Asynchronous receipt and retirement semantics are described
below. The queued backend supports the bounded two-queue contract below; general
cross-queue scheduling remains unsupported.

The 4,096-object limit is shared with the legacy `CreateGraphicsDevice` and
`SubmitGuestDraw` imports. Legacy draw submission reserves five slots before setup;
failed setup or execution releases its unpublished objects. Successful legacy draws
retain their objects until context destruction. Versioned calls can reuse their
recording objects and explicitly destroy resources and pipelines after list reset.

## Asynchronous submissions

Submit requires an asynchronous backend. It validates and owns inputs before
returning a receipt, and the native adapter acknowledges only after Metal commit.
Shader compilation, resource setup and depth restoration may block during preparation;
Submit does not wait for the new rendering command to complete. The legacy async
backend permits one unreleased receipt. The queued backend permits at most two
retained submission nodes, including released ancestors still needed by a dependent,
and at most one pending or uncollected submission per queue. Terminal unreleased
receipts still occupy capacity. Installing the queued backend is a host decision;
creating multiple guest queues does not enable it on a legacy backend.

Poll is nonblocking; Wait blocks for completion. A valid query reporting failed
execution returns service status 0 and result 2. Invalid handles, busy/unsupported
calls and premature release return service status 2 with the result unchanged.
Repeated terminal queries return the same state without republishing. Synchronous
Execute uses the same submit/wait/commit path on an async backend and releases only
its own internal receipt.

Color/depth/state/presentation changes remain unpublished until successful Poll/Wait
collection and output validation. Reset may run while pending. Resources or pipelines
still referenced by recorded lists cannot be destroyed; after those lists reset,
destruction invalidates the handle while retaining storage needed by native work or
produced versions. Completion never resurrects destroyed handles. Already admitted
consumers may still use their retained versions, and receipt outcomes and presentation
may still publish. Transfers involving pinned resources reject; budget charges remain
until the last required reference is released. Context destruction drains pending work
without publishing outputs. Result spans are preflighted before submitting, consuming
completion or releasing.

## Bounded queued dependencies

SubmitAfter (opcode 35) takes a table of zero to sixteen records. Legacy backends
accept at most two records. Each record is
exactly 16 bytes: a full little-endian u64 queue handle followed by a little-endian
u64 point value. For count zero the table address must be zero; no payload is read.
For nonzero count the complete `count * 16` bytes must be readable. The entire table
and writable eight-byte result span are checked before submission. A malformed later
record does not submit work or consume a receipt or queue point. The result may alias
the table because all input records are consumed before the receipt is written.
Unused descriptor argument words must be zero.

SubmissionPoint (opcode 36) returns only the point's u64 value. The caller supplies
the queue half of a later wait record using the queue handle it retained from that
submission. A value is not a global submission identifier: pairing a valid value
with another queue does not identify the original producer. Querying an invalid or
released receipt fails without changing the result. Points increase monotonically
per logical queue and do not reset when native synchronization objects are replaced.

Waits must name existing retained points. Duplicate, unknown, future, expired and
failed prerequisites reject. A released ancestor may remain internally retained while
an admitted descendant needs it, but its released receipt can no longer be queried.
There is no unbounded history of waitable points. Queue order adds an implicit
predecessor dependency while that predecessor is retained. Submit (opcode 30) is the
empty-explicit-wait form; SubmitAfter with zero waits also retains compatibility with
the legacy async backend. Nonempty dependency waits and point queries require the
queued path.

The native queued backend uses two queues on one device. Explicit cross-queue waits
order the GPU producer and consumer; host Wait only collects their results. Unordered
read/write and write/write conflicts reject. Required resource barriers are still
mandatory: dependent validation uses the producer's planned final state, while public
committed state changes only on successful collection.

The initial produced-input contract is read-only sampling of a producer's single-mip
RGBA8 color output, directly or through a linear/sRGB view with base mip 0 and count 1. The consumer refers to the retained native output version rather
than an old host snapshot or a host readback/re-upload. A produced view retains that
exact source point and dependency; it cannot fill missing mips from committed host
bytes or escape into an ordinary upload-capable binding. The produced-color backend
also accepts ColorTargetsWithLoads: it copies the exact dependency version into a
fresh GPU attachment, then overlays draws while preserving untouched pixels. It
never mutates the producer surface. Older queued installers reject this input.
The produced-attachment backend also accepts dependency-backed depth Load through
DepthTarget. It copies the exact immutable Depth32Float version into fresh storage;
read-only Load preserves depth, while Clear replaces it. Clear requires DEPTH_WRITE;
DEPTH_READ permits Load with writes disabled. Both color and depth publish atomically
in dependency order. GPU-produced vertex/index inputs and general resource/subresource
dependencies are not supported.
Ordinary color Load from committed host contents remains available outside that
produced-version path.

Polling a dependent also observes its prerequisites. It remains pending until they
can be successfully validated and published, even if its GPU work has finished.
Publication follows dependency order; each render submission retains atomic color/depth,
state, history and presentation updates. A later dependent failure does not undo an
already published producer. Collecting an older independent result cannot overwrite
a newer presentation.

A producer execution or publication failure invalidates unpublished work in the
native cohort. Dependent outputs never publish, even if their native execution
already succeeded. Failure recovery unblocks doomed waits only after the producer
is terminal, solely to drain work; event progress is not proof of success. A receipt
becomes terminal only after its work drains. Collect failures and release terminal
receipts, including retained ancestor/descendant chains, before starting the next
cohort. Once all old nodes and native mappings are released, a new native generation
can accept successor submissions using the last successfully committed resource
contents and states. Failure does not permanently poison the context. Re-record
barriers as needed; failed planned transitions were not committed.

Terminal native tickets and produced-output mappings remain retained until their
receipt/dependency nodes are pruned, rather than being released merely at GPU
completion. Releasing a producer receipt while a descendant still refers to it does
not free that producer's versions or their remaining budget charges.

The two-slot, two-queue, sampled mip-view and persistent BC1/BC3 contracts are bounded
Stage 2 increments.
General descriptor heaps, texture arrays, writable/subresource-overlap views, broader
formats and filter/address
modes, arbitrary queue graphs, future waits and general concurrent copy/render scheduling
remain open. Graphics Stage 3 supports
[bounded countdowns and innermost early exits](interfaces.md#bounded-countdown-loops-in-original-graphics-stages).
General loops, EXEC/VCC divergence and reconvergence remain open; the runtime uniform
SCC0 contract below does not complete Stage 3.

The persistent BC increment passed all local validation gates and two independent
scoped reviews, including actual PE sync/async and queued integration evidence. This
does not establish broader Stage 2 or Stage 3 completion.

## Per-draw scalar uniforms

ShaderUniforms with present=1 copies eight little-endian u32 words, initializing
s24..s31 independently in each graphics stage. All 32 bytes must be readable before
recording; the result slot may alias the input because the copy precedes the result
write. With present=0, address must be zero and the command unbinds the data.
Explicit all-zero input is distinct from an absent binding. Invalid result memory,
malformed flags, inaccessible inputs and command-budget exhaustion record nothing.

Draws snapshot the latest binding. Pipeline changes preserve it; list reset clears
it. The compiler derives required scalar inputs across all feasible runtime paths.
A nonempty draw requiring uniforms rejects before backend dispatch if the binding
is absent or the backend lacks support, including a bad later draw in a batch.
Zero-count draws retain their existing no-op semantics. Sync, async and queued
native adapters carry the same immutable bytes through completion.

The same uniform words can feed typed scalar locals s32..s105, MOV and the eleven
arithmetic/selection forms documented in `interfaces.md`. No additional guest
opcode or binding layout is required. The original scalar guest regression changes
geometry, exact integer results and carry colors through one pipeline across all
three backend modes, with guarded readbacks and retained bindings.

This interface supports uniform forward scalar branches and
[bounded countdowns with innermost exits](interfaces.md#bounded-countdown-loops-in-original-graphics-stages).
General loops, EXEC/VCC divergence and reconvergence remain open. Local gates and
independent scoped reviews do not establish completion of Stage 3.


## Queued whole-resource copies

CopyResource records one copy in a copy-only command list, with optional resource
barriers. It requires distinct owned resources with identical layouts: a nonempty
buffer or a single-mip RGBA8 texture. At the copy command, the source must be in
COPY_SOURCE and the destination in COPY_DEST. Recording pins both handles until
reset. Mixed draw/copy lists, guest-address-backed storage, partial regions,
format conversion, depth and compressed copies are rejected.

Submit and SubmitAfter require the host's copy-capable queued backend; synchronous
Execute and older backend installers reject copy lists. Legacy backends retain the
two-node limit; the opt-in graph backend permits sixteen retained nodes. An explicit
dependency permits an exact pending render or copy texture version as input; copy
buffers can feed another copy. The opt-in geometry graph backend also consumes exact
pending copy buffers as positions, colors or indices through guarded GPU gathering.
The existing CPU fetch path continues to reject pending geometry. See
[GPU-produced geometry](interfaces.md#gpu-produced-geometry-in-queued-draws) for
validation, dependency and lifetime rules. Successful collection
publishes validated destination bytes and ordered resource states, without creating a
frame or changing presentation order. A failed prerequisite invalidates a consumer even
if its GPU work has already completed. Destroyed destinations are not resurrected.


## Buffer-region copies

CopyBufferRegion records one region copy in a copy-only list, optionally with
barriers. Source and destination must be distinct buffers; the destination must be owned.
A guest-backed source follows the submission snapshot contract below. Offsets may be
zero; the byte count must be positive. Checked ranges must fit logical resource
sizes, including when backing
allocations contain padding. Full source and destination sizes may differ or be odd.

The source must be in COPY_SOURCE and the destination in COPY_DEST at the command.
The region-capable queued backend preserves the complete prior destination in a
fresh immutable output before applying the patch. Both source and prior destination
are reads for dependency validation. A pending version must be covered by the admitted
dependencies; neither input may fall back to stale host bytes. Legacy installers retain the two-node
limit; the graph backend described below permits sixteen retained nodes.

Completion uses the full destination layout and byte count, publishes atomically,
and does not advance presentation. The native queued adapter supports arbitrary-byte
ranges through an additive backend capability. Older aligned-region installers
reject unaligned submission before dispatch; whole-copy installers reject all region
copies. Self-copies remain unsupported. Texture regions and buffer-texture transfers
use CopyImage below.


## RGBA8 rectangles and pitched image transfers

CopyImage records one copy plus optional barriers. Endpoint kind0 denotes an owned
single-mip RGBA8 texture, with A/B as texel x/y origin. Kind1 denotes an owned buffer,
with A/B as byte offset/row pitch. Supported pairs are texture→texture,
buffer→texture and texture→buffer. Kinds must match the resources. Source and
destination must be distinct backings; widths/heights must be positive.

Each texture rectangle must fit its complete backing. Buffer offset and pitch must
be multiples of four; pitch must cover width×4 bytes and not exceed the software
limit of 16,384 bytes. Checked offset+(height−1)×pitch+width×4 must fit the logical
buffer length. No final-row trailing padding is required. Prefix, inter-row gaps,
suffix, odd logical tail and texels outside the destination rectangle are preserved.

Source is COPY_SOURCE and destination COPY_DEST at the command. Both source and
prior destination contribute reads to hazard validation; the destination also
contributes a write. The image-copy-capable backend resolves both pending inputs to
exact native versions, waits before cloning the prior destination and applying the
patch, then publishes a complete destination version only after successful validation.
Older installers revoke image-copy support while preserving their earlier contracts.
Copies do not advance frames or presentation order. The two-retained-node loader
limit remains; depth/compressed transfers, additional mips/slices and larger queue
graphs are not enabled by this operation.


## Multi-node graph backend

The native queued adapter opts into up to 16 retained receipts across two queues,
including released receipts still needed by descendants. Multiple submissions may
remain pending on a queue. SubmitAfter (opcode35) accepts at most 16 dependency
records, each holding a queue handle and point value. Older backend registrations
still reject requests exceeding their two-node contract.

Dependencies cover transitive ancestors. Inputs use the exact latest applicable
ancestor version; incomparable conflicting writers reject in either wait order.
Same-queue predecessors are implicit dependencies. Collecting a descendant first
validates and publishes its ancestors before exposing the descendant. Existing
resource, payload and logical-range limits remain in force. Original native and
guest graph tests validate this bounded extension locally.


## Reusable immutable sampler handles

CreateSampler (opcode46) creates a nearest/clamp descriptor. Its explicit-mip word
uses u32::MAX for no explicit selection. Min/max LOD words contain IEEE754 float32
bits; all three words require zero upper32 bits. LOD bounds must be finite,
nonnegative and ordered. View-dependent mip checks remain at draw preparation.
Live sampler handles count toward the shared guest object limit.

SampledViewHandles (opcode48) reads four little-endian u64 values in this order:
`[view0, view1, sampler0, sampler1]`. Zero independently unbinds each slot, preserving
cross-slot texture/sampler pairs. The entire table and all live handle types are
validated before one binding command is appended. A bad second entry preserves the
previous bindings. The result span is preflighted before any mutation, and input/result
aliasing is supported after the table has been consumed.

Binding copies sampler descriptors into recorded commands. DestroySampler (opcode47)
therefore immediately frees the live handle slot without altering recorded or pending
work; subsequent binds or destruction using that stale handle fail. Commands own
bounded descriptor values, not references to retired sampler objects. View lifetime
rules remain unchanged. Existing value-based binding opcodes remain supported.
This adds reusable descriptors, not general descriptor heaps or new filter modes.


## Guest-backed whole-buffer copy sources

CreateGuestBackedBuffer (opcode49) creates a buffer whose bytes are read from a
caller-owned guest address. Its first three arguments match CreateBuffer (opcode5),
which continues to create owned storage. Creation validates the size, address-range
overflow, state-word encoding and object limits, but does not read the range or require it to be
mapped. The result span is preflighted before creation. If the result aliases the
backing, its handle write is a memory change visible to later submission.

A queued whole-resource copy may use this buffer as its COPY_SOURCE and an owned
buffer as COPY_DEST. Recording retains metadata only. Submission preflights the
copy payload budget and reads the entire logical source through checked guest
memory access into an immutable Host input. Writes between recording and submission
are observed; writes after successful submission cannot change the queued copy.
Read-only guest memory is sufficient. Unreadable source memory fails submission
without consuming a queue point or publishing destination bytes. Retrying captures
fresh bytes. Guest-memory faults retain the existing guest fault behavior.

Pending work owns the snapshot independently of guest mappings and source handles.
The synchronous submission call defines the capture boundary; it does not promise
an atomic snapshot against concurrent external writers. Guest-backed destinations,
image copies from guest-backed resources and general coherent shared memory
remain unsupported. The native backend and shader ABI are unchanged.


### Guest-backed buffer-region sources

CopyBufferRegion (opcode43) also accepts buffers created through opcode49 as sources.
It validates positive length and checked source/destination offsets before admission.
Submission snapshots the entire logical source after full-source payload preflight,
not only the selected range. All source bytes must therefore be readable, including
bytes outside the copied region. The selected bytes patch an exact owned destination
version; untouched prefix/suffix bytes are retained, including when that prior version
is GPU-produced. Existing dependencies, leases and alignment-capability checks apply.

Recording reads no guest bytes. Pre-submit writes are observed, post-submit writes
cannot change the retained snapshot, and failed admission is retryable with fresh
bytes. Guest-backed destinations and guest-backed image transfers remain rejected.
No guest opcode or native backend ABI is added by this extension.
