#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Compile the freestanding guest with native LLVM and link against import stubs.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/../../artifacts/msvc-clog"}
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang++}
if [ ! -x "$clang" ] && [ -z "${CLANG:-}" ]; then
    clang=$(command -v clang++)
fi
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
if [ ! -x "$linker" ] && [ -z "${LLD_LINK:-}" ]; then
    linker="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
fi
mkdir -p "$out"
cd "$out"

audit_native() {
    tool=$1
    file -L "$tool"
    archs=$(lipo -archs "$tool")
    printf '%s\n' "$archs"
    case " $archs " in
        *" arm64 "*) ;;
        *) echo "ARM64 tool required: $tool" >&2; exit 1 ;;
    esac
    libraries=$(otool -L "$tool")
    printf '%s\n' "$libraries"
    tool_dir=$(dirname "$tool")
    runpaths=$(otool -l "$tool" | awk '/LC_RPATH/ { getline; getline; print $2 }' |
        sed "s|@loader_path|$tool_dir|; s|@executable_path|$tool_dir|")
    for library in $(printf '%s\n' "$libraries" | awk 'NR > 1 { print $1 }'); do
        case "$library" in
            @rpath/*)
                for runpath in $runpaths; do
                    resolved="$runpath/${library#@rpath/}"
                    if [ -f "$resolved" ]; then
                        library=$resolved
                        break
                    fi
                done ;;
            @loader_path/*) library="$tool_dir/${library#@loader_path/}" ;;
            @executable_path/*) library="$tool_dir/${library#@executable_path/}" ;;
        esac
        if [ -f "$library" ]; then
            lipo "$library" -verify_arch arm64
        fi
    done
}
if [ "$(uname -s)" = Darwin ]; then
    { audit_native "$clang"; audit_native "$linker"; } > toolchain-native.txt
fi

link() {
    case "${linker##*/}" in
        rust-lld) "$linker" -flavor link "$@" ;;
        *) "$linker" "$@" ;;
    esac
}
cat > msvcp.def <<'DEF'
LIBRARY msvcp140.dll
EXPORTS
    ?cout@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A DATA
    ?clog@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A DATA
    ??0Init@ios_base@std@@QEAA@XZ
    ??1Init@ios_base@std@@QEAA@XZ
    ?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@PEBD_J@Z
    ?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@XZ
    ?good@ios_base@std@@QEBA_NXZ
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
    GetProcessHeap
    HeapFree
DEF
cat > ucrtbase.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    _close
DEF
for library in msvcp kernel32 ucrtbase; do
    link /lib /machine:x64 "/def:$library.def" "/out:$library.lib"
done
build() {
    name=$1
    shift
    "$clang" --target=x86_64-pc-windows-msvc -std=c++20 -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector \
        "$@" -c "$here/msvc-clog.cpp" -o "$name.obj"
    link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
        "/out:$name.exe" "$name.obj" msvcp.lib kernel32.lib ucrtbase.lib
}
build msvc-clog
build msvc-clog-oversize -DOVERSIZED_WRITE
build msvc-clog-final-flush -DUNMAPPED_ON_FINAL_FLUSH
