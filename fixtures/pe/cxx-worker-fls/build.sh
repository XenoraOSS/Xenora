#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}
if [ ! -x "$linker" ] && [ -z "${LLD_LINK:-}" ]; then
    linker=$(command -v lld-link || true)
fi
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

compile() {
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-worker-fls.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CloseHandle
    CreateEventW
    CreateThread
    ExitProcess
    ExitThread
    FlsAlloc
    FlsFree
    FlsGetValue
    FlsSetValue
    GetCurrentThreadId
    GetExitCodeThread
    GetProcessHeap
    HeapAlloc
    HeapFree
    SetEvent
    WaitForSingleObject
DEF
link /lib /machine:x64 /def:kernel32.def /out:kernel32.lib
compile -std=c++17 -fno-rtti -fno-exceptions -c "$here/producer.cpp" -o producer.obj
compile -std=c17 -c "$here/consumer.c" -o consumer.obj
compile -std=c17 -c "$here/entry.c" -o entry.obj
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/fls_producer.dll" /implib:producer.lib producer.obj kernel32.lib
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/fls_consumer.dll" /implib:consumer.lib consumer.obj producer.lib kernel32.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/cxx-worker-fls.exe" entry.obj producer.lib consumer.lib kernel32.lib
