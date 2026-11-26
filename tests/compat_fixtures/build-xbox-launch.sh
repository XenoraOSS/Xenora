#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
cd "$(dirname "$0")/../.."
src="$PWD/tests/compat_fixtures/xbox_launch"
out=${1:-"$PWD/artifacts/xbox-launch"}
mkdir -p "$out"
out=$(cd "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
rust_lib=$(rustc --print sysroot)/lib
if [ -n "${LLD_LINK:-}" ]; then
    linker=$LLD_LINK
    set --
else
    linker=$rust_lib/rustlib/aarch64-apple-darwin/bin/rust-lld
    set -- -flavor link
fi

if [ "$(uname -m)" != arm64 ]; then
    echo "Build the startup fixtures on native ARM64 macOS." >&2
    exit 1
fi
# The guest objects are AMD64 PE. The tools that cross-build them are ARM64.
{
    for tool in "$clang" "$linker"; do
        file -L "$tool"
        lipo -archs "$tool" | grep -Eq '(^| )arm64(e)?( |$)'
        otool -L "$tool"
    done
    if [ -f "$rust_lib/libLLVM.dylib" ]; then
        lipo -archs "$rust_lib/libLLVM.dylib" | grep -Eq '(^| )arm64(e)?( |$)'
    fi
} > "$out/toolchain-native.txt"
cd "$out"
for source in large-startup dependent-startup dependency-one dependency-two; do
    "$clang" -target x86_64-pc-windows-msvc -c "$src/$source.S" -o "$source.obj"
done
"$clang" -target x86_64-pc-windows-msvc -DWRONG_CALLBACK_COUNT \
    -c "$src/large-startup.S" -o large-startup-control.obj
for image in large-startup large-startup-control; do
    "$linker" "$@" /timestamp:0 /machine:x64 /entry:launch_entry /subsystem:console \
        /nodefaultlib /dynamicbase /base:0x140000000 "/out:$image.exe" "$image.obj"
done
"$linker" "$@" /timestamp:0 /dll /noentry /nodefaultlib /export:first_action \
    /out:xenora-launch-one.dll /implib:dependency-one.lib dependency-one.obj
"$linker" "$@" /timestamp:0 /dll /noentry /nodefaultlib /export:second_action \
    /out:xenora-launch-two.dll /implib:dependency-two.lib dependency-two.obj
"$linker" "$@" /timestamp:0 /machine:x64 /entry:launch_entry /subsystem:console /nodefaultlib \
    /base:0x150000000 /out:dependent-startup.exe dependent-startup.obj \
    dependency-one.lib dependency-two.lib

# Produce import metadata only. The fixture ships no implementation DLL.
"$linker" "$@" /lib /machine:x64 "/def:$src/scarlett-runtime.def" /out:scarlett-runtime.lib
"$clang" -target x86_64-pc-windows-msvc -ffreestanding -O2 \
    -c "$src/scarlett-runtime.c" -o scarlett-runtime.obj
"$linker" "$@" /timestamp:0 /machine:x64 /entry:launch_entry /subsystem:console \
    /nodefaultlib /out:scarlett-runtime.exe scarlett-runtime.obj scarlett-runtime.lib
cp "$src/MicrosoftGame.config" "$out/MicrosoftGame.config"
