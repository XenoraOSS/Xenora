#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)

clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-"$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"}
test "$(uname -m)" = arm64
for tool in "$clang" "$linker"; do
    lipo "$tool" -verify_arch arm64
    file -L "$tool"
    otool -L "$tool"
done

link() {
    case "${linker##*/}" in
        rust-lld) "$linker" -flavor link "$@" ;;
        *) "$linker" "$@" ;;
    esac
}

compile() {
    "$clang" --target=x86_64-pc-windows-msvc -std=c++17 -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector \
        -fno-vectorize -fno-slp-vectorize "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-package-crt-lifecycle.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CreateFileW
    SetFilePointerEx
    WriteFile
    CloseHandle
    ExitProcess
DEF
cat > ucrtbase.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    _initterm
    _initialize_onexit_table
    _register_onexit_function
    _execute_onexit_table
DEF
for module in kernel32 ucrtbase; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

compile -DLIFECYCLE_PROVIDER -c "$here/module.cpp" -o provider.obj
compile -c "$here/module.cpp" -o consumer.obj
compile -c "$here/entry.cpp" -o entry.obj

link /dll /entry:DllMain /nodefaultlib /timestamp:0 /machine:x64 /base:0x150100000 \
    "/out:$out/lifecycle_provider.dll" /implib:provider.lib provider.obj ucrtbase.lib kernel32.lib
link /dll /entry:DllMain /nodefaultlib /timestamp:0 /machine:x64 /base:0x150200000 \
    "/out:$out/lifecycle_consumer.dll" /implib:consumer.lib consumer.obj provider.lib ucrtbase.lib kernel32.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /base:0x150000000 \
    "/out:$out/lifecycle.exe" entry.obj consumer.lib kernel32.lib
