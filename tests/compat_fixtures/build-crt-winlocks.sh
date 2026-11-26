#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Builds crt_winlocks.c with mingw-w64 for the hosted lock, file-mode, disk, heap and view tests
# (the sqlite3 survey), at -O0 and -O2, and once at -O1 with CRT_WINLOCKS_BREAK as a control.
# Usage: build-crt-winlocks.sh [output-directory]
set -eu
cd "$(dirname "$0")/../.."
root=$PWD

MINGW_GCC="${MINGW_GCC:-x86_64-w64-mingw32-gcc}"
OUT="${1:-$root/artifacts/crt-winlocks}"
mkdir -p "$OUT"

for tool in "$MINGW_GCC" x86_64-w64-mingw32-objdump; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: $tool not found; install mingw-w64 (brew install mingw-w64)" >&2
        exit 1
    fi
    if lipo -archs "$(command -v "$tool")" 2>/dev/null | grep -q x86_64 && ! lipo -archs "$(command -v "$tool")" 2>/dev/null | grep -q arm64; then
        echo "error: $tool is not a native ARM64 binary" >&2
        exit 1
    fi
done

FLAGS="-Wall -Wextra -Werror -fno-builtin"
"$MINGW_GCC" $FLAGS -O0 -o "$OUT/crt-winlocks-O0.exe" "$root/tests/compat_fixtures/crt_winlocks.c"
"$MINGW_GCC" $FLAGS -O2 -o "$OUT/crt-winlocks-O2.exe" "$root/tests/compat_fixtures/crt_winlocks.c"
"$MINGW_GCC" $FLAGS -O1 -DCRT_WINLOCKS_BREAK -o "$OUT/crt-winlocks-break.exe" "$root/tests/compat_fixtures/crt_winlocks.c"
echo "built crt-winlocks-O0.exe crt-winlocks-O2.exe crt-winlocks-break.exe in $OUT"
