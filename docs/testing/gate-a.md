# Gate A: a compiled C program

Gate A asks for an independently compiled C program, exercising allocation,
file operations, arithmetic, function calls and error handling, to run through
Xenora. The maintainer split it into two milestones. Gate A is complete only
when both pass.

| Milestone | What it is | Status |
| --- | --- | --- |
| A1 | An original C program with no C library linked, importing documented `kernel32.dll` and `ucrtbase.dll` functions, run through the loader and the ARM64 DBT with proper process exit | Passes locally; awaiting independent review |
| A2 | A hosted C program: a real C runtime startup, the standard C library, `argc` and `argv`, stdio and formatted output, normal exit and runtime cleanup | Passes locally; awaiting independent review |

## A1

The program is [`tests/compat_fixtures/gate_a1.c`](../../tests/compat_fixtures/gate_a1.c).
Its build configuration and provenance are in
[`tests/compat_fixtures/README.md`](../../tests/compat_fixtures/README.md#gate-a1-a-c-program-with-no-c-library).
The gate is `crates/loader/tests/gate_a1.rs`:

```sh
cargo test --locked -p xenora-loader --test gate_a1 -- --nocapture
```

### Toolchain setup

A1 needs a native-ARM64 clang and an LLD. The build script
([`tests/compat_fixtures/build-gate-a1.sh`](../../tests/compat_fixtures/build-gate-a1.sh))
discovers them in this order:

1. `LLD_LINK` if set;
2. Homebrew `lld`, then the `lld-link` beside a Homebrew `llvm`;
3. `lld-link` on `PATH`;
4. the `rust-lld` inside the active Rust toolchain.

Homebrew's `rust` formula does not ship `rust-lld`, so a Homebrew Rust
installation alone is not enough — install one of the others, for example:

```sh
brew install llvm lld
```

If nothing is found, the build fails with the install hint rather than
skipping the gate. The discovered tools are checked to be ARM64 Mach-O
executables and the binaries they link against.

The test builds the program from source, then runs each build through
`run_synthetic_startup` with `register_standard_services`, loaded away from its
preferred base. It checks, at `-O0`, `-O1`, `-O2` and `-Os`:

- the 20 imports are bound and the 6 base relocations applied;
- the guest ends with `ExitReason::ProcessExit(0x00A1A1A1)`, all 32 bits;
- it ran as translated code, with service calls;
- the text report it wrote matches a report recomputed in Rust;
- the data file holds the 64 sorted records, the file it looked for does not exist, and the handle it left open was closed by the exit;
- its result block in guest memory holds the expected values, the block it kept is readable and the block it freed is unmapped.

Three controls must fail in the stated way: a wrong expectation exits with the
number of the failed check, an import that no service provides is refused
before entry, and a step budget that is too small is not an exit. A variant
whose entry point returns its status gives the same result as `ExitProcess`.

### What A1 does not show

- It is one program. Local runs used Homebrew clang 23.1.6 and Apple clang 17.0.0, each with LLD 23.1.5. Another compiler or a larger program can need instructions and services this one does not.
- Most services it calls are marked partial in the registry. They did what this program needs.
- It runs in the DBT only. The interpreter has no import dispatch.
- The gate itself is a test. The same program can be run by hand with `xenora run`; see [Running a program from the command line](#running-a-program-from-the-command-line).
- Nothing here concerns a commercial game or an Xbox launch environment.

## A2

The program is [`tests/compat_fixtures/gate_a2.c`](../../tests/compat_fixtures/gate_a2.c).
Its build configuration and provenance are in
[`tests/compat_fixtures/README.md`](../../tests/compat_fixtures/README.md#gate-a2-a-hosted-c-program).
The gate is `crates/loader/tests/gate_a2.rs`:

```sh
cargo test --locked -p xenora-loader --test gate_a2 -- --nocapture
```

"Hosted" means the program is linked with a real C runtime, whose own startup
code runs before `main`. Here that runtime is mingw-w64's, in its UCRT
flavour: the startup objects and a little of GCC's support library are linked
into the executable, and the C library itself is imported by name from the
Universal C Runtime's API-set DLLs and from `kernel32.dll`. Xenora's services
answer those imports. Nothing from Windows is loaded.

The test builds the program from source with `x86_64-w64-mingw32-gcc`. It
fails, and does not skip, when that compiler is missing. It then runs each
build through `run_synthetic_startup` with `register_standard_services`,
loaded away from its preferred base, with the command line
`gate-a2.exe 2026 xenora` set through `ServicesState::set_program_arguments`.
It checks, at `-O0`, `-O1`, `-O2` and `-Os`:

- every import is bound (50 at each level with the toolchain used locally; the test prints the count) and base relocations are applied;
- the guest ends with `ExitReason::ProcessExit(0)`: `main` returned and the runtime's startup code passed its result to `exit`;
- captured standard output equals, byte for byte, text recomputed in Rust from the description of the program, including the line a function registered with `atexit` prints after `main` has returned; standard error is empty;
- the report file the program wrote with `fprintf` and `fputs` is on the in-memory drive with the recomputed contents, the file it looked for was not created, and no handle is left open;
- it ran as translated code, with service calls.

The program checks its own results as well: it prints its arguments, fills
heap memory from a small generator, sorts it with `qsort` and a callback,
checksums it before and after, writes the report, reads it back with `fgets`,
`strtoul` and `strtoull`, and confirms that a missing file is reported. It
exits with the number of the first check that fails, and counts the checks
that passed; a complete run counts 76.

Seven more tests guard against a runtime that reports success for everything:

- another seed and label give another output and another report;
- four wrong command lines each fail the check that looks at them, with that check's number as the exit status and its message on standard error;
- a variant with one wrong expectation exits with 22;
- a variant that calls `exit(42)` half way ends with 42, its `atexit` function still runs, and the report is never written;
- a variant that calls `abort()` half way ends with 3 and its `atexit` function does not run;
- a step budget that is too small is not an exit;
- the Rust restatement of the program is itself checked not to be degenerate.

### What A2 does not show

- It is one program and one C runtime: mingw-w64 14.0.0 with GCC 16.2.0 locally. A program built with Microsoft's compiler starts differently and imports other functions; none has been tried. A larger program can need instructions and services this one does not.
- The same source built natively with Apple clang prints the same text once carriage returns are removed. That comparison was made by hand and is not part of the test. Nothing was compared with Windows or with x86 hardware.
- The program formats no floating point and reads no input. `printf` has since gained the floating-point conversions (#454); it still refuses the wide-character ones, and `stdin` and the `scanf` family do not exist.
- Most services the program calls are marked partial in the registry. No locale exists, no signal handler or exception filter is ever called, the environment table is empty, and a C stream is unbuffered and has no seek. They did what this program needs.
- `VirtualProtect`, `VirtualQuery` and `__C_specific_handler` are stubs that stop the guest if called. The program imports them and never calls them.
- It runs in the DBT only, on one guest thread.
- The gate itself is a test. The same program can be run by hand with `xenora run`; see [Running a program from the command line](#running-a-program-from-the-command-line).
- Nothing here concerns a commercial game or an Xbox launch environment.

## Running a program from the command line

`xenora run` runs any PE32+ executable through the loader, the standard
services and the ARM64 DBT (#439; the maintainer's decision on #257 was to
allow any executable):

```sh
sh tests/compat_fixtures/build-gate-a2.sh artifacts/gate-a2
cargo run --locked -p xenora -- run --experimental \
    --pe artifacts/gate-a2/gate-a2-O2.exe -- 2026 xenora
```

What follows `--` reaches the program after its own file name. The program's
standard output and standard error are written to the host's, and the command
exits with the program's exit status.

- **Which path a file takes.** One of the two embedded fixtures, given as it
  is, keeps its own path: its draw is verified and its result is checked, as
  before. Every other file, and any file given a digest or program arguments,
  runs as a program. A fixture with one byte changed is a program, gets no
  draw service, and stops at its unresolved import.
- **An optional digest.** With `--sha256 <64 hex digits>` the file runs only
  if its SHA-256 is the one given. The file is read once and the bytes that
  were hashed are the bytes that run.
- **Limits.** The program runs in a separate worker process with a wall
  deadline: 15 seconds, or `--wall-seconds <1..=3600>` (#448). It has no step
  limit unless `--max-steps <n>` gives one; a limit that is reached is a
  failure, never an exit. The embedded fixtures keep their own limits: 100
  steps by default, 10,000 at most, and 15 seconds. The file's size is not
  capped: it is read whole, and
  the loader refuses an image that maps to more than 256 MiB. The fixture
  draw service is not registered.
- **Exit codes.** A host exit code has eight bits and a guest status has
  thirty-two. The low eight bits are returned, except that a status other
  than zero never becomes zero (it becomes 1); whenever the two differ the
  full status is printed on standard error. 124 means the wall deadline, 125
  any other failure: a file that is not a loadable executable, an import
  nobody provides, a fault, or a step limit reached before the program
  exited. A program that itself exits with 124 or 125 cannot be told apart
  from those by the code alone. Usage errors are 2.
- **Output.** It is shown when the program ends, standard output first and
  then standard error, so the two are not interleaved as the program wrote
  them. A program stopped by the wall deadline shows what it had written by
  then, and the command exits with 124.
- **What this is not.** Xenora is not a hardened runtime (see
  [`SECURITY.md`](../../SECURITY.md)): the guest's code is translated to
  native ARM64 and run in the worker process, and nothing confines that
  process beyond the checks the translator and the services make. Do not run
  a file you would not otherwise trust on this machine. Most programs will
  not run: they stop at the first import or instruction Xenora lacks. Nothing
  here concerns a commercial game or an Xbox launch environment.

`crates/cli/tests/run_program.rs` runs the real `xenora` binary on the Gate A
programs:

```sh
cargo test --locked -p xenora --test run_program
```

It checks the arguments the program echoes, its output and exit status at four
optimization levels, the three controls (42, 3 and 22), a 32-bit status, the
digest (wrong, one digit off, and a file changed after it was hashed), a step
limit that is too small, the trace, a changed fixture, an import nobody
provides, and files that are not executables. A further build of the Gate A2
program, `GATE_SPIN`, counts to its first argument or for ever: it shows that a
program needing more than 1,000,000 steps runs without `--max-steps`, that a
step limit which is given holds, and that a program which never ends is
stopped at `--wall-seconds` with what it had printed on standard output.
