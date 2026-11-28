#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)
gxx=${MINGW_GXX:-/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-g++}
if [ ! -x "$gxx" ] && [ -z "${MINGW_GXX:-}" ]; then
    gxx=$(command -v x86_64-w64-mingw32-g++ || true)
fi
clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}
if [ ! -x "$linker" ] && [ -z "${LLD_LINK:-}" ]; then
    linker=$(command -v lld-link || true)
fi
for tool in "$gxx" "$clang" "$linker"; do
    test -x "$tool"
    lipo "$tool" -verify_arch arm64
done
for helper in cc1plus as ld collect2; do
    path=$("$gxx" -print-prog-name="$helper")
    if [ ! -x "$path" ]; then
        path=$(command -v "$path")
    fi
    lipo "$path" -verify_arch arm64
done
test "$("$gxx" -dumpmachine)" = x86_64-w64-mingw32

link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-gnu-cross-dll-unwind.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
for relay in inner outer; do
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O0 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-omit-frame-pointer -funwind-tables \
        -c "$here/$relay.c" -o "$relay.obj"
done

link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 /base:0x180000000 \
    "/out:$out/unwind_inner.dll" /implib:inner.lib inner.obj
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 /base:0x180100000 \
    "/out:$out/unwind_outer.dll" /implib:outer.lib outer.obj inner.lib
arch -arm64 "$gxx" -O1 -Wall -Wextra -Werror -static-libstdc++ -static-libgcc \
    -o "$out/gnu-cross-dll-unwind.exe" "$here/entry.cpp" outer.lib
