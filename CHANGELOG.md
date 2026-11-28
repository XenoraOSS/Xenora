# Changelog

Notable changes are grouped using [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Experimental interfaces may change. Authorized releases should identify their
exact revision and known limitations.

## [Unreleased]

### Added
- `WINHTTP_OPTION_CONTEXT_VALUE` in `WinHttpSetOption` and `WinHttpQueryOption`: the caller-owned opaque per-handle value Windows stores on every session, connection and request handle, zero until set, an eight-byte `DWORD_PTR` sized by the option contract, independent per handle and gone with the handle. Sized queries report the required eight bytes and the closed-handle query fails with the invalid-handle error. Local verification only.