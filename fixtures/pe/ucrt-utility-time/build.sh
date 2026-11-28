#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original UCRT utility and time fixtures with native clang/LLD. No Windows SDK is
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

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-ucrt-utility-time.XXXXXX")
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

utility=api-ms-win-crt-utility-l1-1-0
time=api-ms-win-crt-time-l1-1-0
import_library "$utility" rand srand
import_library "$time" _time64
import_library msvcrt 'legacy_rand == rand' 'legacy_srand == srand'
import_library ucrtbase 'ucrt_rand == rand' 'ucrt_srand == srand' 'ucrt_time64 == _time64' _write
import_library kernel32 CloseHandle CreateThread ExitProcess WaitForSingleObject

arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -DRNG_FIXTURE -c "$here/entry.c" -o rng.obj
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/entry.c" -o time.obj
for mode in rng time; do
    name=ucrt-utility-$mode
    link "/entry:${mode}_entry" /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
        "/out:$name.exe" "$mode.obj" kernel32.lib "$utility.lib" "$time.lib" msvcrt.lib ucrtbase.lib
    if "$check"; then
        cmp "$name.exe" "$here/$name.exe"
    else
        install -m 0644 "$name.exe" "$here/$name.exe"
    fi
done
