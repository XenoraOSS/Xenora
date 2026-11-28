#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/../../artifacts/core-apiset-trio"}
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

scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-alias-probes.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"

cat > helpers.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
    LocalAlloc
    LocalFree
DEF
link /lib /def:helpers.def /machine:x64 /out:helpers.lib

build_probe() {
    probe=$1
    selection=$2
    symbol=$3
    apiset=$4
    arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c11 -O1 \
        -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-stack-protector \
        -fno-vectorize -fno-slp-vectorize "-D$selection" -c "$here/core_apiset_trio.c" -o probe.obj
    for target in canonical exact; do
        case "$target" in
            canonical) module=kernel32.dll ;;
            exact) module=$apiset ;;
        esac
        printf 'LIBRARY %s\nEXPORTS\n    %s\n' "$module" "$symbol" > candidate.def
        link /lib /def:candidate.def /machine:x64 /out:candidate.lib
        link /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 \
            "/out:$out/$probe-$target.exe" probe.obj candidate.lib helpers.lib
    done
}

build_probe format-message PROBE_FORMAT_MESSAGE FormatMessageA api-ms-win-core-localization-l1-2-0.dll
build_probe local-free PROBE_LOCAL_FREE LocalFree api-ms-win-core-heap-l2-1-0.dll
build_probe stack-limits PROBE_STACK_LIMITS GetCurrentThreadStackLimits api-ms-win-core-processthreads-l1-1-1.dll
