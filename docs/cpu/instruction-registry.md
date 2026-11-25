# x86-64 instruction registry

The static PE probe, the interpreter and the native ARM64 dynamic binary translator (DBT) decode guest code through one function, `x86_decode::decode`. Its tables are generated from `crates/loader/x86/instructions.toml`.

| Part | File | Role |
| --- | --- | --- |
| Registry | `crates/loader/x86/instructions.toml` | Opcode map, opcode, ModRM digit, operand encoding, size, immediate, flag effects and public reference. Data only. |
| Generator | `crates/loader/generate_x86.py` | `validate`, `generate` and `--check`. Rejects overlapping encodings, opcodes that are prefixes, contradictory operand rules, unknown vocabulary and flags both written and undefined. |
| Tables | `crates/loader/src/x86_table.rs` | Generated static tables. No metadata is parsed at run time. |
| Semantics | `crates/loader/src/x86_exec.rs`, `x86_simd.rs`, `x86_float.rs`, `softfloat.rs` | Rust primitives for integer, vector and floating-point forms. Every instruction is all-or-nothing. |
| Lowering | `crates/loader/src/dbt.rs` | ARM64 for the forms `is_native` accepts. Every other form calls the shared semantics and is counted in `DbtMetrics::interpreter_fallbacks`. |

## Adding a form

1. Add the form, and its operation if new, to the registry with an Intel SDM or AMD APM reference.
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

XMM registers live in `CpuState::xmm`, lane 0 in the low bits, and MXCSR in `CpuState::mxcsr`. A mandatory 0x66, F3 or F2 prefix selects a form; the generator rejects an opcode where 0x66 could be both a mandatory prefix and an operand size. A 16-byte memory operand must be aligned unless its form says otherwise, and a violation exits with `GeneralProtection` before any memory access. Non-temporal stores are ordinary stores.

## Floating point

`softfloat.rs` follows the Intel SDM (Vol. 1, 4.8, 4.9 and 11.5) without host floating point. Expected values come from exact fractions in `tests/x86/float_reference.py`. Some behaviour is implemented one way but not asserted, because the sections used leave it open or it was not found there:

- a denormal dividend over zero raises divide-by-zero only, not also denormal-operand;
- flush-to-zero also flushes a tiny result that is exact, raising underflow and precision;
- DAZ applies before conversion to an integer.

An unmasked overflow records precision only when rounding with an unbounded exponent is inexact (Intel SDM Vol. 1, D.4.2.3, Table D-16). The known-answer cases in `tests/cpu_known_answers.rs` assert this.

None of it has been compared with x86 hardware.

## Locked instructions

XADD and CMPXCHG exist for 8-, 16-, 32- and 64-bit register and memory destinations. The LOCK prefix (F0) is accepted on exactly the forms the registry marks `lock = true`, and only when the destination is memory. Today those are the memory forms of XADD and CMPXCHG. LOCK anywhere else is an illegal instruction, as the Intel SDM (Vol. 2, "LOCK") specifies for a register destination or an instruction that cannot be locked. LOCK on the other instructions the SDM allows (ADD, ADC, AND, BTC, BTR, BTS, DEC, INC, NEG, NOT, OR, SBB, SUB, XOR and XCHG), CMPXCHG8B and CMPXCHG16B are not implemented and stay rejected.

These rules hold for both engines and are what a guest scheduler has to preserve:

- **Ordering.** Each engine runs one guest instruction at a time on one host thread, and an instruction either completes or changes nothing. A locked instruction is therefore indivisible and totally ordered with every other guest access, and LOCK changes no result: the locked and unlocked encodings leave identical state. A scheduler that switches guest threads only between instructions keeps this. One that runs guest threads on several host threads does not, and must first make each locked instruction atomic against every access to its operand, with x86's ordering rather than ARM64's weaker one. Nothing here provides or tests that.
- **Alignment.** A locked operand may have any alignment and may span two mapped regions.
- **Compare failure.** CMPXCHG compares the accumulator with the destination as CMP would and sets all six status flags from it. On a match it stores the source and leaves the accumulator alone. On a mismatch it loads the destination into the accumulator and stores the destination's old value back, so the destination is written either way and a mismatch on memory that cannot be written still faults. With a 32-bit register destination both stores clear the upper half of that register, following the SDM's operation text.
- **Faults.** If any byte of the operand cannot be read or written, the instruction faults before memory, registers or flags change.
- **Lowering.** The DBT runs these forms on the shared semantics. They are not lowered to ARM64 atomic instructions, which could only be judged against concurrent guest threads.

The SDM cases for these forms in `tests/cpu_known_answers.rs` were written before this implementation, by another contributor. `tests/x86_atomic.rs` adds the LOCK rules, both compare outcomes at every width, unaligned operands and faults. None of it has been compared with x86 hardware, and nothing here establishes multi-thread behaviour.

## Memory ordering and fences

Guest memory accesses are totally ordered today. Each engine runs one guest instruction at a time on one host thread, and every load and store of an instruction completes before the next instruction starts. There is no store buffer and nothing is reordered, so a load always sees the latest earlier store. That is stronger than the ordering the Intel SDM (Vol. 3A, "Memory Ordering") gives a real processor, where a store may sit in a buffer while later loads from other locations run.

