#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build original AMD64 guest fixtures with native ARM64 LLVM.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=$1
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}
if [ ! -x "$linker" ] && [ -z "${LLD_LINK:-}" ]; then
    linker=$(command -v lld-link || true)
fi
[ -n "$linker" ] && [ -x "$linker" ] || { echo "Native lld-link not found" >&2; exit 1; }
for tool in "$clang" "$linker"; do
    lipo "$tool" -verify_arch arm64
done

link() {
    case "${linker##*/}" in
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-registry-settings.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

cat > advapi32.def <<'DEF'
LIBRARY advapi32.dll
EXPORTS
    RegCreateKeyExW
    RegOpenKeyW
    RegOpenKeyExW
    RegOpenKeyExA
    RegSetValueExW
    RegQueryValueExA
    RegQueryValueExW
    RegCloseKey
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
for module in advapi32 kernel32; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

compile_guest() {
    output=$1
    shift
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
        "$@" -c "$here/settings.c" -o settings.obj
    link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
        /base:0x150000000 "/out:$out/$output.exe" settings.obj advapi32.lib kernel32.lib
}

compile_guest guest-registry-settings
compile_guest guest-registry-settings-cold -DCOLD_CONTROL=1
