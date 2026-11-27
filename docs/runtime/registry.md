# Runtime service registry

Guest imports resolve through one lookup, `ImportRegistry::resolve`. Binding, the static probe and the command-line reports all use it, and they read the import directory with one bounded parser, `import_table::parse_imports`.

| Part | File | Role |
| --- | --- | --- |
| Typed registry | `crates/loader/runtime/services.txt` | Library, symbol, typed signature, API-set aliases, failure contract, status, reference and tests. Data only. |
| Typed generator | `xenora-runtime-gen` in `crates/import-gen` | `validate`, `generate` and `--check`. Rejects conflicting keys, unlisted API sets, unsupported types, and behaviour without a named, existing test. |
| Adapters | `crates/loader/src/runtime_table.rs` | Generated Win64 argument and result adapters and static metadata. |
| Handlers | `handlers` in `crates/loader/src/services.rs` | Rust implementations. They see guest memory and service state, never the CPU. |
| Typed report | [services.md](services.md) | Generated counts for the typed registry only. Aliases are counted apart from services. |
| Live inventory | `scripts/audit/runtime_inventory.py` and `crates/loader/examples/runtime_inventory.rs` | Constructs the standard registry without loading a PE image or executing guest code, then writes [inventory.md](inventory.md) and `inventory.json`. |

## Rules

- Module names compare without case; symbol names are exact. A name spelled `Ordinal#7` is not ordinal 7.
- An API set resolves only if the registry lists that exact set for that symbol. A similar name forwards nowhere.
- An optional `ordinal <number> <source>` field adds an ordinal for the canonical DLL. Cite the export catalog and version that establish the mapping. Named and ordinal imports share the same adapter. The ordinal applies only to the canonical DLL.
- Registering a key twice is an error. A host module cannot replace a standard service.
- Binding resolves every import before it writes any slot. A malformed table or an unresolved import leaves every slot unbound.
- A service call reads its return address first, and stops the guest with `ServiceAbiViolation` if the service changed a nonvolatile register, including XMM6 to XMM15.

## Manual registrations and implementation status

The typed table is only part of the standard registry. Manual code also registers services in the same `ImportRegistry`: Win32 services that do not yet have typed adapters, legacy CRT entry points and data imports, Winsock, C++ support libraries, module-loader services, the fixture graphics, task and platform services, `XMemAlloc` and exact-base XMem-owned `XMemFree` release. The generated live inventory is the authoritative current list; do not maintain a second list here.

`ImportRegistry::register` keeps its historical default status of `implemented`. A status is explicit only when registration uses `try_register` or `register_with_status`, or when `set_status` is applied later. The inventory's `status_explicit=false` rows therefore identify metadata that was never explicitly chosen. That is an audit finding, not by itself a claim that the implementation is wrong.

The inventory covers the registry built by `register_standard_services`, which is the normal guest service registry. The opt-in synthetic Gaming Runtime lifecycle registered by `services::gaming_runtime::register_synthetic_lifecycle` is outside that default profile and is not counted as available unless a host explicitly registers it.

## Runtime inventory

Generate the committed reports with:

```sh
python3 scripts/audit/runtime_inventory.py
```

Check that they are current with:

```sh
python3 scripts/audit/runtime_inventory.py --check
```

The command writes `docs/runtime/inventory.json` and `docs/runtime/inventory.md`. JSON carries the full sorted service, alias, finding and wanted-import rows. Markdown carries the concise counts and findings. Neither file contains a timestamp, host identifier or guest execution result. The Cargo example only constructs the registry; it does not execute guest instructions or call service handlers.

Service rows carry `kind` (`name` or `ordinal`) and `data_import` as well as their status metadata. Alias rows carry both the resolving key kind and `canonical_kind`, so the literal name `Ordinal#7` cannot be conflated with ordinal 7. Typed-registry rows are always names. The audit-script tests are discoverable with:

```sh
python3 -m unittest discover -s tests/audit -p test_runtime_inventory.py
```

`--snapshot <file>` reads a saved snapshot instead of running the Cargo example. Beyond field and type checks, the snapshot must be structurally consistent: every noncanonical row needs an existing kind-aware canonical target, a canonical row must resolve to itself, a noncanonical row must not, and every distinct service needs exactly one canonical resolution key. Structurally inconsistent snapshots are input errors, not inventory reports.

### Wanted imports for the inventory

`--wanted <file>` accepts either probe output or a simple CSV. Probe `imports.csv` and `missing_imports.csv` are accepted as written, including their quoting. Repeated import slots for the same key are deduplicated, ordinary and delayed rows are both read, and `import_kind` distinguishes a real ordinal from a symbol merely spelled `Ordinal#7`. When probe output has a nonempty `category`, that category is the report family; the fallback below is used when it is absent or empty.

The simple form is UTF-8 CSV with a header of `library,symbol` and optional `kind` and `family` columns:

```csv
library,symbol,kind,family
kernel32.dll,CreateFileW,name,windows-files-console
ucrtbase.dll,Ordinal#42,ordinal,c-runtime
```

`kind` is `name` when omitted. Module names compare without case and symbol names are exact. An ordinal may be written as `42` or `Ordinal#42` when `kind` is `ordinal`; without that kind it remains the literal name `Ordinal#42`, consistent with the registry rule. A family label is at most 80 characters. When a simple CSV omits it or leaves it empty, or when probe output has no nonempty `category`, the inventory uses this deterministic module-family fallback:

| Family | Modules |
| --- | --- |
| `c-runtime` | `ucrtbase.dll`, `msvcrt.dll`, `api-ms-win-crt-*` |
| `c++-support` | `libgcc_s_seh-1.dll`, `libstdc++-6.dll`, `libwinpthread-1.dll` |
| `windows-networking` | `ws2_32.dll` |
| `windows-system` | `kernel32.dll`, `kernelbase.dll`, `advapi32.dll`, `psapi.dll`, `api-ms-win-core-*`, `api-ms-win-downlevel-*`, `ext-ms-*` |
| `xbox-gaming` | `xmem.dll`, `xgameruntime.dll` |
| `xenora-fixture` | modules beginning `xenora` |
| `other` | anything else |

Wanted input is bounded at 1 MiB and 16,384 rows. Library and symbol names must be printable ASCII of at most 4,096 bytes, matching the import parser's name bound. When no wanted file is used, the JSON `wanted_imports` and `wanted_missing` summary counts are null rather than zero. For a probe report, a typical non-destructive invocation is:

```sh
python3 scripts/audit/runtime_inventory.py --wanted diagnostics/probe/imports.csv --output-dir artifacts/runtime-inventory
```

## Adding a typed service

1. Write a test in `crates/loader/tests/runtime_services.rs` that calls it from guest code and derives its expectations from the public documentation.
2. Add the entry to `services.txt` and the handler to `services.rs`.
3. Run `cargo run -p xenora-import-gen --bin xenora-runtime-gen generate`.

The older import-list generator in `crates/import-gen` is separate: its output is not compiled into the loader, and several of its generated conformance tests check Rust library functions rather than a service.
