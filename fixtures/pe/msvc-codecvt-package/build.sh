#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the original EXEs and two independent DLLs with native ARM64 LLVM.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang++)}
linker=${LLD_LINK:-$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld}

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
    # The selected static data members are defined by the imported satellite.
    "$clang" --target=x86_64-pc-windows-msvc -std=c++20 -O1 -Wall -Wextra -Werror -Wno-undefined-var-template \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-exceptions -fno-rtti \
        -fno-vectorize -fno-slp-vectorize "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-codecvt-package.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > codecvt.def <<'DEF'
LIBRARY msvcp140_codecvt_ids.dll
EXPORTS
    ?id@?$codecvt@_SDU_Mbstatet@@@std@@2V0locale@2@A DATA
    ?id@?$codecvt@_UDU_Mbstatet@@@std@@2V0locale@2@A DATA
    ?id@?$codecvt@_S_QU_Mbstatet@@@std@@2V0locale@2@A DATA
    ?id@?$codecvt@_U_QU_Mbstatet@@@std@@2V0locale@2@A DATA
    ?id@?$codecvt@_WDU_Mbstatet@@@std@@2V0locale@2@A DATA
DEF
link /lib /machine:x64 /def:codecvt.def /out:codecvt.lib

for module in producer consumer; do
    compile -c "$here/$module.cpp" -o "$module.obj"
    link /dll /noentry /nodefaultlib /timestamp:0 /machine:x64 \
        "/out:$out/xenora_codecvt_$module.dll" "/implib:$module.lib" "$module.obj" codecvt.lib
done
compile -c "$here/entry.cpp" -o entry.obj
compile -DUNSUPPORTED_SPECIALIZATION -c "$here/entry.cpp" -o control.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/codecvt-package.exe" entry.obj producer.lib consumer.lib codecvt.lib
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/codecvt-package-control.exe" control.obj producer.lib consumer.lib codecvt.lib
