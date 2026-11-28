#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/../../../artifacts/authored-session"}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)

clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-"$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"}
for tool in "$clang" "$linker"; do
    case "$(file -bL "$tool")" in
        *arm64*) ;;
        *) echo "Expected a native ARM64 tool: $tool" >&2; exit 1 ;;
    esac
    lipo "$tool" -verify_arch arm64
done

link() {
    case "${linker##*/}" in
        rust-lld) "$linker" -flavor link "$@" ;;
        *) "$linker" "$@" ;;
    esac
}

temp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-authored-session.XXXXXX")
trap 'rm -rf "$temp"' EXIT
cd "$temp"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CreateFileW
    WriteFile
    ReadFile
    SetFilePointerEx
    CloseHandle
    Sleep
    GetTickCount64
    GetSystemTimeAsFileTime
    ExitProcess
DEF
cat > session.def <<'DEF'
LIBRARY xenora-session-v1.dll
EXPORTS
    XenoraSessionNextInput
    XenoraSessionPresent
    XenoraSessionFinish
DEF
cat > graphics.def <<'DEF'
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
for module in kernel32 session graphics; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done
compile() {
    "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize "$@"
}
compile -c "$here/session.c" -o guest.obj

build() {
    link "/entry:$1" /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
        "/out:$out/$2.exe" guest.obj kernel32.lib session.lib graphics.lib
}

build entry authored-session
build missing_present_entry authored-session-missing-present
build extra_present_entry authored-session-extra-present
build leak_entry authored-session-leak
build stall_frame_entry authored-session-stall-frame
build deterministic_clock_entry authored-session-deterministic-clock
build large_frame_entry authored-session-large-frame

build_companion() {
    variant=$1
    library=$2
    shift 2
    mkdir -p "$out/$variant"
    compile "$@" -c "$here/companion.c" -o companion.obj
    link /dll /entry:DllMain /nodefaultlib /base:0x150100000 /timestamp:0 /machine:x64 \
        "/out:$out/$variant/xenora-authored-companion.dll" "/implib:$library.lib" \
        companion.obj session.lib
}

build_companion . companion
build_companion refuse-attach refused -DREFUSE_ATTACH
build_companion abi-during-attach attach -DCALL_DURING_ATTACH
build_companion abi-during-detach detach -DCALL_DURING_DETACH
build_companion fault-during-detach fault -DFAULT_DURING_DETACH
cp "$out/xenora-authored-companion.dll" "$out/xenora-session-v1.dll"
compile -DSESSION_COMPANION -c "$here/session.c" -o packaged-guest.obj
link /entry:entry /subsystem:console /nodefaultlib /base:0x150000000 /timestamp:0 /machine:x64 \
    "/out:$out/authored-session-package.exe" packaged-guest.obj companion.lib kernel32.lib session.lib graphics.lib
