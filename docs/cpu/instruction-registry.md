# x86-64 instruction registry

The static PE probe, the interpreter and the native ARM64 dynamic binary translator (DBT) decode guest code through one function, `x86_decode::decode`. Its tables are generated from `crates/loader/x86/instructions.toml`.

| Part | File | Role |
| --- | --- | --- |
| Registry | `crates/loader/x86/instructions.toml` | Opcode map, opcode, ModRM digit, operand encoding, size, immediate, flag effects and public reference. Data only. |
| Generator | `crates/loader/generate_x86.py` | `validate`, `generate` and `--check`. Rejects overlapping encodings, opcodes that are prefixes, contradictory operand rules, a fixed r/m field on a form that could take memory, unknown vocabulary and flags both written and undefined. |
| Tables | `crates/loader/src/x86_table.rs` | Generated static tables. No metadata is parsed at run time. |
| Semantics | `crates/loader/src/x86_exec.rs`, `x86_simd.rs`, `x86_strings.rs`, `x86_avx.rs`, `x86_float.rs`, `softfloat.rs` | Rust primitives for integer, vector and floating-point forms. Every instruction is all-or-nothing. |
| Lowering | `crates/loader/src/dbt.rs` | ARM64 for the forms `is_native` accepts. Every other form calls the shared semantics and is counted in `DbtMetrics::interpreter_fallbacks`. |

## Adding a form

1. Add the form, and its operation if new, to the registry with an Intel SDM or AMD APM reference. A form that is one exact ModRM byte, not a whole digit, takes `register_only = true` and `rm = N`.
2. Run `python3 crates/loader/generate_x86.py generate`.
3. Implement the operation in `x86_exec.rs`. Leave flags the manuals call undefined unchanged.
4. Add assembly text to `crates/loader/tests/x86/assemble.py`, run it with `generate`, and extend the reference in `tests/x86_integer.rs` or `tests/x86_vector.rs`. A declared form without a fixture fails `every_declared_form_is_executed_by_a_fixture`.
5. Lower it natively only if the ARM64 result and flags match. The same tests require native forms never to call the helper, and helper forms always to.

A declared form is decodable, not implemented. Count a form as supported only when the tests above cover it.

## Independence

The references in `tests/x86_integer.rs` and `tests/x86_vector.rs` read only LLVM assembly text. They do not use the encodings, registry, decoder or production semantics, and work bit by bit or on byte arrays. They were written by the same contributor as the implementation, so independent review is still required.

## Guest flags

The DBT stores CF, ZF, SF, OF, PF and AF in `CpuState::flags`. It skips the store only when a later instruction in the block overwrites all six before anything can read them, fault or leave the block.

FS and GS overrides together are architecturally undefined; GS applies, in both engines.

## Vector state

XMM registers live in `CpuState::xmm`, lane 0 in the low bits, and MXCSR in `CpuState::mxcsr`. A mandatory 0x66, F3 or F2 prefix selects a form; in the 0F 38 and 0F 3A opcode maps every form needs 0x66, over which F3 and F2 take precedence, except CRC32, which needs F2 and takes 0x66 as an operand size; the generator rejects an opcode where 0x66 could be both a mandatory prefix and an operand size. A 16-byte memory operand must be aligned unless its form says otherwise, and a violation exits with `GeneralProtection` before any memory access. Non-temporal stores are ordinary stores.

## VEX-encoded instructions

C4 and C5 always start a VEX prefix in 64-bit mode. The registry lists VEX forms as `[[vex]]` entries with their vector length, W, VEX.vvvv use and memory width. A form shares its operation with a legacy form that has the same lanes, so `Insn::vex` tells them apart, and `x86_avx.rs` executes it. Rules, from the Intel SDM Vol. 1, 14.1 and Vol. 2A, 2.3 and 2.4:

- A VEX-encoded instruction clears its destination above the vector length. A legacy SSE instruction keeps bits 255:128, which live in `CpuState::ymm_high`.
- Memory operands may have any alignment. VMOVAPS, VMOVAPD, VMOVDQA, VMOVNTPS, VMOVNTPD, VMOVNTDQ and VMOVNTDQA need that of the operand, 16 or 32 bytes.
- 66, F2, F3, LOCK or REX before VEX, an opcode map other than 1 to 3, a vector length or W without a form, and VEX.vvvv other than 1111b on a form without that operand are illegal instructions. A REX that a segment override makes ineffective is refused too.
- Scalar forms ignore VEX.L, where the manual says so.

