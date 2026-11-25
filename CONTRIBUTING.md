# Contributing to BetterXen

BetterXen is an experimental Xbox Series X|S recompilation framework for Apple Silicon. Its graphics subsystem, XenoMetal, targets native Metal execution. Contributions should improve verified behavior rather than expand untested claims.

## Getting started

- Read `README.md` and, when present, `AGENTS.md`, `docs/interfaces.md`, `docs/status.toml` and the relevant architecture decisions.
- Discuss substantial changes in an issue before implementing them; small fixes do not require prior approval.
- Claim an issue before working on it by replying on it in plain words, after checking its comments for an existing claim. Never claim over someone else. A claim with no pull request after about one week may be taken over with a reply saying so. Coding agents follow the same rules; see "Claiming work before you start" in `AGENTS.md`.
- Use a native ARM64 macOS environment with Rust and the appropriate Xcode/Metal tools. LLVM AMDGPU tools are needed for ISA fixture generation; extra tooling depends on the subsystem.
- Keep changes focused and preserve existing public interfaces unless a change is justified and documented.

## Development and verification

When the Cargo workspace exists, run from its root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Follow repository documentation for actual GPU tests, fuzzing, mutation testing and benchmarks. Report commands, versions, pass/fail/skip counts and hardware used. A successful shader compile is not successful GPU execution. Do not report unavailable tools or skipped tests as passes.

For an instruction or compiler change, cite the applicable ISA section, state the exact architecture, document operand and execution-mask semantics, and add tests for encodings, edge cases, unsupported cases and independent expected results. Where applicable, verify the resulting MSL through actual Metal execution. Do not convert unsupported instructions into silent no-ops.

For a performance change, provide reproducible before-and-after results using equivalent inputs, hardware and settings. For a new dependency, explain why it is needed, verify its license and native ARM64 compatibility, and record its provenance.

## Research and clean-room policy

Use publicly accessible specifications and authorized materials only. Do not submit leaked SDKs, confidential documentation, firmware, keys, proprietary game binaries or unauthorized shader dumps. Under BetterXen's clean-room policy, anyone who has accessed leaked or confidential Xbox materials must not contribute. Do not post details of restricted material in issues or PRs.

Distinguish desktop RDNA behavior from Xbox-specific behavior. Cite primary sources with exact applicable architectures and record uncertainties using the project's evidence classification when available. Tests on Apple GPUs do not establish verification on Xbox hardware.

BetterXen implements its own Rust guest runtime, decoder, typed IR, shader
compiler and native Metal backend from public technical specifications
and independently derived tests. Contributions must not copy, port or
adapt code, decoder tables, internal metadata or implementation
architecture from other emulators. External results can inform test
selection. Author the resulting code and fixtures independently.

Before incorporating third-party code, verify per-file license compatibility with GPL-3.0-or-later, record the exact upstream revision and original paths, preserve copyright notices and document changes. Update applicable attribution records. Public visibility alone is not proof of authorization or license compatibility.

## Pull requests

- Branch from the current main branch, keep commits focused, and link relevant issues.
- Fill in every section of the pull request template, keep `Closes #<number>` (or write `No linked issue: <reason>`), and tick the claimed-first and DCO checkboxes. The *PR template* check fails until the description does, and runs again each time you edit it.
- Include a clear explanation, tests and any documentation/status updates. Separate unverified work from supported behavior.
- Disclose material AI assistance and personally review generated changes. AI output is not an authoritative source, test oracle or substitute for independent review.
- Never include secrets, personal paths, credentials or private data in patches or logs.
- Do not claim approval from an independent reviewer unless one has actually reviewed the change.

Sign off each commit under the [Developer Certificate of Origin](https://developercertificate.org/):

```sh
git commit -s -m "Describe the change"
```

Use your configured Git identity and a privacy-preserving email address if desired. Sign-off is your certification of the contribution; do not sign off work on another person's behalf. Contributions are subject to the repository license and any applicable, compatible third-party notices.

For security-sensitive findings, see [SECURITY.md](SECURITY.md). Community expectations are in [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
