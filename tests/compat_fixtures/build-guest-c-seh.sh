#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the guest C structured-exception program,
# tests/compat_fixtures/guest_c_seh.c (#453, #476), as freestanding x86-64 PE32+
# executables: one per case and optimisation level, named
# guest-c-seh-<case>-<level>.exe.
#
#   build-guest-c-seh.sh [output directory]   default: artifacts/guest-c-seh
#
# Tools, native to the host:
#   CLANG     a clang with the x86_64-pc-windows-msvc target. Default:
#             Homebrew LLVM if installed, otherwise the one on PATH.
#   LLD_LINK  an lld-link. Default: Homebrew lld, then the lld-link beside a
#             Homebrew LLVM, then lld-link on PATH, then the rust-lld that
#             ships in the active Rust toolchain, as build-gate-a1.sh does. A
#             CI runner has only the last of these.
#
# -fasync-exceptions makes clang treat a CPU fault inside __try as catchable;
# without it no handler covers the faulting instruction. The program has no C
# runtime: its imports come from import libraries made below from definition
# files, and nothing from a Windows SDK is used.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
src="$root/tests/compat_fixtures/guest_c_seh.c"
out=${1:-"$root/artifacts/guest-c-seh"}

clang=${CLANG:-}
if [ -z "$clang" ]; then
    clang=/opt/homebrew/opt/llvm/bin/clang
    [ -x "$clang" ] || clang=$(command -v clang || true)
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
for tool in "$clang" "$linker"; do
    if [ -z "$tool" ] || [ ! -x "$tool" ]; then
        echo "missing tool: clang and an lld-link are required (set CLANG and LLD_LINK, or 'brew install llvm lld')" >&2
        exit 1
    fi
done

# AGENTS.md: host tools must be ARM64, checked with file, lipo -archs and
# otool -L.
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
        for library in $(otool -L "$tool" | awk 'NR > 1 { print $1 }'); do
            [ -f "$library" ] || continue
            case "$(lipo -archs "$library")" in
                *arm64*) ;;
                *) echo "ARM64 library required by $tool: $library" >&2; exit 1 ;;
            esac
        done
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

# Import libraries for the guest services. xenora_seh.dll is not a
# real module: the test registers its one service, xenora_note.
cat > kernel32.def <<'DEF'
LIBRARY kernel32.dll
EXPORTS
    ExitProcess
    RaiseException
    DebugBreak
    SetUnhandledExceptionFilter
DEF
cat > ucrtbase.def <<'DEF'
LIBRARY ucrtbase.dll
EXPORTS
    __C_specific_handler
DEF
cat > xenora_seh.def <<'DEF'
LIBRARY xenora_seh.dll
EXPORTS
    xenora_note
DEF
for module in kernel32 ucrtbase xenora_seh; do
    link /lib /machine:x64 "/def:$module.def" "/out:$module.lib" > /dev/null
done

for level in O0 O1 O2 Os; do
    for case in divide search unhandled write_fault read_fault execute_fault \
        illegal finally_across_frames finally_same_frame continue_execution \
        filter_continue filter_execute filter_search filter_context \
        except_context filter_nested debug_break; do
        name="guest-c-seh-$case-$level"
        "$clang" --target=x86_64-pc-windows-msvc "-$level" -fasync-exceptions \
            -ffreestanding -fno-stack-protector -Wall -Wextra -Werror \
            -Wno-unused-function "-DCASE=$case" -c "$src" -o "$name.obj"
        link /entry:entry /subsystem:console /nodefaultlib /machine:x64 \
            "/out:$name.exe" "$name.obj" kernel32.lib ucrtbase.lib xenora_seh.lib \
            > /dev/null
    done
done
