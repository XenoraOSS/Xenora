# Adding an instruction through the semantic registry

The semantic registry is a data file that declares what each supported gfx1030
instruction form does. A generator checks it and turns it into Rust tables that
the decoder uses. This page explains where entries live, what each field means,
how to check and regenerate the tables, and what the generator does not do for
you.

The registry covers the nine SOP2 forms listed in [SOP2 scalar ALU](scalar-alu.md).

## Files

| File | Role |
| --- | --- |
| `crates/isa/semantics/rdna2.toml` | The registry root: schema, architecture, the snapshot hash, the cited documents and the ordered `include` list of family files. |
| `crates/isa/semantics/rdna2/<family>.toml` | One file per encoding family (`sop2.toml`, `sopc.toml`, `sopk.toml`, `sop1.toml`, `vop2.toml`, `vopc.toml`, `vop3.toml`): that family's operand rules, forms and encoding variants only. Data only: every value comes from a closed vocabulary. |
| `crates/isa/generate_semantics.py` | Validates the registry and writes the Rust tables. Python 3.11 or newer, standard library only. |
| `crates/isa/data/rdna2.jsonl` | Pinned snapshot of the AMD machine-readable ISA XML, used to cross-check identity and operands. |
| `crates/isa/src/semantics_gen.rs` | Generated output. Never edit it by hand. |
| `crates/isa/src/semantics.rs` | The Rust types the generated tables use (`FormSemantics`, `Operation`, `SccEffect` and so on). |
| `crates/isa/src/primitive.rs` | The shared integer primitives that the generated `evaluate` calls. |
| `tests/semantics_generator/` | Unit tests for the generator. |

## Family files

The root lists its family files under `include`. The generator merges them in
that order, and forms keep their order within each file, so the merged registry
equals one file with the parts concatenated (splitting the original single file
left `semantics_gen.rs` byte-identical). A family file may hold only
`[operand_rules.*]`, `[[form]]` and `[[encoding_variant]]` entries of the family
it is named after: `rdna2/vop2.toml` holds VOP2 rules and forms and nothing else.
Each include must be `rdna2/<lower-case family>.toml`, a regular file in that directory (not a link). The loader rejects a form,
rule or variant of another family, a rule declared in two files, root keys in a
family file, any other include path, and a missing or repeated include. The
coverage report and the RDNA2 planner use the same loader. Work on different
families therefore never edits the same registry file. To add a family, create
`rdna2/<family>.toml` and append it to `include`; the generated order (and so
the generated file) changes only where the new file is placed.

## Encoding variants

An `[[encoding_variant]]` is another encoding of a form that is already declared,
such as the VOP3 (`_e64`) encoding of a VOP2 form. It is pure data in the family
file of its encoding (`rdna2/vop3.toml`) and has exactly these fields:

| Field | Meaning |
| --- | --- |
| `of` | The `name` of the declared base form |
| `family` | The encoding family of this variant; must differ from the form's |
| `opcode` | The XML opcode in that family |
| `operands` | An operand rule of that family that can encode the base form (`vop3_vgpr2` for VOP2 vector forms, `vop3_cmp_vgpr2` for VOPC integer compares) |
| `spec`, `verify` | As for a form |

The variant inherits the operation, signedness, source order and SCC and EXEC
effects. Its operand rule determines where the result is written. The XML
check uses the variant's family, opcode and operand rule to validate the
inherited declaration. The generator emits one enum per variant rule
(`VectorAluVop3Op` or `VectorCompareVop3Op`) with `from_word` (encoding
bits, then an explicit match on the OP field), `base()` (the inherited form),
`assembly_name()` and `semantics()`, and one `FORMS` row per variant after the
forms. Because the enum has no undeclared members, an instruction cannot name a
VOP3 encoding that does not exist. Corresponding explicit XML operands must
have the same data format by role. A variant-only rule says so
in the data (`variant_only = true`), so the RDNA2 planner does not count it as a
rule that can host new forms. A
variant-only rule (one marked `variant_of` in the generator) cannot be used by a
form. The validator rejects a variant of an undeclared form, one in the form's
own family, a duplicate encoding of a form or a duplicate `(family, opcode)`, a
rule that cannot encode the form, missing or unknown fields, and any XML
disagreement.

