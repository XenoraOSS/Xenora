#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Builds original freestanding MSVC-ABI guests against the decorated exports.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang++}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] && lipo -verify_arch arm64 "$tool" || { echo "Native ARM64 tool required: $tool" >&2; exit 1; }
done
mode=${1:-}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-standard-stream.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY msvcp140.dll\nEXPORTS\n ?cout@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A DATA\n ??0Init@ios_base@std@@QEAA@XZ\n ??1Init@ios_base@std@@QEAA@XZ\n ?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@PEBD_J@Z\n ?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@XZ\n ?good@ios_base@std@@QEBA_NXZ\n' > msvcp.def
printf 'LIBRARY kernel32.dll\nEXPORTS\n ExitProcess\n' > kernel.def
for lib in msvcp kernel; do
    arch -arm64 "$linker" /lib /def:"$lib.def" /machine:x64 /out:"$lib.lib"
done
build() {
    name=$1
    define=$2
    obj="guest-$name.obj"
    exe="guest-$name.exe"
    if [ -n "$define" ]; then
        arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c++20 -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector -fno-vectorize -fno-slp-vectorize -D"$define" -c "$here/msvc-standard-stream.cpp" -o "$obj"
    else
        arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c++20 -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector -fno-vectorize -fno-slp-vectorize -c "$here/msvc-standard-stream.cpp" -o "$obj"
    fi
    arch -arm64 "$linker" /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:"$exe" "$obj" msvcp.lib kernel.lib
    if [ "$mode" = "--check" ]; then
        cmp "$exe" "$here/msvc-$name.exe"
    else
        install -m 0644 "$exe" "$here/msvc-$name.exe"
    fi
}
build standard-stream ''
build standard-stream-exception XENORA_EXCEPTION
build standard-stream-invalid XENORA_INVALID
build standard-stream-oversize XENORA_OVERSIZE
printf 'Built msvc-standard-stream*.exe\n'
