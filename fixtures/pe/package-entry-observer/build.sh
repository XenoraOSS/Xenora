#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Compile the original, freestanding observer fixture with native ARM64 LLVM.
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

tmp=$(mktemp -d "${TMPDIR:-/tmp}/xenora-package-entry-observer.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

cat > phase_probe.def <<'DEF'
LIBRARY phase_probe.dll
EXPORTS
    PhaseProbe
DEF
cat > observer_provider.def <<'DEF'
LIBRARY observer_provider.dll
EXPORTS
    ProviderValue
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
for module in phase_probe observer_provider kernel32; do
    link /lib "/def:$module.def" /machine:x64 "/out:$module.lib"
done

compile() {
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror \
        -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize "$@"
}
compile -c "$here/observer-app.c" -o app.obj
compile -c "$here/observer-provider.c" -o provider.obj
compile -DREJECT_ATTACH=1 -c "$here/observer-provider.c" -o reject.obj

mkdir -p "$out/reject"
link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
    /base:0x150000000 "/out:$out/observer-app.exe" app.obj observer_provider.lib phase_probe.lib kernel32.lib
link /dll /entry:DllMain /nodefaultlib /timestamp:0 /machine:x64 \
    /base:0x150100000 "/out:$out/observer_provider.dll" provider.obj phase_probe.lib
link /dll /entry:DllMain /nodefaultlib /timestamp:0 /machine:x64 \
    /base:0x150100000 "/out:$out/reject/observer_provider.dll" reject.obj phase_probe.lib