`vop3_vgpr2` is the subset the decoder accepted for `v_add_nc_u32_e64` before
the registry: VGPR destination, two VGPR sources, ABS, OP_SEL, CLAMP, OMOD and
NEG clear, and SRC2 the assembler's unused inline zero. `v_add_nc_u32_e64` is its
first variant; adding the VOP3 encoding of another VOP2 form under this rule is
one `[[encoding_variant]]` entry. Wider VOP3 operand rules (scalar and inline
sources, legal modifiers) need separate rules.

`vop3_cmp_vgpr2` uses the same VGPR source and modifier restrictions and
specifies a scalar destination. It requires a base form with a `lane_mask`
write to implicit VCC, and generates an `sdst_lane_mask` effect on the
variant row. The generator checks the base VCC output and the variant's
explicit scalar mask destination against the XML. An SDST of 106 selects
`vcc_lo`. See [the compare contract](vector-compare.md#11-vop3a-encoding-_e64)
for Wave32 lowering and allowed consumers.

## Fields

The root file holds:

- `schema`: must be `1`.
- `architecture`: must be `"gfx1030"`.
- `source_sha256`: must equal the `sha256` on the `source` line of
  `crates/isa/data/rdna2.jsonl`, so the registry is always checked against the
  snapshot it was written for.
- `[documents.<name>]`: the public documents that spec references cite. Only
  `guide` (the AMD RDNA 2 ISA reference guide) and `xml` (the AMD
  machine-readable ISA) are allowed, and each must match the title and file
  pinned in the generator, so a citation cannot point at another file.
- `[operand_rules.<name>]`: an operand rule of one family, shared by its forms.
  The decoder implements each rule once, and the generator only accepts a rule
  whose declaration matches the decoder's implementation exactly. A family may
  have several rules (VOP2 has `vop2_vgpr` and the VGPR-only `vop2_vgpr2`). For
  example `sop2_sgpr` is an SGPR destination `s0`-`s105`, two scalar sources
  (an SGPR, an inline integer -16 to 64, or a literal word) and at most one
  literal. A rule's `unsupported` list names the encodings it rejects, which
  stay unsupported instead of being guessed. The list must name exactly the
  encodings the decoder rejects.

Each `[[form]]` entry has exactly these fields. A missing or unknown field is an
error.

| Field | Meaning | Allowed values |
| --- | --- | --- |
| `name` | XML instruction name, the form's stable identity | upper-case, e.g. `S_ADD_U32` |
| `family` | Encoding family; must match the XML base encoding | e.g. `SOP2` |
| `opcode` | Opcode; must match the XML | non-negative integer |
| `variant` | Rust enum variant of the typed operation | CamelCase, e.g. `AddU32` |
| `operation` | The primitive the form uses | `add`, `sub`, `and`, `or`, `xor`, `andn2`, `orn2`, `nand`, `nor`, `xnor`, `lshl`, `lshr`, `ashr`, `min`, `max`, `cselect`, `bfm`, `mul`, `mul_hi`, `bfe`, `absdiff`, `lshl_add`, `mul24`, `mul_hi24` (vector only), `mov` (SOP1 and SOPK), `cmov` (SOPK only), `compare`, the SOP1 unaries, the VOP1 binary32 roundings `trunc`, `ceil`, `rndne` and `floor`, and the VOP1 integer to binary32 conversions `cvt_f32` (`signed` or `unsigned`) and `cvt_f32_ubyte0`-`3` (`unsigned`) |
| `signedness` | How operands are read | `unsigned`, `signed`, `bitwise`, `float` (`float` exactly for the binary32 roundings and the `vopc_vcc_f32` compares; always `bitwise` for the logical operations, `cselect` and `bfm`; never for the arithmetic ones, `add`, `sub`, `min`, `max`, `mul`, `mul_hi`, `bfe`, `absdiff` and `lshl_add`; shifts take their primitive's signedness) |
| `carry_in` | Implicit carry, borrow or select input | `none`, `scc` for unsigned scalar `add` and `sub` and required for scalar `cselect`, `vcc` for the Wave32 VOP2 select and carry forms |
| `width` | Operand and result width in bits (operand width for compares) | `32`, `16` with `vopc_vcc_half`, or `64` with `vopc_vcc_pair` |
| `source_bits` | Sign extension only: the low bits of S0 that are read. The XML must list S0 with that size and format (`FMT_NUM_I8` or `FMT_NUM_I16`) | `8`, `16`; required for `sext`, rejected elsewhere |
| `relation` | Compares only: the relation tested with S0 on the left | `eq`, `lg` (not equal; less or greater for floats), `gt`, `ge`, `lt`, `le`, and for VOPC only `f`, `t`; the float-only `o`, `u`, `nge`, `nlg`, `ngt`, `nle`, `neq`, `nlt` (#786); required for `compare`, rejected elsewhere |
| `source_order` | Vector forms only: `reversed` when the guide computes `op(S1, S0)` (the XML records no operand order); absent otherwise | `reversed` |
| `shift_amount` | `lshl_add` only: the fixed left shift of S0 before the add | `1` to `4`; required for `lshl_add`, rejected elsewhere |
| `shift_count_bits` | Shifts only: the low bits of the count source used as the count: S1, or S0 for a vector form with `source_order = "reversed"`. Scalar forms mask it in the generated evaluation, vector forms in their MSL template; the XML must list the count source as unsigned | `5` (log2 of `width`); required for shifts, rejected elsewhere |
| `scc` | What the form writes to SCC | `carry_out`, `borrow`, `signed_overflow`, `nonzero`, `relation` (SOPC and SOPK compares only), `selected_src0` (min and max: S0 won the strict compare, so a tie gives 0), `unchanged` |
| `vcc`, `exec` | Implicit effect on VCC and EXEC | `vcc` is `lane_mask` for [VOPC compares](vector-compare.md) and the three [VOP2 carry forms](vector-vcc.md). These write all of VCC_LO, zero inactive lane bits and leave VCC_HI unknown. Other forms declare `vcc` as `unchanged`. `exec` is `unchanged` for every form except the twelve [SOP1 EXEC-writing forms](sop1-saveexec.md), which declare `exec_save` or `exec_write`. Operand rules cover explicit destinations. |
| `wave` | Wave sizes the form is valid in | `wave32` for SOPP controls, VOPC compares, VOP1 integer, rounding and conversion forms and VCC-reading VOP2 forms in the Wave32 compute fixture ABI, `any` for other registry forms |
| `operands` | Name of an `[operand_rules]` entry for the same family | e.g. `sop2_sgpr` |
| `spec` | References `{ document, section, page }` | at least one `guide` reference with a page above 0 |
| `verify` | Evidence the form needs before it counts as verified | `reference`, `independent`, `metal`, `llvm_encoding` |

The `operation` and `signedness` pair picks one primitive, and that primitive
fixes the `scc` value:

| `operation` / `signedness` | Required `scc` | Primitive | Reads carry-in |
| --- | --- | --- | --- |
| `add` / `unsigned` | `carry_out` | `add_carry_u32` | allowed |
| `sub` / `unsigned` | `borrow` | `sub_borrow_u32` | allowed |
| `add` / `signed` | `signed_overflow` | `add_overflow_i32` | no |
| `sub` / `signed` | `signed_overflow` | `sub_overflow_i32` | no |
| `and`, `or`, `xor`, `andn2`, `orn2`, `nand`, `nor`, `xnor` / `bitwise` | `nonzero` | `and_nonzero_b32`, `or_nonzero_b32`, `xor_nonzero_b32`, `andn2_nonzero_b32`, `orn2_nonzero_b32`, `nand_nonzero_b32`, `nor_nonzero_b32`, `xnor_nonzero_b32` | no |
| `lshl` / `bitwise` | `nonzero` | `lshl_nonzero_b32` | no |
| `lshr` / `unsigned` | `nonzero` | `lshr_nonzero_b32` | no |
| `ashr` / `signed` | `nonzero` | `ashr_nonzero_i32` | no |
| `not`, `bcnt0`, `bcnt1` / `bitwise` | `nonzero` | `not_nonzero_b32`, `bcnt0_nonzero_b32`, `bcnt1_nonzero_b32` | no |
| `brev`, `ff0`, `ff1`, `flbit` / `bitwise` | `unchanged` | `brev_b32`, `ff0_b32`, `ff1_b32`, `flbit_b32` | no |
| `flbit`, `sext` / `signed` | `unchanged` | `flbit_i32`, `sext_i32` | no |
| `abs` / `signed` | `nonzero` | `abs_nonzero_i32` | no |
| `min`, `max` / `signed` or `unsigned` | `selected_src0` | `min_i32`/`min_u32`, `max_i32`/`max_u32` | no |
| `cselect` / `bitwise` | `unchanged` | `cselect_b32` | required |
| `bfm` / `bitwise` | `unchanged` | `bfm_b32` | no |
| `mul` / `signed` | `unchanged` | `mul_lo_i32` | no |
| `mul_hi` / `unsigned` or `signed` | `unchanged` | `mul_hi_u32`, `mul_hi_i32` | no |
| `bfe` / `unsigned` or `signed` | `nonzero` | `bfe_nonzero_u32`, `bfe_nonzero_i32` | no |
| `absdiff` / `signed` | `nonzero` | `absdiff_nonzero_i32` | no |
| `lshl_add` / `unsigned` | `carry_out` | `lshl_add_carry_u32` (with `shift_amount`) | no |
| `mov` / `signed` (SOPK only) | `unchanged` | `sext_simm16` | no |
| `cmov` / `signed` (SOPK only) | `unchanged` | `sext_simm16` | required |
| `compare` / `signed` or `unsigned` | `relation` | `eq_b32`, `ne_b32` (for both signednesses), `gt_i32`/`gt_u32`, `ge_*`, `lt_*`, `le_*` | no |

SOP1 unary forms share `ScalarUnaryOp`. The `sop1_sgpr` rule accepts
destination encodings 0–105. The `sop1_move` rule for `s_mov_b32` also accepts
106–107 (VCC halves) and 126–127 (EXEC halves) as explicit destinations.
Both rules accept source registers s0–s105, inline integers −16 through 64,
and one literal dword. The move copies the source and leaves SCC unchanged.
The destination vocabulary preserves the original decoder subset of the
RDNA 2 guide sections 5.3 and 12.3.

Binary vector forms (operand rules `vop2_vgpr` and `vop2_vgpr2`, kind `vector`; both
generate the one `VectorAluOp` enum, whose `operand_rule()` tells the decoder
which rule a form uses) use a separate table:
the pair picks the per-lane `VectorBinary` the MSL lowering uses, and `scc`
must be `unchanged` (these binary forms write no SCC, VCC or EXEC):

| `operation` / `signedness` | `VectorBinary` |
| --- | --- |
| `add` / `unsigned`, `sub` / `unsigned` | `Add`, `Sub` |
| `xnor` / `bitwise` | `Xnor` |
| `min`, `max` / `signed` or `unsigned` | `MinSigned`, `MinUnsigned`, `MaxSigned`, `MaxUnsigned` |
| `mul24`, `mul_hi24` / `signed` or `unsigned` | `Mul24Signed`, `Mul24Unsigned`, `MulHi24Signed`, `MulHi24Unsigned` |
| `and`, `or`, `xor` / `bitwise` | `And`, `Or`, `Xor` |
| `lshl`, `lshr` / `bitwise`, `ashr` / `signed` (with `shift_count_bits = 5`) | `ShiftLeft`, `ShiftRight`, `ArithmeticShiftRight` |

The [Wave32 VOP2 select and carry forms](vector-vcc.md) use `vop2_vcc` with
the general SRC0 domain and an implicit VCC input. The generator checks the
direction of each fieldless VCC operand in the pinned XML. The three arithmetic
forms also declare a lane-mask output and use the same reverse-source mechanism
as the no-carry subtraction.

`vop2_vgpr2` is the VGPR-only subset (SRC0 256-511) that the hand-written decoder
accepted for `v_and_b32`, `v_or_b32`, `v_xor_b32`, `v_lshlrev_b32`, `v_lshrrev_b32`
and `v_ashrrev_i32` before they moved into the registry (#215 M27); widening them
to `vop2_vgpr` would be a behaviour change and needs its own oracle. The three
shifts declare `source_order = "reversed"` (D = S1 shifted by S0[4:0]), so the
generator checks the XML's unsigned count format on SRC0, not VSRC1.

VOPC compares (kind `vector_compare`) write `vcc_lo`. The 32-bit rule
`vopc_vcc` accepts a `vector_source` as for VOP2 and a VGPR. The 16-bit rule
`vopc_vcc_half` reads the low half of the same source types and rejects
literals (guide sections 6.2, 6.2.5 and 6.8). The 64-bit rule
`vopc_vcc_pair` accepts a VGPR pair, an even normal SGPR pair or an inline
integer as SRC0 and a VGPR pair as VSRC1. The binary32 rule `vopc_vcc_f32`
(#786) is `vopc_vcc` without integer inline constants; its forms declare
`signedness = "float"` and accept NaN sources. All four declare a `relation`,
`scc = "unchanged"`, `vcc = "lane_mask"` and `wave = "wave32"`. The XML's implicit
64-bit VCC output must be present and is accounted for by `lane_mask`. Signedness
still matters for `eq` and `lg`, because the MSL lowering casts signed operands.
The VOP3A variants redirect the same Wave32 mask to SDST. EXEC-writing
compares (`v_cmpx_*`) and Wave64 stay unsupported. The SOP1 forms that write
EXEC are the exception: they declare `exec_save` or `exec_write`, and the
generator checks that their XML record has one implicit EXEC read and one
implicit EXEC write.

A SOPC compare must use an operand rule of the compare kind (`sopc_scalar`: two
sources, no destination), a unary operation one of the unary kind (`sop1_sgpr`:
one source and an SGPR destination), and a two-source ALU operation
`sop2_sgpr`.

SOPK forms (guide 12.2) use two rules. `sopk_sgpr` (kind `immediate`, enum
`ScalarImmediateOp`) has SDST `s0`-`s105` as the destination and SIMM16 as the one
source; it takes `mov`, `cmov` (reads SCC, so `carry_in = "scc"`), `add` (`D = D +
signext(SIMM16)`, `scc = "signed_overflow"`) and `mul` (`scc = "unchanged"`), all
`signed`, and the `add` and `mul` forms read the old SDST. `sopk_compare` (kind
`immediate_compare`, enum `ScalarImmediateCompareOp`) takes `compare` with SDST as
the left operand and the SIMM16 as the right one: sign-extended for `signed`,
zero-extended for `unsigned`. A SOPK compare's opcode must be 3 (`signed`) or 9
(`unsigned`) plus the relation's index in `eq, lg, gt, ge, lt, le` (guide 13.1.2).
The XML must give SIMM16 as 16-bit `FMT_NUM_I16` (`FMT_NUM_U16` for `unsigned`).
The word never takes a literal dword. The details are in
[scalar-immediate.md](scalar-immediate.md).

The SOPP control rules (AMD RDNA 2 ISA guide §§12.5 and 13.1.5) generate
`SoppControlOp` for the existing Wave32 compute subset. `sopp_end` accepts
`s_endpgm` with zero in SIMM16. `sopp_wait` accepts only `s_waitcnt vmcnt(0)`
(`0x3f70`), completing a pending vector load. `sopp_branch` accepts
`s_branch`, `s_cbranch_scc0`, `s_cbranch_vccz` and `s_cbranch_execz` with a
nonnegative signed SIMM16 displacement in dwords from the next instruction.
The decoder and compiler require a valid target boundary. The conditional
branches read SCC, VCC_LO or EXEC_LO without writing those registers.
Other SOPP forms, including `s_nop`, the nonzero branches, `s_sendmsg`,
`s_trap`, `s_sethalt` and `s_setprio`, remain unsupported.

Behaviour is chosen only from these declared fields, never from the mnemonic.

## What the validator checks

`validate` reports every error it finds, not only the first (a form with a missing field is not checked further). Besides the
vocabularies above, it rejects:

- contradictions: a declared `scc` or `carry_in` that the primitive does not
  have, `bitwise` on an arithmetic operation or the reverse;
- VOPC compares whose opcode is not the group base (128 and 192 for signed and
  unsigned 32-bit forms, 136 and 168 for 16-bit forms, 160 and 224 for 64-bit
  forms) plus the relation's offset in guide Table 59 (F 0, LT 1,
  EQ 2, LE 3, GT 4, LG/NE 5, GE 6, T 7); for the `float` forms the base 0
  plus the Table 61 offset (F 0, LT 1, EQ 2, LE 3, GT 4, LG 5, GE 6, O 7, U 8,
  NGE 9, NLG 10, NGT 11, NLE 12, NEQ 13, NLT 14, TRU 15); and a `lane_mask` form whose XML
  does not have exactly one implicit VCC operand that is written and not read;
- duplicates: the same `name`, the same `(family, opcode)`, or the same
  `variant` twice in one generated enum;
- XML disagreements: a family with no XML encoding, a form with no default XML
  variant, a different opcode, an SCC input or output the XML does not show (or
  that the declaration omits), an operand width other than `width`, a signed
  declaration on unsigned XML formats or the reverse (a shift count or the `bfe`
  field descriptor in S1 must be `FMT_NUM_U32` whatever the signedness), an operand direction that
  differs, and XML operands the operand rule does not cover;
- references: a spec citing an undeclared document, a bad section or page, or
  no guide page at all;
- operand rules the decoder does not implement, or whose declaration differs
  from the implemented one;
- wrong types: a float width, a boolean schema, an opcode or page outside
  0-65535, or a reserved variant name such as `Self`.

CI runs `generate_semantics.py --check`, `scripts/audit/semantic_coverage.py
--check` and both Python test suites next to the existing
`generate_rdna2_meta.py --check`. Both scripts stop with a clear message on
Python older than 3.11.

## What the generator does not do

- **The XML gives identity and encodings only.** The snapshot confirms the
  name, family, opcode, operand fields, widths, number formats and whether SCC
  is read or written. It says nothing about the arithmetic. Behaviour comes from
  the AMD guide, cited in `spec`, through the closed primitive table.
- **It does not invent behaviour.** A form whose operation, signedness or
  operand rule is not already implemented is rejected. Adding a new operation
  means hand-writing a primitive in `primitive.rs`, a variant in
  `semantics.rs` and a row in the generator's vocabulary tables, reviewed like
  any other code.
- **Undeclared forms stay unsupported.** The generated `from_word` returns
  `None` for every opcode not in the registry, and the decoder reports
  `UnsupportedOpcode`. Nothing falls back to a guess or a no-op.
- **It does not write tests or reference semantics.** The reference
  interpreter (`crates/ref`) and the independent interpreter
  (`tests/validation/independent_ref.rs`) never read the registry or the
  generated code, and their known-answer tests are written by hand from the
  guide. That independence is what lets them catch a wrong registry entry; keep
  it when you add a form. Agreement with these references is agreement with a
  reading of the public AMD guide, not with Xbox hardware.

## Commands

Run these from the repository root. `python3` must be 3.11 or newer.

```sh
# Check the registry only.
python3 crates/isa/generate_semantics.py validate

# Validate, then rewrite crates/isa/src/semantics_gen.rs.
python3 crates/isa/generate_semantics.py generate

# Validate and regenerate in memory; exit 1 if the committed file is stale. Never writes.
python3 crates/isa/generate_semantics.py --check

# Generator unit tests.
python3 -m unittest discover -s tests/semantics_generator

# The XML metadata tables the registry is cross-checked against are still current.
python3 crates/isa/generate_rdna2_meta.py --check

# Rust tests of the generated tables and the decoder that uses them.
cargo test --locked -p xenora-isa -p xenora-decoder

# Independent CPU validation.
cargo test --locked -p xenora-semantic-review

# Native GPU correctness on Apple silicon.
scripts/test-gpu.sh
```

The LLVM encoding corpus check needs an LLVM build with the AMDGPU target:

```sh
python3 crates/isa/tests/llvm/corpus.py --llvm-bin /path/to/llvm/bin --check
```

## Worked example: `s_addc_u32`

`s_addc_u32` adds two 32-bit sources plus SCC and writes the carry-out of
bit 31 to SCC (guide section 12.1, page 102; SCC rules in section 5.3, page 33).
Its registry entry is:

```toml
[[form]]
name = "S_ADDC_U32"
family = "SOP2"
opcode = 4
variant = "AddcU32"
operation = "add"
signedness = "unsigned"
carry_in = "scc"
width = 32
scc = "carry_out"
vcc = "unchanged"
exec = "unchanged"
wave = "any"
operands = "sop2_sgpr"
spec = [
  { document = "guide", section = "12.1", page = 102 },
  { document = "guide", section = "5.3", page = 33 },
]
verify = ["reference", "independent", "metal", "llvm_encoding"]
```

How each field is checked:

1. The XML snapshot has a default `ENC_SOP2` variant named `S_ADDC_U32` with
   opcode 4, so `name`, `family` and `opcode` agree.
2. That XML variant reads and writes SCC, which matches `carry_in = "scc"` and
   an `scc` other than `unchanged`.
3. `add` / `unsigned` selects `add_carry_u32`, which requires
   `scc = "carry_out"` and accepts a carry-in.
4. The XML `SDST`, `SSRC0` and `SSRC1` operands are 32-bit `FMT_NUM_U32`
   fields, which matches `width = 32`, `unsigned` and the `sop2_sgpr` rule.

From this entry the generator emits, in `semantics_gen.rs`, the `AddcU32`
variant of `ScalarAluOp`, its `from_word` arm (`0x8200_0000`), the mnemonic
`s_addc_u32`, `reads_scc() == true`, an `evaluate` arm that calls
`primitive::add_carry_u32(src0, src1, carry_in)`, and a `FormSemantics` row.

To add a form the same way:

1. Confirm its behaviour in the AMD guide and note the section and page.
2. Check that an existing primitive and operand rule cover it. If not, the
   form needs hand-written code first (see above).
3. Add a `[[form]]` entry and run `validate` until it reports no errors.
4. Run `generate` and commit the registry and `semantics_gen.rs` together.
5. Add known-answer tests to the reference and independent interpreters by
   hand, then run every command in [Commands](#commands).
