#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the guest C++ program, tests/compat_fixtures/guest_cpp.cpp, as x86-64
# PE32+ executables linked with the mingw-w64 C++ runtime (UCRT flavour). The
# runtime's own startup code runs before main, which is what makes the program
# "hosted".
#
#   build-guest-cpp.sh [output directory]      default: artifacts/guest-cpp
#
# Tools, native to the host:
#   MINGW_GXX  an x86_64-w64-mingw32-g++. Default: Homebrew mingw-w64 if
#              installed, otherwise the one on PATH.
#   LLD_LINK   an lld-link. Default: Homebrew lld if installed, otherwise the
#              rust-lld that ships in the active Rust toolchain. It makes the
#              import library of the control, from a definition file written
#              below.
#
# Four optimisation levels are built, guest-cpp-O0, guest-cpp-O1, guest-cpp-O2
# and guest-cpp-Os, plus one control, guest-cpp-missing, whose import table
# holds a module no runtime service provides.
#
# Nothing from mingw-w64 is stored in the repository. Each built executable
# contains that project's startup objects and a little of GCC's support
# library, as any program built with it does; the executables are test output
# and are not committed.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
src="$root/tests/compat_fixtures/guest_cpp.cpp"
out=${1:-"$root/artifacts/guest-cpp"}

gxx=${MINGW_GXX:-}
if [ -z "$gxx" ]; then
    gxx=/opt/homebrew/opt/mingw-w64/bin/x86_64-w64-mingw32-g++
    [ -x "$gxx" ] || gxx=$(command -v x86_64-w64-mingw32-g++ || true)
fi
if [ -z "$gxx" ] || [ ! -x "$gxx" ]; then
    echo "missing tool: x86_64-w64-mingw32-g++ (set MINGW_GXX, or install mingw-w64)" >&2
    exit 1
fi
flavor=
linker=${LLD_LINK:-}
if [ -z "$linker" ]; then
    linker=/opt/homebrew/opt/lld/bin/lld-link
    if [ ! -x "$linker" ]; then
        host=$(rustc -vV | sed -n 's/^host: //p')
        linker="$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld"
        flavor="-flavor link"
    fi
fi
if [ -z "$linker" ] || [ ! -x "$linker" ]; then
    echo "missing tool: lld-link (set LLD_LINK, or install lld)" >&2
    exit 1
fi
ar=$("$gxx" -print-prog-name=ar)
case "$ar" in
    /*) [ -x "$ar" ] || { echo "missing tool: $ar" >&2; exit 1; } ;;
    *) ar=$(command -v "$ar") || { echo "missing tool: ar" >&2; exit 1; } ;;
esac

# AGENTS.md: host tools must be ARM64, checked with file, lipo -archs and
# otool -L. The driver is a symbolic link into the toolchain; the compiler
# proper, the assembler and the linker it runs are checked as well, and so is
# the linker that makes the import library.
native=
if [ "$(uname -s)" = Darwin ]; then
    tools="$gxx $linker $ar"
    for helper in cc1plus as ld collect2; do
        path=$("$gxx" -print-prog-name="$helper")
        case "$path" in
            /*) [ -x "$path" ] && tools="$tools $path" ;;
        esac
    done
    for tool in $tools; do
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
            native="$native  $library: $(lipo -archs "$library")
"
        done
        native="$native$tool: $(file -bL "$tool")
"
    done
fi

mkdir -p "$out"
cd "$out"
{
    "$gxx" --version | head -1
    printf 'target: %s\n' "$("$gxx" -dumpmachine)"
    # $flavor is empty or two words on purpose.
    # shellcheck disable=SC2086
    "$linker" $flavor --version | head -1
} > toolchain.txt
# What the native checks above looked at.
printf '%s' "$native" > toolchain-native.txt

# The program is built without exceptions and without RTTI, because the guest
# runtime provides neither. Function-local statics use the Itanium C++ ABI
# guard services now, so the compiler's default thread-safe initialisation is
# left enabled and exercises those services in the hosted C++ path too.
cxxflags="-fno-exceptions -fno-rtti"

# The control's missing service. The program references the symbol without
# calling it, so the four running variants need a local definition of it and
# the control needs an import library that makes it an import.
cat > xenora_missing.def <<'DEF'
LIBRARY xenora_missing.dll
EXPORTS
    MissingService
DEF
cat > missing_stub.cpp <<'CPP'
extern "C" void MissingService(void) {}
CPP
# shellcheck disable=SC2086
"$linker" $flavor /lib /machine:x64 /def:xenora_missing.def /out:xenora_missing.lib
"$gxx" -O1 -fno-exceptions -fno-rtti -c missing_stub.cpp -o missing_stub.o
"$ar" rcs missing_stub.a missing_stub.o

# name, optimization level
build() {
    "$gxx" "-$2" -Wall -Wextra -Werror $cxxflags -c "$src" -o "$1.o"
    "$gxx" "-$2" $cxxflags -o "$1.exe" "$1.o" missing_stub.a
}
for level in O0 O1 O2 Os; do
    build "guest-cpp-$level" "$level"
done
# The control: the same object, linked with the import library of a module no
# runtime service provides, so the loader must refuse the image before the
# entry point runs.
"$gxx" -O1 $cxxflags -o guest-cpp-missing.exe guest-cpp-O1.o xenora_missing.lib

# The exception fixture (#447) is built with exceptions and RTTI, and links
# GCC's unwinder and C++ runtime statically so they run as guest code and call
# the guest runtime's exception services.
for level in O0 O1 O2 Os; do
    "$gxx" "-$level" -Wall -Wextra -Werror -static-libstdc++ -static-libgcc \
        -o "guest-cpp-exceptions-$level.exe" "$root/tests/compat_fixtures/guest_cpp_exceptions.cpp"
done
