#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/../../artifacts/module-fixed-package"}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)

clang=${CLANG:-$(xcrun --find clang)}
linker=${LLD_LINK:-"$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"}
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
        -ffreestanding -fno-builtin -fno-stack-protector \
        -c "$here/module_fixed_package.c" "$@"
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-fixed-package.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
link /lib /def:kernel32.def /machine:x64 /out:kernel32.lib
compile -DFIXED_PROVIDER -o provider.obj
compile -o consumer.obj
link /dll /entry:DllMain /nodefaultlib /fixed /dynamicbase:no /base:0x150000000 \
    /timestamp:0 /machine:x64 "/out:$out/fixed-provider.dll" /implib:fixed-provider.lib provider.obj
link /entry:entry /subsystem:console /nodefaultlib /fixed:no /dynamicbase /base:0x150000000 \
    /timestamp:0 /machine:x64 "/out:$out/movable-root.exe" consumer.obj fixed-provider.lib kernel32.lib
