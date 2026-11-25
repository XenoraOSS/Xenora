# BetterXen engineering rules

### Native Apple Silicon

Never introduce Rosetta 2, x86-64 host executables, MoltenVK, Vulkan runtime dependencies, Wine, CrossOver, Windows compatibility layers, OpenGL fallback rendering, or unnecessary graphics abstraction layers.

- SPIR-V is permitted as a compiler intermediate representation. It does not justify a Vulkan runtime.
- All host executables and native dependencies must support ARM64. Verify with file, lipo -archs and otool -L.
- Never run Intel binaries through Rosetta, even temporarily. Never add a dependency that requires Rosetta to build or run. If a dependency cannot run natively, find an alternative or document the limitation.
- The guest architecture being decoded may be x86-64, but BetterXen itself runs natively on ARM64.
- Support M1 through M5 and future generations through runtime capability detection. Do not assume all Apple GPUs share capabilities.
- Use aarch64-apple-darwin as the Rust target.

### Honesty and evidence

- Never report a test as passed unless it ran and passed. Never count a skipped test as GPU verification.
- Never invent benchmark results, hardware compatibility, API names, instruction encodings, hardware capabilities or implementation status.
- Never claim a clean build unless the commands actually succeeded.
- Never present planned features as working. State clearly that game execution is not supported unless demonstrated.
- Never replace GPU tests with mocks and then claim GPU support.
- Unknown information is classified as unknown and becomes a documented research task.
- Every hardware claim carries an evidence class (section 9). Never present a lower-class claim as if it were a higher-class one.
- Never claim "verified on Xbox hardware". BetterXen has no Xbox hardware access. Agreement with a spec-derived reference is not agreement with Xbox hardware, and docs must say so.
- Do not hide errors behind fallbacks. Do not translate unsupported instructions into silent no-ops. Do not weaken or delete tests to make them pass. Do not change mutation counting, tolerances or gate definitions to improve a score.

### Independence of verification

- The agent that writes a component does not certify it. The two independent reviewers (section 5) re-derive expectations from primary sources and are not told the implementers' conclusions as ground truth.
- The same faulty code must never produce both the expected and the actual values in a test.

### Legal and clean-room

- Use only publicly accessible documentation and authorized materials. Never obtain or use leaked SDKs, confidential Xbox documentation, restricted console tools, firmware, encryption keys, or proprietary game binaries and shader collections. Do not attempt to bypass documentation access restrictions.
- Contributors who have seen leaked or confidential Xbox materials must not contribute. State this in CONTRIBUTING.md and require a DCO sign-off (Signed-off-by) on contributions.
- Include a plain statement in docs/legal.md: BetterXen does not circumvent console security or DRM, does not load encrypted or protected content, and is a research project built from public documentation. Add a note that this is not legal advice and that contributors should know their local laws, including anti-circumvention provisions such as DMCA section 1201.
- Before copying or adapting any upstream code: verify the exact license of that file (not just the repo), check GPL-3.0-or-later compatibility, preserve copyright notices, record source repository and exact commit, document modifications, include required attribution. Never remove upstream copyright notices. If licensing is uncertain, stop using that code until resolved.
- Keep BetterXen's emulator implementation original. Never copy, port, translate or adapt emulator source from Leonida, KyTx, AnyPS5 or similar projects.
- Validate the technical compatibility and safety of user-provided files. Do not ask for ownership receipts, accounts or acquisition provenance as part of file loading. Contributor provenance and licensing obligations still apply.
- Never place restricted material in the repository or its Git history.
- Do not push, publish, or change remote visibility without explicit user authorization. Prepare locally and report readiness.

### Quality

No AI slop (section 29).

Read docs/interfaces.md and docs/progress.md before implementation.

## Claiming work before you start

Several people and coding agents work on this repository at the same time. Claim an issue before you edit anything, and never take work someone else has claimed.

