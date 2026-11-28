# Wave32 binary32 vector arithmetic

Xenora translates eight gfx1030 binary32 arithmetic or selection forms for the
synthetic Wave32 compute ABI: the six VOP2 forms `v_add_f32`, `v_sub_f32`,
`v_subrev_f32`, `v_mul_f32`, `v_min_f32` and `v_max_f32`, the DX9 legacy
multiply `v_mul_legacy_f32` in its VOP2 and VOP3 encodings, and the three-input
VOP3 median `v_med3_f32`. The AMD RDNA 2 ISA Reference Guide defines the VOP2
operations in section 12.7, MED3 in section 12.12, the encodings in sections
13.3.1 and 13.3.4, and the execution mask (EXEC) in section 3.3.

| Form | Result in each active lane | Guide |
| --- | --- | --- |
| `v_add_f32_e32` | `D.f32 = S0.f32 + S1.f32` | p131 |
| `v_sub_f32_e32` | `D.f32 = S0.f32 - S1.f32` | p131 |
| `v_subrev_f32_e32` | `D.f32 = S1.f32 - S0.f32` | p131 |
| `v_mul_f32_e32` | `D.f32 = S0.f32 * S1.f32` | p131 |
| `v_min_f32_e32` | The p132 minimum. | p132 |
| `v_max_f32_e32` | The p133 maximum. | p133 |
| `v_mul_legacy_f32_e32` / `_e64` | `D.f32 = S0.f32 * S1.f32` with the DX9 rule that `0.0 * x = 0.0`; explicitly not IEEE-compliant. | p131 |
| `v_med3_f32` | The three-input median of page 170, through its MIN3/MAX3 branch order. | p170 |

`v_subrev_f32_e32` reverses the operands: it is the second input minus the
first, so it is not the negation of `v_sub_f32_e32`.

## Operand limits

In the VOP2 forms, SRC0 accepts a VGPR, a normal scalar register `s0`–`s105`,
`vcc_lo` (the low half of the vector condition code), or a literal word. VSRC1
must be a VGPR and VDST is a VGPR. The VOP3 legacy multiply has the same
narrowed source domain on SRC0/SRC1, while MED3 uses it on all three sources;
both accept at most one literal and transport per-source ABS/NEG modifiers.
Inactive lanes keep their old values, and none of these forms writes VCC, SCC
or EXEC.

VOP3 OP_SEL, CLAMP and OMOD are refused rather than dropped. For the legacy
multiply the unused SRC2 field must encode inline zero and must not carry a
modifier bit; for MED3 SRC2 is a real input.

An integer inline constant is refused at decode: the guide does not say
whether a binary32 operation reads one as an integer bit pattern or as a
converted value, and the two readings differ. A float inline constant stays
outside the VOP2 subset for the same reason.

## The modelled domain, and what is refused

The six forms are modelled over the inputs whose result the CPU reference and
native Metal agree on. Everything outside that domain is refused rather than
approximated, so a program that needs one of these cases is told so instead of
receiving a plausible wrong answer.

- **Rounding.** MODE.FP_ROUND is pinned to 0, round to nearest with ties to
  even (guide p15). ADD, SUB, SUBREV and MUL are IEEE 754 binary32
  operations; an exact zero sum is `+0` unless both addends are `-0`, and an
  overflow gives a signed infinity.
- **Subnormal inputs are refused**, for all six forms. The pinned mode allows
  input denormals, but Metal flushes them in float arithmetic, so the two
  paths would disagree on the same program.
- **A NaN input or a NaN result is refused for ADD, SUB, SUBREV and MUL.** The
  guide gives no NaN result encoding, and Metal returns the same quiet NaN
  for every NaN-producing operation.
- **Legacy multiply is the deliberate exception for a zero operand.** The DX9
  rule makes an exact `+0` or `-0` input win over every other operand,
  including NaN, infinity or a subnormal. The guide does not pin the sign of
  that forced zero, so validation requires a zero magnitude and reports the
  implementation sign only as an observation. When neither operand is zero the
  arithmetic domain is the same as MUL.
