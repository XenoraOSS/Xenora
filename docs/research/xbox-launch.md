# Xbox Series X|S executable startup

Xenora can inspect a loose, original fixture with a public
`MicrosoftGame.config`, report its declared target and PE32+ dependencies,
and run separate Windows-shaped startup tests on macOS. Xbox startup remains
unsupported. The preflight stops at the console bootstrap boundary.

## Experimental launch preflight

Build the native CLI, build the original PE32+ fixture, and declare the file
as a Series X|S target using the fixture builder’s manifest. Set
`LLD_LINK` to a native `lld-link` if your Rust installation lacks `rust-lld`:

```sh
cargo build --locked --target aarch64-apple-darwin -p xenora
tests/compat_fixtures/build-xbox-launch.sh
python3 scripts/xbox-launch-preflight.py \
  --xenora target/aarch64-apple-darwin/debug/xenora \
  --config artifacts/xbox-launch/MicrosoftGame.config
```

The script emits JSON with `schema_version` 1. It records completed stages
in order, then reports the first incomplete stage and a machine-readable
reason. A completed preflight exits 0 with a `blocked` outcome at
`console_bootstrap`. A failed preflight exits 1 at `manifest`,
`executable_selection`, `executable_read`, or `static_pe_probe`. The static
report carries the probe's relocation, TLS and import counts, the first
listed unresolved ordinary import, delayed-import inventory, and the first
statically encountered unsupported CPU instruction. These are static
findings. Actual startup order and the first observed guest failure come
from the [worker startup report](../testing/startup-reports.md).

The profile accepts a selected, explicitly declared `Scarlett` executable.
With several declarations, pass `--exe` with the package-relative name.
The manifest's Windows path separators are normalized for the host.
Ambiguous names and paths outside the resolved manifest directory are
refused. Use a fixture directory under your control. The tool bounds the
manifest at 256 KiB and the PE file at 64 MiB on disk. The probe runs on a
private copy of those bytes with a 30-second deadline. PE file length and
mapped image length are independent.

The manifest report preserves `Id`, `IsDevOnly`, `Architecture`, the
`Identity` attributes, and an eight-digit `TitleId` when supplied. Omitted
declarations stay null. The profile inspects a subset of the schema, so the
sample can omit package identity. It cannot validate a submission or
authenticate a declaration. It makes no account, licensing, registration,
or entitlement requests. [Executable reference](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/system/microsoftgameconfig/elements/microsoftgameconfig-element-executable) ·
[MicrosoftGame.config schema and submission requirements](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/system/microsoftgameconfig/microsoftgameconfig-schema)

## PE32+ startup audit

The public Microsoft PE format describes Windows image structure. The
loader findings below are from upstream main at `c76e974`. Xenora's
bounded Windows-shaped fixture choices are implementation policy. Public
Xbox guidance establishes the title-facing contracts in the next section.

| Area | Windows documentation and Xenora behavior | Remaining prerequisite |
| --- | --- | --- |
| Image mapping | PE32+ defines headers, aligned sections, permissions and zero-filled virtual data. Xenora maps a single AMD64 image with read-only headers, section permissions and zero fill. It caps the virtual span at 256 MiB and uses a 4 KiB header mapping. The runtime rewrites the mapped optional-header image base on rebasing. | Validate section and file alignment, unusual layouts and header behavior against independently built Windows fixtures. The static probe and strict runtime mapper have different acceptance boundaries. |
| Base relocation | Xenora handles AMD64 `DIR64` fixups and `ABSOLUTE` padding. The original large fixture checks every data pointer after rebasing. | Audit absent or stripped relocation data at a nonpreferred base, the full directory span, and 32-bit block-start alignment. |
| Ordinary imports | Xenora binds ordinary names and ordinals against its registry. A companion DLL on disk supplies no runtime binding by itself. | Audit full all-zero descriptor termination and bounds. Connect real mapped module dependencies through the separately owned DLL implementation. |
| Delay imports | The PE directory inventory retains delay entries separately. Windows linker-generated thunks call a delay helper on demand. | Verify first-call resolution, import-address-table changes, and supported unload behavior after the DLL loader exists. |
| Process and thread state | Main and worker Thread Environment Blocks (TEBs) have selected fields. Main image identity and command-line data also exist through runtime services. The original fixture checks Win64 stack alignment and callback argument registers. | No guest Process Environment Block (PEB), process-parameter records or PEB loader list are built. Desktop Windows exposes partial, changeable definitions. Verify the smallest guest-observable subset before extending the layout. |
| Module lifecycle and TLS | Xenora initializes the main PE's static TLS at index zero and calls its process-attach callbacks in array order. Guest thread creation uses the cooperative scheduler and has separate dynamic Win32 TLS coverage. | Real DLL process attach and detach, thread notifications, initialization rollback and static TLS for worker threads need independent fixtures. The general CLI worker uses the unscheduled startup path. |
| Exceptions and unwinding | The loader registers image exception data and implements a bounded Windows x64 unwinder. Original C and C++ tests cover handled faults, filter and finally order, propagation, destructor unwinding, and continuation. | Verify chained records that inherit a primary language handler, exit unwinds, nested or collided unwinds, and context-pointer outputs. Top-level filtering is separately owned. |

