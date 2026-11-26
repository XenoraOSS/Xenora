# SOP1 EXEC-writing forms

Twelve gfx1030 SOP1 forms read the Wave32 execution mask, combine it with a
source and write the result back. They are the only forms in the registry that
change EXEC.

| Form | Opcode | D | EXEC_LO after | SCC |
| --- | --- | --- | --- | --- |
| `s_and_saveexec_b32` | 60 | previous EXEC_LO | S0 & EXEC_LO | EXEC_LO != 0 |
| `s_or_saveexec_b32` | 61 | previous EXEC_LO | S0 \| EXEC_LO | EXEC_LO != 0 |
| `s_xor_saveexec_b32` | 62 | previous EXEC_LO | S0 ^ EXEC_LO | EXEC_LO != 0 |
| `s_andn2_saveexec_b32` | 63 | previous EXEC_LO | S0 & ~EXEC_LO | EXEC_LO != 0 |
| `s_orn2_saveexec_b32` | 64 | previous EXEC_LO | S0 \| ~EXEC_LO | EXEC_LO != 0 |
| `s_nand_saveexec_b32` | 65 | previous EXEC_LO | ~(S0 & EXEC_LO) | EXEC_LO != 0 |
| `s_nor_saveexec_b32` | 66 | previous EXEC_LO | ~(S0 \| EXEC_LO) | EXEC_LO != 0 |
| `s_xnor_saveexec_b32` | 67 | previous EXEC_LO | ~(S0 ^ EXEC_LO) | EXEC_LO != 0 |
| `s_andn1_saveexec_b32` | 68 | previous EXEC_LO | ~S0 & EXEC_LO | EXEC_LO != 0 |
| `s_orn1_saveexec_b32` | 69 | previous EXEC_LO | ~S0 \| EXEC_LO | EXEC_LO != 0 |
| `s_andn1_wrexec_b32` | 70 | new EXEC_LO | ~S0 & EXEC_LO | EXEC_LO != 0 |
| `s_andn2_wrexec_b32` | 71 | new EXEC_LO | S0 & ~EXEC_LO | EXEC_LO != 0 |

The guide's pseudocode is in section 12.3, printed pages 120 to 122, and the
same forms are summarised in the bit-wise instruction table on page 36. The
order of the two writes is the difference between the two families, and it is
observable:

```
s_and_saveexec_b32:    D.u32 = EXEC_LO;  EXEC_LO = S0.u32 & EXEC_LO;  SCC = (EXEC_LO != 0);
s_andn1_wrexec_b32:     EXEC_LO = ~S0.u32 & EXEC_LO;  D.u32 = EXEC_LO;  SCC = (EXEC_LO != 0);
```

`andn1` negates the source and `andn2` negates the mask. The other eight
combine the two operands directly.

## What the mask means here

The EXEC mask is 64 bits, but the guide states that under Wave32 "the upper
32-bit of EXEC are ignored" and that `EXECZ` reflects only the lower 32 bits
(section 3.3, page 13). These forms therefore touch `EXEC_LO` only, and a
`EXEC_LO` of zero does not mean the upper lanes are enabled.

The mask affects vector ALU, vector memory, LDS, GDS and export instructions.
It "does not affect scalar (ALU or memory) execution or branches" (page 13),
so one of these forms executes even when the mask is empty.

## Destinations

The destination is a normal SGPR, s0 to s105. The guide also says that when the
destination SGPR is out of range "no SGPR is written with the result. However,
SCC and possibly EXEC (if saveexec) is still written" (page 33). The decoder
refuses such a destination instead of executing the form with a discarded
result, and the refusal is recorded as an unsupported operand variant rather
than being hidden.

Sources are normal SGPRs, inline integers (-16 to 64) or one literal word. The
guide's operand table gives the source as `OPR_SSRC_NOLDS`, so EXEC itself
cannot be a source.

## How the compiler lowers them

EXEC is a compile-time value in this compiler: the fixture starts with a full
mask, and a scalar move to `exec_lo` sets a known one. A form that writes EXEC
therefore computes a known new mask, and the vector instructions that follow it
are lowered with that mask.

That works only while the source is known at translation time. A source that
comes from a lane-derived mask, such as the destination of a compare, is
refused rather than guessed, because the compiler has no runtime EXEC model.

## Registry and generator

The forms are declared as data in `crates/isa/semantics/rdna2/sop1.toml`, one
TOML entry each, with the new operand rule `sop1_saveexec`. The `exec` field
has two values for them:

- `exec_save` for the ten SAVEEXEC forms: EXEC_LO is written with the combined
  result and the destination receives the previous EXEC_LO.
- `exec_write` for the two WREXEC forms: EXEC_LO and the destination both
  receive the combined result.

`crates/isa/generate_semantics.py` checks each declaration against the pinned
AMD XML snapshot. For a form that writes EXEC the XML must list one implicit
EXEC read and one implicit EXEC write, and the form must be a 32-bit SOP1
form with one of the combining operations and the nonzero SCC effect. The
generator then emits the typed operation, its evaluation and the dispatch
table into `crates/isa/src/semantics_gen.rs`; no Rust is written per form.

The same generator change is what unblocks the EXEC-writing compares
(`v_cmpx_*`), which stay unsupported for now: they write EXEC with a lane mask
rather than with a wave-uniform source, which the compiler's model of EXEC
cannot express.

## Hazards

These are hardware behaviours that the ISA contract does not model, recorded
so that a program that writes EXEC is not surprised:

- A SALU instruction that changes EXEC from zero to non-zero immediately
  before an NSA MIMG instruction with four or more dwords can cause stability
  problems on gfx10.3. Any instruction, including `s_nop`, mitigates it.
- A non-VALU instruction that reads the EXEC mask followed by a VALU
  instruction that writes it needs a mitigating VALU instruction or
  `s_waitcnt_depctr`.
- A VMEM, FLAT, GLOBAL, SCRATCH or DS instruction that reads an SGPR, EXEC or
  M0 followed by a SALU or SMEM instruction that writes the same SGPR needs a
  VALU instruction or `s_waitcnt` between them.

## Evidence

The expectations in `tests/validation/tests/sop1_saveexec.rs` are written from
the guide's pseudocode only, and run on the reference interpreter, on the
second independent interpreter and, in an ignored test, on native Metal. The
LLVM encoder comparison covers the twelve opcodes through
`fixtures/rdna2/scalar-saveexec.s`.

The guide's own defects and the readings Xenora adopts for them are listed in
[the RDNA 2 errata record](../research/rdna2-errata.md).