1. **Check.** Read every comment on the issue and look for open pull requests that link it. A claim is any comment in which someone says they are taking the issue, in any words (for example "I'll take this" or `/claim`). If a claim exists and is not stale (step 4), pick another issue.
2. **Claim.** Reply on the issue in plain words, for example: "I'm taking this. Plan: …". No command, label or bot step is needed. The earliest claim wins. Claim one issue per comment.
3. **Stay in scope.** If the issue lists files, edit only those, and ask on the issue before widening the list. Do not start work that edits the same files as another claimed issue.
4. **Stale claims.** A claim is stale when the claimer has not opened a pull request for the issue within about one week (7 days) of the claim. Only then may someone else take it, by replying on the issue, for example: "Taking this over: @name's claim from <date> has no pull request after 7 days." A claim with an open pull request is never stale; ask the claimer or the maintainer instead.
5. **Release.** If you stop working on an issue, say so on the issue so others can take it.
6. **Link.** Put `Closes #<n>` in the pull request description.

A coding agent claims in the name of the person it works for and follows the same rules. It must read the comments before claiming, and must never claim over, remove or ignore another person's claim. The optional task-queue commands in `docs/coordination/agent-tasks.md` do not replace these rules: a plain-reply claim counts even though the bot does not track it.

## Changelog maintenance

- At the start of every engineering task, read `CHANGELOG.md` alongside the relevant project documentation. Review the existing `[Unreleased]` entries before implementation to avoid duplicating completed work.
- Update the changelog in the same task for every meaningful addition, modification, fix, removal or security change, including work implemented by another agent. Use `Added`, `Changed`, `Fixed`, `Removed` or `Security` as appropriate.
- Write concise entries that explain the effect for users or contributors. Distinguish implemented behavior, local verification and independent acceptance. Never invent release dates, versions, benchmark results, passing tests or compatibility claims.
- Omit trivial formatting and intermediate steps unless they materially affect maintainability. Keep unreleased changes under `[Unreleased]` until a release is authorized.
- Before concluding the task, check whether the changelog needs an update and make it when warranted. Rewrite historical entries only to correct factual errors. Preserve existing content when tightening the prose.

## Public documentation and terminology

- Write public documentation so readers can understand it without earlier agent conversations. Keep `README.md` and `docs/README.md` accessible to new contributors.
- Introduce descriptive names before internal identifiers such as X1, G10 or P2-04. Define specialist abbreviations on first meaningful use.
- Write changelog entries around actual behavior and its effect on users or contributors. Distinguish implementation, synthetic demonstrations, independent verification and real Xbox execution.
- Review terminology whenever editing documentation. Use concise, precise explanations and preserve evidence scope.
- Update `docs/README.md` whenever you add, remove, rename or substantially reorganize documentation.

## Subagent reasoning

**Every spawned subagent must use LOW reasoning effort.** Explicitly select LOW and verify the resolved configuration. Never use Medium, High or Extra High. If LOW cannot be guaranteed, proceed without that subagent. The lead agent may use the user-selected reasoning setting.

## Checkout commands

- Format: `cargo fmt --all -- --check`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Test: `cargo test --workspace`
- Verify the synthetic fixtures: `scripts/assemble-fixture.sh --check`
- Run native GPU correctness: `scripts/test-gpu.sh`
- Run the three RDNA-to-Metal fixtures: `scripts/demo.sh`
- Run independent CPU validation: `cargo test --locked -p xenora-semantic-review`
- Run the correctness-gated benchmark suite: `scripts/benchmark.sh` (idle machine, release, validation off)
- Run source mutation on decoder/shader/ref: `scripts/mutation.sh` (R2 owns triage)
- Check generated status tables: `python3 scripts/audit/status.py --check`.
- Check claims and dependency direction: `scripts/audit/claims.sh` and `scripts/audit/dependencies.sh`
- Fuzz one target at a time: `scripts/fuzz.sh <instruction_stream|fixture|reference|runtime_services>` (600 seconds by default). `runtime_services` drives the standard guest service registry.

Keep generated and gate logs under ignored `artifacts/`. Use at most four Cargo build jobs across two simultaneous CPU-heavy jobs, with one GPU job and one fuzz or mutation campaign at a time.

Do not add AI attribution or Co-authored-by trailers to commits.
Never commit or push without explicit permission.
Preserve the configured Git author and committer identity.

## Ownership

Xbox and claims owns public console research. ISA owns the decoder and oracle fixtures. Compiler owns the parser, IR and MSL. Metal owns the native GPU backend. Testing owns the independent reference and fixture validation. Infrastructure owns shared Cargo configuration, policies and the CPU boundary. R1 independently audits claims and release evidence. R2 independently validates semantic expectations and mutation results.
