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
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-runtime-ownership.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > ucrtbase.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    malloc
    free
    _errno
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
compile -c "$here/allocate.c" -o allocate.obj
compile -c "$here/release.c" -o release.obj
compile -c "$here/entry.c" -o entry.obj
compile -DXENORA_CORRUPT_PAYLOAD -c "$here/entry.c" -o corrupt.obj

link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/xenora_allocate.dll" /implib:allocate.lib allocate.obj ucrtbase.lib
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/xenora_release.dll" /implib:release.lib release.obj allocate.lib ucrtbase.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/ownership.exe" entry.obj allocate.lib release.lib ucrtbase.lib kernel32.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$outdir/ownership-corrupt.exe" corrupt.obj allocate.lib release.lib ucrtbase.lib kernel32.lib
