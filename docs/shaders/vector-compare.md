# Execution contract: bounded Wave32 VOPC integer compares (issue #186)

Status: v2, after independent review (codex-contract.log); findings 1-7 addressed. Scope is the Xenora compute-fixture ABI
(one input word and one output word per dispatched thread; dispatch index `i`
is wave `i / 32`, lane `i % 32`; every wave starts with EXEC_LO = 0xffffffff;
scalar state is wave-uniform).

## 1. Sources (public only)

- AMD "RDNA 2" Instruction Set Architecture Reference Guide, 30 Nov 2020
  ([public PDF](https://www.amd.com/content/dam/amd/en/documents/radeon-tech-docs/instruction-set-architectures/rdna2-shader-instruction-set-architecture.pdf)):
  - S1 §2.1 "Wave32 and Wave64", printed p. 7-8 (PDF p. 15-16): "The upper half
    of EXEC and VCC are ignored for wave32 waves."
  - S2 §3.9 "Vector Compares: VCC and VCCZ", printed p. 19-20 (PDF p. 27-28):
    compares set VCC (1 = pass, 0 = fail); "VCCZ is updated every time VCC is
    updated: vector compares and scalar writes to VCC"; "V_CMP_* =>
    VCC[n] = EXEC[n] & (test passed for thread[n])"; "VCC is fully written;
    there are no partial mask updates"; in wave32 "only the lower 32 bits of
    VCC contribute to the value of VCCZ".
  - S3 §4.5 "Manually Inserted Wait States", printed p. 30 (PDF p. 38):
    "Inserting S_NOP is not required to achieve correct operation."
  - S3b §3.3 "EXECute Mask", printed p. 13 (PDF p. 21-22): EXEC "does not
    affect scalar (ALU or memory) execution or branches"; in wave32 EXECZ
    reflects only the lower 32 bits.
  - S4 §6.2.2 "Instruction Outputs", printed p. 43 (PDF p. 51): a thread
    writes a VGPR result only if its EXEC bit is set; V_CMPX writes EXEC.
  - S4b §6.2 Table 20 (operand selectors), PDF p. 49-50: 106 = VCC_LO.
  - S5 §12.9 "VOPC Instructions", printed p. 150-152 (PDF p. 158-160): field map
    SRC0 / VSRC1 / OP; "producing a 1 bit result per lane into VCC or EXEC";
    Table 59: integer compare offsets LT 1, EQ 2, LE 3, GT 4, LG/NE 5, GE 6.
    Table 61 "VOPC Compare Opcodes", printed p. 155 and 159 (PDF p. 163, 167):
    128-135 V_CMP_{F,LT,EQ,LE,GT,NE,GE,T}_I32 and 192-199 the _U32 forms,
    "D = VCC in VOPC encoding". The XML is authoritative for encodings.
- AMD RDNA 2 machine-readable ISA XML (pinned SHA-256 `d671ecbc...`, used by
  `crates/isa/generate_rdna2.py`; see `docs/research/rdna2.md`): ENC_VOPC opcodes V_CMP_{LT,EQ,LE,GT,NE,GE}_I32
  = 129-134 and V_CMP_{LT,EQ,LE,GT,NE,GE}_U32 = 193-198; descriptions "Set the
  per-lane condition code to 1 iff the first input is <rel> the second input";
  SRC0 and VSRC1 data format FMT_NUM_I32 for the _I32 forms and FMT_NUM_U32
  for the _U32 forms; destination OPR_VCC.
- LLVM 23.1.6 AMDGPU (`llvm-mc`, gfx1030, `+wavefrontsize32`) as an encoding
  oracle only: `compares_fixture_instructions_against_llvm` checks every form
  with every source class against the decoder and the reference.

## 2. Supported forms

The 32-bit VOPC encoding (bits 31:25 = 0b0111110), no SDWA or DPP:

| Form | per-lane test, S0 = SRC0, S1 = VSRC1 |
| --- | --- |
| v_cmp_lt_i32 / v_cmp_lt_u32 | S0 < S1 |
| v_cmp_eq_i32 / v_cmp_eq_u32 | S0 == S1 |
| v_cmp_le_i32 / v_cmp_le_u32 | S0 <= S1 |
| v_cmp_gt_i32 / v_cmp_gt_u32 | S0 > S1 |
| v_cmp_ne_i32 / v_cmp_ne_u32 | S0 != S1 |
| v_cmp_ge_i32 / v_cmp_ge_u32 | S0 >= S1 |
| v_cmp_f_i32 / v_cmp_f_u32 | false (Table 59 offset 0) |
| v_cmp_t_i32 / v_cmp_t_u32 | true (offset 7) |

`_i32` compares the 32-bit two's-complement values, `_u32` the unsigned values.
The forms are declared in the semantic registry
(`crates/isa/semantics/rdna2/vopc.toml`, operand rule `vopc_vcc`); the decoder
dispatches them from the generated `VectorCompareOp`.
Operand order matters (S0 is SRC0): lt with swapped operands is gt.

SRC0 selectors, exhaustively: 0-105 (normal SGPR), 106 (VCC_LO, see 5),
128-208 (inline integers 0..64 and -1..-16), 255 (one 32-bit literal, which
must be present in full after the word), 256-511 (VGPR). Every other selector
is rejected at decode, independent of EXEC: TTMPs, M0, NULL, EXEC_LO/HI,
VCC_HI, apertures, wave-ID, reserved values, inline floats 240-248, SDWA 249,
DPP8 233/234, DPP16 250, VCCZ/EXECZ/SCC 251-253, LDS direct 254. VSRC1 is a
VGPR.

Wave size: the compute fixture ABI is defined as Wave32. A raw instruction
stream carries no wave-size declaration (the VOPC word has no wave-size field),
so every caller of the translator and the reference establishes Wave32 by
using this ABI; there is no Wave64 entry point to reject.

### 2a. 16-bit and 64-bit compares

The decoder, translator and both reference interpreters accept the `_e32`
forms `v_cmp_{lt,eq,le,gt,ne,ge}_{i16,u16}` (opcodes 137–142, 169–174) and
`v_cmp_{f,lt,eq,le,gt,ne,ge,t}_{i64,u64}` (160–167, 224–231). The registry
uses `vopc_vcc_half` for the 16-bit forms and `vopc_vcc_pair` for the 64-bit
forms.

- 16-bit forms read bits 15:0 of each 32-bit source and ignore bits 31:16
  (an inference from guide 6.2, 6.2.5 and 6.8: only SDWA selects a half).
  SRC0 accepts a VGPR, a known SGPR, `vcc_lo` or an inline integer
  (for example, −1 supplies `0xffff`). VSRC1 is a VGPR. Literals are rejected.
- 64-bit forms read a pair: v[n:n+1] with n <= 254, an even-aligned
  s[2k:2k+1] with k <= 52 (3.6.3), or an inline integer, which is
  sign-extended to 64 bits (-1 is all ones, also for `_u64`; a reading of
  Table 20). VSRC1 is v[m:m+1] with m <= 254. Literals and VCC as a 64-bit
  source are rejected.
- The result, EXEC masking, VCCZ and the empty-EXEC rule are those of
  sections 3 and 5.

The expectations and the guide readings are in
`tests/validation/tests/vopc_wide.rs`.

### 2b. Binary32 compares (#786)

The decoder, translator and both reference interpreters accept the sixteen
`_e32` forms `v_cmp_{f,lt,eq,le,gt,lg,ge,o,u,nge,nlg,ngt,nle,neq,nlt,tru}_f32`
(opcodes 0-15, operand rule `vopc_vcc_f32`). The per-lane tests are the
per-opcode lines of guide Table 61 (pages 152-153):

| Opcode | Form | test, S0 = SRC0, S1 = VSRC1 |
| --- | --- | --- |
| 0 | v_cmp_f_f32 | false |
| 1-4 | v_cmp_{lt,eq,le,gt}_f32 | S0 < S1, S0 == S1, S0 <= S1, S0 > S1 |
| 5 | v_cmp_lg_f32 | S0 <> S1 (S0 < S1 or S0 > S1) |
| 6 | v_cmp_ge_f32 | S0 >= S1 |
| 7 | v_cmp_o_f32 | !isNan(S0) && !isNan(S1) |
| 8 | v_cmp_u_f32 | isNan(S0) \|\| isNan(S1) |
| 9-14 | v_cmp_{nge,nlg,ngt,nle,neq,nlt}_f32 | !(S0 >= S1), !(S0 <> S1), !(S0 > S1), !(S0 <= S1), !(S0 == S1), !(S0 < S1) |
| 15 | v_cmp_tru_f32 | true |

- The operands are IEEE 754 binary32 values. A NaN, quiet or signalling and
  of either sign, is unordered with every value, itself included, so LT..GE
  and LG are false and the N* forms are true on it. `isNan` is read as true
  for both NaN kinds; the guide does not distinguish them.
- U follows the per-opcode table. The summary table's
  `!isNaN(S0) || !isNaN(S1)` is its complement and is not used
  (`docs/research/rdna2-errata.md`).
- +0 and -0 compare equal. The compute ABI pins MODE input denormals allowed,
  so a subnormal compares by its value, not as zero.
- SRC0 accepts a VGPR, a known normal SGPR, `vcc_lo` or a literal. Integer
  inline constants (128-208) are rejected, as for the binary32 VOP1 forms,
  because the guide does not say how a float operation reads them. Float
  inline constants (240-248) stay rejected. VSRC1 is a VGPR.
- The result, EXEC masking, VCCZ and the empty-EXEC rule are those of
  sections 3 and 5.
- The MSL does no float arithmetic: it tests for a NaN as an exponent of all
  ones with a nonzero fraction, and orders the other values through an
  unsigned key on the bits with both zeros mapped to the same key. The Metal
  math mode therefore cannot change a NaN or signed-zero result.

The expectations and the guide readings are in
`tests/validation/tests/vopc_f32.rs`.

## 3. Semantics of one supported compare, Wave32

Let E = EXEC_LO before the instruction and A = the set of lanes that exist in
this wave (all 32 except in a final partial wave, see 7). As an unsigned
32-bit value,

    VCC_LO := OR over 0 <= n < 32 of ( [n in A] & E[n] & test(S0[n], S1[n]) ) << n

- VCC_LO is fully written, every time, including bits of inactive or absent
  lanes, which become 0 (S2). With E = 0 the result is VCC_LO = 0 even if VCC
  held a nonzero value before.
- Inactive lanes (E[n] = 0) read no VGPR source; their bit is 0 regardless of
  their data.
- VCC_HI: S1 says it is ignored in wave32; whether hardware writes it is not
  settled by the sources. After any supported compare, VCC_HI has no known
  value. (No supported instruction can read vcc_hi today; this keeps any
  future reader honest.)
- EXEC, SCC, every VGPR and every other SGPR are unchanged.
- VCCZ, as read by `s_cbranch_vccz`, is (VCC_LO == 0) (S2, wave32 sentence).
- No wait states are needed between the compare and a reader of VCC (S3).

## 4. Rejected (controlled translation/reference error, never a silent no-op)

- V_CMPX_* (writes EXEC: divergent execution; needs its own contract).
- VOP3A compares outside the integer subset in section 11.
- SDWA and DPP forms; float compares other than the binary32 forms in 2b
  (F16 and F64); v_cmp_class.
- 16-bit compares in the decoder and translator (the reference interpreters
  accept the subset in 2a).
- SRC0 inline float constants (240-248), SGPRs other than s0-s106, TTMPs,
  M0, EXEC_LO/HI, VCC_HI, SCC/VCCZ/EXECZ selectors, LDS direct.
- A normal-SGPR SRC0 without a known value, when EXEC_LO != 0.
- Wave64 in any form.

## 5. VCC_LO state and where a compare result may flow

Both the translator and the reference track VCC_LO in one of three static
states, decided by the instruction path alone, never by lane data:
- *uninitialized* (program start);
- *known*: written by `s_mov_b32 vcc_lo, <known value>`, or by a compare
  executed while EXEC_LO is statically 0 (result: known 0);
- *lane-derived*: written by a compare executed while EXEC_LO != 0. It stays
  lane-derived even when a particular wave's result happens to be 0 or equal
  to a constant; acceptance never depends on that value.

Acceptance rules for one compare:
- EXEC_LO = 0 (EXEC is always a known value in this ABI): the encoding is
  validated, no source is read or checked (not VGPR readiness, SGPR knowledge
  or VCC state), and VCC_LO becomes known 0.
- EXEC_LO != 0: VSRC1 and a VGPR SRC0 must be initialized and not pending a
  load; a normal-SGPR SRC0 must be known; a `vcc_lo` SRC0 must be known or
  lane-derived (uninitialized is rejected). The 64-bit forms check both words
  of each register pair. VCC_LO becomes lane-derived.

While VCC_LO is lane-derived it may be read only by:

1. `s_cbranch_vccz` (6);
2. `v_mov_b32_e32 vD, vcc_lo`, which writes the 32-bit mask into vD of active
   lanes (inactive lanes keep vD, as for any partial-EXEC move);
3. SRC0 of a supported 32-bit compare (`v_cmp_* vcc_lo, vcc_lo, vN`), which reads the
   old VCC_LO in every active lane before the new value is written.
4. SRC0 of a registry VOP2 integer form (for example
   `v_add_nc_u32_e32 vD, vcc_lo, vN`), which reads the 32-bit mask in every
   active lane, as `v_mov_b32_e32` does.

`v_mov_b32_e32 vD, vcc_lo` and the VOP2 forms follow the usual EXEC rules: with EXEC_LO = 0 it
is skipped without reading VCC; otherwise VCC_LO must be known or
lane-derived. Every other read of vcc_lo is rejected in any state: the
supported scalar instructions (`s_mov_b32`, SOP2, `s_cmp_eq_u32`) do not
accept vcc_lo as a source at decode, so `s_mov_b32 exec_lo, vcc_lo` cannot
create divergent EXEC. `s_mov_b32 vcc_lo, <known>` makes VCC_LO known again.

## 6. Branches

- `s_cbranch_scc0`: unaffected (compares do not touch SCC).
- `s_cbranch_execz`: unaffected (compares do not touch EXEC).
- `s_cbranch_vccz` with a known VCC_LO: resolved during translation, as today.
- `s_cbranch_vccz` with a lane-derived VCC_LO (a *runtime branch*): the whole
  wave takes it iff VCC_LO == 0. Control flow stays wave-uniform; no lane
  diverges. Accepted only if the skipped region R (from the end of the branch
  to the target, exclusive) satisfies all of:
  - the target is a forward instruction boundary (existing rule);
  - no vector load is outstanding at the branch;
  - R contains only these integer vector forms: `v_mov_b32_e32` (from a
    VGPR, an immediate (inline integer, the inline +1.0f bit pattern or a
    literal; the move copies bits), a known normal SGPR, or vcc_lo known or
    lane-derived), `v_add_nc_u32_e64`, `v_lshlrev_b32_e32`,
    `v_lshrrev_b32_e32`, `v_ashrrev_i32_e32`, `v_and_b32_e32`, `v_or_b32_e32`,
    `v_xor_b32_e32`. Nothing else: no FP32 form, scalar instruction, compare,
    branch, wait, memory access or `s_endpgm`;
  - every VGPR written in R was initialized before the branch;
  - every source read in R is ready where it is read (the normal rules, as on
    the fall-through path).
  These rules are checked on R's instructions whether the branch is taken or
  not, and whatever EXEC_LO is, so acceptance does not depend on lane data.
  After R, each VGPR written in R holds its value from R if the branch was not
  taken, else its value from before the branch; scalar state (SGPRs, SCC,
  EXEC, VCC state) is the same on both paths. The translator's known-bit
  facts for a joined VGPR keep only bits known and equal on both inputs, and
  the union of the candidate values when it has at most two (so an exact
  FP32 operation after the join is still accepted when both edges allow it).
  An outstanding store at the branch is allowed: the store already read its
  data.

## 7. Wave formation and absent lanes

Waves are formed from consecutive dispatch indices: wave w is indices
32w..32w+31. A final partial wave (input length not a multiple of 32) has
absent lanes; they never execute and contribute 0 to every compare result.
(The fixture's EXEC_LO still reads 0xffffffff in such a wave; that existing
ABI simplification is unchanged and documented as a limitation.)

## 8. Mapping to Metal (no SIMD-group assumption)

Metal SIMD groups, threadgroups and RDNA 2 waves are not assumed to coincide.
The Metal kernel makes no cross-thread call: no simd_ballot, no threadgroup
memory, no barrier. For each compare, every thread recomputes the compare's
per-lane operand values for each of the 32 lanes of its own RDNA wave
(dispatch indices (index & ~31) + n, bounded by the grid size from
`[[threads_per_grid]]`), from those lanes' input words, and assembles VCC_LO
itself. All values are pure functions of input words and translation-time
constants, so every thread of a wave computes the same VCC_LO. A runtime
branch becomes a per-register select on (VCC_LO != 0) at the join; region
instructions are pure 32-bit ALU and are evaluated unconditionally.

## 9. Verification

`fixtures/rdna2/vector-compare.s` is an original program, assembled by LLVM.
It uses the twelve relational forms with VGPR, SGPR, inline, literal and `vcc_lo`
sources, a compare and a mask move under partial EXEC, a compare under empty
EXEC that must clear a stale VCC, SCC kept across compares, and a runtime
`s_cbranch_vccz`. Its expected output is written in
`tests/validation/known_answers.rs` from the listing, with an explicit
relation for each row. The test input has a wave whose branch is taken, one
whose branch is not, and a final partial wave of 9 lanes.

`fixtures/rdna2/vector-compare-ft.s` does the same for the four F and T
forms: an F that clears a stale VCC, T and F under partial EXEC, a T reading
`vcc_lo`, and runtime `s_cbranch_vccz` on a T result (never taken) and an F
result (always taken). The independent oracle for F and T is
`tests/validation/tests/vopc_f_t.rs`.

`tests/validation/known_answers.rs` runs each relational form against 10 boundary values (0, 1, 0x7ffffffe,
0x7fffffff, 0x80000000, 0x80000001, 0xfffffff0, 0xfffffffe, 0xffffffff,
0x0f0f0f0f), in both operand orders. That is 240 programs, checked against
exact i128 differences. It also covers partial and empty EXEC with seeded
destinations, both runtime branch edges in one dispatch, and 32 rejections
pinned to their byte offset. The translator, the reference (lockstep waves,
compares on widened i64 values) and the second interpreter (sign-bit flip,
then an unsigned compare) must all agree. The 9 ordered compare slots in the
relational fixture each change the result when their signedness is flipped. The same
programs, plus 2 mask-chain programs (252 in all), and the F/T fixture (253) match on native Metal with API and shader validation
(`vector_compare_native_metal_known_answers`, run by `scripts/test-gpu.sh`). The F/T oracle's own native Metal cases
run in `vopc_f_t_match_the_guide_on_native_metal`.

The 16-bit and 64-bit oracle (`tests/validation/tests/vopc_wide.rs`) checks
6,212 cases on both reference interpreters, plus 1,208 rejections and 13
neighbouring opcodes. CPU tests check compilation of 1,572 16-bit and 4,640
64-bit cases, plus compiler rejection of out-of-contract programs. LLVM
assembled `vector-compare-16.s` and `vector-compare-64.s` cover all 28 opcodes
in the decoder's fixture inventory. The native Metal gate runs both
`vopc16_match_the_guide_on_native_metal` and
`vopc64_match_the_guide_on_native_metal`. On a local Apple M5 with API and
shader validation, all 4,640 64-bit cases (3,818 programs) agree with the
oracle. On a local Apple M5 under API and shader validation, all 1,572 16-bit cases (1,420 programs) agree; independent acceptance is pending. Deliberately broken copies of both
interpreters (16-bit reads of 32 bits, swapped pair halves, zero-extended
inline integers, flipped signedness, reads under empty EXEC and others) each
fail it.

The binary32 oracle (`tests/validation/tests/vopc_f32.rs`) checks 976 cases
on both reference interpreters: every form against 16 values (quiet and
signalling NaNs of both signs, both zeros, both infinities, subnormals and
ordinary values) in both operand orders, an aliased source, literal, SGPR and
`vcc_lo` sources, five EXEC masks over a stale VCC_LO and partial waves. It
also checks 1,456 rejected encodings (every integer and float inline
constant and a missing literal) at their byte offset in both interpreters and
the compiler, a hand classification of 20 operand pairs, and negative
controls showing that the expectations reject the summary table's U, the
complements of the N* forms, a raw-bit integer compare and denormal
flushing. A copy of the reference with U inverted fails it. LLVM assembled
`vector-compare-f32.s` covers the sixteen opcodes in the decoder's fixture
inventory. On a local Apple M4 with API and shader validation, all 976 cases
(626 programs) agree with the oracle on native Metal; independent acceptance
is pending.

For the 32-bit forms, a deliberately broken copy of each implementation fails
the tests above: the
reference with unsigned-only compares or with the EXEC test removed; the
second interpreter with unsigned-only compares or with no empty-EXEC rule;
the translator with a stale VCC under empty EXEC (CPU), and, on Metal, with
swapped signedness, without the EXEC mask, without the absent-lane bound
(shader validation aborts on the out-of-range read) or with an inverted join.

The 32-bit readback evidence is local agreement with a specification-derived
reference on Apple M5 Pro. There is no RDNA 2 or Xbox hardware evidence.

## 10. Limitations

- Wave32 only. No `v_cmpx`, SDWA, DPP, or F16 and F64 compares. VOP3 encodings cover the 32-bit integer compares and the six ordered binary32 compares LT, EQ, LE, GT, LG and GE.
- A lane-derived VCC_LO cannot reach EXEC or any scalar instruction, so there
  is no divergent execution. That needs its own contract.
- The skipped region of a runtime `s_cbranch_vccz` holds only integer vector
  ALU instructions. Loops, nested runtime branches and scalar state that
  differs between the two edges are not supported.
- Each thread recomputes up to 32 lanes per compare. The cost grows with the
  compare's dependency chain. That is fine for fixtures, and no performance is
  claimed.

## 11. VOP3A encoding (`_e64`)

All sixteen 32-bit signed and unsigned compares in section 2 also have a 64-bit
VOP3A encoding. The pinned AMD XML supplies each opcode. Bits 7:0 of the first
word are the scalar destination selector (SDST), in the field named VDST.
Selectors 0–105 name scalar general-purpose registers (SGPRs), and 106 names
`vcc_lo`. All other destinations are rejected. Both SRC0 and SRC1 must name
vector general-purpose registers (VGPRs), and the unused SRC2 must be the
assembler's inline-zero selector (128). ABS, OP_SEL, CLAMP, OMOD and NEG must
be clear. The guide describes CLAMP on compares as enabling floating-point
exception signaling (§6.5, p. 48 and §12.9.1, p. 162).

The AMD guide's compare-specific text (§3.9, pp. 19–20 and §13.3.3, p. 252)
specifies an SGPR destination for this encoding. The generic VDST label in
§12.9.1 (p. 162) and §13.3.4 Table 82 (pp. 257–258) describes the VOP3A field
as a VGPR destination. The pinned XML identifies VDST for these compares as
`OPR_SREG`, with the Wave64 pair described by `FMT_NUM_M64`. The Wave32 write
occupies one 32-bit SGPR. The guide defines 32 work-items in §2.1 (pp. 7–8),
32-bit SGPRs in §3.1 Table 2 (p. 12) and one compare result bit per lane in
§12.9 (p. 150). Wave64 stays unsupported.

LLVM 23 `llvm-mc` for gfx1030 with `+wavefrontsize32` assembles
`v_cmp_eq_u32_e64 s4, v1, v2` as bytes `04 00 c2 d4 01 05 02 02`.
Selecting `vcc_lo` changes the first byte to `6a`. The decoder tests cover
the assembler's words for all sixteen forms. This is encoding evidence.

The compare replaces all 32 bits of SDST with the lane mask from section 3,
including zero bits for inactive and absent lanes. It leaves SCC and EXEC
unchanged. VCC is written only when SDST selects `vcc_lo`, which uses the
same lowering and consumers as the VOPC form, including `s_cbranch_vccz`.
Every other SDST is tracked as a runtime-only SGPR result, including under
empty EXEC. A vector move may copy that wave mask into a VGPR for readback;
scalar operations and resource-descriptor uses of it remain rejected. The
runtime branch region rules in section 6 also apply here.

The six ordered binary32 compares `v_cmp_{lt,eq,le,gt,lg,ge}_f32_e64` use
the same destination and mask rules. Unlike the integer forms, they accept
the documented ABS and NEG input modifiers on SRC0 and SRC1: ABS clears the
input sign bit and NEG flips it before comparison. The modifiers on the
unused SRC2, OP_SEL, CLAMP, OMOD, non-VGPR sources and every use of SRC2
other than inline zero remain rejected. Quiet and signalling NaNs are
unordered and make all six relations false; +0 and -0 compare equal. The
reference, independent interpreter and Metal lowering compare the modified
binary32 bits without using host or Metal floating-point comparison
instructions, so the result does not depend on a floating-point math mode.
