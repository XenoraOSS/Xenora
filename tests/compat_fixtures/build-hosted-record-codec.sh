#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later

# Usage: build-hosted-record-codec.sh [output directory]
# Set MINGW_GCC to override the native mingw-w64 GCC driver.
set -eu

fail() {
    printf '%s\n' "$*" >&2
    exit 1
}

[ "$#" -le 1 ] || fail "usage: $0 [output directory]"
root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
out=${1:-"$root/artifacts/hosted-record-codec"}
src="$root/tests/compat_fixtures/hosted_record_codec.c"

[ "$(uname -sm)" = "Darwin arm64" ] || fail "a native ARM64 macOS host is required"
gcc=${MINGW_GCC:-/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-gcc}
if [ -z "${MINGW_GCC:-}" ] && [ ! -x "$gcc" ]; then
    gcc=x86_64-w64-mingw32-gcc
fi
gcc=$(command -v "$gcc") || fail "missing x86_64-w64-mingw32-gcc (set MINGW_GCC)"
[ -f "$gcc" ] && [ -x "$gcc" ] || fail "compiler driver is not executable: $gcc"

mkdir -p -- "$out"
native="$out/toolchain-native.txt"
: > "$native"

# This runs in a subshell to keep the path and dependency list local to each
# recursive check. The log also serves as the visited set for dylib IDs.
check_native() (
    binary=$1
    if grep -Fqx -- "$binary" "$native"; then
        exit 0
    fi
    description=$(file -bL -- "$binary")
    case "$description" in
        *Mach-O*arm64*) ;;
        *) fail "ARM64 Mach-O file required: $binary ($description)" ;;
    esac
    architectures=$(lipo -archs "$binary")
    case " $architectures " in
        *" arm64 "*) ;;
        *) fail "ARM64 slice required: $binary ($architectures)" ;;
    esac
    dependencies=$(otool -arch arm64 -L "$binary")
    printf '%s\n  file: %s\n  lipo: %s\n%s\n' \
        "$binary" "$description" "$architectures" "$dependencies" >> "$native"
    libraries=$(printf '%s\n' "$dependencies" |
        awk 'NR > 1 { sub(/^[[:space:]]+/, ""); sub(/ \(compatibility version.*/, ""); print }')
    printf '%s\n' "$libraries" | while IFS= read -r library; do
        case "$library" in
            '') ;;
            /usr/lib/*|/System/Library/*)
                # macOS system dylibs may exist only in the ARM64 dyld cache.
                if [ -f "$library" ]; then
                    check_native "$library"
                else
                    printf '  macOS dyld cache: %s\n' "$library" >> "$native"
                fi
                ;;
            /*) check_native "$library" ;;
            *) fail "cannot verify dependency $library of $binary" ;;
        esac
    done
)

# Evaluate compiler queries only after checking that the driver is native.
check_native "$gcc"
for helper in cc1 as ld collect2; do
    tool=$("$gcc" -print-prog-name="$helper")
    tool=$(command -v "$tool") || fail "cannot locate GCC helper: $helper"
    [ -f "$tool" ] && [ -x "$tool" ] || fail "GCC helper is not executable: $tool"
    check_native "$tool"
done

target=$("$gcc" -dumpmachine)
[ "$target" = x86_64-w64-mingw32 ] || fail "unexpected compiler target: $target"
{
    "$gcc" --version
    printf 'target: %s\nCRT: ucrt\n' "$target"
} > "$out/toolchain.txt"

for level in O0 O1 O2 O3; do
    exe="$out/hosted-record-codec-$level.exe"
    "$gcc" "-$level" -mcrtdll=ucrt -std=c11 -Wall -Wextra -Werror \
        -Wl,--no-insert-timestamp -o "$exe" "$src"
    description=$(file -bL -- "$exe")
    case "$description" in
        PE32+*x86-64*|PE32+*AMD64*) printf '%s: %s\n' "$level" "$description" ;;
        *) fail "expected AMD64 PE32+ output: $exe ($description)" ;;
    esac
done
