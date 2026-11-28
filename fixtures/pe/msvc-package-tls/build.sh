#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out/reject"
out=$(CDPATH= cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang++)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}
test "$(uname -m)" = arm64
for tool in "$clang" "$linker"; do
    file -L "$tool"
    lipo -archs "$tool"
    lipo "$tool" -verify_arch arm64
    otool -L "$tool"
done

compile() {
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c++17 -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-rtti -fno-exceptions -fno-stack-protector \
        -fno-vectorize -fno-slp-vectorize "$@"
}

link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-package-tls.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
for source in directory producer consumer entry; do
    compile -c "$here/$source.cpp" -o "$source.obj"
done
compile -DPRODUCER_SEED=11 -c "$here/producer.cpp" -o control.obj

link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 /base:0x150100000 /include:_tls_used \
    "/out:$out/xenora_tls_producer.dll" /implib:producer.lib directory.obj producer.obj
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 /base:0x150100000 /include:_tls_used \
    "/out:$out/reject/xenora_tls_producer.dll" /implib:control.lib directory.obj control.obj
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 /base:0x150200000 /include:_tls_used \
    "/out:$out/xenora_tls_consumer.dll" /implib:consumer.lib directory.obj consumer.obj producer.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /base:0x150000000 \
    /include:_tls_used "/out:$out/msvc-package-tls.exe" directory.obj entry.obj producer.lib consumer.lib
