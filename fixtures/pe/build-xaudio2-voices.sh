#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ -z "${LLD_LINK:-}" ] && [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] && lipo -verify_arch arm64 "$tool" || { echo "Native ARM64 tool required: $tool" >&2; exit 1; }
done
link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-xaudio2-voices.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY kernel32.dll\nEXPORTS\n    ExitProcess\n' > kernel32.def
link /lib /def:kernel32.def /machine:x64 /out:kernel32.lib
link /lib /def:"$here/xaudio2_9.def" /machine:x64 /out:xaudio2_9.lib
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -x c++ -std=c++20 -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector -fno-vectorize -fno-slp-vectorize -c "$here/xaudio2-voices.cpp" -o guest.obj
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -x c -std=c17 -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -c "$here/xaudio2-fltused.c" -o support.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:xaudio2-voices.exe guest.obj support.obj kernel32.lib xaudio2_9.lib
if [ "${1:-}" = --check ]; then cmp xaudio2-voices.exe "$here/xaudio2-voices.exe"; else install -m 0644 xaudio2-voices.exe "$here/xaudio2-voices.exe"; fi
