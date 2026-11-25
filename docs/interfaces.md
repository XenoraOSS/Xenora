# Interfaces

## gfx1030 compute fixtures

The first input is little-endian gfx1030 machine code for a compute fixture with optional statically resolved forward scalar branches. The synthetic ABI starts with full Wave32 EXEC and reserves vector general-purpose register (VGPR) 0 for the grid index used by buffer accesses. Resource operands select documented synthetic bindings. The parser must require explicit target identification and validate all lengths before allocating. Console containers and AMDHSA initialization require separate contracts.

The production decoder takes borrowed code bytes. Its output records typed instructions, operand classes, original offsets and byte lengths. Unknown encodings and unsupported forms fail with a byte offset. The reference interpreter takes the original bytes and input words and performs its own decoding. It shares no production instruction definitions.

The compiler lifts decoded instructions to typed operations and verifies register initialization, supported resource offsets and termination before generating MSL. The initial subset permits only lane-indexed 32-bit buffer accesses against two disjoint buffers with equal lengths. The input is read-only. Each invocation may write only its own output word. The compiler and reference check instruction boundaries, termination placement and buffer bindings throughout the stream. Register initialization and transfer state are checked on the path selected by compile-time known forward scalar branches. Dynamic branches, workgroup barriers and cross-lane operations remain unsupported.

The Wave32 execution mask (EXEC) starts full. A supported scalar move can
assign a known 32-bit value to its low half. Each bit selects the corresponding
logical lane, numbered by the one-dimensional grid index modulo 32. Partial
masks limit vector arithmetic and moves to selected lanes. These instructions
must write an initialized destination so inactive lanes can retain their values.
Empty EXEC suppresses vector arithmetic and moves. Reachable vector memory
operations require full EXEC. Forward branches on EXEC or the vector condition
code (VCC) test only their low halves. VCC must be initialized before use.

The native backend exposes `NativeCompute::new()` and `run_words(source, entry, input)` for trusted MSL. It binds input at index 0 and output at index 1, dispatches one invocation per word and returns owned readback after completion. `run_words_profiled` returns the same output plus measured library, pipeline, GPU and host durations for the benchmark harness; those timings are single submissions, not distributions. The caller runs generated MSL in a bounded child process. Raw MSL is a trusted developer input. Errors report the operation and backend detail. Across other stages, diagnostics carry a stage, error kind, optional byte offset and human-readable context.

## Synthetic guest experiment

The separate [X1–X4 driver](../experiments/x1/README.md) loads fixed-base
PE32+ fixtures, interprets a bounded x86-64 subset and dispatches synthetic
services. X3 and X4 validate fixed-size command records from read-only
guest bytes before issuing compute or draw work. Their command formats and
Metal shaders are authored for the experiment.

The multi-function guest fixture (X6) maps PE32+ sections with separate
read, write and execute permissions. Its interpreter tracks the expected
return slot and continuation for each call. A return from the authenticated
root frame ends execution.

The named-import fixture (X7) resolves imports for one synthetic module
before guest entry. An indirect call may invoke only a resolved slot in the
guest import address table. The handler validates the guest buffer and
operates on words in checked guest memory. The command prints the guest
exit value, while its host exit code reports whether loading and execution
succeeded.

With the Metal feature, the guest compute fixture (X8) adds the
`xenora_compute` import to the same loader. RCX points to a four-byte-aligned
readable and writable three-word buffer. RDX points to a readable aligned
16-byte record containing four little-endian 32-bit fields: command 1,
handle 0, input slot 0, and word count 3. Handle 0 names the buffer supplied
on this call and is consumed after a successful dispatch. A second submission
within the same guest run fails with a stale-resource fault.
The CPU validates both spans before creating a Metal context.
It selects the original translated XOR program, waits for completion,
writes the three output words back to guest memory, and returns 3.
The guest can read the result through its existing report import.

A general guest command stream must submit validated resource handles and ranges through an explicit address translation boundary. The graphics side owns mappings and the lifetime of in-flight views. Completion and fence signaling flow back across the command boundary. Guest addresses are never host pointers.

`NativeCompute::run_guest_words` takes a host-provided immutable resource
table and a synthetic guest view. The view selects an input slot, table index,
byte offset and byte length. The backend validates slot zero, little-endian
32-bit words, alignment, bounds and dispatch size before copying the selected
range into a Metal buffer. Slot one holds a separate output buffer. The call
returns owned words after command completion or a stage-specific error.

## PE32+ fixture startup and static import reports

`xenora_loader::startup::run_synthetic_startup` takes an original, trusted
fixture, guest memory addresses, a service registry and an entry instruction
budget. It maps and rebases the image, binds imports, initializes thread-local
storage, invokes its callbacks and runs the entry point. The fixture entry point
uses a Win64-aligned stack. It may end with HLT, call `ExitProcess`, or return.
The result includes the loaded guest memory and the entry execution outcome.

A guest ends its process through `kernel32.dll!ExitProcess`, also reachable as
`api-ms-win-core-processthreads-l1-1-0.dll!ExitProcess`. Execution stops with
`ExitReason::ProcessExit(status)`, where the status is the full 32-bit value
the guest passed. Only the guest stops: the host process is not ended. A
returning entry point does the same with EAX as the status, which matches
Windows while the process has one thread. The exit closes every guest handle:
kernel objects, Winsock sockets and task-queue handles, which
`ServicesState::open_handle_count` counts together. Files stay on the synthetic `C:` drive, guest memory stays mapped so the host
can inspect it, and `atexit` functions are not run: that belongs to the C
runtime's `exit`, which is not implemented. `ServicesState::exit_status`
reports the status. After an exit the `DbtEngine` that ran the guest executes
no further guest instruction and answers every later `run` with the same
`ProcessExit`. A service callback that was in progress is never resumed.

A TLS callback may end the process too. Startup then reports the exit as its
outcome, with the status, and runs neither the remaining callbacks nor the
entry point; `StartupResult::tls_callbacks` counts the callbacks that finished.
Any other stop inside a TLS callback is still a startup error.

The engine recognises two return addresses, one under the entry point and one
under a guest callback, before it executes anything at them. Both are in the
4 KiB page at `imports::RETURN_SENTINEL_PAGE`, which startup reserves with no
access. An image or layout that covers that page is refused, and a guest
allocation there fails. The import trampolines above that page are intercepted
in the same way and are not reserved.

Loading an image maps its headers as well as its sections. The first
`SizeOfHeaders` bytes of the file are mapped read-only at the image base, in
whole 4 KiB pages. Everything in those pages past `SizeOfHeaders` is zero, even
where the file holds more bytes there. A guest can read its own DOS header,
section table and data directories; it cannot write or execute them.

When startup loads an image away from its preferred base, it writes the load
base into the mapped optional header's `ImageBase` field after the relocations
have been applied, so an entry aimed at that field cannot leave any other value
there. This is Xenora's policy: the PE specification defines `ImageBase` as the
preferred base, and the behaviour has not been compared with Windows.

The load is refused with `PeError::InvalidHeaderSize` when `SizeOfHeaders` is
zero, ends before the section table does, runs past the end of the file, or its
pages would reach into a section, and when the bytes given to the loader are not
the PE32+ headers that were parsed. The page rule is stricter than the loader
was before: an image whose section alignment is below 4 KiB, with a section
that starts inside the header page, used to load and is now refused. The header
pages count toward the 256 MiB mapping budget of one image, which is checked
before anything is mapped. `ParsedPe::parse` does not make these checks, so
static inspection of a file is unchanged.

