#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original reopen-and-verify fixture with native clang and LLD.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
check=false
if [ "${1:-}" = --check ]; then
    check=true
    shift
fi
if [ "$#" -gt 1 ]; then
    echo "usage: $0 [--check] [output PE path]" >&2
    exit 2
fi
out=${1:-"$here/persistent-reopen-guest.exe"}
case "$out" in
    /*) ;;
    *) out="$PWD/$out" ;;
esac
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ -z "${CLANG:-}" ] && [ ! -x "$clang" ]; then
    clang=$(command -v clang)
fi
if [ -z "${LLD_LINK:-}" ] && [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
for tool in "$clang" "$linker"; do
    case "$(file -bL "$tool")" in
        *arm64*) ;;
        *) echo "Expected a native ARM64 tool: $tool" >&2; exit 1 ;;
    esac
    lipo "$tool" -verify_arch arm64
done
link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}
temp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-persistent-reopen-guest.XXXXXX")
trap 'rm -rf "$temp"' EXIT
cd "$temp"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CreateFileW
    WriteFile
    ReadFile
    SetFilePointerEx
    CreateEventW
    GetCurrentProcess
    DuplicateHandle
    SetEvent
    WaitForSingleObject
    CloseHandle
    ExitProcess
DEF
cat > xenora_session.def <<'DEF'
LIBRARY xenora_session.dll
EXPORTS
    XenoraNextEvent
    XenoraPresent
    XenoraFinish
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
    XenoraGetLastFrameInfo
    XenoraDestroyResource
    XenoraDestroyCommandList
    XenoraDestroyCommandAllocator
    XenoraDestroyCommandQueue
    XenoraDestroyDevice
DEF
for module in kernel32 xenora_session xenora_graphics; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/persistent-reopen-guest.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    /out:fixture.exe guest.obj kernel32.lib xenora_session.lib xenora_graphics.lib
if "$check"; then
    cmp fixture.exe "$out"
else
    install -m 0644 fixture.exe "$out"
fi
