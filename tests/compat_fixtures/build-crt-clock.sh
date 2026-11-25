#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build tests/compat_fixtures/crt_clock.c (#459, package 5) as x86-64 PE32+ executables
# linked with the mingw-w64 C runtime (UCRT flavour). With -fno-builtin every
# stdio.h function it calls is an import of the Universal CRT.
#
#   build-crt-clock.sh [output directory]      default: artifacts/crt-clock
#
# Tool, native to the host:
#   MINGW_GCC  an x86_64-w64-mingw32-gcc. Default: Homebrew mingw-w64 if
#              installed, otherwise the one on PATH.
#
# Nothing from mingw-w64 is stored in the repository; the executables are test
# output and are not committed.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
src="$root/tests/compat_fixtures/crt_clock.c"
out=${1:-"$root/artifacts/crt-clock"}

gcc=${MINGW_GCC:-}
if [ -z "$gcc" ]; then
    gcc=/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-gcc
    [ -x "$gcc" ] || gcc=$(command -v x86_64-w64-mingw32-gcc || true)
fi
if [ -z "$gcc" ] || [ ! -x "$gcc" ]; then
    echo "missing tool: x86_64-w64-mingw32-gcc (set MINGW_GCC, or install mingw-w64)" >&2
    exit 1
fi
# AGENTS.md: host tools must be ARM64.
if [ "$(uname -s)" = Darwin ]; then
    for tool in "$gcc" "$("$gcc" -print-prog-name=cc1)" \
        "$("$gcc" -print-prog-name=as)" "$("$gcc" -print-prog-name=ld)"; do
        case "$(lipo -archs "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required: $tool" >&2; exit 1 ;;
        esac
    done
fi

mkdir -p "$out"
cd "$out"
"$gcc" --version | head -1 > toolchain.txt

# name, optimization level, extra definition
build() {
    "$gcc" "-$2" -Wall -Wextra -Werror -fno-builtin ${3:+"-D$3"} -o "$1.exe" "$src"
}
for level in O0 O2; do
    build "crt-clock-$level" "$level" ""
done
# Control: one expected value is wrong, so check 17 fails.
build crt-clock-break O1 CRT_CLOCK_BREAK
