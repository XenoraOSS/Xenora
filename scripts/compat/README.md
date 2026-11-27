# Application fixture runner

This runner selects existing original Xenora fixture tests and saves their
host command outcomes. It does not accept external executables. Run on native
Apple Silicon macOS with the Rust target `aarch64-apple-darwin` and the native
toolchains required by the selected tests. The fixture descriptions and build
requirements are in [the fixture guide](../../tests/compat_fixtures/README.md).

```sh
python3 scripts/compat/runner.py list
python3 scripts/compat/runner.py run --suite c
python3 scripts/compat/runner.py run guest_cpp_path --timeout 300
python3 scripts/compat/runner.py report artifacts/compat/RESULT.json
python3 scripts/compat/runner.py report artifacts/compat/RESULT.json --format markdown
python3 scripts/compat/runner.py compare artifacts/compat/BASE.json artifacts/compat/HEAD.json
```

Runs are sequential. The runner first compiles the host test harness with
`cargo test --no-run`, then runs the fixture test only if that succeeds. The
host-build stage does not compile the guest fixture. The timeout applies to
each stage separately. A timed-out command and its process group are killed.
Output is retained up to 32 KiB per stream; longer output keeps the beginning
and end and sets a truncation flag. Markdown summaries omit captured output.
JSON reports contain
the command, host status, exit code, output, elapsed time, host and Rust
versions, commit and worktree status. Review reports before sharing: compiler
and test output may contain local paths. Results default to the ignored
`artifacts/compat/` directory.

Each selected Cargo target builds its guest fixtures and checks them in the
fixture command. A pass means that target's assertions passed, including its
negative controls. A failure there does not identify whether the guest build, image load, guest
execution or another assertion failed. The runner does not yet report separate
guest stdout, framebuffer or audio evidence, classify guest faults, enforce a
guest memory limit, or count initialized or completed applications. The
fixture tests have their own guest execution bounds. The runner's host deadline
is not a guest instruction budget. Existing fixture build paths may collide
with other runs in the same checkout, so do not run them concurrently.

`run` returns 0 only if all selected commands pass and 1 if any do not.
`compare` returns 1 when a previously passing stage regresses, 0 when no such
regression occurs, and 2 for invalid or incompatible reports. A status
comparison is not a performance benchmark or evidence of Xbox execution.
