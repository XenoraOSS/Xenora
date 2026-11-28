#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build original AMD64 guest code with native ARM64 LLVM.
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

tmp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-seh-filter-exe.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

cat > runtime.def <<'DEF'
LIBRARY api-ms-win-crt-runtime-l1-1-0.dll
EXPORTS
    _seh_filter_exe
DEF
cat > ucrt.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    __C_specific_handler
    signal
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
    RaiseException
DEF
for module in runtime ucrt kernel32; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

build_guest() {
    name=$1
    shift
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
        -fasync-exceptions -ffreestanding -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
        "$@" -c "$here/guest.c" -o "$name.obj"
    link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
        /base:0x150000000 "/out:$out/$name.exe" "$name.obj" runtime.lib ucrt.lib kernel32.lib
}

build_guest seh-filter-exe
build_guest seh-filter-exe-fpe -DFPE_REFUSAL
