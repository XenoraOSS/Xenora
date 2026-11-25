#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build process_lifecycle.c, without a C library, as x86-64 PE32+ executables
# at -O0, -O1, -O2 and -Os, plus a control with a wrong expectation.
#
#   build-process-lifecycle.sh [output directory]  default: artifacts/process-lifecycle
#
# MINGW_GCC names an x86_64-w64-mingw32-gcc; default: Homebrew mingw-w64 if
# installed, otherwise PATH. Nothing from mingw-w64 is stored in the repository.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
out=${1:-"$root/artifacts/process-lifecycle"}

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
    for tool in "$gcc" "$("$gcc" -print-prog-name=cc1)" "$("$gcc" -print-prog-name=as)" \
        "$("$gcc" -print-prog-name=ld)"; do
        case "$(lipo -archs "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required: $tool" >&2; exit 1 ;;
        esac
    done
fi
mkdir -p "$out"
flags="-Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-tree-loop-distribute-patterns
    -fno-stack-protector -fno-asynchronous-unwind-tables -nostdlib -nostartfiles -Wl,-e,start"
build() {
    # shellcheck disable=SC2086
    "$gcc" "-$2" $flags ${3:+"-D$3"} -o "$out/$1.exe" \
        "$root/tests/compat_fixtures/process_lifecycle.c" -lkernel32
}
for level in O0 O1 O2 Os; do
    build "process-lifecycle-$level" "$level" ""
done
build process-lifecycle-break O1 LIFECYCLE_BREAK
