#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the guest textured scene program, tests/compat_fixtures/guest_textured.c,
# as x86-64 PE32+ executables with no C library linked.
#
#   build-guest-textured.sh [output directory]   default: artifacts/guest-textured
#
# Tools, all native to the host:
#   CLANG     a clang that accepts --target=x86_64-pc-windows-msvc. Default:
#             Homebrew LLVM if installed, otherwise the clang on PATH.
#   LLD_LINK  an lld-link. Default: Homebrew lld if installed, otherwise the
#             rust-lld that ships in the active Rust toolchain.
#
# The linker also makes the two import libraries, from the definition files
# written below, so nothing from a Windows SDK or another C runtime is used.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
src="$root/tests/compat_fixtures/guest_textured.c"
out=${1:-"$root/artifacts/guest-textured"}

clang=${CLANG:-}
if [ -z "$clang" ]; then
    clang=/opt/homebrew/opt/llvm/bin/clang
    [ -x "$clang" ] || clang=$(command -v clang)
fi
flavor=
linker=${LLD_LINK:-}
if [ -z "$linker" ]; then
    linker=/opt/homebrew/opt/lld/bin/lld-link
    if [ ! -x "$linker" ]; then
        host=$(rustc -vV | sed -n 's/^host: //p')
        linker="$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld"
        flavor="-flavor link"
    fi
fi
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] || { echo "missing tool: $tool" >&2; exit 1; }
done
# AGENTS.md: host tools and what they load must be ARM64, checked with file,
# lipo -archs and otool -L. An @rpath library is looked up through the tool's
# own run paths. A library that is still not a file on disk is in the system's
# shared cache, and is listed only.
native=
if [ "$(uname -s)" = Darwin ]; then
    for tool in "$clang" "$linker"; do
        case "$(file -bL "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required (file): $tool" >&2; exit 1 ;;
        esac
        case "$(lipo -archs "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required (lipo): $tool" >&2; exit 1 ;;
        esac
        here=$(dirname "$tool")
        runpaths=$(otool -l "$tool" | awk '/LC_RPATH/ { getline; getline; print $2 }' |
            sed "s|@loader_path|$here|; s|@executable_path|$here|")
        for library in $(otool -L "$tool" | awk 'NR > 1 { print $1 }'); do
            case "$library" in
                @rpath/*)
                    for runpath in $runpaths; do
                        if [ -f "$runpath/${library#@rpath/}" ]; then
                            library="$runpath/${library#@rpath/}"
                            break
                        fi
                    done ;;
            esac
            [ -f "$library" ] || continue
            case "$(lipo -archs "$library")" in
                *arm64*) ;;
                *) echo "ARM64 library required by $tool: $library" >&2; exit 1 ;;
            esac
            native="$native  $library: $(lipo -archs "$library")
"
        done
        native="$native$(file -bL "$tool")
$(otool -L "$tool")
"
    done
fi
# $flavor is empty or two words on purpose.
# shellcheck disable=SC2086
link() { "$linker" $flavor "$@"; }

mkdir -p "$out"
cd "$out"
{
    "$clang" --version | head -1
    link --version | head -1
} > toolchain.txt
# What the native checks above looked at.
printf '%s' "$native" > toolchain-native.txt

cat > xenora_graphics.def <<'DEF'
LIBRARY xenora_graphics.dll
EXPORTS
    XenoraCreateCommandAllocator
    XenoraCreateCommandList
    XenoraCreateCommandQueue
    XenoraCreateBuffer
    XenoraCreateDescriptorHeap
    XenoraCreateDevice
    XenoraCreateRenderTarget
    XenoraCreateRootSignature
    XenoraCreateSampler
    XenoraCreateShaderResourceView
    XenoraCreateTexture2D
    XenoraCloseCommandList
    XenoraDestroyCommandAllocator
    XenoraDestroyCommandList
    XenoraDestroyCommandQueue
    XenoraDestroyDevice
    XenoraDestroyResource
    XenoraDraw
    XenoraExecuteCommandLists
    XenoraGetLastFrameInfo
    XenoraNoSuchService
    XenoraReadbackResource
    XenoraSetDescriptorHeaps
    XenoraSetGraphicsRootDescriptorTable
    XenoraSetGraphicsRootSignature
    XenoraSetRenderTarget
    XenoraSetVertexBuffer
    XenoraSetViewport
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
link /lib /machine:x64 /def:xenora_graphics.def /out:xenora_graphics.lib
link /lib /machine:x64 /def:kernel32.def /out:kernel32.lib

# name, optimization level, extra definition
build() {
    "$clang" --target=x86_64-pc-windows-msvc "-$2" -ffreestanding -fno-stack-protector \
        -Wall -Wextra -Werror ${3:+"-D$3"} -c "$src" -o "$1.obj"
    link /entry:guest_textured_entry /subsystem:console /nodefaultlib /timestamp:0 \
        "/out:$1.exe" "$1.obj" xenora_graphics.lib kernel32.lib
}
for level in O0 O1 O2 Os; do
    build "guest-textured-$level" "$level" ""
done
# Control: an import nobody provides. The loader must refuse the image before
# the entry point runs.
build guest-textured-missing O1 GATE_MISSING
