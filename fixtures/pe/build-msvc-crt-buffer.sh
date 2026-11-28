#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] && lipo -verify_arch arm64 "$tool" || { echo "Native ARM64 tool required: $tool" >&2; exit 1; }
done
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-crt-buffer.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY ucrtbase.dll\nEXPORTS\n __acrt_iob_func\n _get_stream_buffer_pointers\n fputc\n fflush\n _errno\n' > ucrt.def
printf 'LIBRARY kernel32.dll\nEXPORTS\n ExitProcess\n' > kernel.def
for lib in ucrt kernel; do
    arch -arm64 "$linker" /lib /def:"$lib.def" /machine:x64 /out:"$lib.lib"
done
for mode in normal invalid bad_output; do
    define=
    [ "$mode" = normal ] || define="-DXENORA_$(printf '%s' "$mode" | tr '[:lower:]' '[:upper:]')"
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -x c++ -std=c++20 -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector -fno-vectorize -fno-slp-vectorize $define -c "$here/msvc-crt-buffer.cpp" -o guest.obj
    arch -arm64 "$linker" /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:guest.exe guest.obj ucrt.lib kernel.lib
    destination="$here/msvc-crt-buffer.exe"
    [ "$mode" = normal ] || destination="$here/msvc-crt-buffer-$mode.exe"
    if [ "${1:-}" = --check ]; then cmp guest.exe "$destination"; else install -m 0644 guest.exe "$destination"; fi
done
