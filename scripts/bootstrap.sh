#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

if [ "$(uname -m)" != arm64 ] || [ "$(arch)" != arm64 ] ||
   [ "$(sysctl -n sysctl.proc_translated)" != 0 ]; then
    printf '%s\n' 'Run on native Apple Silicon macOS.' >&2
    exit 1
fi

rustc -vV
cargo --version
xcodebuild -version
xcrun --find metal
xcrun metal --version

llvm_bin=${LLVM_BIN:-}
llvm_required=false
if [ -n "$llvm_bin" ]; then
    llvm_required=true
fi
if [ -z "$llvm_bin" ] && command -v brew >/dev/null 2>&1 &&
   llvm_prefix=$(brew --prefix llvm 2>/dev/null); then
    llvm_bin="$llvm_prefix/bin"
fi

llvm_version() {
    if [ ! -f "$1" ] || [ ! -x "$1" ]; then
        printf 'bootstrap: LLVM tool is missing or not executable: %s\n' "$1" >&2
        return 1
    fi
    if ! lipo "$1" -verify_arch arm64 >/dev/null 2>&1; then
        printf 'bootstrap: LLVM tool must contain a native arm64 slice: %s\n' "$1" >&2
        return 1
    fi
    if version=$(arch -arm64 "$1" --version); then
        printf '%s\n' "$version" | sed -n '1p'
    else
        status=$?
        printf 'bootstrap: LLVM version probe failed (status %s): %s\n' "$status" "$1" >&2
        return "$status"
    fi
}

for tool in llvm-mc llvm-objcopy llvm-objdump; do
    if [ -z "$llvm_bin" ]; then
        printf '%s\n' "Optional fixture tooling: set LLVM_BIN to a native LLVM bin directory containing $tool."
        continue
    fi
    case "$llvm_bin" in
        /*) tool_path="$llvm_bin/$tool" ;;
        *) tool_path="./$llvm_bin/$tool" ;;
    esac
    if llvm_version "$tool_path"; then
        continue
    else
        status=$?
        if "$llvm_required"; then
            exit "$status"
        fi
    fi
done
