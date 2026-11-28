#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}
test "$(uname -m)" = arm64
for tool in "$clang" "$linker"; do
    file -L "$tool"
    lipo -archs "$tool"
    lipo -verify_arch arm64 "$tool"
    otool -L "$tool"
done
output=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$output"
output=$(CDPATH= cd -- "$output" && pwd)
link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-format-message-escapes.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY kernel32.dll\nEXPORTS\n FormatMessageA\n FormatMessageW\n GetLastError\n ExitProcess\n' > kernel.def
link /lib /def:kernel.def /machine:x64 /out:kernel.lib
build() {
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c17 -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
        -DTEST_WIDE="$2" -c "$here/entry.c" -o guest.obj
    link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 \
        /machine:x64 /out:"$output/$1.exe" guest.obj kernel.lib
}
build ansi 0
build wide 1
