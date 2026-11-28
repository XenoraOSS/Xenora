#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] && lipo -verify_arch arm64 "$tool" || { echo "Native ARM64 tool required: $tool" >&2; exit 1; }
done
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-memory.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY vcruntime140.dll\nEXPORTS\n memcpy\n memmove\n memset\n memcmp\n memchr\n' > runtime.def
printf 'LIBRARY kernel32.dll\nEXPORTS\n ExitProcess\n' > kernel.def
for lib in runtime kernel; do
    arch -arm64 "$linker" /lib /def:"$lib.def" /machine:x64 /out:"$lib.lib"
done
for mode in normal fault; do
    define=
    [ "$mode" = normal ] || define=-DXENORA_FAULT
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -x c++ -std=c++20 -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector -fno-vectorize -fno-slp-vectorize $define -c "$here/msvc-memory.cpp" -o guest.obj
    arch -arm64 "$linker" /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:guest.exe guest.obj runtime.lib kernel.lib
    destination="$here/msvc-memory.exe"
    [ "$mode" = normal ] || destination="$here/msvc-memory-fault.exe"
    if [ "${1:-}" = --check ]; then cmp guest.exe "$destination"; else install -m 0644 guest.exe "$destination"; fi
done
