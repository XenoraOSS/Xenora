# Wave32 vector unary operations and swap

Xenora translates six gfx1030 VOP1 integer forms for the synthetic Wave32 compute
ABI. The AMD RDNA 2 ISA Reference Guide defines the operations in section 12.8,
the encoding in section 13.3.2, and the execution mask (EXEC) in section 3.3.

| Form | Result in each active lane |
| --- | --- |
| `v_not_b32_e32` | Complement all 32 bits. |
| `v_bfrev_b32_e32` | Reverse the order of the 32 bits. |
| `v_ffbh_u32_e32` | Count leading zeros. Return all ones if the source is zero. |
| `v_ffbl_b32_e32` | Count trailing zeros. Return all ones if the source is zero. |
| `v_ffbh_i32_e32` | Count leading bits equal to the sign bit. Return all ones if all bits match. |
| `v_swap_b32` | Exchange the old values of two vector general-purpose registers (VGPRs). A register swapped with itself keeps its value. |

The unary forms accept a VGPR, scalar register `s0`–`s105`, `vcc_lo` (the low
half of the vector condition code), inline integer `-16`–`64`, or a literal
word as SRC0. Swap requires a VGPR as SRC0 because it writes both operands.
Both operands of swap must be initialized when EXEC is nonzero. The compute
ABI reserves `v0` for indexing and requires waiting for buffer loads before
using or overwriting their destination. Inactive lanes retain their old
values. The six forms leave EXEC, VCC and the scalar condition code (SCC)
unchanged.

The compiler emits per-thread Metal Shading Language for unary operations.
For swap it captures both old values in the intermediate representation before
updating either register binding. Under partial EXEC, each register receives
its own lane merge.

The [guide-derived oracle](../../tests/validation/tests/vop1_integer.rs) checks
the 752 accepted cases' outputs against its expected values on each CPU
interpreter and, through the translator, on native Metal (Apple M5, API and
shader validation on: 752 cases in 613 programs). A CPU test checks that the
compiler accepts every case and refuses each of the 610 rejected programs at
the same instruction offset as the interpreters. The
[LLVM-assembled fixture](../../fixtures/rdna2/vector-unary.s) exercises all
six forms with a partial EXEC swap. Independent acceptance of the
translator is pending. Xbox hardware agreement is unknown.

## Binary32 rounding

Four more VOP1 forms round a binary32 value to an integral value, defined by
the pseudocode in guide section 12.8 (page 140 of the 30 November 2020 guide).

| Form | Result in each active lane |
| --- | --- |
| `v_trunc_f32_e32` | Round toward zero. |
| `v_ceil_f32_e32` | Round toward +infinity. |
| `v_rndne_f32_e32` | Round to nearest, ties to even. |
| `v_floor_f32_e32` | Round toward -infinity. |

SRC0 is a VGPR, `s0`–`s105`, `vcc_lo` or a literal word. Integer inline
constants are refused, because the guide does not say whether a float operation
reads them as integer bits or as converted values. Float inline constants stay
outside the VOP1 subset. EXEC, VCC and SCC behave as for the integer forms.

The results depend on the MODE register (guide sections 3.5 and 6.4) in two
ways, and Xenora does not model MODE writes:

- Subnormal inputs. The compute ABI pins MODE.FP_DENORM single-precision input
  denormals as allowed (modes 1 or 3). A subnormal input keeps its exact value,
  so `v_floor_f32` of the smallest negative subnormal gives -1.0. Under input
  flushing (modes 0 and 2) it would give -0, which is outside the contract.
  Results are integers or signed zeros, so output flushing changes nothing.
- NaN inputs. The pseudocode has no NaN case, and MODE.IEEE and
  MODE.DX10_CLAMP change NaN results. NaN inputs are an unsupported operand
  variant (`src_nan`). The CPU interpreters refuse every quiet or signalling
  NaN input. The Metal kernel passes a NaN's bits through unchanged, so a
  signalling NaN (for example `0x7f800001`) stays signalling, where hardware
  with MODE.IEEE = 1 presumably quiets it. That result is not verified.

The guide describes `v_rndne_f32` as "Round-to-nearest-even semantics", and
Xenora implements that description as IEEE 754 roundTiesToEven. Its pseudocode,
`floor(S0 + 0.5)` corrected for even ties, agrees when read in exact
arithmetic except for the sign of a zero result: for a negative input of
magnitude at most 0.5 (including -0 and the negative subnormals) it would give
+0, while round-to-nearest-even gives -0. Xenora returns -0. The interpreters
can only refuse NaN lane data at run time, so the translator still accepts
those programs.

