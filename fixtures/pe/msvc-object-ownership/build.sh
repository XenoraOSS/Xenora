#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: fixtures/pe/runtime-ownership/build.sh artifacts/runtime-ownership
# The original application and independently compiled provider DLLs use local
# import definitions. CLANG and LLD_LINK may select other native ARM64 tools.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
outdir=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$outdir"
outdir=$(cd -- "$outdir" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
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
    "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
        -std=c++17 -ffreestanding -fno-exceptions -fno-rtti -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-object-ownership.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > ucrtbase.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    malloc
    free
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    GetProcessHeap
    HeapSize
DEF
for module in ucrtbase kernel32; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done
# Build each translation unit independently. The consumer sees only Object.
compile -c "$here/producer.cpp" -o producer.obj
compile -c "$here/consumer.cpp" -o consumer.obj
compile -c "$here/entry.cpp" -o entry.obj
compile -DXENORA_REJECT_CONSUME -c "$here/entry.cpp" -o reject.obj

link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/xenora_producer.dll" /implib:producer.lib producer.obj ucrtbase.lib
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/xenora_consumer.dll" /implib:consumer.lib consumer.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/object-ownership.exe" entry.obj producer.lib consumer.lib kernel32.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/object-ownership-reject.exe" reject.obj producer.lib consumer.lib kernel32.lib
