#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build a PE32+ guest with an in-image C++ unwinder and Xenora-bound guards.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/../../artifacts/guest-cxx-guard-constructor-retry"}
gxx=${MINGW_GXX:-$(command -v x86_64-w64-mingw32-g++)}
dlltool=${MINGW_DLLTOOL:-$(command -v x86_64-w64-mingw32-dlltool)}
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
    directory=$(dirname "$tool")
    runpaths=$(otool -l "$tool" | awk '/LC_RPATH/ { getline; getline; print $2 }' |
        sed "s|@loader_path|$directory|; s|@executable_path|$directory|")
    for library in $(printf '%s\n' "$libraries" | awk 'NR > 1 { print $1 }'); do
        case "$library" in
            @rpath/*)
                for runpath in $runpaths; do
                    if [ -f "$runpath/${library#@rpath/}" ]; then
                        library="$runpath/${library#@rpath/}"
                        break
                    fi
                done ;;
            @loader_path/*) library="$directory/${library#@loader_path/}" ;;
            @executable_path/*) library="$directory/${library#@executable_path/}" ;;
        esac
        [ -f "$library" ] || continue
        lipo "$library" -verify_arch arm64
    done
}

if [ "$(uname -s)" = Darwin ]; then
    {
        audit_native "$gxx"
        audit_native "$dlltool"
        for helper in cc1plus as ld collect2; do
            path=$("$gxx" -print-prog-name="$helper")
            audit_native "$(command -v "$path")"
        done
    } > toolchain-native.txt
fi
"$gxx" --version > toolchain.txt

cat > guards.def <<'DEF'
LIBRARY libstdc++-6.dll
EXPORTS
    __cxa_guard_acquire
    __cxa_guard_release
    __cxa_guard_abort
DEF
"$dlltool" --machine i386:x86-64 --def guards.def --output-lib guards.a

# Resolve generated guard calls through the import archive. Libstdc++ and
# libgcc then provide the exception personality as code inside the guest.
for level in O0 O2; do
    "$gxx" "-$level" -Wall -Wextra -Werror -static-libstdc++ -static-libgcc \
        "$here/cxx_guard_constructor_retry.cpp" guards.a \
        "-Wl,-Map,guest-cxx-guard-constructor-retry-$level.map" \
        -o "guest-cxx-guard-constructor-retry-$level.exe"
done