`xenora probe --mode static` inventories ordinary and delay-loaded PE imports
without guest execution. Without `--report` it writes to `diagnostics/probe`,
where `xenora imports sync` also looks. `missing_imports.csv` lists unresolved ordinary keys
with their import address table relative virtual addresses. The full inventory,
`imports.csv`, marks each entry `ordinary` or `delayed` in its `import_table`
column and records whether each key is registered, its implementation
status, and the canonical module behind API-set aliases. Modules and services
loaded by other means at run time require separate analysis. The coverage matrix
groups unresolved ordinary imports by shared startup prerequisites. Static
imports do not reveal call order. The JSON report counts import slots, DLL and
symbol keys, distinct names with DLL-scoped ordinals, and canonical registered
handlers separately. The unresolved total includes explicitly tracked
unsupported APIs and other missing handlers. Existing stubs remain visible in
the full inventory.

Delay-loaded imports come from the PE32+ delay-load directory, data directory
13 in Microsoft's PE format. The probe reports each one with its DLL, symbol or
ordinal, and slot in the delay import address table, after the ordinary rows.
Binding does not resolve them: the PE format leaves that to a delay-load helper
inside the image. The totals above and `missing_imports.csv` therefore cover
ordinary imports only, and the JSON report counts delayed slots and DLLs in
`delayed_imports` and `delayed_imported_dlls_count`. `xenora imports sync`
leaves delayed rows out of its backlog for the same reason.

The probe reads only descriptors that lie wholly inside the directory's
declared size, and expects that size to hold the empty descriptor that ends the
table. It reads descriptors whose `RvaBased` attribute is set, and requires each
one's DLL name, import address table and import name table to be present and to
start in section data. A table that breaks one of these rules, sets a reserved
attribute bit, is truncated, or uses the earlier virtual-address descriptor form
is reported in `delay_import_error` and in the Markdown report, together with
the entries read before it.

`xenora_import_gen::demangle` decodes Microsoft Visual C++ decorated names
for diagnostics, in the style of Microsoft's `undname`. It covers functions,
data, member access and storage, `this` qualifiers, calling conventions,
constructors, destructors, common operators, `vftable` names, classes,
structs, unions, enums, pointers, references, primitive types, templates
with type and integer arguments, and name and type back-references. Function
pointer types, RTTI descriptors, nested function scopes and other constructs
return `None`, so callers show the decorated name instead of a guess. The
generator manifest's list of imports that need manual engineering shows the
undecorated form beside each decorated C++ import. Tests check names mangled
by clang for `x86_64-pc-windows-msvc` against their source declarations.

The synthetic process registry supports the current-process pseudo handle and
its process ID. The priority boost setter and getter record and query a guest
policy. They are partial until a guest scheduler applies the policy. The process
ID lookup is limited to the current-process pseudo handle. An unsupported
handle reports `ERROR_INVALID_HANDLE` through the guest error slot. The getter
rejects invalid guest output addresses with `ERROR_NOACCESS`.

Win32 references include Microsoft's documentation for
[GetCurrentProcess](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-getcurrentprocess),
[GetProcessId](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocessid),
[SetProcessPriorityBoost](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocesspriorityboost),
and [GetProcessPriorityBoost](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesspriorityboost).

### Performance timing

The guest can import `QueryPerformanceCounter` and `QueryPerformanceFrequency`
from Kernel32 or the profile API set. The counter measures monotonic elapsed
time at a fixed 10 MHz, independently of wall-clock time. Synthetic fixtures
can advance it deterministically. A failed guest output write returns false
and records `ERROR_NOACCESS`. `GetTickCount` and `GetTickCount64` return
whole milliseconds since the synthetic runtime was created. The 32-bit value
wraps at 2³² milliseconds. Both use the monotonic clock and are available
through Kernel32 and the system-information API set.