The MSL computes every result from the register bits with unsigned integer
operations: no Metal float operation runs, so fast math, subnormal flushing
and NaN handling in Metal cannot change a result. Constant sources are not
folded on the CPU, so the generated MSL is the only translator path. The CPU
reference splits the value into an integer significand and a power of two; the
second interpreter rounds the exactly widened binary64 value on the host.
Neither shares code with the MSL.

The [guide-derived oracle](../../tests/validation/tests/vop1_float_rounding.rs)
has a hand-written table of 43 inputs (signed zeros, infinities, quiet and
signalling NaNs, the smallest and largest subnormals, values one unit in the
last place around 0.5, 1, 2.5 and 2^23, halfway values) and a transcription of
the pseudocode checked against it. Its 152 accepted cases pass on both CPU
interpreters and, through the translator, on native Metal (Apple M4, macOS
27.2.1, API and shader validation on: 152 cases in 65 programs). Both
interpreters refuse the 604 rejected encodings at the instruction and the 32
NaN-input programs at the ALU word (CPU only; these are not read back on
Metal), and the compiler refuses the 604 encodings at the same offset. The
[LLVM-assembled fixture](../../fixtures/rdna2/vector-round.s) exercises all
four forms, a partial EXEC and a literal source, and the LLVM three-way check
covers each form with every accepted source class. Independent acceptance is
pending. Xbox hardware agreement is unknown.

## Integer to binary32 conversion

Six more VOP1 forms convert an integer to binary32, defined in guide section
12.8 (pages 138 and 139 of the 30 November 2020 guide).

| Form | Result in each active lane |
| --- | --- |
| `v_cvt_f32_i32_e32` | `(float)S0.i`, the signed integer. |
| `v_cvt_f32_u32_e32` | `(float)S0.u`, the unsigned integer. |
| `v_cvt_f32_ubyte0_e32` to `v_cvt_f32_ubyte3_e32` | `(float)S0.u[8n+7:8n]`, unsigned byte n. |

SRC0 takes the same sources as the integer forms, including inline integers
`-16`-`64`, which convert as their values. Float inline constants stay outside
the VOP1 subset. EXEC, VCC and SCC behave as for the integer forms.

A byte, and any I32 or U32 magnitude below 2^24, converts exactly. From 2^24
up the result can be inexact, and the guide gives "0.5ULP accuracy" with the
rounding set by MODE.FP_ROUND[1:0] (guide 3.5, page 15, and 6.4, page 47).
The compute ABI pins it to 0, round to nearest, ties to even: `2^24 + 1` gives
`2^24`, `2^24 + 3` gives `2^24 + 4`, `0x7fffffff` gives `2^31` and
`0xffffffff` as U32 gives `2^32`. No MODE-writing instruction is in the subset,
so the other three round modes cannot be selected and are outside the
contract. No input value is refused.

The MSL computes every result from the register bits with unsigned integer
operations and `clz`: no Metal float conversion runs, so its rounding mode
and fast math cannot change a result. The CPU reference uses the host's
integer-to-float conversion, which rounds to nearest even; the second
interpreter finds the leading power of two and rounds the remainder of an
integer division. Neither shares code with the MSL.

The [guide-derived oracle](../../tests/validation/tests/vop1_int_to_float.rs)
has a hand-written table of 21 inputs (0, 1, -1 and `UINT_MAX`, -16,
`INT_MIN` and `INT_MIN + 1`, `2^31 - 1`, `2^24 - 1`, `2^24`, `2^24 + 1`,
`2^24 + 3` and their negations, and mixed byte patterns), checked against an
exact binary64 widening rounded once to binary32. A negative control checks
that the table's round-up cases differ from truncation. Its 258 accepted cases
pass on both CPU interpreters and, through the translator, on native Metal
(Apple M4, macOS 27.2.1, API and shader validation on: 258 cases in 127
programs). Both interpreters and the compiler refuse the 420 rejected
encodings at the same instruction. The
[LLVM-assembled fixture](../../fixtures/rdna2/vector-convert.s) exercises all
six forms, a partial EXEC, an inexact literal and an inline integer, and the
LLVM three-way check covers each form with every accepted source class.
Independent acceptance is pending. Xbox hardware agreement is unknown.
