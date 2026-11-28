#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}

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

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-rtti.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY kernel32.dll\nEXPORTS\n ExitProcess\n' > kernel32.def
printf 'LIBRARY vcruntime140.dll\nEXPORTS\n __RTtypeid\n __RTDynamicCast\n __RTCastToVoid\n' > vcruntime140.def
for library in kernel32 vcruntime140; do
    link /lib /machine:x64 "/def:$library.def" "/out:$library.lib"
done

arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -c \
    "$here/inert_type_info_vtable.s" -o inert_type_info_vtable.obj
for optimization in 0 2; do
    for case_number in 0 1 2 3; do
        image="msvc-rtti-O$optimization-case$case_number"
        rtti=-frtti
        if [ "$case_number" -eq 0 ]; then rtti=-fno-rtti; fi
        arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c++17 \
            "-O$optimization" "-DCASE=$case_number" "$rtti" -fno-exceptions \
            -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
            -Wall -Wextra -Werror -c "$here/rtti.cpp" -o "$out/$image.obj"
        set -- "$out/$image.obj" kernel32.lib
        if [ "$case_number" -ne 0 ]; then
            set -- "$@" inert_type_info_vtable.obj vcruntime140.lib
        fi
        link /entry:entry /subsystem:console /nodefaultlib /machine:x64 /timestamp:0 \
            "/out:$out/$image.exe" "$@"
    done
done