See Microsoft's documentation for
[QueryPerformanceCounter](https://learn.microsoft.com/windows/win32/api/profileapi/nf-profileapi-queryperformancecounter)
[QueryPerformanceFrequency](https://learn.microsoft.com/windows/win32/api/profileapi/nf-profileapi-queryperformancefrequency),
[GetTickCount](https://learn.microsoft.com/windows/win32/api/sysinfoapi/nf-sysinfoapi-gettickcount),
and [GetTickCount64](https://learn.microsoft.com/windows/win32/api/sysinfoapi/nf-sysinfoapi-gettickcount64).

### Thread handles and virtual memory

The thread object model exposes one guest thread with a stable synthetic ID.
`OpenThread` issues rights-checked handles to that thread, and `CloseHandle`
invalidates real handles. A thread pseudo handle refers to the current synthetic
thread. `GetThreadId` accepts the pseudo handle and live thread handles with
query rights. The priority APIs store and query the thread's base-priority setting.
Thread creation, inheritance, debugger exceptions, and scheduling remain outside
this model. Within the synthetic process, `DuplicateHandle` can convert the
thread pseudo handle to a real handle or copy a live thread handle. The new
handle can keep or reduce the source's rights. The original and duplicate can
be closed independently. Cross-process duplication, inheritable handles,
process-handle duplication, and closing the source during duplication are
outside this subset. A failed output write leaves the handle table unchanged.

`VirtualAlloc` supports combined reserve and commit for data-only pages.
`VirtualFree` supports `MEM_RELEASE` with the exact allocation base and a
zero size. The guest memory map tracks allocation ownership and bounds live
virtual allocations to 64 MiB. Decommit, executable pages, and console-specific
allocation lifecycle remain unsupported.

References include Microsoft's
[thread handle](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-openthread),
[thread ID](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-getthreadid),
[thread priority](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadpriority),
[handle duplication](https://learn.microsoft.com/windows/win32/api/handleapi/nf-handleapi-duplicatehandle),
[handle close](https://learn.microsoft.com/windows/win32/api/handleapi/nf-handleapi-closehandle),
[virtual allocation](https://learn.microsoft.com/windows/win32/api/memoryapi/nf-memoryapi-virtualalloc),
and [virtual release](https://learn.microsoft.com/windows/win32/api/memoryapi/nf-memoryapi-virtualfree) contracts.

### CRT strings and byte memory

The standard registry provides `strlen`, `strnlen`, `strcpy`, `strncpy`,
`strcat`, `strncat`, `memchr`, `strstr`, `strspn`, `strcspn`, and the wide
forms `wcscpy`, `wcsncpy`, `wcscat`, `wcschr` and `wcsrchr` under UCRT and the CRT
string API set. They count bytes using checked guest memory. The scan stops at
a null byte or the requested bound for `strnlen`. A scan that would continue
past 1 MiB reports an unsupported-service fault. See the
[Microsoft CRT string-length contract](https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/strnlen-strnlen-s).

The same modules expose `strcmp` and `strncmp`. These compare unsigned bytes
and stop at the first difference, null byte, or supplied comparison bound.
They share the 1 MiB scan budget. `wcslen`, `wcsnlen`, `wcscmp`, and
`wcsncmp` apply the same rules to unsigned 16-bit Windows code units.
Counts and bounds for the wide variants are in code units. `_stricmp`,
`_strnicmp`, `_wcsicmp` and `_wcsnicmp` use the same rules with C-locale ASCII
case folding only (`A`–`Z`); they are not Unicode case mapping.

The same modules expose `memset`, `memcpy`, `memmove`, and `memcmp` for spans
up to 1 MiB. Each nonempty span must be fully mapped with the required permissions.
A span may cross adjacent guest regions. Writes validate the full destination before changing
bytes, and moves preserve the original source bytes when ranges overlap.

### Guest heaps

`GetProcessHeap`, `HeapAlloc`, `HeapSize`, and `HeapFree` expose the
default synthetic heap. Allocations are at least 16-byte aligned and zero initialized.
A zero-size request allocates one byte. All heaps in an address space share a 64 MiB budget and a limit of
4096 live allocations. Heap and virtual allocations have distinct ownership.
`HeapReAlloc` preserves the retained bytes and zeroes newly added bytes.
It can move a block unless `HEAP_REALLOC_IN_PLACE_ONLY` is set. Failed
resizes preserve the original block. `HeapCreate` supports up to 256 growable private heaps with default flags.
The initial size is checked against the budget and backing is allocated on
demand. `HeapDestroy` frees all blocks owned by that private heap and
invalidates its handle. The default heap cannot be destroyed. See the [Windows heap API reference](https://learn.microsoft.com/windows/win32/api/heapapi/).

UCRT and the CRT heap API set expose `malloc`, `calloc`, `realloc`, and
`free` on the bounded default guest heap. `calloc` checks for size overflow.
`realloc(NULL, size)` allocates a new block, and resizing a nonnull block to
zero frees it. Allocation failures return null and preserve existing blocks.
Allocation failures also set the synthetic main thread's CRT `errno` to
`ENOMEM`. The runtime API set and UCRT expose `_errno`, `_get_errno`, and
`_set_errno` through a stable guest memory slot. Additional guest threads
and CRT parameter or new-handler callbacks remain unsupported. See
[Microsoft CRT allocation documentation](https://learn.microsoft.com/cpp/c-runtime-library/memory-allocation).

### CRT integer conversion

UCRT and the CRT conversion API set expose strtol, strtoul, wcstol, and
wcstoul with 32-bit Windows long results. The parser supports C-locale
whitespace, explicit bases 2–36 and automatic base selection, and writes
the first unparsed guest address to a supplied end pointer. Overflow
clamps the result and sets ERANGE. Scans have a 1 MiB input budget. The long-long variants and
Microsoft's _strtoi64, _strtoui64, _wcstoi64, and _wcstoui64 apply the
same rules with 64-bit results. `atoi`, `atol`, `atoll`, `_atoi64`,
`_wtoi`, `_wtol` and `_wtoi64` reuse that parser with base 10 and no end
pointer; on Windows LLP64, `atol` matches `atoi` (32-bit `long`).

### CRT locale and formatted output

`setlocale` accepts only the C locale (`"C"`, `"POSIX"`, empty string, and
ASCII-case variants of `C.UTF-8`). Querying with a null locale name returns
a durable `"C"` string. Other locale names fail with `EINVAL` and a null
result. `localeconv` returns a guest `lconv` for that C locale. Thread
locales and `_create_locale` remain unsupported.

C-locale `ctype.h` predicates (`isalnum`, `isalpha`, `isdigit`, `islower`,
`isupper`, `isspace`, `isxdigit`, `isprint`, `iscntrl`, `ispunct`) and
`tolower` / `toupper` accept `EOF` or an `unsigned char` value. Other
`int` arguments stop the guest.

`sprintf` and `snprintf` implement a bounded C-locale subset: `%%`, `%c`,
`%s`, `%d`/`%i`, `%u`, `%x`/`%X`, `%p`, optional zero-pad widths, and
`l`/`ll`/`z`/`t` length modifiers. Null string arguments render as
`(null)`. Other conversions stop the guest. Transfers stay within the 1 MiB
service budget.

FILE streams implement `fopen`, `fclose`, `fread`, `fwrite`, `fseek`, `ftell`,
`feof`, `ferror`, `rewind` and `__acrt_iob_func`. Single path components (no
separators) use an in-memory store. Paths with a mounted drive letter open
through the same host mount policy as `CreateFileW`: the file is loaded into a
bounded buffer (up to 16 MiB, and not past the mount's size cap), and a
writable stream replaces the host contents on `fclose`. Up to 32 streams.
Stdin is empty, stdout/stderr discard writes. `printf` and `fprintf` use the
same format subset as `sprintf` (stdout discards the payload and returns the
length). The `__stdio_common_*` helpers remain unimplemented. Concurrent host
edits while a stream is open are not visible; this is a research subset, not
full CRT or Win32 file semantics.

### Process environment

`getenv`, `GetEnvironmentVariableA` and `GetEnvironmentVariableW` read a
runtime-owned environment block (defaults include `OS=Windows_NT`).
`_putenv` (`NAME=value`) and `SetEnvironmentVariableA` /
`SetEnvironmentVariableW` update or delete entries in that block. The host
process environment is not visible unless an entry is installed here.

### CRT calendar time

`time`, `_time64`, `gmtime_s`, `localtime_s`, `_gmtime64_s` and
`_localtime64_s` use the injectable guest clock (the same FILETIME source as
`GetSystemTimeAsFileTime`). There is no guest timezone database: local time
matches UTC and `tm_isdst` is always zero. 32-bit `time` saturates at the
2038 bounds rather than wrapping.

### CRT `rand`

`srand`, `rand` and `rand_s` use the publicly documented MSVC LCG
(`seed = seed * 214013 + 2531011`, return `(seed >> 16) & 0x7fff`). The seed is
process-wide. `rand_s` writes that 15-bit value into a guest `unsigned int`;
it is not a CSPRNG.

### Process-local Winsock UDP and TCP loopback

`ws2_32.dll` exposes a process-local IPv4 UDP loopback: `WSAStartup` /
`WSACleanup`, `socket` (AF_INET / SOCK_DGRAM / IPPROTO_UDP only),
`closesocket`, `bind`, `connect`, `send`, `recv`, `sendto`, `recvfrom`,
`getsockname`, `getpeername`, `getsockopt` (`SO_TYPE`, `SO_MAX_MSG_SIZE`),
`WSAPoll`, byte-order helpers, and `inet_ntop` / `inet_pton` for IPv4.
Datagrams stay in the runtime; nothing reaches the host network stack. Bound
addresses report `127.0.0.1`. At most 64 sockets, 16 pending datagrams per
socket and 65,507 bytes per datagram. `recv` / `recvfrom` are non-blocking
(`WSAEWOULDBLOCK` when empty). `sendto` may auto-bind an ephemeral port.
`WSAPoll` timeouts are capped at 1 second.

TCP streams (`socket` with SOCK_STREAM and protocol 0 or IPPROTO_TCP) connect
only to a listening guest socket on `127.0.0.1`, also inside the process.
`listen` clamps the backlog to 1–16 (SOMAXCONN included); a full queue or a
port with no listener reports `WSAECONNREFUSED`. `connect` completes at once,
and `accept` is non-blocking (`WSAEWOULDBLOCK` with nothing waiting). Each
socket buffers up to 64 KiB of unread bytes; `send` then takes what fits and
reports `WSAEWOULDBLOCK` when nothing fits, without reading the guest bytes it
does not report. `recv` returns 0 at end of file, after the peer's
`shutdown(SD_SEND)` or `closesocket`; sending to a closed peer reports
`WSAECONNRESET`, and `WSAPoll` reports `POLLHUP`. Closing a listener closes
connections it has not accepted. TCP and UDP ports are separate. `sendto`,
`recvfrom` and socket options other than `SO_TYPE` are not supported on
streams, and `shutdown` is not supported on datagram sockets. Raw sockets,
IPv6, blocking waits on streams and host interfaces remain unsupported.
`ioctlsocket` supports `FIONREAD` (unread stream bytes, or the size of the
next datagram; `WSAEINVAL` on a listening socket) and `FIONBIO` with a nonzero
argument. Every socket is already non-blocking, so asking for blocking mode
returns `WSAEOPNOTSUPP` instead of being ignored.

### CRT character and line streams

`fputc`, `putc`, `puts`, `fgetc` and `getc` are registered on `ucrtbase.dll`
and `api-ms-win-crt-stdio-l1-1-0.dll` and share the stream state of `fopen`.
The character functions return the byte as an unsigned char or `EOF`; writing
a read-only stream or reading a write-only one returns `EOF` and sets the
error indicator. `puts` adds a newline; standard output accepts and discards
output. A host-backed stream reaches its mounted file at `fclose`. `fputs`,
`fflush` and `fgets` are left to the separate hosted-C-program stdio tasks.

`ungetc` pushes back one byte after at least one byte has been read: the
position steps back, and the next read returns the pushed-back byte. It
clears the end-of-file indicator and is never written to the file; a seek,
`rewind` or write discards it. Pushing back `EOF`, a second byte, or a byte at
the start of the stream returns `EOF`. C guarantees only one byte of
push-back, and Xenora does not provide more.

### Bounds-checked string copies

`strcpy_s`, `strcat_s`, `strncpy_s`, `strncat_s`, `wcscpy_s`, `wcscat_s`,
`wcsncpy_s` and `wcsncat_s` are registered on `ucrtbase.dll` and
`api-ms-win-crt-string-l1-1-0.dll`. Sizes and counts are in characters. A null
or zero-sized destination returns `EINVAL` and leaves it untouched. A null
source (with a nonzero count), an unterminated append destination, or a result
that does not fit returns `EINVAL` or `ERANGE` and leaves the destination
empty. A count of `_TRUNCATE` copies what fits and returns `STRUNCATE`, which
is not stored in `errno`; a count of zero writes only the terminator. Every
other error is also stored in `errno`. The invalid-parameter handler is not
modelled.

### CRT block sizes and aligned allocation

`_msize`, `_recalloc`, `_aligned_malloc`, `_aligned_realloc`, `_aligned_msize`
and `_aligned_free` are registered on `ucrtbase.dll` and
`api-ms-win-crt-heap-l1-1-0.dll`, using the same CRT heap as `malloc`.
`_msize` reports the requested size. `_recalloc` checks `count * size` for
overflow (`ENOMEM`, block unchanged), zeroes bytes past the old size, and frees
the block for a size of zero.

Heap blocks start on 64 KiB boundaries, so an aligned block is an ordinary
heap block whose address is checked. Alignments that are not powers of two
return `NULL` with `EINVAL`; alignments above 64 KiB return `NULL` with
`ENOMEM` instead of a misaligned block. `_aligned_realloc` resizes in place
when it can and otherwise moves the contents to a new aligned block.
`_aligned_offset_malloc` is not provided, so `_aligned_msize` with a nonzero
offset returns `EINVAL`. Aligned blocks are recorded: passing `_aligned_free`,
`_aligned_realloc` or `_aligned_msize` a pointer that `_aligned_malloc` did not
return, or one already freed, stops the guest with a memory fault. Calls the
UCRT sends to the invalid-parameter handler set `errno` and return the
documented failure value; the handler is not modelled.

### C++ type identity

`__std_type_info_compare`, `__std_type_info_hash`, `__std_type_info_name` and
`__std_type_info_destroy_list` are registered on `vcruntime140.dll`. They back
`typeid` equality, `type_info::before`, `hash_code` and `name`. Each takes the
type descriptor's data: an 8-byte cached-name slot followed by the decorated
name, as in the publicly described MSVC RTTI type descriptor.

Two descriptors with the same decorated name compare equal and hash equally,
so a type seen from two modules is one type. The hash is FNV-1a over the
decorated name and is not claimed to match Microsoft's runtime. `name`
undecorates plain class, struct, union and int-based enum names with their
namespaces (`.?AVWidget@ui@@` becomes `class ui::Widget`), allocates the text
on the guest heap and caches it in the descriptor; `destroy_list` frees those
names. Templates, back-references, anonymous namespaces and non-class types
stop the guest instead of returning a wrong name.

`__std_exception_copy` and `__std_exception_destroy` manage the message record
inside `std::exception`: the message pointer, then a one-byte flag that says
whether the exception owns the text. A copy duplicates an owned message on the
CRT heap, so `what()` stays the same after the original is destroyed, and
shares a borrowed one. If the duplicate cannot be allocated, the copy gets an
empty record. Destroy frees an owned message (guest `free` and these helpers
use the same heap) and clears the record. Destroying an owned pointer the heap
never allocated stops the guest with a memory fault.

Throwing and catching exceptions, `dynamic_cast` and thread-safe static
initialization are not implemented. `terminate` and `_purecall` are described
under abnormal termination below.

### Abnormal CRT termination

`terminate` and `_purecall` (on `ucrtbase.dll` and
`api-ms-win-crt-runtime-l1-1-0.dll`) and `__std_terminate` and `_purecall` (on
`vcruntime140.dll`) end the guest process with exit code 3. Their default
handlers call `abort`, and 3 is the code the Microsoft C runtime documents for
`abort` without a `SIGABRT` handler. They use the same process exit as
`ExitProcess`: handles close, the first exit decides the status, and the host
process never ends. `abort` and `_exit` themselves are left to the separate
CRT exit task.

No handler can be installed: `signal`, `set_terminate` and
`_set_purecall_handler` are not runtime services, so a guest that calls them
stops there instead of getting a handler that is silently ignored. Debug-CRT message
boxes and error reporting are not modelled.

### CRT integer absolute value and division

`abs`, `labs`, `llabs`, `div`, `ldiv` and `lldiv` are registered on
`ucrtbase.dll` and `api-ms-win-crt-utility-l1-1-0.dll`. They follow the
Windows LLP64 model, where `long` is 32 bits, so `labs` and `ldiv` match `abs`
and `div` and ignore the upper half of each argument register. `div_t` and
`ldiv_t` return in RAX with the quotient in the low 32 bits and the remainder
in the high 32 bits. `lldiv_t` is 16 bytes, so the caller passes a result
pointer in RCX, the operands arrive in RDX and R8, and RAX returns the pointer.
Quotients truncate toward zero. The absolute value of the minimum integer
wraps to itself. Division by zero and the minimum divided by -1 stop the guest
with a divide error, as the x86 `idiv` instruction does.

### CRT floating-point string conversion

`strtod`, `strtof`, `wcstod`, `wcstof`, `atof` and `_wtof` are registered on
`ucrtbase.dll` and `api-ms-win-crt-convert-l1-1-0.dll` and return their
result in XMM0. They parse the ISO C grammar in the "C" locale: leading white
space, an optional sign, then decimal digits with an optional `.` and an
optional exponent (consumed only if a digit follows `e`), `inf`, `infinity`,
or `nan` with an optional closed `(n-char-sequence)`, all case-insensitive.
Decimal input is rounded once, directly to the result type, so `strtof` does
not round through `double`. With no conversion the result is 0 and the end
pointer is the string itself.

Overflow returns a signed infinity and sets `ERANGE`, and so do nonzero digits
that round to zero. Whether the UCRT also sets `ERANGE` for a subnormal
nonzero result is not established from public documentation, so Xenora
leaves `errno` unchanged there. A NaN payload is accepted and ignored; the
result is the default quiet NaN with the parsed sign. Hexadecimal floating
constants such as `0x1.8p3` stop the guest rather than being parsed as `0`.

### XInput controllers

`XInputGetState`, `XInputSetState`, `XInputGetCapabilities` and
`XInputEnable` are registered on `xinput1_4.dll`; `xinput1_3.dll` and
`xinput9_1_0.dll` reach the same services (`xinput9_1_0.dll` has no
`XInputEnable`). Readings come from a `ControllerSource` the host connects
with `ServicesState::set_controllers`. Without one, every slot reports
`ERROR_DEVICE_NOT_CONNECTED`. `SimulatedControllers` is a deterministic source
for tests; no physical controller or app input path is connected yet.

The functions return their error code and leave last-error alone: a slot of 4
or more, a null buffer or unknown capability flags return
`ERROR_BAD_ARGUMENTS`, and an unmapped buffer returns `ERROR_NOACCESS`.
`dwPacketNumber` advances only when the reading changes. While
`XInputEnable(FALSE)` is in effect, `XInputGetState` reports a neutral
controller and the motors stop; the last vibration request is applied again
when XInput is enabled. Capabilities report a gamepad with every defined
button, full trigger and stick resolution, and force feedback only when the
source says the controller can vibrate. Battery, keystroke and audio-device
functions are not implemented.

### XAsync helpers

`XAsyncRun`, `XAsyncGetStatus`, `XAsyncCancel` and `XAsyncGetResultSize` are
registered on `xgameruntime.dll`. With a null queue in the `XAsyncBlock`, the
work callback and optional completion callback run on the calling thread
before `XAsyncRun` returns (immediate dispatch). With a non-null manual
`XTaskQueue` handle, work is submitted to the work port and completion to the
completion port; the guest must `XTaskQueueDispatch` those ports.
`XAsyncGetStatus` with `wait=true` returns `ERROR_NOT_SUPPORTED`.

### Built-in guest modules

GetModuleHandleA and GetModuleHandleW recognize synthetic Kernel32 and UCRT
module names and their registered API-set aliases. GetProcAddress resolves
case-sensitive named exports to guest service trampolines. The registry
captures built-in bindings at standard service registration and excludes
stubs. Missing names and unsupported ordinals fail explicitly. These APIs
expose synthetic module handles and never load host libraries.
`GetModuleFileNameA` / `GetModuleFileNameW` return synthetic paths:
`C:\xenora\guest.exe` for a null handle, and
`C:\Windows\System32\<module>` for built-in DLL handles. They do not reveal
host filesystem paths.

### Synthetic guest files

The runtime exposes an in-memory C: drive through CreateFileW, ReadFile,
WriteFile, GetFileSizeEx, and CloseHandle. It supports UTF-16 file and
directory names, ASCII case folding, synchronous I/O, per-open file positions, and
read/write sharing. SetFilePointerEx supports seeking from the beginning,
current position, and end. SetEndOfFile truncates or zero-extends writable
files at the current position. Seeks alone do not change file length.
Paths cannot access host files. The drive supports
256 files and 16 MiB of data, with a 1 MiB limit per transfer.
CreateDirectoryW and RemoveDirectoryW manage up to 256 directories. Parents
must exist, and directories must be empty before removal. DeleteFileW
deletes closed files. MoveFileW moves closed files to an unused name under
an existing directory. GetFileAttributesW distinguishes files and directories.
Traversal components and device names are rejected.
FindFirstFileW, FindNextFileW, and FindClose enumerate a sorted snapshot of
direct children. The final path component accepts literal names, * and ?
wildcards, and *.* matches every name. Results include attributes, byte size,
and a UTF-16 name. Times and alternate names are zero. Up to 32 searches
can be open. Same-process file handle duplicates share the file position and keep the
original sharing reservation until the last duplicate closes. Access can
be reduced per handle. DOS wildcard edge cases, persistence, and
asynchronous I/O remain outside this subset.

### Host-backed guest drives

`ServicesState::mount` exposes a host directory to the guest as a drive
letter from D: to Z:, through `HostMount::new(drive, root, writable)`. Without
a mount, nothing on the host is reachable through these services. Mounts are
read-only unless created writable.

The partial `GetFileAttributesExA/W` services return attributes and 64-bit
sizes for `GetFileExInfoStandard`. Host-mounted entries carry access and
modification timestamps and creation time where the host supplies it.
Synthetic files use the guest clock and accept explicit timestamp changes. Directory enumeration uses the same
timestamp representation. `GetFileTime` reads these values from an open
file with read access and accepts null pointers for omitted values.
The partial `SetFileTime` service updates access and modification times on
writable mounted files. On synthetic files it also accepts creation-time
updates. Null inputs and zero FILETIMEs leave fields unchanged. Host
creation-time updates and host timestamp suppression are unsupported.
Synthetic handles accept all-ones access and write times to suppress
automatic updates for that open file.
Synthetic files update access time after a nonempty read and modification
time after a nonempty write or truncation.
The ANSI form accepts ASCII paths.

The partial `FlushFileBuffers` service requires a writable file handle.
It synchronizes the backing file on a host mount. Synthetic file writes
are already visible in the guest's in-memory filesystem.

The ANSI forms `CreateFileA`, `DeleteFileA`, and `GetFileAttributesA`
accept ASCII paths and use the same guest namespace and mount rules as the
wide forms.

The partial `GetTempPathA/W` services use the first nonempty guest value
among `TMP`, `TEMP`, and `USERPROFILE`, falling back to the synthetic
`C:\` root. They add a trailing separator without checking whether the
directory exists. The narrow entry point accepts ASCII. No host environment
is consulted.

The partial `GetTempFileNameW` service accepts ASCII prefixes on the synthetic
drive and explicitly mounted drives. Automatic creation requires a writable
mount and uses exclusive creation inside the resolved directory. A nonzero unique value formats a name without creating a file. A zero
value picks an available name using the guest clock and creates an empty file
without retaining a handle. Callers can remove it with `DeleteFileW`.

The confinement policy below is enforced in `services/host_files.rs` and
covered by `crates/loader/tests/runtime_host_files.rs` and unit tests in that
module. Guest path confinement for this boundary was reviewed by Cursor
security-review and implementation-review agents (not a human audit).

- **Path resolution.** The root directory is opened once with
  `O_DIRECTORY | O_NOFOLLOW`, so a symlink mount root is rejected. Every
  operation resolves the guest path from that descriptor one component at a
  time with `openat` and `O_NOFOLLOW`. `.` and `..` components, device names,
  Windows-invalid characters and drive-relative paths are rejected. Names
  match the host listing ignoring ASCII case. Two host entries that match
  the same name under this rule are refused as ambiguous. Non-ASCII letters
  must match the spelling in the listing. If the host filesystem treats a
  different spelling as a collision, exclusive file creation returns
  `ERROR_FILE_EXISTS` and directory creation returns `ERROR_ALREADY_EXISTS`.
- **Hidden entries.** The guest cannot see or open symbolic links, devices,
  sockets, FIFOs, non-UTF-8 names, or regular files with more than one hard
  link. Hiding multiply linked files prevents a hard link from aliasing a
  host file outside the root. The cost is that legitimately hard-linked files
  are hidden too.
- **Descriptor verification.** After each `openat`, the descriptor is
  checked with `fstat`: it must be the expected type, a file must have one
  link, and the device and inode must match what the listing reported. A host
  change that swaps an entry between the listing and the open fails with
  `ERROR_ACCESS_DENIED`. Removal opens and re-verifies the entry, re-checks
  the directory listing, then uses name-based `unlinkat`. A concurrent host
  rename in that window can still remove a different entry inside the mount
  (never outside it). Host writers under the mount are trusted for that
  residual race. A file unlink that did not drop the held inode's link count
  fails closed; directories have no post-unlink check.
- **File identity.** Reads, writes and truncation use the verified
  descriptor, so they reach the same file after a host rename. Sharing checks
  and the open-file check in DeleteFileW compare the live device and inode of
  every open handle, not guest paths. A truncating open is checked for
  sharing before it truncates. If an open handle's identity cannot be read,
  the check treats it as the same file, so failures deny access rather than
  grant it.
- **Read-only enforcement.** Read-only mounts reject creation, truncation,
  writes, deletion and directory changes with `ERROR_ACCESS_DENIED`. They also
  open every file `O_RDONLY`, so the host kernel refuses writes as well.
- **Resource limits.** A directory listing is bounded at 4,096 visible
  entries. Larger directories report `ERROR_NOT_ENOUGH_MEMORY` rather than a
  partial listing. Open handles share the 1,024-handle object table.
  Transfers are limited to 1 MiB per call. On writable mounts, writes and
  truncation cannot make a file larger than the mount's limit
  (`HostMount::with_max_file_size`, 1 GiB by default); they report
  `ERROR_DISK_FULL` instead.
- **Mount quota.** `HostMount::with_quota(bytes, entries)` optionally bounds
  the net bytes and the net number of files and directories that guest
  operations add to a writable mount, relative to its contents when mounted.
  Writes, extending `SetEndOfFile`, `CreateFileW`, `CreateDirectoryW`,
  `GetTempFileNameW` and CRT `fopen` are charged; shrinking, `DeleteFileW`
  and `RemoveDirectoryW` give the amount back, never below zero. An operation
  that would exceed a limit fails with `ERROR_DISK_FULL` (CRT: `ENOSPC`) and
  leaves the host unchanged. CRT streams are charged when `fclose` writes them
  back. Changes made by host processes are not counted, and
  `HostMount::usage` reports the current amounts. No quota is set by default.
- **Errors.** Errors from reading a directory or examining an entry are
  reported, except an entry removed while it is being listed. Host errors map
  to specific Win32 codes (for example `ENOENT` to `ERROR_FILE_NOT_FOUND`,
  `ENOTEMPTY` to `ERROR_DIR_NOT_EMPTY`, `ENOSPC` to `ERROR_DISK_FULL`);
  unrecognized errors report `ERROR_GEN_FAILURE`.

Not covered: another host process that moves a directory or file the guest
already holds open out of the mount. The guest keeps its handle to that
object, because handles refer to opened files, not paths. Host processes with
write access to a mounted directory are therefore trusted if confinement
matters against concurrent host writers. Name-based removal still has a short
in-mount TOCTOU against such writers (check-then-unlink by name). Some file
systems do not keep inode numbers stable (exFAT renumbers a file when its
storage is first allocated), so identities are compared live rather than
cached; if an open handle's identity cannot be read, sharing checks fail
closed.

CreateFileW, ReadFile, WriteFile, GetFileSizeEx, SetFilePointerEx,
SetEndOfFile, GetFileAttributesW, FindFirstFileW and FindNextFileW work on
mounted drives with the same handle types, rights, sharing modes and file
positions as the synthetic drive. CreateDirectoryW, RemoveDirectoryW and
DeleteFileW also work on writable mounts. Files on read-only mounts report
`FILE_ATTRIBUTE_READONLY`. MoveFileW renames a closed file or a directory
within one writable mount with an exclusive host rename, so it never replaces
an existing name (`ERROR_ALREADY_EXISTS`); a change of case alone renames in
place. An open file reports `ERROR_SHARING_VIOLATION`, since
`FILE_SHARE_DELETE` is not supported. MoveFileExW accepts
`MOVEFILE_REPLACE_EXISTING`, which atomically replaces a closed file (never a
directory, `ERROR_ACCESS_DENIED`), `MOVEFILE_WRITE_THROUGH`, which flushes both
host directories before returning, and `MOVEFILE_COPY_ALLOWED`; other flags
return `ERROR_NOT_SUPPORTED`. The synthetic C: drive follows the same rules
for files. Moves between drives return `ERROR_NOT_SAME_DEVICE`, or
`ERROR_NOT_SUPPORTED` with `MOVEFILE_COPY_ALLOWED` (and from MoveFileW), since
the copy Windows would then make is not implemented. As with deletion, the source
is matched by name after a final listing check, so a concurrent host rename
can move a different entry inside the mount, never one outside it.
Times, alternate names and asynchronous I/O are not reported. The mount
tests need a temporary directory that supports hard links and POSIX
permissions: `/tmp`, or the directory named by `XENORA_TEST_TMP`.

### Event and semaphore signaling

The synthetic runtime supports unnamed manual-reset and auto-reset events
created with default security attributes. `SetEvent` signals an event,
`ResetEvent` clears it, and `CloseHandle` invalidates the handle.
`WaitForSingleObject` consumes an auto-reset signal once, leaves a manual-reset
signal set, and reports `WAIT_TIMEOUT` when a zero-timeout poll finds no signal.
Without an attached thread scheduler, a wait that would block returns `WAIT_FAILED` with `ERROR_NOT_SUPPORTED`; with one it blocks the calling guest thread ([guest threads](runtime/threads.md)).
Thread and event handles are distinct types in the same bounded table.
Event duplicates share signal state and keep the event alive independently.
A duplicate can retain or reduce its rights. Waiting requires synchronize
access, and setting or resetting requires modify-state access.

`WaitForMultipleObjects` supports immediate waits on up to 64 events or semaphores.
Wait-any selects the first signaled handle. Wait-all consumes auto-reset
signals only when every object is ready. Repeated handles and wait-all
aliases to the same event are rejected. The extended single- and multiple-
object wait APIs support the same immediate behavior when alerting is
disabled. Alertable calls and waits that would block return an unsupported
error.

`CreateSemaphoreA` and `CreateSemaphoreW` create unnamed semaphores
with default security attributes. A successful wait consumes one permit.
`ReleaseSemaphore` can return the previous count and rejects a release
that exceeds the maximum without changing the count. Duplicates share
the same permit count.

`CreateMutexA`, `CreateMutexW` and their `Ex` forms create unnamed mutexes,
optionally owned by the caller. Ownership belongs to the calling guest
thread, identified by its TEB. The owner can acquire the mutex again, and
each acquisition needs a matching `ReleaseMutex`. Another thread sees the
mutex as unsignaled. A release by a thread that is not the owner fails with
`ERROR_NOT_OWNER`. Waits, duplicates and wait-all treat mutexes like the
other waitable objects. A mutex whose owner thread ended
is reported once as abandoned.

See Microsoft's [event object documentation](https://learn.microsoft.com/windows/win32/sync/event-objects).

### Locks, one-time initialization and thread-local storage

These services keep their state in the runtime and key ownership by guest
TEB. These objects do not block through the guest scheduler yet, so any call that would block stops the
guest with the runtime-limit fault (`IllegalInstruction(0xFE)`) rather than
reporting success. Guest code reaches that fault by entering a critical
section owned by another thread, acquiring a held SRW lock, or beginning a
one-time initialization that another thread has pending.

- Critical sections use the x64 `RTL_CRITICAL_SECTION` layout from
  `winnt.h`. Initialization writes the 40-byte structure. `LockCount` is -1
  when free and -2 when held; the runtime also updates `RecursionCount` and
  `OwningThread`. The owner may enter recursively. `TryEnterCriticalSection`
  returns FALSE when another thread owns it. Leaving a section the caller
  does not own, or using one that was never initialized, stops the guest.
  The runtime holds up to 4,096 sections.
- SRW locks allow any number of shared holders or one exclusive holder. The
  try forms return FALSE instead of waiting. Exclusive acquisition is not
  recursive. Releasing a mode the caller does not hold stops the guest.
- Condition variables check that the caller holds the named lock. A
  zero-timeout sleep returns FALSE with `ERROR_TIMEOUT` and leaves the lock
  held in its original mode. Longer sleeps return `ERROR_NOT_SUPPORTED`.
  Wake calls do nothing, because no thread can be sleeping.
- `InitOnceBeginInitialize` and `InitOnceComplete` support the synchronous
  mode, `INIT_ONCE_CHECK_ONLY` and `INIT_ONCE_INIT_FAILED`. The completed
  context is stored in the `INIT_ONCE` word, so later calls read it back.
  Asynchronous mode returns `ERROR_NOT_SUPPORTED`. `InitOnceExecuteOnce`
  calls the guest initializer once with the object, the parameter and a
  context pointer. A FALSE result, or a context that uses the two reserved
  low bits, leaves the object uninitialized so a later call can retry.
- Interlocked SLists use the x64 `SLIST_HEADER` layout. The low 16 bits hold
  the depth, and bits 4 to 63 of the second quadword hold the first entry.
  Headers and entries must be 16-byte aligned. Updates are atomic with
  respect to other services. They are not atomic with respect to guest
  instructions that modify the header directly on another host thread.
- TLS provides the documented 1,088 indexes. FLS provides the 128
  `FLS_MAXIMUM_AVAILABLE` slots defined in the MinGW-w64 headers. Values are
  per guest thread, new indexes read as zero everywhere, and successful get
  calls clear the last error. Values live in the runtime, not in the TEB
  `TlsSlots` array, so guest code that reads the TEB directly does not see
  them. `FlsFree` releases the slot, then calls its cleanup callback on the
  calling thread for each thread's nonnull value, in TEB order.

### System information, calendar time and sleeping

The guest sees a simulated machine described by `GuestSystemProfile` on
`ServicesState`, and never the host's hardware. `HostCapabilities::detect`
reports the Mac's logical processors, physical memory and page size
separately, for diagnostics.

- `GetSystemInfo` and `GetNativeSystemInfo` report an AMD64 processor
  (`PROCESSOR_ARCHITECTURE_AMD64`, `PROCESSOR_AMD_X8664`), 4 KiB pages,
  64 KiB allocation granularity and the x64 user address range
  `0x10000`–`0x7FFFFFFEFFFF`. The processor count comes from the profile and
  defaults to one, because guest code runs on one thread at a time. The
  processor level and revision are zero, because no CPU model is simulated.
- `IsProcessorFeaturePresent` answers from the same fixed table that guest
  `CPUID` returns, so the two always agree. A feature is present only when
  every CPUID bit it depends on is set; today that table sets none of the
  features the function can report, so every answer is `FALSE`. Features
  with no CPUID test, such as `PF_NX_ENABLED` and `PF_FASTFAIL_AVAILABLE`,
  and unknown numbers report `FALSE`.
- `GetLogicalProcessorInformation` reports one core per profile processor
  and one package holding them, 32 bytes each. No cache or NUMA entries are
  reported, because no cache hierarchy is simulated. A short buffer fails with
  `ERROR_INSUFFICIENT_BUFFER` and the required length.
- `GetTimeZoneInformation` reports UTC: zero bias, both names "Coordinated
  Universal Time", no daylight transition, and `TIME_ZONE_ID_UNKNOWN`.
- `GlobalMemoryStatusEx` requires `dwLength` to be 64. Guest physical memory is
  the virtual-allocation and heap budgets together, 128 MiB. Available memory
  and `dwMemoryLoad` reflect current guest allocations. There is no paging
  file, and the virtual totals cover the x64 user range.
- `GetSystemTime`, `GetLocalTime` and `GetSystemTimePreciseAsFileTime` read
  the injected clock. The guest time zone is UTC, so local time equals system
  time. `FileTimeToSystemTime` and `SystemTimeToFileTime` convert using the
  proleptic Gregorian calendar. They reject FILETIME values with the top bit
  set and invalid dates (years outside 1601–30827), ignore the day of week on
  input, and return `ERROR_INVALID_PARAMETER` without writing output.
- `QueryUnbiasedInterruptTime` reports 100 ns ticks since the service state
  was created.
- `Sleep` and `SleepEx` suspend the calling thread through the clock, so the
  mock clock advances instead of waiting. No APCs exist, so an alertable sleep
  always returns zero. `INFINITE` and sleeps longer than 10 seconds stop the
  guest with the runtime-limit fault, because a single guest thread could not
  be woken.

### Services that call guest code

A service registered as a callback task can call guest functions before it
returns. The dispatcher reserves 72 bytes of guest stack below the caller's
return address. That space holds the callback's return address, its 32-byte
home area and 16 bytes of scratch owned by the service, and it keeps RSP at 8
modulo 16 on entry, as after a CALL. Each callback receives up to four
integer register arguments and must preserve the Win64 nonvolatile
registers, or the guest stops with `ServiceAbiViolation`. The callback's RAX
is passed to the task, which either calls guest code again or returns a value
to the original caller with its RSP restored. Up to 32 callback frames may be
active. Callback entry points must be executable. The earlier one-shot
dispatch used by the GDK task queue is unchanged.

The CRT uses this for:

- `qsort` and `qsort_s`, which sort up to 1 MiB in place with a guest
  comparator. Elements stay in place while the comparator runs and are
  permuted once at the end. `qsort_s` passes its context as the first
  comparator argument.
- `bsearch` and `bsearch_s`, which return a pointer to a matching element
  or null.
- `_initterm` and `_initterm_e`, which call each nonnull entry of an
  initializer table in order. `_initterm_e` stops at the first nonzero
  result and returns it. Tables hold at most 65,536 entries.

- `_initialize_onexit_table`, `_register_onexit_function` and
  `_execute_onexit_table` manage `_onexit_table_t` tables. Their storage is
  on the default guest heap, and they hold at most 65,536 entries. Execution
  resets the table first, then calls the functions in reverse order of
  registration. `atexit` and `_crt_atexit` record process exit functions,
  which `_cexit` runs in reverse order. Process termination through `exit` is
  not modeled.

Null comparators, null arrays with a nonzero count, and reversed or
misaligned tables would reach the CRT invalid-parameter handler. That
handler is not modeled, so these inputs stop the guest with the
runtime-limit fault.

API-set aliases follow Microsoft's
[Win32 APIs for UWP apps](https://learn.microsoft.com/uwp/win32-and-com/win32-apis)
listing: critical sections, SRW locks and mutexes use
`api-ms-win-core-synch-l1-1-0`; condition variables and one-time
initialization use `api-ms-win-core-synch-l1-2-0`; SLists use
`api-ms-win-core-interlocked-l1-1-0`; TLS uses
`api-ms-win-core-processthreads-l1-1-0`; and FLS uses
`api-ms-win-core-fibers-l1-1-0`.

## Synthetic tiled textures

The texture crate reads original fixtures in an array-layer-major layout. Each
layer stores a consecutive mip chain. Within each mip, row-major 2×2 tiles store
four row-major Block Compression 1 (BC1) blocks, including slots beyond the
logical edge. The reader validates the full byte length, rejects padding
coordinates and can decode each logical block to sixteen RGBA8 texels.
The decoder uses bit replication to expand 5:6:5 endpoints and rounds integer
palette interpolation down. This layout is specific to Xenora fixtures. The block encoding follows the
[Direct3D BC1 description](https://learn.microsoft.com/en-us/windows/win32/direct3d10/d3d10-graphics-programming-guide-resources-block-compression).

`TiledBc1::linearize` strips the tile padding from one selected subresource
while retaining complete compressed blocks at logical image edges.
`NativeCompute::read_tiled_bc1` checks the pixel budget and the Metal device's
BC texture capability, then uploads the selected blocks as a single
BC1 RGBA texture. A fixed Metal compute kernel reads each texel and returns
row-major RGBA8 words. The texture path uses authored MSL and takes its
input from the host fixture.

## Persistent guest graphics resources (X9)

A separate fixed-base PE32+ test profile exposes `xenora_resource`. RCX
points to a readable, four-byte-aligned request of five little-endian
32-bit words (operation, handle, offset, length and kind). RDX carries a
guest data address for uploads and readback. Other operations require it
to be zero. Handle values encode a slot and generation. Every guest run
owns a separate eight-slot table. Slots can be reused only with a new
generation. Guest addresses and handles never expose host pointers.

| Operation | Request data |
| --- | --- |
| 1, create buffer | Length in bytes and kind 1 (compute words) or 2 (draw vertices). RDX points to initial data. |
| 2, update buffer | Handle, byte offset and length. RDX points to source bytes. |
| 3, destroy resource | Handle. A queued snapshot retains its bytes after destruction. |
| 4, queue compute | Handle, byte offset and length. The selected view binds the original translated XOR compute fixture. |
| 5, complete and read back | Exact result length and a writable RDX destination. |
| 6, queue draw | Handle, byte offset, length 96 and program ID 1 (constant color) or 2 (interpolated color). The input contains three float4 clip positions followed by three float4 colors. Readback is 4×3 row-major RGBA8. |
| 7, create texture | Length of compressed data in bytes. RDX points to eight descriptor words followed immediately by the compressed data. |
| 8, queue texture read | Handle, array layer in the offset field and mip index in the length field. Completion returns row-major RGBA8. |

The texture descriptor contains width, height, mip count, layer count,
format (1 for BC1), usage (1 for sampling), row pitch and slice pitch.
The row pitch is aligned to the eight-byte BC1 block size. Each mip
occupies one slice and uses the same pitches. Slices appear in mip order
within each layer. The whole descriptor and payload must occupy one
readable guest span. Textures are immutable. This is an original linear
upload contract for the test profile. Opcode 8 uploads the selected
subresource and uses an authored Metal texel reader.

Buffers and compressed texture payloads are limited to 4096 bytes.
Queueing captures one immutable view. Completion validates the guest
destination before submitting GPU work and waits for Metal before writing
the result. An update or destruction after queueing cannot change the
captured bytes. Guest exit drops resources and any queued view. Data persists
across guest calls in host memory and is uploaded to Metal for execution.
Persistent device allocations and GPU work in flight across guest calls
are outside this interface.

### Translated offscreen stages

`compile_graphics` decodes two original gfx1030 byte streams and links
one vertex position export, an optional four-component parameter export,
and one pixel color export. Buffer 0 supplies positions to vertex vector
registers 0–3 and buffer 1 supplies colors to registers 4–7. The pixel
stage can consume parameter zero through matching, adjacent interpolation
phase pairs. This maps to center-perspective interpolation in Metal.
The vertex stage supports a bounded set of 32-bit floating-point add and
multiply forms. The graphics ABI also supports register and finite
literal moves. Unsupported stage instructions or export combinations
fail explicitly. The Xbox graphics ABI, hardware-specific floating-point
modes, image instructions, and additional varyings remain unsupported.

## Graphics draw and blend contracts

`NativeCompute::render_indexed` issues one real `drawIndexedPrimitives` call
per draw (up to 16) on a 16- or 32-bit index buffer with a start index and a
signed base vertex, into an offscreen RGBA8 target. A draw can sample an
original BC1 texture uploaded natively. Before encoding anything it rejects
malformed draws; execution checks each draw in turn.

`NativeCompute::render_blend` draws up to 16 flat-coloured triangles in order
into an offscreen RGBA8 target. Each draw has its own colour write mask and
either blending disabled or separate RGB and alpha blend factors and
operations. Colours are compared per channel against expectations derived
independently from the public Metal blend equations.

`NativeCompute::render_depth` draws up to 16 triangles in order with an
optional `Depth32Float` attachment, a chosen clear depth and a per-draw depth
compare function. Colour and the stored depth are both read back.

These passes are checked on the GPU with Metal validation against expectations
derived independently from the public semantics; synthetic fixtures only.

### Guest texture binding (descriptor tables)

`GraphicsContext` lets a guest command list sample one texture in the pixel
stage through a D3D12-shaped binding. A draw samples only if a root signature
is bound. Draws without one keep the flat-colour path. The supported subset
is fixed. Anything outside it fails with `GraphicsError::Unsupported` when the
object is created, or with an error at execution. A rejected draw never
reaches the backend. Draws recorded before it in the same list have already
run.

| Object | Supported |
| --- | --- |
| `create_descriptor_heap` | Shader-visible `CBV_SRV_UAV` (0) or `SAMPLER` (1) heaps of 1–64 descriptors. RTV, DSV and CPU-only heaps are unsupported. |
| `create_committed_texture2d` | One level of `R8G8B8A8_UNORM` (28) or `BC1_UNORM` (71), 1–64 texels per side. BC1 sizes must be multiples of four. Rows are linear: 4 bytes per texel, or 8 bytes per 4×4 block. Xbox tiling is not handled. |
| `create_shader_resource_view` | A `Texture2D` view of mip 0 of a single-level, single-slice 2D texture, in the resource's own format. |
| `create_sampler` | Point filtering (`MIN_MAG_MIP_POINT`), clamp addressing on U, V and W, no LOD bias and a minimum LOD of 0. |
| `create_root_signature` | Exactly two pixel-visibility descriptor tables, in either order: one with a single SRV range for `t0`, and one with a single sampler range for `s0` (one descriptor each, space 0). Root constants, root descriptors, CBV and UAV ranges, extra tables or ranges and static samplers are unsupported. |
| `cmd_set_descriptor_heaps` | At most one heap of each type. |
| `cmd_set_graphics_root_descriptor_table` | The base must be a GPU handle to a slot of the bound heap of the table's type. The slot must hold a descriptor when a draw reads it. Changing the heaps or binding a different root signature clears every table. Setting the same heaps or the same root signature again keeps them. |

Descriptor handles follow D3D12 pointer arithmetic: the heap start plus
`index × GetDescriptorHandleIncrementSize` (32 here; guests must query the
value). `offset` returns `None` rather than carry into the next heap. CPU and
GPU handles are separate spaces. A textured draw is a `DrawInstanced` of
one instance of three vertices with start instance 0, starting at any vertex. Zero vertices or
instances draw nothing. The sampled texture must be in `COMMON` or a read-only state that includes
`PIXEL_SHADER_RESOURCE`. The software reference draws at most 4096 pixels. Each vertex record holds a float4 position
at byte 0 and a float4 texture coordinate at byte 16, with a stride of at
least 32. Textured indexed draws are unsupported.

The native backend receives a `TexturedDraw` with the bound level and must
bind it to fragment texture 0 and sampler 0, which are the `tex0` and `samp0` arguments of
the translated `image_sample_lz` pixel stage. `NativeCompute::render_rgba8_sampled_bc1`
binds a BC1 level natively for that purpose. The software reference has no
clipping or depth test. A pixel is covered when its centre lies inside or on
an edge of the triangle. The texture coordinate is interpolated affinely, so
`w = 1` is required, and point sampled with
`min(floor(clamp(u, 0, 1) × width), width − 1)`. It decodes BC1 with the
spec-exact `xenora_texture::decode_bc1`. Apple's hardware rounds BC1's two
interpolated palette entries differently (171/84 against 170/85, measured
on an M5 Pro), so the tests use endpoint colours only. Pipeline
state objects do not exist yet, so the backend, not the guest, chooses the
pixel shader. These entry points are not yet wired to guest imports.
