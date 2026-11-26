#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original PE with native clang/LLD. No Windows SDK is needed.
set -eu

usage() {
    echo "usage: $0 [--check]" >&2
    exit 2
}

case $# in
    0) check=false ;;
    1) [ "$1" = --check ] || usage; check=true ;;
    *) usage ;;
esac

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
if [ -z "${CLANG:-}" ] && [ ! -x "$clang" ]; then
    clang=$(command -v clang)
fi
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ -z "${LLD_LINK:-}" ] && [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
for tool in "$clang" "$linker"; do
    if [ ! -x "$tool" ] || ! lipo "$tool" -verify_arch arm64; then
        printf 'Native ARM64 tool required: %s\n' "$tool" >&2
        exit 1
    fi
done
link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-app-graphics-loop.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"

cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CreateFileW
    WriteFile
    ReadFile
    SetFilePointerEx
    CloseHandle
    ExitProcess
DEF
cat > xenora.def <<'DEF'
LIBRARY xenora.dll
EXPORTS
    frame_begin
    input_poll
    frame_present
    frame_finish
DEF
cat > xenora_graphics.def <<'DEF'
LIBRARY xenora_graphics.dll
EXPORTS
    XenoraCreateDevice
    XenoraCreateCommandQueue
    XenoraCreateCommandAllocator
    XenoraCreateCommandList
    XenoraCreateBuffer
    XenoraCreateRenderTarget
    XenoraSetVertexBuffer
    XenoraSetRenderTarget
    XenoraSetViewport
    XenoraDraw
    XenoraCloseCommandList
    XenoraExecuteCommandLists
    XenoraReadbackResource
    XenoraDestroyResource
    XenoraDestroyCommandList
    XenoraDestroyCommandAllocator
    XenoraDestroyCommandQueue
    XenoraDestroyDevice
DEF
for module in kernel32 xenora xenora_graphics; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/app-graphics-loop.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    /out:app-graphics-loop.exe guest.obj kernel32.lib xenora.lib xenora_graphics.lib

if "$check"; then
    cmp app-graphics-loop.exe "$here/app-graphics-loop.exe"
else
    install -m 0644 app-graphics-loop.exe "$here/app-graphics-loop.exe"
fi
