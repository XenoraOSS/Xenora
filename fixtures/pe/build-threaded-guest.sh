#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/threaded-guest.exe"}
case "$out" in
    /*) ;;
    *) out="$PWD/$out" ;;
esac
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
for tool in "$clang" "$linker"; do
    case "$(file -bL "$tool")" in
        *arm64*) ;;
        *) echo "Expected a native ARM64 tool: $tool" >&2; exit 1 ;;
    esac
    if ! lipo "$tool" -verify_arch arm64; then
        echo "Expected a native ARM64 tool: $tool" >&2
        exit 1
    fi
done
link() {
    case "${linker##*/}" in
        rust-lld) "$linker" -flavor link "$@" ;;
        *) "$linker" "$@" ;;
    esac
}
temp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-threaded-guest.XXXXXX")
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
    SetEvent
    WaitForSingleObject
    SignalObjectAndWait
    CloseHandle
    ExitProcess
    CreateThread
    GetCurrentThreadId
    GetExitCodeThread
    GetLastError
    VirtualAlloc
    VirtualFree
DEF
cat > xenora_threaded_app.def <<'DEF'
LIBRARY xenora_threaded_app.dll
EXPORTS
    XenoraNextSceneCommand
    XenoraReportSceneFrame
    XenoraReportSceneCleanup
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
for module in kernel32 xenora_threaded_app xenora_graphics; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done
"$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/threaded-guest.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out" guest.obj kernel32.lib xenora_threaded_app.lib xenora_graphics.lib
chmod 644 "$out"
