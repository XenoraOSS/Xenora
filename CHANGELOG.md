# Changelog

Notable changes are grouped using [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Experimental interfaces may change. Authorized releases should identify their
exact revision and known limitations.

## [Unreleased]

### Added
- `_locking` and `_commit` in `ucrtbase.dll`, `msvcrt.dll` and `api-ms-win-crt-stdio-l1-1-0.dll` (#1236). `_locking` keeps exclusive byte ranges at the current position of the stream: exact-range unlock, conflicts across independent opens of one file and against the stream own overlapping ranges as LockFile refuses them, EINVAL for invalid modes and unmatched unlocks, EDEADLOCK at once for a conflict - the documented ten-retry window collapses because a service call cannot wait on another guest thread lock without deadlocking the table. Locks live in the stream itself, so a duplication shares them and closing the last descriptor releases them. `_commit` answers success for a file-backed descriptor because the hosted drive is the storage with no cache below it. Local verification only.