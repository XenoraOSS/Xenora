#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the module programs as x86-64 PE32+ executables:
# modules.c without a C library, and modules.cpp with the mingw-w64
# C runtime.
#
#   build-modules.sh [output directory]     default: artifacts/modules
#
# Tools, native to the host:
#   MINGW_GCC  an x86_64-w64-mingw32-gcc; its g++ and dlltool are beside it.
#              Default: Homebrew mingw-w64 if installed, otherwise PATH.
#
# Nothing from mingw-w64 is stored in the repository.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
fixtures="$root/tests/compat_fixtures"
out=${1:-"$root/artifacts/modules"}

gcc=${MINGW_GCC:-}
if [ -z "$gcc" ]; then
    gcc=/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-gcc
    [ -x "$gcc" ] || gcc=$(command -v x86_64-w64-mingw32-gcc || true)
fi
if [ -z "$gcc" ] || [ ! -x "$gcc" ]; then
    echo "missing tool: x86_64-w64-mingw32-gcc (set MINGW_GCC, or install mingw-w64)" >&2
    exit 1
fi
gxx=${gcc%gcc}g++
dlltool=${gcc%gcc}dlltool
# AGENTS.md: host tools must be ARM64.
if [ "$(uname -s)" = Darwin ]; then
    for tool in "$gcc" "$gxx" "$dlltool" "$("$gcc" -print-prog-name=cc1)" \
        "$("$gcc" -print-prog-name=cc1plus)" "$("$gcc" -print-prog-name=as)" \
        "$("$gcc" -print-prog-name=ld)"; do
        case "$(lipo -archs "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required: $tool" >&2; exit 1 ;;
        esac
    done
fi
mkdir -p "$out"
cd "$out"
"$gcc" --version | head -1 > toolchain.txt

flags="-Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-tree-loop-distribute-patterns
    -fno-stack-protector -fno-asynchronous-unwind-tables -nostdlib -nostartfiles -Wl,-e,start"
# name, optimization level, extra definition, extra libraries before kernel32
build() {
    # shellcheck disable=SC2086
    "$gcc" "-$2" $flags ${3:+"-D$3"} -o "$1.exe" "$fixtures/modules.c" ${4:-} -lkernel32
}
for level in O0 O1 O2 Os; do
    build "modules-$level" "$level" "" ""
done
# A control with a wrong expectation, and the variant that pins a module.
build modules-break O1 MODULES_BREAK ""
build modules-pin O1 MODULES_PIN ""
# The same program, importing from the API sets and KernelBase instead.
library() {
    file=$1
    name=$2
    shift 2
    { printf 'LIBRARY %s\nEXPORTS\n' "$name"; printf '    %s\n' "$@"; } > "$file.def"
    "$dlltool" -d "$file.def" -l "$file.a"
}
library loader api-ms-win-core-libraryloader-l1-1-0.dll GetModuleHandleA GetModuleHandleW GetProcAddress
library base kernelbase.dll GetModuleHandleExA GetModuleHandleExW FreeLibrary
library legacy api-ms-win-core-kernel32-legacy-l1-1-0.dll LoadLibraryA
library processes api-ms-win-core-psapi-l1-1-0.dll K32GetModuleInformation
build modules-apiset O1 "" "loader.a base.a legacy.a processes.a"

for level in O0 O1 O2 Os; do
    "$gxx" "-$level" -Wall -Wextra -Werror -std=c++17 -fno-exceptions -fno-rtti \
        -o "modules-cpp-$level.exe" "$fixtures/modules.cpp" -lpsapi
done
