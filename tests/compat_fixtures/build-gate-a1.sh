#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the Gate A1 program, tests/compat_fixtures/gate_a1.c, as x86-64 PE32+
# executables with no C library linked.
#
#   build-gate-a1.sh [output directory]      default: artifacts/gate-a1
#
# Tools, all native to the host:
#   CLANG     a clang that accepts --target=x86_64-pc-windows-msvc. Default:
#             Homebrew LLVM if installed, otherwise the clang on PATH.
#   LLD_LINK  an lld-link. Default: Homebrew lld, then the lld-link beside a
#             Homebrew LLVM, then lld-link on PATH, then the rust-lld that
#             ships in the active Rust toolchain. A Homebrew Rust installation
#             does not include rust-lld, so install one of the others.
#
# The linker also makes the two import libraries, from the definition files
# written below, so nothing from a Windows SDK or another C runtime is used.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
src="$root/tests/compat_fixtures/gate_a1.c"
out=${1:-"$root/artifacts/gate-a1"}

clang=${CLANG:-}
if [ -z "$clang" ]; then
    clang=/opt/homebrew/opt/llvm/bin/clang
    [ -x "$clang" ] || clang=$(command -v clang)
fi
flavor=
linker=${LLD_LINK:-}
if [ -z "$linker" ]; then
    prefixes="/opt/homebrew /usr/local"
    if command -v brew >/dev/null 2>&1; then
        prefixes="$(brew --prefix 2>/dev/null || true) $prefixes"
    fi
    for prefix in $prefixes; do
        for candidate in "$prefix/opt/lld/bin/lld-link" "$prefix/opt/llvm/bin/lld-link"; do
            if [ -z "$linker" ] && [ -x "$candidate" ]; then
                linker=$candidate
            fi
        done
    done
fi
if [ -z "$linker" ]; then
    linker=$(command -v lld-link 2>/dev/null || true)
fi
if [ -z "$linker" ] && command -v rustc >/dev/null 2>&1; then
    host=$(rustc -vV | sed -n 's/^host: //p')
    candidate="$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld"
    if [ -x "$candidate" ]; then
        linker=$candidate
        flavor="-flavor link"
    fi
fi
if [ -z "$linker" ]; then
    echo "missing linker: install one with 'brew install llvm lld', or set LLD_LINK to an lld-link" >&2
    exit 1
fi
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] || {
        echo "missing tool: $tool" >&2
        echo "install the Gate A1 toolchain with 'brew install llvm lld', or set CLANG and LLD_LINK" >&2
        exit 1
    }
done
# AGENTS.md: host tools and what they load must be ARM64, checked with file,
# lipo -archs and otool -L. An @rpath library is looked up through the tool's
# own run paths. A library that is still not a file on disk is in the system's
# shared cache, and is listed only.
native=
if [ "$(uname -s)" = Darwin ]; then
    for tool in "$clang" "$linker"; do
        case "$(file -bL "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required (file): $tool" >&2; exit 1 ;;
        esac
        case "$(lipo -archs "$tool")" in
            *arm64*) ;;
            *) echo "ARM64 tool required (lipo): $tool" >&2; exit 1 ;;
        esac
        here=$(dirname "$tool")
        runpaths=$(otool -l "$tool" | awk '/LC_RPATH/ { getline; getline; print $2 }' |
            sed "s|@loader_path|$here|; s|@executable_path|$here|")
        for library in $(otool -L "$tool" | awk 'NR > 1 { print $1 }'); do
            case "$library" in
                @rpath/*)
                    for runpath in $runpaths; do
                        if [ -f "$runpath/${library#@rpath/}" ]; then
                            library="$runpath/${library#@rpath/}"
                            break
                        fi
                    done ;;
            esac
            [ -f "$library" ] || continue
            case "$(lipo -archs "$library")" in
                *arm64*) ;;
                *) echo "ARM64 library required by $tool: $library" >&2; exit 1 ;;
            esac
            native="$native  $library: $(lipo -archs "$library")
"
        done
        native="$native$(file -bL "$tool")
$(otool -L "$tool")
"
    done
fi
# $flavor is empty or two words on purpose.
# shellcheck disable=SC2086
link() { "$linker" $flavor "$@"; }

mkdir -p "$out"
cd "$out"
{
    "$clang" --version | head -1
    link --version | head -1
} > toolchain.txt
# What the native checks above looked at.
printf '%s' "$native" > toolchain-native.txt

cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    CloseHandle
    CreateFileW
    ExitProcess
    GateMissingService
    GetFileSizeEx
    GetLastError
    ReadFile
    SetFilePointerEx
    WriteFile
DEF
cat > ucrtbase.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    _errno
    free
    malloc
    memcmp
    memcpy
    memset
    qsort
    realloc
    strcmp
    strlen
    strncmp
    strtol
DEF
link /lib /machine:x64 /def:kernel32.def /out:kernel32.lib
link /lib /machine:x64 /def:ucrtbase.def /out:ucrtbase.lib

# name, optimization level, extra definition
build() {
    "$clang" --target=x86_64-pc-windows-msvc "-$2" -ffreestanding -fno-stack-protector \
        -Wall -Wextra -Werror ${3:+"-D$3"} -c "$src" -o "$1.obj"
    link /entry:gate_entry /subsystem:console /nodefaultlib /timestamp:0 \
        "/out:$1.exe" "$1.obj" kernel32.lib ucrtbase.lib
}
for level in O0 O1 O2 Os; do
    build "gate-a1-$level" "$level" ""
done
# Controls: a wrong expectation, a returning entry point, an import nobody provides.
build gate-a1-break O1 GATE_BREAK
build gate-a1-return O1 GATE_RETURN
build gate-a1-missing O1 GATE_MISSING

# The CLI checks that a TLS exit is reported before any entry event.
"$clang" --target=x86_64-pc-windows-msvc -c \
    "$root/tests/compat_fixtures/gate_a1_tls_exit.s" -o gate-a1-tls-exit.obj
link /entry:gate_entry /subsystem:console /nodefaultlib /timestamp:0 \
    /base:0x150000000 /fixed /out:gate-a1-tls-exit.exe gate-a1-tls-exit.obj kernel32.lib
