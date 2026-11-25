# Windows x64 exception and unwind requirements

This page maps the public Windows x64 exception-handling and unwinding ABI to what Xenora's guest runtime would need in order to run compiled C and C++ programs that raise, catch or unwind through exceptions. It records what exists on main `d362e17`, what two original probe programs do there today, the pieces that are missing in dependency order, the unknowns, a fixture plan, and ranked follow-ups (#441).

It was written as research. Since then #447 implemented follow-ups 1, 2 and 4 for statically linked mingw-w64 C++ programs: the exception directory, `RtlLookupFunctionEntry`, `RtlVirtualUnwind`, `RtlCaptureContext`, `RaiseException` and `RtlUnwindEx` (`crates/loader/src/exceptions.rs`, tested by `crates/loader/tests/guest_cpp_exceptions.rs`). #453 then implemented follow-up 3: guest CPU faults as exceptions and `__C_specific_handler` (`crates/loader/src/exceptions/faults.rs` and `scope.rs`, tested by `crates/loader/tests/guest_c_seh.rs`); see [Implemented fault policy](#implemented-fault-policy-453) and [Derived scope-table format](#derived-scope-table-format-453). The tables below describe main at `d362e17`, before that work. The unhandled-exception filter (follow-up 5) remains open. Nothing here describes Xbox console behaviour, which is unknown. Commercial game execution remains unsupported.

## Sources

