#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Builds crt_compare.c with mingw-w64 for the hosted string comparison tests
# (package 7 of #459), at -O0 and -O2, and once at -O1 with CRT_COMPARE_BREAK as a control.
# Usage: build-crt-compare.sh [output-directory]
set -eu
cd "$(dirname "$0")/../.."
root=$PWD

MINGW_GCC="${MINGW_GCC:-x86_64-w64-mingw32-gcc}"
OUT="${1:-$root/artifacts/crt-compare}"
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
"$MINGW_GCC" $FLAGS -O0 -o "$OUT/crt-compare-O0.exe" "$root/tests/compat_fixtures/crt_compare.c"
"$MINGW_GCC" $FLAGS -O2 -o "$OUT/crt-compare-O2.exe" "$root/tests/compat_fixtures/crt_compare.c"
"$MINGW_GCC" $FLAGS -O1 -DCRT_COMPARE_BREAK -o "$OUT/crt-compare-break.exe" "$root/tests/compat_fixtures/crt_compare.c"
echo "built crt-compare-O0.exe crt-compare-O2.exe crt-compare-break.exe in $OUT"
