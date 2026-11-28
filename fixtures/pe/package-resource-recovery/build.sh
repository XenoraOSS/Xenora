#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
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

compile() {
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector "$@"
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-package-resource-recovery.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CreateEventA
    WaitForSingleObject
    SetEvent
    CloseHandle
    GetLastError
    ExitProcess
DEF
link /lib /def:kernel32.def /machine:x64 /out:kernel32.lib

compile -c "$here/provider.c" -o provider.obj
link /dll /entry:DllMain /nodefaultlib /timestamp:0 /machine:x64 \
    /base:0x150100000 "/out:$out/resource_provider.dll" /implib:provider.lib provider.obj kernel32.lib

compile -DLEAVE_EVENT_OPEN=0 -c "$here/root.c" -o explicit-close.obj
compile -DLEAVE_EVENT_OPEN=1 -c "$here/root.c" -o open-at-exit.obj
for variant in explicit-close open-at-exit; do
    link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
        /base:0x150000000 "/out:$out/$variant.exe" "$variant.obj" provider.lib kernel32.lib
done