- **A signalling NaN input is refused for MED3.** Quiet NaNs follow the guide's
  MIN3 branch exactly: one quiet NaN gives the minimum of the two non-NaN
  sources, two quiet NaNs leave the non-NaN source, and three quiet NaNs select
  the S2 payload through the nested MIN rules. Because MED3 is selection only,
  subnormal inputs are bit-exact and are not refused.
- **A signalling NaN input is refused for MIN and MAX.** The guide quiets a
  signalling NaN only under `IEEE_MODE`, and defines neither `IEEE_MODE` as a
  mode nor the `Quiet()` function it calls; `IEEE_MODE` is not pinned.
- **An ADD, SUB, SUBREV or MUL result whose exact value is nonzero and below
  2^-126 in magnitude is refused**, because output flushing is not pinned and
  Metal flushes.

The MIN and MAX rules are the guide's own pseudocode (pages 132 and 133), not
Metal's `fmin` and `fmax`, which differ on signed zeros. A quiet NaN operand
gives the other operand (`S1` when both are NaN); a `+0`/`-0` pair gives `-0`
for MIN and `+0` for MAX; otherwise the result is `S0 < S1 ? S0 : S1` for MIN
and `S0 >= S1 ? S0 : S1` for MAX. The guide's `IEEE_MODE` branch changes only
the signalling NaN cases and MAX's `>=` against `>`, and the latter selects
the same bits for equal non-zero operands.

MED3 is not simplified into a sorting network. Its page 170 branch order first
uses MIN3 whenever any source is NaN. Otherwise it compares MAX3 numerically
with S0 and then S1; because this is floating-point equality, `+0` equals `-0`
and the source-position branch can decide the sign of a zero result. The final
step uses the same MAX rule on the remaining pair.

## Where the domain is enforced

The compiler accepts every program whose encoding is in the subset. The
value domain above is enforced by the CPU interpreters, at run time, on the
value a lane actually holds: a value outside the domain is refused there
rather than answered with a number the two paths would not agree on. A
program whose operands are constants is not refused at compile time, because
the compiler does not evaluate the float domain; the two hand-written forms
this batch replaces used to fold an exact constant float operation and
refuse an inexact one at compile time, and neither happens now.

A translator therefore accepts a program that the interpreters will refuse if
its runtime values leave the domain, and Metal computes such a program with
Metal's own rules, which flush a subnormal to zero. The reference and the
Metal path therefore disagree for a program that uses an out-of-domain
value, and the disagreement is the documented limit of this batch rather
than a silent difference.

## How each path computes the result

The CPU reference computes binary32 arithmetic exactly, in integer
arithmetic on the bit patterns, so it does not inherit the host floating-point
unit's behaviour. It rounds the exact result once, to nearest even.

The Metal Shading Language path uses native binary32 arithmetic for ADD, SUB,
SUBREV and MUL, with contraction and reassociation turned off and the library
built in safe math mode. MIN and MAX are computed with integer tests on the
register bits, because no Metal float operation reproduces the guide's
signed-zero rule. The compare path used by the VOPC binary32 forms does the
same for the same reason.

## Evidence

The [guide-derived oracle](../../tests/validation/tests/vop2_float_arithmetic.rs)
holds hand-written expected values transcribed from the guide's definitions
and checks them against both CPU interpreters and, through the translator, on
native Metal. The [LLVM-assembled fixture](../../fixtures/rdna2/vector-f32.s)
exercises all six forms, including the reversed subtraction, a literal
multiplier, a minimum against a scalar register and a maximum against `-0`
under a partial EXEC mask.

The [#975 oracle](../../tests/validation/tests/vop3_med3_mullegacy.rs)
independently covers VOP2/VOP3 legacy multiply and MED3: zero against quiet
NaN, infinity and subnormal operands; nonzero exact products; ABS/NEG on every
VOP3 source; signed-zero positions that distinguish numeric equality from bit
equality; quiet NaNs in every position; subnormal MED3 selection; scalar and
literal sources; and controlled rejection of OP_SEL, CLAMP, OMOD, bad SRC2,
inline constants, special sources and multiple literals. Native Metal readback
passed locally under API and shader validation, while independent acceptance
remains pending. The [LLVM-assembled #975 fixture](../../fixtures/rdna2/vop3-float-975.s)
checks the VOP2/VOP3 encodings and modifier spellings.

Independent acceptance of the translator is pending. Xbox hardware agreement is
unknown.
