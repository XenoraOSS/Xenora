#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build-guest-cpp-ordered-nodes.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)

gxx=${MINGW_GXX:-/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-g++}
if [ ! -x "$gxx" ] && [ -z "${MINGW_GXX:-}" ]; then
    gxx=$(command -v x86_64-w64-mingw32-g++ || true)
fi
test "$(uname -m)" = arm64
test -x "$gxx"
lipo "$gxx" -verify_arch arm64
test "$("$gxx" -dumpmachine)" = x86_64-w64-mingw32

verify_native() {
    file -L "$1"
    lipo -archs "$1"
    lipo "$1" -verify_arch arm64
    otool -L "$1"
}
verify_native "$gxx"
for helper in cc1plus as ld collect2; do
    tool=$("$gxx" -print-prog-name="$helper")
    if [ ! -x "$tool" ]; then
        tool=$(command -v "$tool")
    fi
    verify_native "$tool"
done

for optimization in O1 O2; do
    arch -arm64 "$gxx" "-$optimization" -std=c++17 -Wall -Wextra -Werror \
        -static-libstdc++ -static-libgcc -o "$out/ordered-nodes-$optimization.exe" \
        "$here/guest_cpp_ordered_nodes.cpp"
done