Implemented: VMOVAPS, VMOVAPD, VMOVUPS, VMOVUPD, VMOVDQA, VMOVDQU, VLDDQU, VMOVNTPS, VMOVNTPD, VMOVNTDQ, VMOVNTDQA, VMOVSS, VMOVSD, VMOVD, VMOVQ, VMOVLPS, VMOVLPD, VMOVHPS, VMOVHPD, VMOVHLPS, VMOVLHPS, VMOVMSKPS, VMOVMSKPD, VMOVDDUP, VMOVSLDUP, VMOVSHDUP, bitwise logic, VBROADCASTSS, VBROADCASTSD, VBROADCASTF128, VINSERTF128, VEXTRACTF128, VPERM2F128, VPERMILPS, VPERMILPD, VLDMXCSR, VSTMXCSR, VZEROUPPER, VZEROALL, and the VEX form of every other legacy instruction in the registry that has one. AVX2 adds 256-bit forms for packed integer arithmetic, comparisons, min/max, saturation, packing, unpacking, horizontal arithmetic, absolute value and byte shuffles. These operations process each 128-bit half separately. The scalar BMI1 and BMI2 bit-manipulation forms ANDN, BLSI, BLSMSK, BLSR, SHLX, SHRX, SARX and RORX are implemented in both operand widths, with the flag rules of the manual (the shifts and RORX write no flags; BLSMSK clears ZF). Refused are VRCPPS, VRSQRTPS and their scalar forms, VHADDPS, VHSUBPS, VADDSUBPS and their double forms, VTESTPS, VTESTPD, VMASKMOVPS, VMASKMOVPD, VMASKMOVDQU, AES, PCLMULQDQ, AVX2 gathers, masked moves, variable shifts and cross-lane permutations, FMA, F16C and the remaining BMI forms BEXTR, BZHI, PDEP, PEXT and MULX.

- A 256-bit form of a legacy operation computes both halves as that operation does. The exceptions of both halves are collected before any is reported. VDPPS computes the low half first, as the operation section of the Intel SDM lists them, and stops there on an unmasked exception.
- Scalar forms take the bits above the result from VEX.vvvv.
- The VEX compares have 32 predicates. Their reserved immediate bits 7:5 are ignored, as are bits 7:3 in the legacy forms, which is a local rule.
- VBLENDVPS, VBLENDVPD and VPBLENDVB name the selecting register in bits 7:4 of the immediate and ignore bits 3:0.
- A shift by an immediate writes the register of VEX.vvvv.
- VEX.W selects RAX and RDX as the lengths of VPCMPESTRI and VPCMPESTRM.
- The scalar forms accept either VEX.L. The manual marks them as ignoring it, and advises against setting it.

Every VEX form runs on the shared semantics in both engines. CPUID reports neither AVX nor OSXSAVE, and XGETBV is refused. An exception context holds the low halves only, like the Windows `CONTEXT` without extended state, so restoring one leaves the upper halves as they are.

`tests/x86_avx.rs` covers the rules of encodings, and runs the forms of `tests/x86/avx.txt` against a reference that works on byte arrays from assembly text. `tests/x86_avx_forms.rs` runs those of `avx_forms.txt` against the references and committed vectors of the legacy forms.

## String compares

`x86_strings.rs` implements PCMPESTRI, PCMPESTRM, PCMPISTRI and PCMPISTRM by the steps of the Intel SDM Vol. 2B, 4.1. Their memory operand may have any alignment. REX.W selects RAX and RDX instead of EAX and EDX as the explicit lengths. Immediate bit 7 is reserved and ignored, which is a local rule. The reference in `tests/x86_vector.rs` states each aggregation as an operation on strings instead.

## Floating point

`softfloat.rs` follows the Intel SDM (Vol. 1, 4.8, 4.9 and 11.5) without host floating point. Expected values come from exact fractions in `tests/x86/float_reference.py`. Some behaviour is implemented one way but not asserted, because the sections used leave it open or it was not found there:

