#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the original Xenora multi-DLL fixture: a leaf provider DLL, a middle
# provider DLL that imports from the leaf, and a no-CRT consumer EXE. All three
# are compiled by the native ARM64 LLVM toolchain into x86-64 PE32+ images.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
outdir=${1:-"$here"}
case "$outdir" in
    /*) ;;
    *) outdir="$PWD/$outdir" ;;
esac
mkdir -p "$outdir"
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
if [ -z "${CLANG:-}" ] && [ ! -x "$clang" ]; then
    clang=$(xcrun --find clang)
fi
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
temp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-multidll.XXXXXX")
trap 'rm -rf "$temp"' EXIT
cd "$temp"

cat > xenora_math.def <<'DEF'
LIBRARY xenora_math.dll
EXPORTS
    XenoraAdd
    XenoraScale DATA
    XenoraMathTls DATA
    XenoraMathDetach DATA
    XenoraMathInit DATA
DEF
cat > xenora_chain.def <<'DEF'
LIBRARY xenora_chain.dll
EXPORTS
    XenoraQuad
    XenoraChainInit DATA
    XenoraChainDetach DATA
DEF
cat > xenora_chain_no_quad.def <<'DEF'
LIBRARY xenora_chain_no_quad.dll
EXPORTS
    XenoraChainInit DATA
    XenoraChainDetach DATA
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
for module in xenora_math xenora_chain kernel32; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

flags="--target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize"
"$clang" $flags -c "$here/math.c" -o math.obj
"$clang" $flags -c "$here/chain.c" -o chain.obj
"$clang" $flags -DXENORA_NO_QUAD_EXPORT -c "$here/chain.c" -o chain-no-quad.obj
"$clang" $flags -c "$here/consumer.c" -o consumer.obj

link /dll /entry:DllMain /nodefaultlib /dynamicbase /timestamp:0 /machine:x64 \
    "/out:$outdir/xenora_math.dll" math.obj
link /dll /entry:DllMain /nodefaultlib /dynamicbase /timestamp:0 /machine:x64 \
    "/out:$outdir/xenora_chain.dll" chain.obj xenora_math.lib
# Control image: the middle provider without the function the consumer imports.
link /dll /entry:DllMain /nodefaultlib /dynamicbase /timestamp:0 /machine:x64 \
    "/out:$outdir/xenora_chain_no_quad.dll" chain-no-quad.obj xenora_math.lib
link /entry:entry /subsystem:console /nodefaultlib /dynamicbase /timestamp:0 /machine:x64 \
    "/out:$outdir/multidll-consumer.exe" consumer.obj \
    xenora_math.lib xenora_chain.lib kernel32.lib

chmod 644 "$outdir/xenora_math.dll" "$outdir/xenora_chain.dll" \
    "$outdir/xenora_chain_no_quad.dll" "$outdir/multidll-consumer.exe"
