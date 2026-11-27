#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the original Xenora dynamic-load fixture: a no-CRT DLL with an
# exported function, TLS marker and DllMain evidence, plus a no-CRT EXE that
# calls LoadLibraryA/GetProcAddress/FreeLibrary through kernel32.dll. Both are
# compiled by the native ARM64 LLVM toolchain into x86-64 PE32+ images.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
outdir=${1:-"$here"}
case "$outdir" in
    /*) ;;
    *) outdir="$PWD/$outdir" ;;
esac
mkdir -p "$outdir"
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
for tool in "$clang" "$linker"; do
    case "$(file -bL "$tool")" in
        *arm64*) ;;
        *) echo "Expected a native ARM64 tool: $tool" >&2; exit 1 ;;
    esac
    if ! lipo "$tool" -verify_arch arm64; then
        echo "Expected a native ARM64 tool: $tool" >&2; exit 1
    fi
done
link() {
    case "${linker##*/}" in
        rust-lld) "$linker" -flavor link "$@" ;;
        *) "$linker" "$@" ;;
    esac
}
temp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-loadlib.XXXXXX")
trap 'rm -rf "$temp"' EXIT
cd "$temp"

cat > xenoralib.def <<'DEF'
LIBRARY xenoralib.dll
EXPORTS
    XenoraTriple
    XenoraLibInit DATA
    XenoraLibTls DATA
    XenoraLibDetach DATA
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    LoadLibraryA
    GetProcAddress
    FreeLibrary
    ExitProcess
DEF
for module in kernel32; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

flags="--target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize"
"$clang" $flags -c "$here/lib.c" -o lib.obj
"$clang" $flags -c "$here/app.c" -o app.obj

link /dll /entry:DllMain /nodefaultlib /dynamicbase /timestamp:0 /machine:x64 \
    "/out:$outdir/xenoralib.dll" lib.obj
link /entry:entry /subsystem:console /nodefaultlib /dynamicbase /timestamp:0 /machine:x64 \
    "/out:$outdir/loadlib.exe" app.obj kernel32.lib

chmod 644 "$outdir/xenoralib.dll" "$outdir/loadlib.exe"