- a denormal dividend over zero raises divide-by-zero only, not also denormal-operand;
- flush-to-zero also flushes a tiny result that is exact, raising underflow and precision;
- DAZ applies before conversion to an integer.

An unmasked overflow records precision only when rounding with an unbounded exponent is inexact (Intel SDM Vol. 1, D.4.2.3, Table D-16). The known-answer cases in `tests/cpu_known_answers.rs` assert this.

DPPS and DPPD raise the exceptions of all multiplications, then those of each addition before the next, which is where the operation sections of the Intel SDM call the handler, so an intermediate result can raise a denormal-operand exception or be zeroed by DAZ. `tests/x86/dot_vectors.txt` covers all 256 immediates of both from exact fractions. An addition of two different NaNs, and multiplications that would stop differently one by one, are not asserted.

None of it has been compared with x86 hardware.

## Locked instructions

XADD and CMPXCHG exist for 8-, 16-, 32- and 64-bit register and memory destinations. CMPXCHG8B and CMPXCHG16B exist for 8- and 16-byte memory operands. The LOCK prefix (F0) is accepted on exactly the forms the registry marks, and only when the destination is memory: ADD, ADC, AND, DEC, INC, NEG, NOT, OR, SBB, SUB, XOR, XCHG, XADD and CMPXCHG at every width, CMPXCHG8B and CMPXCHG16B, and BTC, BTR and BTS with an immediate. That is the list the Intel SDM gives (Vol. 2, "LOCK"), without bit tests of memory by a register index, which are not implemented. LOCK anywhere else is an illegal instruction, as the SDM specifies for a register destination or an instruction that cannot be locked.

These rules hold for both engines and are what a guest scheduler has to preserve:

- **Ordering.** Each engine runs one guest instruction at a time on one host thread, and an instruction either completes or changes nothing. A locked instruction is therefore indivisible and totally ordered with every other guest access, and LOCK changes no result: the locked and unlocked encodings leave identical state. A scheduler that switches guest threads only between instructions keeps this. One that runs guest threads on several host threads does not, and must first make each locked instruction atomic against every access to its operand, with x86's ordering rather than ARM64's weaker one. Nothing here provides or tests that.
- **Alignment.** A locked operand may have any alignment and may span two mapped regions. The exception is CMPXCHG16B, which needs a 16-byte aligned operand with or without LOCK and is a general-protection fault otherwise (Intel SDM Vol. 2A, CMPXCHG8B/CMPXCHG16B).
- **Compare failure.** CMPXCHG compares the accumulator with the destination as CMP would and sets all six status flags from it. On a match it stores the source and leaves the accumulator alone. On a mismatch it loads the destination into the accumulator and stores the destination's old value back, so the destination is written either way and a mismatch on memory that cannot be written still faults. With a 32-bit register destination both stores clear the upper half of that register, following the SDM's operation text.
- **Paired compare.** CMPXCHG8B (`0F C7 /1`) compares EDX:EAX with an 8-byte operand. CMPXCHG16B, the same form with REX.W, compares RDX:RAX with a 16-byte one. Only ZF is written. On a match the instruction stores ECX:EBX or RCX:RBX and writes no register. On a mismatch it loads the operand into the pair and writes the operand back. A 32-bit load into EDX:EAX clears bits 63:32 of RAX and RDX. A register operand is an illegal instruction. CPUID reports both instructions (CX8 and CMPXCHG16B).
- **Faults.** If any byte of the operand cannot be read or written, the instruction faults before memory, registers or flags change.
- **Lowering.** The DBT runs every locked form on the shared semantics, also where it lowers the unlocked form to ARM64. They are not lowered to ARM64 atomic instructions, which could only be judged against concurrent guest threads.

