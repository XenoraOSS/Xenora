#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build tests/compat_fixtures/cxx_guard.cpp as x86-64 PE32+ executables with
# the Itanium C++ ABI's thread-safe function-local static guard calls and no C
# or C++ runtime linked.
#
#   build-cxx-guard.sh [output directory]   default: artifacts/guest-cxx-guard
#
# Tools, all native to the host:
#   CXX       a clang++ that accepts --target=x86_64-pc-windows-gnu. Default:
#             Homebrew LLVM if installed, otherwise the clang++ on PATH.
#   LLD_LINK  an lld-link. Default: Homebrew lld if installed, otherwise the
#             rust-lld that ships in the active Rust toolchain.
#
# The linker makes both import libraries from definition files written below,
# so no Windows SDK or C runtime is used. Nothing from a guest runtime is
# stored in the repository.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
src="$root/tests/compat_fixtures/cxx_guard.cpp"
out=${1:-"$root/artifacts/guest-cxx-guard"}

clang=${CXX:-}
if [ -z "$clang" ]; then
    clang=/opt/homebrew/opt/llvm/bin/clang++
    [ -x "$clang" ] || clang=$(command -v clang++)
fi
linker=${LLD_LINK:-}
if [ -z "$linker" ]; then
    linker=/opt/homebrew/opt/lld/bin/lld-link
    if [ ! -x "$linker" ]; then
        host=$(rustc -vV | sed -n 's/^host: //p')
        linker="$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld"
    fi
fi
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] || { echo "missing tool: $tool" >&2; exit 1; }
done
# AGENTS.md: host tools and the libraries they load must be ARM64. Check the
# file type, slice list and linked libraries rather than assuming the default
# Homebrew prefix is native.
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
# rust-lld needs -flavor link; the Homebrew lld-link does not.
flavor=
if [ "$(basename "$linker")" = rust-lld ]; then
    flavor="-flavor link"
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
printf '%s' "$native" > toolchain-native.txt

cat > libstdc++.def <<'DEF'
LIBRARY libstdc++-6.dll
EXPORTS
    __cxa_guard_acquire
    __cxa_guard_release
    __cxa_guard_abort
DEF
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
DEF
link /lib /machine:x64 /def:libstdc++.def /out:libstdc++.lib
link /lib /machine:x64 /def:kernel32.def /out:kernel32.lib

build() {
    # shellcheck disable=SC2086
    "$clang" --target=x86_64-pc-windows-gnu -fms-extensions "-$2" -std=c++17 \
        -ffreestanding -fno-exceptions -fno-rtti -fno-stack-protector \
        -fno-asynchronous-unwind-tables -Wall -Wextra -Werror \
        ${3:+"-D$3"} -c "$src" -o "$1.obj"
    link /entry:guest_cxx_guard_entry /subsystem:console /nodefaultlib \
        /timestamp:0 "/out:$1.exe" "$1.obj" libstdc++.lib kernel32.lib
}

for level in O0 O1 O2 Os; do
    build "guest-cxx-guard-$level" "$level" ""
done
build guest-cxx-guard-reentrant O1 REENTRANT_CONTROL
build guest-cxx-guard-invalid-release O1 INVALID_RELEASE_CONTROL
build guest-cxx-guard-invalid-abort O1 INVALID_ABORT_CONTROL
