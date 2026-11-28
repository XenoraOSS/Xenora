#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: build.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(cd -- "$out" && pwd)
clang=${CLANG:-$(xcrun --find clang)}
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
        rust-lld) arch -arm64 "$linker" -flavor link "$@" ;;
        *) arch -arm64 "$linker" "$@" ;;
    esac
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-com-apartments.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CloseHandle
    CreateEventW
    CreateThread
    ExitProcess
    GetExitCodeThread
    SetEvent
    WaitForSingleObject
DEF
cat > ole32.def <<'DEF'
LIBRARY ole32.dll
EXPORTS
    CoCreateInstance
    CoInitializeEx
    CoUninitialize
DEF
cat > fixture.def <<'DEF'
LIBRARY xenora_com_fixture.dll
EXPORTS
    DllCanUnloadNow
DEF
for library in kernel32 ole32 fixture; do
    link /lib /machine:x64 "/def:$library.def" "/out:$library.lib"
done

arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c17 -O1 -Wall -Wextra -Werror \
    -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize \
    -c "$here/entry.c" -o guest.obj
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    "/out:$out/com-apartments.exe" guest.obj kernel32.lib ole32.lib fixture.lib
