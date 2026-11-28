#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the three original x86-64 guest images using native ARM64 tools.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(cd -- "$out" && pwd)
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

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-com-package.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > ole32.def <<'DEF'
LIBRARY ole32.dll
EXPORTS
    CoInitializeEx
    CoUninitialize
    CoCreateInstance
DEF
cat > fixture.def <<'DEF'
LIBRARY xenora_com_fixture.dll
EXPORTS
    DllCanUnloadNow
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
for library in ole32 fixture kernel32; do
    link /lib /machine:x64 "/def:$library.def" "/out:$library.lib"
done

for source in producer consumer entry; do
    "$clang" --target=x86_64-pc-windows-msvc -std=c17 -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
        -c "$here/$source.c" -o "$source.obj"
done

link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/xenora_com_producer.dll" /implib:producer.lib producer.obj ole32.lib
link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/xenora_com_consumer.dll" /implib:consumer.lib consumer.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/com-package.exe" entry.obj producer.lib consumer.lib ole32.lib fixture.lib kernel32.lib
