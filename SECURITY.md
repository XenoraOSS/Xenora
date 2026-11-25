# Security policy

## Scope

Xenora is experimental and is not currently represented as a security-hardened runtime. Treat shaders, binary containers, caches and native library inputs as untrusted. Do not use it to process untrusted content in a sensitive environment.

`xenora run` loads and executes any PE32+ executable it is given: the guest's code is translated to native ARM64 and run in a worker process with a wall deadline and a step limit. Those limits bound a run; they are not a sandbox. Do not run a file you would not otherwise trust on the machine. `--sha256` makes the command refuse a file whose digest is not the one stated; it identifies the file and does not make it trustworthy.

The project does not promise a fixed security support window at this stage. Report issues against a specific revision, preferably reproduced on current `main` where safe.

## Reporting vulnerabilities

Use [GitHub private vulnerability reporting](https://github.com/ranveersoni98/Xenora/security/advisories/new), if enabled. If it is unavailable, request a private reporting channel without disclosing exploit details publicly. Do not open a public issue containing sensitive vulnerability details, credentials, or private data.

Include the affected commit, environment, affected component, expected and actual behavior, impact and a minimal, authorized reproduction. Use synthetic inputs wherever possible. Do not attach protected content, leaked SDK material or proprietary shaders.

Relevant concerns include parser memory safety, malformed input handling, resource exhaustion, unsound FFI, unsafe cache handling and GPU hangs. Stop testing if a workload makes the system unstable; do not repeat destructive or unbounded workloads.

## Handling and disclosure

Please coordinate disclosure with the maintainers. A report should be acknowledged and investigated as resources permit; no response or patch deadline is guaranteed. Public advisories should describe affected versions, impact, fixes and available mitigations once these are established.

Unsupported shader semantics and ordinary correctness bugs can generally go through the issue tracker unless they have a security impact.