MFENCE, LFENCE and SFENCE (`0F AE` with a register ModRM byte and digit 6, 5 or 7; the SDM states that the r/m field is ignored) are therefore accepted and change no register, flag or memory. They have nothing left to enforce. Non-temporal stores are ordinary stores, so SFENCE has no weakly ordered store to wait for either. The memory forms that share those digits, the XSAVE family and CLFLUSH, stay rejected.

This holds only while guest threads do not run on several host threads. ARM64 orders memory more weakly than x86, so an engine that ran guest threads in parallel would have to restore x86's ordering for ordinary accesses and give the fences a real effect. Nothing here provides or tests that. `tests/cpu_known_answers.rs` holds the single-thread cases: a store, a fence and a later load in program order, and no change to any other state. None of it has been compared with x86 hardware.

## String instructions

MOVS and STOS exist for 8-, 16-, 32- and 64-bit elements, alone and with REP (F3), together with the direction flag and CLD and STD. `CpuState::df` holds the flag. It is not one of the six status flags the registry tracks.

- **One element per step.** A REP form moves one element, decrements RCX and, while RCX is not zero, leaves RIP on the instruction. Each element is a step of its own in both engines and counts against the step budget, so a repeat cannot outrun the budget and can stop part-way and resume. A zero count touches no memory and no register. Without REP, RCX is not read. Step counts therefore count elements, not instructions, for a REP form.
- **Direction.** RSI and RDI move up by the element size when the flag is clear and down when it is set. Each element is read whole and then written, so overlapping ranges behave as that order dictates.
- **Faults.** An element completes or changes nothing. A fault keeps the elements already done, leaves RSI, RDI and RCX at the element that faulted, and leaves RIP on the instruction.
- **Segments.** An FS or GS override applies to the MOVS source. The destination, and STOS as a whole, take no override.
- **Lowering.** The DBT runs these forms on the shared semantics, one helper call per element, and a string instruction ends its translated block. There is no bulk-copy path, so large copies are slow.

The SDM cases for these forms in `tests/cpu_known_answers.rs` were written before this implementation, by another contributor. `tests/cpu_rep.rs` adds a byte-array model over every form, direction and count, the step accounting, faults and the segment rule. None of it has been compared with x86 hardware.

## Guest CPUID

CPUID (`0F A2`) returns a fixed table that never reads the host. It describes a synthetic processor: the vendor string is `Xenora guest`, not Intel's or AMD's, and leaf 1 reports no family, model or stepping.

| Leaf (EAX) | EAX | EBX | ECX | EDX |
| --- | --- | --- | --- | --- |
| 0 | 1, the highest basic leaf | `Xeno` | `uest` | `ra g` |
| 1 | 0 | 0 | POPCNT (bit 23) | CMOV (bit 15) |
| 0x8000_0000 | 0x8000_0001, the highest extended leaf | 0 | 0 | 0 |
| 0x8000_0001 | 0 | 0 | LZCNT (bit 5) | long mode (bit 29) |
| any other | 0 | 0 | 0 | 0 |

A feature bit is set only when every instruction of that feature is implemented. That is stricter than "some of it works", and it has consequences:

- SSE and SSE2 are not reported, although most of their instructions exist, because parts are still rejected: RSQRTPS and MASKMOVDQU, for example. BMI1 is not reported although TZCNT exists, because ANDN does not. The x87 FPU, MMX, FXSAVE, CMPXCHG8B, the time-stamp counter, LAHF and SYSRET are absent too.
- The table is therefore truthful about BetterXen, not plausible as hardware: every real 64-bit processor reports the FPU and SSE2. A guest that tests a bit before using a feature avoids it; a guest that assumes SSE2 because it runs in 64-bit mode still gets every form that exists and an illegal-instruction exit for one that does not.
- A leaf that is not listed returns zeros, whatever the subleaf. Intel processors answer a leaf above the highest basic one with that leaf's data instead; software is expected to read the highest leaf first.

`xenora_loader::cpu::cpuid` is the one copy of the table. A runtime service that reports processor features should read it rather than keep its own list.

`tests/cpu_features.rs` pins every leaf in both engines. For each reported feature it requires the feature's whole instruction set among the fixtures that `tests/x86_integer.rs` executes. For each feature software commonly tests that is not reported, it holds one instruction of that feature that both engines must refuse. To report a new feature: implement all of its instructions, remove its witness, add its instruction list to the reported set, then set the bit. The pairing of instructions with feature bits follows the Intel SDM; nothing was compared with a processor.

## Rejected on purpose

LOCK except as described above, F2 or F3 where it is neither PAUSE nor part of a declared form, F2 combined with F3, address-size overrides, an ignored segment override after FS or GS, 16-bit stack and branch forms, 16-bit BSWAP and MOVSXD, bit tests of memory by a register index, LODS, CMPS, SCAS, REPE and REPNE, MMX, the RCP and RSQRT approximations, SSE3 and later, VEX, CMPXCHG8B and CMPXCHG16B. The DBT also refuses raw SYSCALL.
