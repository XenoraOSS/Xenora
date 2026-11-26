#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original PE with native clang/LLD. No Windows SDK is needed.
set -eu

usage() {
    echo "usage: $0 [--check]" >&2
    exit 2
}

case $# in
    0) check=false ;;
    1) [ "$1" = --check ] || usage; check=true ;;
    *) usage ;;
esac

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
if [ -z "${CLANG:-}" ] && [ ! -x "$clang" ]; then
    clang=$(command -v clang)
fi
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ -z "${LLD_LINK:-}" ] && [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
for tool in "$clang" "$linker"; do
    if [ ! -x "$tool" ] || ! lipo -verify_arch arm64 "$tool"; then
        printf 'Native ARM64 tool required: %s\n' "$tool" >&2
        exit 1
    fi
done
link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-event-rights.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"

cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CreateEventExW
    SetEvent
    ResetEvent
    WaitForSingleObject
    DuplicateHandle
    CloseHandle
    GetLastError
    CreateThread
    GetExitCodeThread
    ExitProcess
DEF
cat > synch.def <<'DEF'
LIBRARY api-ms-win-core-synch-l1-2-0.dll
EXPORTS
    CreateEventExA
DEF
for module in kernel32 synch; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/event-rights.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    /out:event-rights.exe guest.obj kernel32.lib synch.lib

if "$check"; then
    cmp event-rights.exe "$here/event-rights.exe"
else
    install -m 0644 event-rights.exe "$here/event-rights.exe"
fi
