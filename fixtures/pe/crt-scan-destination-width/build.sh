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
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-crt-scan-width.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY kernel32.dll\nEXPORTS\n ExitProcess\n' > kernel.def
printf 'LIBRARY api-ms-win-crt-stdio-l1-1-0.dll\nEXPORTS\n __stdio_common_vsscanf\n' > stdio.def
for library in kernel stdio; do
    link /lib /def:"$library.def" /machine:x64 /out:"$library.lib"
done
build() {
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c17 -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
        -DTEST_CASE="$2" -c "$here/entry.c" -o guest.obj
    link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 \
        /machine:x64 /out:"$output/$1.exe" guest.obj kernel.lib stdio.lib
}
build ordinary 0
build secure 1
build high-byte 2
build short-wide-buffer 3
