#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original VirtualQueryEx fixture with native clang/LLD. No Windows SDK is
# needed: the import libraries come from the documented export names alone.
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

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-virtual-query-ex.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"

import_library() {
    module=$1
    shift
    {
        printf 'LIBRARY %s.dll\nEXPORTS\n' "$module"
        printf '    %s\n' "$@"
    } > "$module.def"
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
}

memory=api-ms-win-core-memory-l1-1-0
import_library "$memory" VirtualQueryEx
import_library kernel32 CloseHandle CreateEventW ExitProcess GetCurrentProcess GetCurrentProcessId \
    GetLastError OpenProcess VirtualAlloc VirtualFree VirtualProtect VirtualQuery

arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/entry.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    /out:virtual-query-ex.exe guest.obj kernel32.lib "$memory.lib"

if "$check"; then
    cmp virtual-query-ex.exe "$here/virtual-query-ex.exe"
else
    install -m 0644 virtual-query-ex.exe "$here/virtual-query-ex.exe"
fi
