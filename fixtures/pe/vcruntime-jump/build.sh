#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original VCRUNTIME jump fixtures with native clang/LLD. No Windows SDK is
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

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-vcruntime-jump.XXXXXX")
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

import_library vcruntime140 __intrinsic_setjmp longjmp
import_library kernel32 ExitProcess

build_guest() {
    name=$1
    shift
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc "$@" -c "$here/entry.S" -o guest.obj
    link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
        "/out:$name.exe" guest.obj kernel32.lib vcruntime140.lib
    if "$check"; then
        cmp "$name.exe" "$here/$name.exe"
    else
        install -m 0644 "$name.exe" "$here/$name.exe"
    fi
}

build_guest vcruntime-jump
build_guest vcruntime-jump-nonnull-frame -DNONNULL_FRAME
build_guest vcruntime-jump-nonzero-saved-frame -DNONZERO_SAVED_FRAME
