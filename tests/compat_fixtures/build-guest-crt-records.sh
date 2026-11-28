#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build-guest-crt-records.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)

gcc=${MINGW_GCC:-/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-gcc}
if [ ! -x "$gcc" ] && [ -z "${MINGW_GCC:-}" ]; then
    gcc=$(command -v x86_64-w64-mingw32-gcc || true)
fi
test "$(uname -m)" = arm64
test -x "$gcc"
lipo "$gcc" -verify_arch arm64
test "$("$gcc" -dumpmachine)" = x86_64-w64-mingw32

verify_native() {
    file -L "$1"
    lipo -archs "$1"
    lipo "$1" -verify_arch arm64
    otool -L "$1"
}
verify_native "$gcc"
for helper in cc1 as ld collect2; do
    tool=$("$gcc" -print-prog-name="$helper")
    if [ ! -x "$tool" ]; then
        tool=$(command -v "$tool")
    fi
    verify_native "$tool"
done

for optimization in O1 O2; do
    arch -arm64 "$gcc" "-$optimization" -std=c17 -Wall -Wextra -Werror \
        -fno-builtin -o "$out/crt-records-$optimization.exe" \
        "$here/guest_crt_records.c"
done
