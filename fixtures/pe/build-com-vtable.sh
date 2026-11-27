#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] && lipo -verify_arch arm64 "$tool" || { echo "Native ARM64 tool required: $tool" >&2; exit 1; }
done
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-com-vtable.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY kernel32.dll\nEXPORTS\n    ExitProcess\n' > kernel32.def
arch -arm64 "$linker" /lib /def:kernel32.def /machine:x64 /out:kernel32.lib
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -x c++ -std=c++20 -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector -fno-vectorize -fno-slp-vectorize -c "$here/com-vtable.cpp" -o guest.obj
arch -arm64 "$linker" /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:com-vtable.exe guest.obj kernel32.lib
if [ "${1:-}" = --check ]; then cmp com-vtable.exe "$here/com-vtable.exe"; else install -m 0644 com-vtable.exe "$here/com-vtable.exe"; fi
