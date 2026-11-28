#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Compile the original inheritance package for the Microsoft x64 C++ ABI.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang++)}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ -z "${LLD_LINK:-}" ] && [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi

test "$(uname -m)" = arm64
for tool in "$clang" "$linker"; do
    file -L "$tool"
    lipo -archs "$tool"
    lipo "$tool" -verify_arch arm64
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
        -ffreestanding -fno-builtin -fno-stack-protector -fno-exceptions -fno-rtti \
        -fno-vectorize -fno-slp-vectorize "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-inheritance.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > ucrtbase.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    malloc
    free
    putchar
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    GetProcessHeap
    HeapSize
DEF
for module in ucrtbase kernel32; do
    link /lib /machine:x64 "/def:$module.def" "/out:$module.lib"
done
compile -c "$here/producer.cpp" -o "$out/producer.obj"
compile -c "$here/consumer.cpp" -o "$out/consumer.obj"
compile -c "$here/entry.cpp" -o entry.obj
compile -DREJECT_HANDOFF -c "$here/entry.cpp" -o rejected.obj
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/xenora_inheritance_producer.dll" /implib:producer.lib "$out/producer.obj" ucrtbase.lib
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/xenora_inheritance_consumer.dll" /implib:consumer.lib "$out/consumer.obj"
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/inheritance.exe" entry.obj producer.lib consumer.lib kernel32.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/inheritance-reject.exe" rejected.obj producer.lib consumer.lib kernel32.lib
