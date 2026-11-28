#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original Microsoft x64 C++ exception and normal-return witnesses.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang++)}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ -z "${LLD_LINK:-}" ] && [ ! -x "$linker" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi

test "$(uname -m)" = arm64
for tool in "$clang" "$linker"; do
    file -L "$tool"
    lipo -archs "$tool"
    lipo "$tool" -verify_arch arm64
    otool -L "$tool"
done

link() {
    case "${linker##*/}" in
        rust-lld) "$linker" -flavor link "$@" ;;
        *) "$linker" "$@" ;;
    esac
}

compile() {
    "$clang" --target=x86_64-pc-windows-msvc -std=c++20 -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-rtti \
        -fexceptions -fcxx-exceptions -c "$here/witness.cpp" "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-exceptions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
cat > vcruntime140.def <<'DEF'
LIBRARY vcruntime140.dll
EXPORTS
    _CxxThrowException
    __CxxFrameHandler3
DEF
for library in kernel32 vcruntime140; do
    link /lib /machine:x64 "/def:$library.def" "/out:$library.lib"
done

# The typed throw/catch produces a direct type_info vftable relocation. Keep
# the compiler object intact without supplying Microsoft runtime data.
compile -DTHROW_WITNESS -o "$out/typed-throw.obj"
compile -DRETHROW_WITNESS -o rethrow.obj
compile -o normal.obj

link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/rethrow.exe" rethrow.obj kernel32.lib vcruntime140.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/normal.exe" normal.obj kernel32.lib
