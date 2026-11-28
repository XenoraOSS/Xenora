#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original hosted x86-64 PE32+ fixture with native MinGW-w64 UCRT.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/../../artifacts/crt-runtime-acceptance"}
gcc=${MINGW_GCC:-/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-gcc}
if [ ! -x "$gcc" ] && [ -z "${MINGW_GCC:-}" ]; then
    gcc=$(command -v x86_64-w64-mingw32-gcc)
fi
mkdir -p "$out"
cd "$out"

audit_native() {
    binary=$1
    file -L "$binary"
    archs=$(lipo -archs "$binary")
    printf '%s\n' "$archs"
    case " $archs " in
        *" arm64 "*) ;;
        *) echo "ARM64 tool required: $binary" >&2; exit 1 ;;
    esac
    libraries=$(otool -L "$binary")
    printf '%s\n' "$libraries"
    for library in $(printf '%s\n' "$libraries" | awk 'NR > 1 { print $1 }'); do
        if [ -f "$library" ]; then
            file -L "$library"
            lipo "$library" -verify_arch arm64
        fi
    done
}
if [ "$(uname -s)" = Darwin ]; then
    {
        audit_native "$gcc"
        for helper in cc1 as ld collect2; do
            binary=$("$gcc" -print-prog-name="$helper")
            audit_native "$(command -v "$binary")"
        done
    } > toolchain-native.txt
fi
"$gcc" --version > toolchain.txt

build() {
    name=$1
    shift
    "$gcc" -Wall -Wextra -Werror -fno-builtin "$@" \
        -o "$name.exe" "$here/crt_runtime_acceptance.c"
}
build crt-runtime-acceptance-O0 -O0
build crt-runtime-acceptance-O2 -O2
build crt-runtime-acceptance-control -O2 -DWRONG_EXPECTATION
