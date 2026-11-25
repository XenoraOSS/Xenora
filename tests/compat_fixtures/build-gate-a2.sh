#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the Gate A2 program, tests/compat_fixtures/gate_a2.c, as x86-64 PE32+
# executables linked with the mingw-w64 C runtime (UCRT flavour). The
# runtime's own startup code runs before main, which is what makes the program
# "hosted".
#
#   build-gate-a2.sh [output directory]      default: artifacts/gate-a2
#
# Tool, native to the host:
#   MINGW_GCC  an x86_64-w64-mingw32-gcc. Default: Homebrew mingw-w64 if
#              installed, otherwise the one on PATH.
#
# Nothing from mingw-w64 is stored in the repository. Each built executable
# contains that project's startup objects and a little of GCC's support
# library, as any program built with it does; the executables are test
# output and are not committed.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
src="$root/tests/compat_fixtures/gate_a2.c"
out=${1:-"$root/artifacts/gate-a2"}

gcc=${MINGW_GCC:-}
if [ -z "$gcc" ]; then
    gcc=/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-gcc
    [ -x "$gcc" ] || gcc=$(command -v x86_64-w64-mingw32-gcc || true)
fi
if [ -z "$gcc" ] || [ ! -x "$gcc" ]; then
    echo "missing tool: x86_64-w64-mingw32-gcc (set MINGW_GCC, or install mingw-w64)" >&2
    exit 1
fi
# AGENTS.md: host tools must be ARM64, checked with file, lipo -archs and
# otool -L. The driver is a symbolic link into the toolchain; the compiler
# proper, the assembler and the linker it runs are checked as well.
native=
if [ "$(uname -s)" = Darwin ]; then
    tools="$gcc"
    for helper in cc1 as ld collect2; do
        path=$("$gcc" -print-prog-name="$helper")
        case "$path" in
            /*) [ -x "$path" ] && tools="$tools $path" ;;
        esac
    done
    for tool in $tools; do
        case "$(file -bL "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required (file): $tool" >&2; exit 1 ;;
        esac
        case "$(lipo -archs "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required (lipo): $tool" >&2; exit 1 ;;
        esac
        for library in $(otool -L "$tool" | awk 'NR > 1 { print $1 }'); do
            [ -f "$library" ] || continue
            case "$(lipo -archs "$library")" in
                *arm64*) ;;
                *) echo "ARM64 library required by $tool: $library" >&2; exit 1 ;;
            esac
            native="$native  $library: $(lipo -archs "$library")
"
        done
        native="$native$tool: $(file -bL "$tool")
"
    done
fi

mkdir -p "$out"
cd "$out"
{
    "$gcc" --version | head -1
    printf 'target: %s\n' "$("$gcc" -dumpmachine)"
} > toolchain.txt
printf '%s' "$native" > toolchain-native.txt

# name, optimization level, extra definition
build() {
    "$gcc" "-$2" -Wall -Wextra -Werror ${3:+"-D$3"} -o "$1.exe" "$src"
}
for level in O0 O1 O2 Os; do
    build "gate-a2-$level" "$level" ""
done
# Controls: a wrong expectation, exit() half way, abort() half way.
build gate-a2-break O1 GATE_BREAK
build gate-a2-exit O1 GATE_EXIT
build gate-a2-abort O1 GATE_ABORT
# Not a control of the gate: a program that runs for as long as it is told,
# or for ever, for the limits of `xenora run`.
build gate-a2-spin O1 GATE_SPIN
