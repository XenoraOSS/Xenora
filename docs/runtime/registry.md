# Runtime service registry

Guest imports resolve through one lookup, `ImportRegistry::resolve`. Binding, the static probe and the command-line reports all use it, and they read the import directory with one bounded parser, `import_table::parse_imports`.

| Part | File | Role |
| --- | --- | --- |
| Registry | `crates/loader/runtime/services.txt` | Library, symbol, typed signature, API-set aliases, failure contract, status, reference and tests. Data only. |
| Generator | `xenora-runtime-gen` in `crates/import-gen` | `validate`, `generate` and `--check`. Rejects conflicting keys, unlisted API sets, unsupported types, and behaviour without a named, existing test. |
| Adapters | `crates/loader/src/runtime_table.rs` | Generated Win64 argument and result adapters and static metadata. |
| Handlers | `handlers` in `crates/loader/src/services.rs` | Rust implementations. They see guest memory and service state, never the CPU. |
| Report | [services.md](services.md) | Generated counts. Aliases are counted apart from services. |
| Coverage | `xenora imports coverage` | Every service the standard registry provides, by module and status, checked against the import metadata. Writes `artifacts/coverage/api-coverage.md` by default. |

## Rules

- Module names compare without case; symbol names are exact. A name spelled `Ordinal#7` is not ordinal 7.
- An API set resolves only if the registry lists that exact set for that symbol. A similar name forwards nowhere.
- An optional `ordinal <number> <source>` field adds an ordinal for the canonical DLL. Cite the export catalog and version that establish the mapping. Named and ordinal imports share the same adapter. The ordinal applies only to the canonical DLL.
- Registering a key twice is an error. A host module cannot replace a standard service.
- Binding resolves every import before it writes any slot. A malformed table or an unresolved import leaves every slot unbound.
- A service call reads its return address first, and stops the guest with `ServiceAbiViolation` if the service changed a nonvolatile register, including XMM6 to XMM15.

## Adding a service

1. Write a test in `crates/loader/tests/runtime_services.rs` that calls it from guest code and derives its expectations from the public documentation.
2. Add the entry to `services.txt` and the handler to `services.rs`.
3. Run `cargo run -p xenora-import-gen --bin xenora-runtime-gen generate`.

## Coverage report

`cargo run -p xenora -- imports coverage [--out <file>]` builds the standard registry and the built-in import metadata and writes one Markdown report. It reads no executable, network or clock, so the same tree gives the same bytes; a CLI test runs it twice and compares. The report lists three kinds of disagreement:

- the metadata expects a standard service that the registry does not resolve, which the CLI tests also reject;
- the registry provides a service the metadata classifies as something else, such as a stub, so the import generator would emit a stub for a working service;
- the registry provides a service the metadata does not list at all.

Counts are not compatibility claims, and the report is not committed: it changes whenever a service is added.

## Outside this registry

The fixture graphics services, `XMemAlloc` and the `XMemFree` stub are still registered by hand. The older import-list generator in `crates/import-gen` is separate: its output is not compiled into the loader, and several of its generated conformance tests check Rust library functions rather than a service.