[PE/COFF reference](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format) ·
[x64 calling convention](https://learn.microsoft.com/en-us/cpp/build/x64-calling-convention) ·
[Windows TEB](https://learn.microsoft.com/en-us/windows/win32/api/winternl/ns-winternl-teb) ·
[Windows PEB](https://learn.microsoft.com/en-us/windows/win32/api/winternl/ns-winternl-peb) ·
[Windows process parameters](https://learn.microsoft.com/en-us/windows/win32/api/winternl/ns-winternl-rtl_user_process_parameters) ·
[Windows loader data](https://learn.microsoft.com/en-us/windows/win32/api/winternl/ns-winternl-peb_ldr_data) ·
[DLL entry notifications](https://learn.microsoft.com/en-us/windows/win32/dlls/dllmain) ·
[Delay-load helper](https://learn.microsoft.com/en-us/cpp/build/reference/understanding-the-helper-function) ·
[Windows x64 exception handling](https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64) ·
[Xenora exception research](exception-handling.md)

## Public Xbox title contracts

- `TargetDeviceFamily="Scarlett"` declares Series X|S intent. `XboxOne`
  has different development-kit behavior on Series consoles. Leaving the
  target unspecified leaves the profile without explicit Series X|S intent.
  `Executable.Id` identifies an application within the package, while
  `TitleId` is used by Xbox services and `Identity` is package metadata.
  [MicrosoftGame.config overview](https://learn.microsoft.com/en-us/gaming/gdk/docs/features/common/game-config/microsoftgameconfig-overview) ·
  [TitleId reference](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/system/microsoftgameconfig/elements/microsoftgameconfig-element-titleid)
- Microsoft documents calling `XGameRuntimeInitialize` early in each binary
  using the runtime and calling `XGameRuntimeUninitialize` late before
  exit. Missing runtime libraries and initialization errors are observable
  failures. Xenora's profile records dependencies and provides no
  Gaming Runtime success result.
  [Initialize](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/system/xgameruntimeinit/functions/xgameruntimeinitialize) ·
  [Uninitialize](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/system/xgameruntimeinit/functions/xgameruntimeuninitialize)
- Xbox titles receive suspend, constrain and resume notifications. Microsoft's
  public GameInput example demonstrates title-side callback registration,
  main-thread handoff and teardown. Exact host scheduling, notification timing,
  and console resource policy remain unverified.
  [GDK overview](https://learn.microsoft.com/en-us/xbox/gdk/docs/gdk-dev/intro/introduction) ·
  [Microsoft GameInput example](https://github.com/microsoft/Xbox-GDK-Samples/blob/main/Samples/System/GameInputInterfacing/Main.cpp)

The launch contract still needs authorized evidence for initial register and
process state, stack and thread layout, loader-supplied modules or API sets,
module and TLS ordering, exception delivery, identity and package registration
interfaces, and lifecycle semantics. A reproducible run of an independently
authored Xbox-targeted program in an authorized console environment is required
to verify these. A Windows-compatible PE or a manifest declaration alone
establishes neither the console loader ABI nor a valid entitlement.

## Reproduce the local verification

```sh
XENORA_STARTUP_FIXTURES="$PWD/artifacts/xbox-launch" \
XENORA_STARTUP_CLI="$PWD/target/aarch64-apple-darwin/debug/xenora" \
  python3 -m unittest discover -s tests/qa -p test_xbox_launch_preflight.py -v
```

The original assembly fixture checks a 128 MiB zero-filled region, 8,192
data-pointer fixups, two process-attach callbacks, and the entry frame.
The linker adds six TLS addresses, for 8,198 `DIR64` entries in total.
The control expects an incorrect callback count and exits 12. A second
original executable references two original DLLs. The worker stops at
the first unresolved ordinary import before reaching guest entry.
Other cases check truncated input, invalid mapped-header size, malformed
relocation and TLS data, the entry step budget, and the distinction between
ordinary and delay-import inventory.

The build also emits `scarlett-runtime.exe` and its loose
`MicrosoftGame.config`. The original C entry requests Gaming Runtime
initialization, checks the result, and requests teardown on success.
The import library encodes an intentionally synthetic DLL linkage for
those documented API names. Static inspection counts both imports as
unresolved. The Windows-shaped worker maps the image and stops at import
binding before reaching the entry point. This exercises the diagnostics for
a missing service. The exact console import linkage remains unknown.
[Gaming Runtime initialization](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/system/xgameruntimeinit/functions/xgameruntimeinitialize)

GNU objdump can independently inspect the linker's PE headers and
relocation records. It is a static format check. The fixture suite does
not include a Windows or console execution comparison. Existing workspace
tests cover C and C++ runtime startup, structured exception propagation,
process lifecycle, cooperative thread creation and dynamic TLS.
