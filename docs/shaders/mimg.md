# MIMG explicit-LOD 2D sampling

`IMAGE_SAMPLE_LZ` and `IMAGE_SAMPLE_L` sample a 2D image at level of detail
zero or at an explicit LOD from the address tuple. The forms are gfx1030
Wave32, non-NSA, with a 2D resource and a point-filtered clamp-to-last-texel
sampler. They are implemented in the decoder, the reference interpreter, the
second independent interpreter, the compute compiler and the native Metal
backend. Nothing here is claimed for Xbox hardware; the expectations come from
AMD's public RDNA 2 guide and are checked against a native Apple GPU.

## Encoding and operands

The decoder accepts only the non-NSA two-dword encoding with DIM 2D, R128,
TFE, LWE and every cache/modifier flag clear, and DMASK 1-15. VDATA is the
first destination VGPR and receives one VGPR per selected DMASK component in
R, G, B, A order. VADDR is `(u, v)` for `IMAGE_SAMPLE_LZ` and `(u, v, lod)` for
`IMAGE_SAMPLE_L`. SRSRC names eight consecutive SGPRs (four aligned) and SSAMP
four consecutive SGPRs (four aligned).

## Resource descriptor (T#)

The bounded scope accepts only:

- type 9 (2D) and format 56 (8_8_8_8 unorm);
- resource level 1 and destination select R, G, B, A;
- base level 0;
- width-1 and height-1 with both reserved high bits zero;
- swizzle/tiling mode 0 (linear);
- minimum LOD 0;
- last level at or above the base level.

The base-address field is address bits 47:8, so the byte offset is
`field << 8` and is always 256-byte aligned; the field is 40 bits wide and
all of it is read. The 256-bit extension words (the further four SGPRs) must
be zero. Each malformed field is a distinct `SamplingError` in the
reference.

Every bit of the descriptor is either checked or required to be zero. The
bits that carry no meaning for the bounded scope are the reserved bit 61, the
reserved bit 94 and the BC swizzle field 123:121; a nonzero extension word is
refused separately. The reference reports the offending bits as a mask.

## Sampler descriptor (S#)

The bounded scope accepts only clamp-to-last-texel on x, y and z (with z equal
to x), normalized coordinates, point xy and z filtering, a point mip filter,
zero LOD bias, minimum LOD 0, and a maximum LOD at or above the last level in
4.8 fixed point. A mip filter of none (0) is refused because it disables
mipmapping and could not select the explicit level.

Every bit of the descriptor is either checked or required to be zero. The
fields the bounded scope does not interpret are the LOD bias 75:64, the LOD
bias high bits 77:76, the LOD bias sec 83:78, force degamma (20), trunc
coord (27), bit 19, the anisotropy, depth compare, filter mode, performance
and border colour fields, and the reserved bits. A set bit among them changes
what real hardware samples, so the reference refuses the descriptor and
reports the offending bits as a mask instead of ignoring them.

## Mapping

Nearest-neighbour point sampling clamps `u` and `v` to `[0, 1]`, scales by the
selected mip's extent and floors:

```
x = min(floor(clamp(u, 0, 1) * width),  width  - 1)
y = min(floor(clamp(v, 0, 1) * height), height - 1)
```

A texel byte converts to binary32 as `byte / 255.0`; a selected component is
written to its destination VGPR as those f32 bits. `IMAGE_SAMPLE_LZ` uses mip
0, and `IMAGE_SAMPLE_L` requires an integral LOD in `[0, last level]`.

## Known limits

The CPU reference refuses an explicit LOD that is not an exact integer mip
level, and answers `SamplingError::InvalidLod` for a fractional, negative or
out-of-range LOD. The GPU path cannot refuse a runtime value: the generated
MSL calls `tex0.sample(samp0, float2(u, v), level(lod))` and the bound Metal
sampler state sets `MTLSamplerMipFilter::Nearest`, so Metal rounds the LOD
to the nearest mip level instead of failing. The sampler's LOD clamps
(`LodMinClamp` and `LodMaxClamp`) also pull a negative or too large LOD
into the sampler's range rather than refusing it.

The two paths therefore differ for a non-integer explicit LOD: the reference
reports `InvalidLod` and the GPU samples the nearest mip level. The bounded
contract assumes an integral explicit LOD, and a program that depends on the
refusal must not be run on the GPU path. Nothing here is claimed for Xbox
hardware; Metal's rounding rule is Metal's own and not a guide-derived
expectation.

## Translation and Metal

The compute compiler requires the synthetic binding ABI: s[8:15] and s[16:19]
must have been written by `s_buffer_load_dwordx4` from s[0:3] at offsets 0x0,
0x10 and 0x20, with no later write to those SGPRs before the sample. The
generated MSL binds the validated texture and sampler and calls
`tex0.sample(samp0, float2(u, v), level(lod))`. The native backend parses the
same T#/S# words from the input buffer and refuses a descriptor that fails
validation or disagrees with the bound texture (width, height, mip count) or
sampler state.

## Evidence

Local unit tests cover the descriptor refusals and the sampling arithmetic.
The validation suite runs six assembled fixtures (DMASK 1, 8, 5 and 15 at LOD
0, plus `IMAGE_SAMPLE_L` at LOD 0 and 1) through the reference and the second
interpreter, and the native Metal readback compares every destination VGPR of
every case with hand-derived expectations. The compiler tests pin the DMASK
channel order and the level of the generated IR and MSL without a GPU, and a
partially active sample keeps the prior destination value in the lanes the EXEC
mask excludes. The fixtures are assembled with `llvm-mc` and compared with its
disassembly, which also checks the single-component destination text. Local
native verification is on Apple M4 with Metal API and shader validation;
independent acceptance is pending.