The SDM cases for these forms in `tests/cpu_known_answers.rs` were written before this implementation, by another contributor. `tests/x86_atomic.rs` adds the LOCK rules, both compare outcomes at every width, unaligned operands and faults. It also holds hand-derived results and flags for every other lockable form at every width (#451); those, like the implementation, are by one contributor. `tests/x86_cmpxchg_pair.rs` covers CMPXCHG8B and CMPXCHG16B (#971): each half of the compare, both outcomes with and without LOCK, addressing and segment bases, alignment, noncanonical addresses, faults on read-only and unmapped bytes, and a retry loop. The implementer also derived those expectations. None of it has been compared with x86 hardware, and nothing here establishes multi-thread behaviour.

## Memory ordering and fences

Guest memory accesses are totally ordered today. Each engine runs one guest instruction at a time on one host thread, and every load and store of an instruction completes before the next instruction starts. There is no store buffer and nothing is reordered, so a load always sees the latest earlier store. That is stronger than the ordering the Intel SDM (Vol. 3A, "Memory Ordering") gives a real processor, where a store may sit in a buffer while later loads from other locations run.

MFENCE, LFENCE and SFENCE (`0F AE` with a register ModRM byte and digit 6, 5 or 7; the SDM states that the r/m field is ignored) are therefore accepted and change no register, flag or memory. They have nothing left to enforce. Non-temporal stores are ordinary stores, so SFENCE has no weakly ordered store to wait for either. The memory forms that share those digits, the XSAVE family and CLFLUSH, stay rejected.

This holds only while guest threads do not run on several host threads. ARM64 orders memory more weakly than x86, so an engine that ran guest threads in parallel would have to restore x86's ordering for ordinary accesses and give the fences a real effect. Nothing here provides or tests that. `tests/cpu_known_answers.rs` holds the single-thread cases: a store, a fence and a later load in program order, and no change to any other state. None of it has been compared with x86 hardware.

## x87 state and FNINIT

FNINIT (`DB E3`) is the only x87 instruction. It exists because the mingw-w64 C runtime runs it once before `main`, in `_fpreset`, and a program built with that runtime could not start without it.

`CpuState::x87` holds the control, status and tag words. FNINIT sets them to the values the Intel SDM gives (Vol. 2A, "FINIT/FNINIT"): control `037F` (every exception masked, 64-bit precision, round to nearest), status `0` and tag `FFFF` (every register empty). It changes nothing else, and both engines run the same shared code for it; nothing is lowered to ARM64. The host's floating-point state is never read or written.

What this does not include:

- **No other x87 instruction.** FNSTCW, FLDCW, FNCLEX, FWAIT (so FINIT, which is `9B DB E3`), every load, store and arithmetic form stay rejected. Nothing can read the three words back, so today they are only visible to a host that inspects `CpuState`.
- **No data registers.** FNINIT leaves them unchanged on a real processor and tags them empty; Xenora has none. The x87 instruction and data pointers and the last opcode, which FNINIT also clears, are not modelled either.
- **No operating-system start value.** A new `CpuState` holds the FNINIT values. An operating system may start a thread with a different control word, which is not modelled.
- **CPUID still reports no FPU.** One instruction of the feature is not the feature.

The registry key `rm = 3` makes the form match that one ModRM byte. The other register bytes of `DB /4` are different instructions (`DB E2` is FNCLEX), so the form cannot ignore the r/m field the way the fences do. `fninit_is_the_only_x87_encoding_accepted` in `tests/x86_integer.rs` tries every ModRM byte under the eight x87 opcodes, `D8` to `DF`, in both engines. A segment override or REX prefix before FNINIT is accepted and has no effect, as for other forms without operands. `prefixes_accepted_before_fninit_have_no_effect` pins that as a local rule; it was not derived from a manual statement. The 0x66, F2, F3 and LOCK prefixes are refused. Nothing here has been compared with x86 hardware.

## String instructions

MOVS, STOS and LODS exist for 8-, 16-, 32- and 64-bit elements, alone and with REP (F3). So do the string compares SCAS and CMPS, alone, with REPE (F3) and with REPNE (F2). The direction flag exists, with CLD and STD. `CpuState::df` holds the flag. It is not one of the six status flags the registry tracks.

- **Operations.** MOVS copies `[RSI]` to `[RDI]`. STOS stores the accumulator at `[RDI]`. LODS loads `[RSI]` into AL, AX, EAX or RAX; a 32-bit load clears bits 63:32 and the narrower ones keep the bits above them. SCAS compares the accumulator with `[RDI]`, and CMPS compares `[RSI]` with `[RDI]`, source minus destination. Each compare sets the six status flags as CMP would. MOVS, STOS and LODS change no flag.
- **One element per step.** A repeated form handles one element, decrements RCX and, while RCX is not zero, leaves RIP on the instruction. REPE also stops after an element that clears ZF, and REPNE after one that sets it (Intel SDM Vol. 2B, "REP/REPE/REPZ/REPNE/REPNZ"). Each element is a step of its own in both engines and counts against the step budget, so a repeat cannot outrun the budget and can stop part-way and resume. A zero count touches no memory, no register and no flag. Without a repeat prefix, RCX is not read. Step counts therefore count elements, not instructions, for a repeated form.
- **Direction.** RSI and RDI move up by the element size when the flag is clear and down when it is set. Each element is read whole and then written, so overlapping ranges behave as that order dictates.
- **Faults.** An element completes or changes nothing. A fault keeps the elements already done, leaves RSI, RDI and RCX at the element that faulted, and leaves RIP on the instruction. A repeated CMPS or SCAS that faults also gets back the status flags it began with, as Intel SDM Vol. 2B (REP) describes for REPE and REPNE. `CpuState::repeat_flags` keeps them while the repeat runs. It is not architectural state: it exists so that a step budget that splits a repeat cannot change the result.
- **Segments.** An FS or GS override applies to the `[RSI]` source of MOVS, LODS and CMPS. `[RDI]`, and so STOS and SCAS as a whole, take no override.
- **Prefixes.** REPNE (F2) is accepted only on SCAS and CMPS. F2 together with F3, LOCK and the address-size override that would select ECX, ESI and EDI are rejected on every string form.
- **Lowering.** The DBT runs these forms on the shared semantics, one helper call per element, and a string instruction ends its translated block. There is no bulk-copy path, so large copies are slow.

The SDM cases for these forms in `tests/cpu_known_answers.rs` were written before this implementation, by another contributor. `tests/cpu_rep.rs` adds a byte-array model for MOVS and STOS over every form, direction and count, the step accounting, faults and the segment rule. `tests/cpu_string_compare.rs` does the same for LODS, SCAS and CMPS (#988) with every repeat prefix. Its model derives the flags from 128-bit arithmetic, arranges the data so that a repeat stops at a chosen element, and also covers `strlen` and `memcmp` idioms. The implementer wrote both the model and the implementation. None of it has been compared with x86 hardware.

## Guest CPUID

CPUID (`0F A2`) returns a fixed table that never reads the host. It describes a synthetic processor: the vendor string is `Xenora guest`, not Intel's or AMD's, and leaf 1 reports no family, model or stepping.

| Leaf (EAX) | EAX | EBX | ECX | EDX |
| --- | --- | --- | --- | --- |
| 0 | 1, the highest basic leaf | `Xeno` | `uest` | `ra g` |
| 1 | 0 | 0 | CMPXCHG16B (bit 13), POPCNT (bit 23) | CX8 (bit 8), CMOV (bit 15) |
| 0x8000_0000 | 0x8000_0001, the highest extended leaf | 0 | 0 | 0 |
| 0x8000_0001 | 0 | 0 | LAHF/SAHF (bit 0), LZCNT (bit 5) | long mode (bit 29) |
| any other | 0 | 0 | 0 | 0 |

A feature bit is set only when every instruction of that feature is implemented. That is stricter than "some of it works", and it has consequences:

- SSE and SSE2 are not reported, although most of their instructions exist, because parts are still rejected: RSQRTPS and MASKMOVDQU, for example. SSSE3 is not reported although all its XMM forms exist, because its MMX forms do not, and SSE4.1 and SSE4.2 are not although every one of their instructions exists, because software checks others first (Intel SDM Vol. 1, 12.12.2 and 12.12.3): SSE, SSE2, SSE3 and SSSE3 before SSE4.1, and SSE, SSE2, SSSE3 and SSE4.1 before the SSE4.2 string compares and PCMPGTQ. CRC32 needs only the SSE4.2 bit, which covers the whole feature and so stays clear. POPCNT has a feature bit of its own, which is reported; Intel also has an application check SSE4.2 before it, so a guest that does will not use it. BMI1 is not reported although TZCNT, ANDN, BLSI, BLSMSK and BLSR exist, because BEXTR does not; BMI2 is not either, because BZHI, PDEP, PEXT and MULX do not. The x87 FPU is not reported although FNINIT exists. MMX, FXSAVE, the time-stamp counter and SYSRET are absent too.
- The table is therefore truthful about Xenora, not plausible as hardware: every real 64-bit processor reports the FPU and SSE2. A guest that tests a bit before using a feature avoids it; a guest that assumes SSE2 because it runs in 64-bit mode still gets every form that exists and an illegal-instruction exit for one that does not.
- A leaf that is not listed returns zeros, whatever the subleaf. Intel processors answer a leaf above the highest basic one with that leaf's data instead; software is expected to read the highest leaf first.

`xenora_loader::cpu::cpuid` is the one copy of the table. A runtime service that reports processor features should read it rather than keep its own list.

`tests/cpu_features.rs` pins every leaf in both engines. For each reported feature it requires the feature's whole instruction set among the fixtures that `tests/x86_integer.rs` executes. The exceptions are CMPXCHG8B and CMPXCHG16B: that reference does not model their 8- and 16-byte operands, so `tests/x86_cmpxchg_pair.rs` and the SDM known answers run them instead. For each feature software commonly tests that is not reported, it holds one instruction of that feature that both engines must refuse, or, for SSE4.1 and SSE4.2, the instruction list and the prerequisites that are not reported. To report a new feature: implement all of its instructions, remove its witness, add its instruction list to the reported set, then set the bit. The pairing of instructions with feature bits follows the Intel SDM; nothing was compared with a processor.

## Rejected on purpose

LOCK except as described above, F2 or F3 where it is neither PAUSE nor part of a declared form, F2 combined with F3, address-size overrides, an ignored segment override after FS or GS, 16-bit stack and branch forms, 16-bit BSWAP and MOVSXD, bit tests of memory by a register index, REPNE on MOVS, STOS and LODS, every x87 instruction except FNINIT, MMX, the RCP and RSQRT approximations, SSE3, the MMX forms of SSSE3, SSE4a, AES, PCLMULQDQ, SHA, MOVBE, VEX except as described above, EVEX and XOP. The DBT also refuses raw SYSCALL.


## CPU semantic expansion audit

The first AVX2 batch widens 73 existing registry rows from 128-bit-only to
128- and 256-bit operands. It adds 359 assembly cases. Opcode-slot counts
stay constant because the vector-length predicate belongs to an existing row.
The current CPU inventory omits that predicate, so its slot total alone cannot
measure this expansion.

Identification and decoding follow the generated VEX table. Interpreter and
dynamic binary translator execution use the same software vector operations.
All VEX forms use the DBT helper, with no direct ARM64 NEON lowering.
Assembly-text references and explicit signed-multiply boundary values check
architectural results separately from execution. Independent acceptance of
the full expanded set remains pending.

The remaining extension work has these implementation dependencies.

- Gather needs vector-index SIB decoding and restartable per-lane destination
  and mask updates on faults.
- Masked loads and stores need enabled-element address and permission checks.
- Per-element variable shifts and whole-YMM permutations need dedicated
  operations. Existing F128 bitwise lane movement can serve integer lane moves.
- FMA3 needs an exact product-plus-add primitive with one final rounding,
  operand permutations and instruction-specific MXCSR exception handling.
- F16C needs binary16 conversion and its DAZ, FTZ and rounding-control rules.
- BMI1 now has TZCNT, ANDN, BLSI, BLSMSK and BLSR, and BMI2 has SHLX,
  SHRX, SARX and RORX, all with their architectural flag rules. BEXTR and
  BZHI need the same scalar VEX dispatch with one further operand arrangement;
  PDEP, PEXT and MULX need their own bit-level primitives.
- XSAVE, XRSTOR and XGETBV need a coherent guest XCR0 policy, CPUID state
  enumeration, save-area validation and extended exception-context handling.

CPUID continues to withhold these incomplete feature groups. No speedup is
claimed. Direct host lowering requires measurements and semantic equivalence.
The architectural reference is the public [Intel instruction-set manual](https://cdrdv2-public.intel.com/843829/325383-sdm-vol-2abcd-dec-24.pdf).
