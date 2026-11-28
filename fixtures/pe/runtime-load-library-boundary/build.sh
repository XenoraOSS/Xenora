#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}

test "$(uname -m)" = arm64
for tool in "$clang" "$linker"; do
    file -L "$tool"
    lipo -archs "$tool"
    lipo -verify_arch arm64 "$tool"
    otool -L "$tool"
done

link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-runtime-load-boundary.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
    GetLastError
    GetModuleHandleA
    GetStdHandle
    LoadLibraryA
    SetLastError
    WriteConsoleA
DEF
link /lib /machine:x64 /def:kernel32.def /out:kernel32.lib

for source in provider provisioned packaged; do
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c17 -O1 \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
        -Wall -Wextra -Werror -c "$here/$source.c" -o "$source.obj"
done
link /dll /entry:DllMain /nodefaultlib /machine:x64 /timestamp:0 \
    "/out:$out/xenora_runtime_boundary.dll" /implib:provider.lib provider.obj kernel32.lib
link /entry:entry /subsystem:console /nodefaultlib /machine:x64 /timestamp:0 \
    "/out:$out/provisioned.exe" provisioned.obj kernel32.lib
link /entry:entry /subsystem:console /nodefaultlib /machine:x64 /timestamp:0 \
    "/out:$out/packaged.exe" packaged.obj kernel32.lib provider.lib
