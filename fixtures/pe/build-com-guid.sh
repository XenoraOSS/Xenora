#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original COM base fixture with native clang/LLD. No Windows SDK is
# needed: the import libraries come from the documented export names alone.
set -eu

usage() {
    echo "usage: $0 [--check]" >&2
    exit 2
}

case $# in
    0) check=false ;;
    1) [ "$1" = --check ] || usage; check=true ;;
    *) usage ;;
esac

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
if [ -z "${CLANG:-}" ] && [ ! -x "$clang" ]; then
    clang=$(command -v clang)
fi
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ -z "${LLD_LINK:-}" ] && [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
for tool in "$clang" "$linker"; do
    if [ ! -x "$tool" ] || ! lipo -verify_arch arm64 "$tool"; then
        printf 'Native ARM64 tool required: %s\n' "$tool" >&2
        exit 1
    fi
done
link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-com-guid.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"

cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
cat > combase.def <<'DEF'
LIBRARY combase.dll
EXPORTS
    CoCreateGuid
    StringFromGUID2
    CLSIDFromString
    CoTaskMemAlloc
    CoTaskMemFree
    CoTaskMemRealloc
    CoCreateInstance
    PropVariantClear
DEF
cat > "api-ms-win-core-com-l1-1-0.def" <<'DEF'
LIBRARY api-ms-win-core-com-l1-1-0.dll
EXPORTS
    CoInitializeEx
DEF
cat > ole32.def <<'DEF'
LIBRARY ole32.dll
EXPORTS
    CoUninitialize
DEF
for module in kernel32 combase api-ms-win-core-com-l1-1-0 ole32; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/com-guid.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    /out:com-guid.exe guest.obj kernel32.lib combase.lib \
    api-ms-win-core-com-l1-1-0.lib ole32.lib

if "$check"; then
    cmp com-guid.exe "$here/com-guid.exe"
else
    install -m 0644 com-guid.exe "$here/com-guid.exe"
fi