| Source | Used for |
| --- | --- |
| [x64 exception handling](https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64) (Microsoft C++ docs) | `RUNTIME_FUNCTION`, `UNWIND_INFO`, unwind codes, chained info, the unwind procedure, the handler prototype, `DISPATCHER_CONTEXT` |
| [PE format](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format), data directories and `.pdata` | Exception directory (index 3) and the x64 function table entry |
| [`RtlVirtualUnwind`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlvirtualunwind) | Signature, handler types, the x64 epilog forms |
| [`RtlLookupFunctionEntry`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtllookupfunctionentry) | Signature and NULL result for leaf code |
| [`RtlUnwindEx`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlunwindex) | Signature, exit unwind when `TargetFrame` is NULL |
| [`RtlCaptureContext`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlcapturecontext) | Signature |
| [`RaiseException`](https://learn.microsoft.com/en-us/windows/win32/api/errhandlingapi/nf-errhandlingapi-raiseexception) | Signature, flags, dispatch order |
| [`EXCEPTION_RECORD`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-exception_record) | Record fields, access-violation parameters |
| [`try-except` statement](https://learn.microsoft.com/en-us/cpp/cpp/try-except-statement) | Filter results and their values |
| mingw-w64 14.0.0 `winnt.h` and `excpt.h` (public headers, installed with the Homebrew `mingw-w64` package) | Numeric constants the Microsoft pages name without values. Only constant values are cited; no header text or code is copied |

The prolog and epilog page linked from the x64 page (`/cpp/build/prolog-epilog`) returned HTTP 404 when this was written, so its rules are listed under unknowns.

## ABI summary

### Function table

- Data directory 3 of the optional header is the exception table: an RVA and a size. It sits at offset 136 in PE32+. Check `NumberOfRvaAndSizes` before reading it, and do not assume the table starts a section or that the section is named `.pdata`.
- The table is an array of 12-byte `RUNTIME_FUNCTION` entries: `BeginAddress`, `EndAddress` and `UnwindData`, each a 4-byte RVA. Entries are DWORD aligned and sorted by address.
- Every function that allocates stack or calls another function must have an entry. A leaf function has none. Code generated at run time must register its own table (`RtlAddFunctionTable` or `RtlInstallFunctionTableCallback`).

### `UNWIND_INFO`

| Byte | Field |
| --- | --- |
| 0 | `Version` (3 bits, currently 1) and `Flags` (5 bits) |
| 1 | `SizeOfProlog` in bytes |
| 2 | `CountOfCodes`, in 2-byte slots |
| 3 | `FrameRegister` (4 bits, 0 = none) and `FrameOffset` (4 bits). The frame pointer is RSP + 16 × `FrameOffset`, at most 240 |

The code array follows, padded to an even number of slots. After it comes either a 4-byte handler RVA followed by handler-defined data (when `UNW_FLAG_EHANDLER` 0x1 or `UNW_FLAG_UHANDLER` 0x2 is set), or a chained `RUNTIME_FUNCTION` (when `UNW_FLAG_CHAININFO` 0x4 is set, which excludes both handler flags and must keep the primary entry's frame register and offset). A chain ends at an entry without `UNW_FLAG_CHAININFO`. The handler or chain is found at slot `(CountOfCodes + 1) & ~1`.

### Unwind codes

Each code is 2 bytes: `CodeOffset` (the end of the prolog instruction, plus 1), `UnwindOp` (4 bits) and `OpInfo` (4 bits). Codes are stored in descending `CodeOffset` order. `OpInfo` names registers 0–15 in the order RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8–R15.

| Value | Operation | Slots | Meaning |
| --- | --- | --- | --- |
| 0 | `UWOP_PUSH_NONVOL` | 1 | Push of the register in `OpInfo` |
| 1 | `UWOP_ALLOC_LARGE` | 2 or 3 | `OpInfo` 0: the next slot is the size / 8 (up to 512K − 8). `OpInfo` 1: the next two slots are the unscaled size (up to 4G − 8) |
| 2 | `UWOP_ALLOC_SMALL` | 1 | Size = `OpInfo` × 8 + 8 (8 to 128 bytes) |
| 3 | `UWOP_SET_FPREG` | 1 | Frame register = RSP + 16 × `FrameOffset` |
| 4 | `UWOP_SAVE_NONVOL` | 2 | Register saved at the next slot × 8 |
| 5 | `UWOP_SAVE_NONVOL_FAR` | 3 | Register saved at an unscaled 32-bit offset |
| 8 | `UWOP_SAVE_XMM128` | 2 | XMM register saved at the next slot × 16 |
| 9 | `UWOP_SAVE_XMM128_FAR` | 3 | XMM register saved at an unscaled 32-bit offset |
| 10 | `UWOP_PUSH_MACHFRAME` | 1 | Machine frame of 40 bytes (`OpInfo` 0) or 48 bytes with an error code (`OpInfo` 1); only in a dummy prolog |

Values 6, 7 and 11–15 are not described on the page.

### Unwinding one frame

1. Look up the `RUNTIME_FUNCTION` for RIP. For chained info, use the function portion.
2. If there is none, the frame is a leaf: the return address is at [RSP]; pop it and repeat.
3. Otherwise RIP is in one of three places:
   - **Epilog:** handlers do not apply; simulate the rest of the epilog and repeat. The `RtlVirtualUnwind` page lists the x64 epilog markers: `ret`, `ret n`, `rep ret`, `jmp imm8|imm32` with a target outside the function, `jmp qword ptr imm32`, and `rex.w jmp reg`.
   - **Prolog:** the offset from the function start is no more than `SizeOfProlog`. Skip the codes whose `CodeOffset` is beyond RIP's offset, undo the rest, and repeat.
   - **Body:** if `UNW_FLAG_EHANDLER` is set, call the language handler, which may handle the exception, continue the search, or start an unwind.
4. Without a handler, or after continue-search, undo every code to reach the caller and repeat.

### Handlers and dispatch

- Handler prototype: `EXCEPTION_DISPOSITION handler(EXCEPTION_RECORD *, ULONG64 EstablisherFrame, CONTEXT *, DISPATCHER_CONTEXT *)`. `EstablisherFrame` is the base of the function's fixed stack allocation.
- `DISPATCHER_CONTEXT` holds, in order: `ControlPc`, `ImageBase`, `FunctionEntry`, `EstablisherFrame`, `TargetIp`, `ContextRecord`, `LanguageHandler` and `HandlerData`.
- Dispositions (`excpt.h`): `ExceptionContinueExecution` 0, `ExceptionContinueSearch` 1, `ExceptionNestedException` 2, `ExceptionCollidedUnwind` 3.
- `__except` filter results: `EXCEPTION_CONTINUE_EXECUTION` −1, `EXCEPTION_CONTINUE_SEARCH` 0, `EXCEPTION_EXECUTE_HANDLER` 1.
- Dispatch is in two phases. The search walks frames from the faulting one outwards, calling exception handlers. The unwind walks them again to the target frame with the unwinding flags set, calling termination handlers, and then continues at the target. `RaiseException` documents this order: debugger first, then frame handlers, then the debugger again, then the default action (`ExitProcess` for most exceptions).
- Exception flags (`winnt.h`): `EXCEPTION_NONCONTINUABLE` 0x1, `EXCEPTION_UNWINDING` 0x2, `EXCEPTION_EXIT_UNWIND` 0x4, `EXCEPTION_STACK_INVALID` 0x8, `EXCEPTION_NESTED_CALL` 0x10, `EXCEPTION_TARGET_UNWIND` 0x20, `EXCEPTION_COLLIDED_UNWIND` 0x40. Continuing a noncontinuable exception raises `STATUS_NONCONTINUABLE_EXCEPTION`.

### `EXCEPTION_RECORD`

Fields in order: `ExceptionCode` (DWORD), `ExceptionFlags` (DWORD), `ExceptionRecord` (pointer to a chained record for nested exceptions), `ExceptionAddress`, `NumberParameters` (DWORD) and `ExceptionInformation[EXCEPTION_MAXIMUM_PARAMETERS]`, where the maximum is 15. For an access violation, element 0 is 0 for a read, 1 for a write and 8 for a DEP violation, and element 1 is the inaccessible address.

Status codes (`winnt.h`) that Xenora's CPU faults map to most directly: `STATUS_ACCESS_VIOLATION` 0xC0000005, `STATUS_ILLEGAL_INSTRUCTION` 0xC000001D, `STATUS_INTEGER_DIVIDE_BY_ZERO` 0xC0000094, `STATUS_PRIVILEGED_INSTRUCTION` 0xC0000096 and `STATUS_BREAKPOINT` 0x80000003. Also `STATUS_NONCONTINUABLE_EXCEPTION` 0xC0000025, `STATUS_INVALID_DISPOSITION` 0xC0000026 and `STATUS_UNWIND_CONSOLIDATE` 0x80000029.

### The six services

| Service | Documented signature |
| --- | --- |
| `RaiseException` | `(DWORD code, DWORD flags, DWORD count, const ULONG_PTR *arguments)`, no return. Bit 28 of the code is reserved for the system. |
| `RtlCaptureContext` | `(CONTEXT *)`, no return; captures the caller's context |
| `RtlLookupFunctionEntry` | `(DWORD64 ControlPc, DWORD64 *ImageBase, UNWIND_HISTORY_TABLE *)` → `RUNTIME_FUNCTION *` or NULL |
| `RtlVirtualUnwind` | `(DWORD HandlerType, DWORD64 ImageBase, DWORD64 ControlPc, RUNTIME_FUNCTION *, CONTEXT *, PVOID *HandlerData, DWORD64 *EstablisherFrame, KNONVOLATILE_CONTEXT_POINTERS *)` → the frame's handler or NULL |
| `RtlUnwindEx` | `(PVOID TargetFrame, PVOID TargetIp, EXCEPTION_RECORD *, PVOID ReturnValue, CONTEXT *, UNWIND_HISTORY_TABLE *)`, no return; `ReturnValue` lands in RAX at the target |
| `__C_specific_handler` | `(EXCEPTION_RECORD *, void *EstablisherFrame, CONTEXT *, DISPATCHER_CONTEXT *)` → `EXCEPTION_DISPOSITION` (prototype as declared in `excpt.h`) |

## What Xenora has today (`d362e17`)

| Piece | State | Where |
| --- | --- | --- |
| Exception directory | Not read. The PE parser and the static probe never look at data directory 3 | `crates/loader/src/pe.rs`, `crates/loader/src/probe.rs` |
| `__C_specific_handler` | Registered as `Stub`; always stops the guest with the runtime-limit fault | `crates/loader/src/services.rs` (`c_specific_handler`), `crates/loader/src/runtime_table.rs`, `crates/loader/runtime/services.txt` |
| `RaiseException`, `RtlCaptureContext`, `RtlLookupFunctionEntry`, `RtlUnwindEx`, `RtlVirtualUnwind` | No service. The probe files the `rtlsupport` and error-handling modules under "exceptions and unwind" and marks `RtlVirtualUnwind` unsupported | `crates/loader/src/probe.rs` |
| `SetUnhandledExceptionFilter` | `Partial`: stores and returns the filter, which nothing ever calls | `crates/loader/src/services.rs`, `docs/interfaces.md` |
| CPU faults | Become `ExitReason` values that end the guest: `DivideError`, `GeneralProtection`, `MemoryFault`, `IllegalInstruction`, `SimdException`. RIP is left at the faulting instruction (seen in the C probe below) | `crates/loader/src/cpu.rs` |
| Guest callbacks from services | Exist: service dispatch can call guest code and resume (used by `qsort`, `atexit`, FLS callbacks), which a dispatcher calling guest handlers would build on | `crates/loader/src/imports.rs` |
| C++ fixture | #437 builds with `-fno-exceptions` because no exception path exists | `tests/compat_fixtures/build-guest-cpp.sh` |

`IllegalInstruction(0xFE)` doubles as the runtime-limit stop that stubs and blocking calls use. A dispatcher must not turn that into a guest `STATUS_ILLEGAL_INSTRUCTION`.

## Measured probe results

Two original programs, written for this research, were built on an Apple M4 and run through `startup::run_synthetic_startup_scheduled` with the standard services at main `d362e17`, image base `0x2_5000_0000`.

**C, MSVC-style SEH.** A function divides 100 by a `volatile` zero inside `__try`. Its `__except` filter returns 1 for `0xC0000094` and 0 otherwise, the handler sets the result to 42, and the program exits through `ExitProcess`. It was built with Homebrew clang 23.1.6, `--target=x86_64-pc-windows-msvc -O1 -fasync-exceptions -ffreestanding -fno-stack-protector`, and linked by lld-link 23.1.6 with `/entry` and `/nodefaultlib`. The import libraries came from two one-line `.def` files (`ucrtbase.dll!__C_specific_handler`, `kernel32.dll!ExitProcess`). Without `-fasync-exceptions`, clang does not treat a hardware fault inside `__try` as catchable and emits no handler.

- The image has one `RUNTIME_FUNCTION` covering the function, with unwind data in `.rdata`, and imports exactly `__C_specific_handler` and `ExitProcess`.
- Result: both imports bind. The guest stops after 9 steps with `ExitReason::DivideError` at RIP `0x250001020`, the `idiv`. The expected result on Windows is exit code 42.

**C++, GCC exceptions.** The program throws and catches an `int` (adding 7) and throws a `std::runtime_error` from five calls deep, catching it as `std::exception` and adding 10. It prints the score and returns it (17). It was built with mingw-w64 g++ 16.2.0 `-O1`.

- With `-static-libstdc++ -static-libgcc`, GCC's unwinder and personality are linked into the image. Its exception-specific imports are `RaiseException`, `RtlCaptureContext`, `RtlLookupFunctionEntry`, `RtlUnwindEx` and `RtlVirtualUnwind` (no service) and `__C_specific_handler` (stub). Startup refuses it before entry at the first unresolved import, `KERNEL32.dll!FormatMessageA`. The static probe lists 21 unresolved import thunks in all; the others are CRT, `libwinpthread-1.dll` and `FormatMessageA`/`LocalFree`.
- Dynamically linked, it imports the unwinder from `libgcc_s_seh-1.dll` (`_Unwind_Resume`) and the C++ ABI from `libstdc++-6.dll` (`__cxa_throw`, `__gxx_personality_seh0` and others), and is refused at `_Unwind_Resume`.

The static C++ case therefore needs only the OS-level surface. GCC's own unwinder and personality run as guest code, read the image's `.pdata` and `.xdata` themselves through `RtlLookupFunctionEntry` and `RtlVirtualUnwind`, and start dispatch with `RaiseException`.

## Pieces required, in dependency order

1. **Exception directory reader.** Bounds-checked parsing of data directory 3: 12-byte entries, DWORD alignment, sorted and non-overlapping ranges inside the image, unwind data inside the image. Per-image lookup by RVA. A malformed table must be a precise loader or probe error, not a silent miss.
2. **Function lookup.** `RtlLookupFunctionEntry` semantics over every mapped image (a single image today, more with #412), returning NULL for leaf code, with chained entries followed to their primary.
3. **Guest `CONTEXT`.** A guest-memory layout of the x64 `CONTEXT` record, with conversion to and from `CpuState`. The x64 layout was not on the pages read (see unknowns).
4. **Virtual unwinder.** Every unwind code in the table above, the prolog rule, epilog recognition with the listed forms, the leaf rule and chained info, run against guest memory with checked reads. It must reject unknown operations and versions instead of guessing.
5. **Fault-to-exception mapping.** From `ExitReason` to `EXCEPTION_RECORD`: `DivideError` → `STATUS_INTEGER_DIVIDE_BY_ZERO`, `MemoryFault` → `STATUS_ACCESS_VIOLATION` with read/write and address, `IllegalInstruction` (other than the runtime-limit marker) → `STATUS_ILLEGAL_INSTRUCTION`. `GeneralProtection` and `SimdException` have no single documented code on these pages, so their mapping is a policy decision.
6. **Two-phase dispatcher.** Search and unwind as described, calling guest handlers through the existing guest-callback mechanism, honouring dispositions and the unwinding flags, and stopping with a precise `ExitReason` on an invalid disposition, a corrupt stack or an exhausted budget.
7. **Services.** `RtlCaptureContext`, `RtlLookupFunctionEntry` and `RtlVirtualUnwind` on top of 2–4; `RaiseException` and `RtlUnwindEx` on top of 6; `__C_specific_handler` on top of 6 and the scope-table format (see unknowns).
8. **Unhandled exceptions.** Call the filter stored by `SetUnhandledExceptionFilter`, then the default action. The exit status of an unhandled exception is not documented on the pages read, so it is a policy decision to record.

## Implemented fault policy (#453)

A fault raised by a guest instruction, while it was fetched, decoded or run, becomes an `EXCEPTION_RECORD` with `ExceptionAddress` at that instruction and starts the same search as `RaiseException`. The search-phase handlers receive the CONTEXT of the fault, and continuing execution resumes from that CONTEXT, so a handler's edits apply. The x64 page states both: `ContextRecord` "points to the exception context at the time the exception was raised" for an exception handler, and a handled status continues "by using the original context record".

| `ExitReason` | Becomes | Why |
| --- | --- | --- |
| `DivideError` | `STATUS_INTEGER_DIVIDE_BY_ZERO`, no parameters | Xenora's #DE does not separate a zero divisor from a quotient overflow, so an overflow gets the same code. Which code Windows reports for an overflow was not checked |
| `MemoryFault(UnmappedAddress)`, `MemoryFault(PermissionViolation)` | `STATUS_ACCESS_VIOLATION`, parameters: 0 read, 1 write, 8 instruction fetch; then the address | 0, 1 and 8 are the documented values. 8 is documented for a DEP violation; using it for a fetch from unmapped memory is Xenora's choice, as the pages read do not say what Windows reports there. For a read-modify-write instruction the read happens first, so it is reported as a read; what Windows reports is unknown |
| `IllegalInstruction` at UD2 (`0F 0B`) | `STATUS_ILLEGAL_INSTRUCTION`, no parameters | UD2 is defined to be invalid. Any other refused opcode may be a valid instruction Xenora does not implement, which a guest handler must not hide, so it still stops the guest |
| `IllegalInstruction(0xFE)`, the runtime-limit marker | Still stops the guest | A Xenora limit, not a guest fault |
| `GeneralProtection`, `SimdException` | Still stop the guest | The reasons do not carry the detail a record needs (which #GP cause, which MXCSR flag), and their Windows codes are not on the pages read |
| `MemoryFault(Overflow)` and other faults without an address | Still stop the guest | No address for the record |

Faults inside host services, in a process with no exception directory, or with too little writable stack below RSP for the dispatcher also still stop the guest. A fault no frame accepts stops the guest with the original `ExitReason` at the faulting instruction, after every filter ran; a raised exception no frame accepts stops it with `ExitReason::UnhandledException(code)`. The search ends at the return address of the entry point, of a thread's start routine and of a guest callback made by a host service. The unhandled-exception filter is not called (follow-up 5). Exceptions raised inside a filter are not flagged as nested.

## Derived scope-table format (#453)

No page read describes the handler data of `__C_specific_handler`. The layout Xenora implements was derived from the unwind data and disassembly of original programs: `tests/compat_fixtures/guest_c_seh.c` built by clang 23.1.6 (`--target=x86_64-pc-windows-msvc -fasync-exceptions`, `-O0`, `-O1`, `-O2`), and the `mainCRTStartup` and `WinMainCRTStartup` frames of the mingw-w64 14.0.0 C runtime linked into `guest_cpp_exceptions.cpp`. It is a derived format, not a documented one.

| Field | Size | Meaning |
| --- | --- | --- |
| `Count` | 4 | Number of records that follow |
| `Begin` | 4 | Image-relative start of the guarded code |
| `End` | 4 | Image-relative end (exclusive) |
| `Handler` | 4 | `__except`: the filter, or the literal 1 for the constant `EXCEPTION_EXECUTE_HANDLER`. `__finally`: the termination handler |
| `Target` | 4 | `__except`: the image-relative start of the handler body. `__finally`: 0 |

- Records are ordered innermost scope first. A `__finally` and the `__except` around it can share one range, and one scope can need several records.
- A filter is called as `filter(EXCEPTION_POINTERS *, EstablisherFrame)` and a termination handler as `handler(abnormal, EstablisherFrame)`; both reach the parent's locals through the establisher frame. The `__except` body reads the exception code from EAX, so the unwind's `ReturnValue` is the code.
- The mingw-w64 startup scope's filter is the C runtime's `_gnu_exception_handler`. Read from its disassembly: for a code that equals `0x20474343` under the mask `0x20ffffff` (GCC's C++ exceptions) without `EXCEPTION_NONCONTINUABLE` it returns `EXCEPTION_CONTINUE_EXECUTION`, so an uncaught C++ exception returns from `RaiseException` and the C++ runtime calls `std::terminate`.
- `__C_specific_handler` resumes at `DISPATCHER_CONTEXT.ScopeIndex` (offset 0x48, after `HistoryTable`) and stores the next index there before calling a termination handler. In the unwind phase it stops at the `__except` record whose body is the unwind target, in the target frame.

## Unknowns and inconsistencies

- The prolog and epilog page returned 404. The required prolog shape and the `lea rsp` epilog rule are therefore unverified; only the `RtlVirtualUnwind` list of epilog markers was read.
- Unwind version 2 and its epilog codes, and operation values 6, 7 and 11–15, are not described on the pages read. The x64 page links an "Unwind V3 Preview" for APX, which was not read.
- The scope table that `__C_specific_handler` reads from the language-specific data is not described on any page read. #453 derived it from original compiled fixtures (above); MSVC's own output was not examined, and how Windows handles collided unwinds through it is unknown.
- The x64 `CONTEXT` layout was not on the pages read: `RtlCaptureContext` links an ARM64 `CONTEXT` page. The x64 definition of `UNWIND_HISTORY_TABLE` was not in the fetched content either.
- Which registers `RtlCaptureContext` captures is not stated on its page.
- The exit status and messages of an unhandled exception are not documented on the pages read.
- The x64 page says the smallest unwind data is 8 bytes and is also the size of a chained structure, which does not match its own field sizes (a 4-byte header plus a 12-byte chained entry).
- The x64 page's C enum comment says `UWOP_ALLOC_LARGE` uses the next two slots in every case, but the prose says one slot when `OpInfo` is 0. The prose agrees with the encoding rules.
- The `RtlVirtualUnwind` page describes `HandlerData` in x86 terms while saying the parameter is absent on x64.
- Console (Xbox) exception behaviour is unknown. Nothing here was compared with Windows or with console hardware.

## Fixture plan

All fixtures are original programs built by repository scripts with the tools above, following `tests/compat_fixtures/build-guest-cpp.sh`, including its ARM64 host-tool checks.

| Fixture | Expected result | Exercises |
| --- | --- | --- |
| C divide by zero in `__try`/`__except`, filter accepts `0xC0000094` (implemented by #453, like the next three) | Exit 42 | Lookup, unwind, fault mapping, search phase, `__C_specific_handler` |
| The same with the filter returning `EXCEPTION_CONTINUE_SEARCH` | The unhandled-exception outcome Xenora defines | Search to the outermost frame, unhandled path |
| C write to an unmapped address, filter inspects `ExceptionInformation` | Exit code encodes element 0 = 1 and the address | Access-violation record parameters |
| C `__try`/`__finally` around a `RaiseException` caught one frame up | The termination handler runs once, before the outer handler body | Unwind phase, `UNW_FLAG_UHANDLER`, `EXCEPTION_UNWINDING` |
| C++ `throw`/`catch` (the probe above), `-static-libstdc++ -static-libgcc`, at O0, O1, O2 and Os | Prints `score 17`, exits 17 | The six services with GCC's own unwinder |
| C++ exception through a frame with a large stack allocation and saved XMM registers | Caught, nonvolatile registers intact | `UWOP_ALLOC_LARGE`, `UWOP_SAVE_XMM128`, frame register |

Negative variants, each with a precise failure stage: an exception directory with an unsorted or overlapping table, a range outside the image, or a misaligned entry; unwind data with an unknown version or operation; a chain that loops; a handler returning an invalid disposition; and a fault inside a handler (nested exception). Expected values come from the programs' own definitions and the documented codes, never from Xenora's output.

## Ranked follow-ups

To be filed as separate issues after review of this page:

1. **Exception directory and function lookup** (pieces 1–2). Loader and probe parsing with negative fixtures, and the probe reporting `.pdata` presence. Small and self-contained, and everything else depends on it.
2. **Virtual unwinder, `RtlCaptureContext`, `RtlLookupFunctionEntry`, `RtlVirtualUnwind`** (pieces 3–4, 7). Known-answer tests on compiled prologs at several optimisation levels.
3. **CPU-fault dispatch and `__C_specific_handler`** (pieces 5–7), after the scope-table format is settled. It makes the C `__try`/`__except` fixtures pass. Done in #453, with the scope-table format derived from fixtures.
4. **`RaiseException` and `RtlUnwindEx`** (pieces 6–7). It makes the static mingw-w64 C++ fixture pass, together with the unrelated unresolved imports that fixture needs (`FormatMessageA`, `LocalFree`, the CRT functions and the `libwinpthread-1.dll` functions).
5. **Unhandled-exception policy** (piece 8). Filter call, default action and the recorded exit status.

Deferred: running GCC's runtime DLLs (`libgcc_s_seh-1.dll`, `libstdc++-6.dll`) as guest modules, which needs multi-module loading (#412) and a licence review of redistributing them; MSVC C++ exceptions (`__CxxFrameHandler*`), which need MSVC's runtime; and vectored exception handlers.
