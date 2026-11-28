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
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-env-expansion-utf8.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY kernel32.dll\nEXPORTS\n ExitProcess\n' > kernel.def
printf 'LIBRARY api-ms-win-core-processenvironment-l1-1-0.dll\nEXPORTS\n ExpandEnvironmentStringsA\n ExpandEnvironmentStringsW\n SetEnvironmentVariableW\n' > string.def
for library in kernel string; do
    link /lib /def:"$library.def" /machine:x64 /out:"$library.lib"
done
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c17 -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/entry.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 \
    /machine:x64 /out:"$output/guest.exe" guest.obj kernel.lib string.lib
