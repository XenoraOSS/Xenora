#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Requires local Microsoft STL, VC runtime and Windows SDK headers. It does not
# download, install or redistribute those materials.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang++}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
: "${MS_STL_INCLUDE:?set MS_STL_INCLUDE to the Microsoft STL stl/inc directory}"
: "${MSVC_INCLUDE:?set MSVC_INCLUDE to the Microsoft VC runtime include directory}"
: "${WINDOWS_SDK_INCLUDE:?set WINDOWS_SDK_INCLUDE to the Windows SDK versioned Include directory}"
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] && lipo -verify_arch arm64 "$tool" || { echo "Native ARM64 tool required: $tool" >&2; exit 1; }
done
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-msvc-iostream.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY msvcp140.dll\nEXPORTS\n ?_Init_ctor@Init@ios_base@std@@CAXPEAV123@@Z\n ??1Init@ios_base@std@@QEAA@XZ\n ?cout@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A DATA\n ?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@XZ\n ?good@ios_base@std@@QEBA_NXZ\n ?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@PEBD_J@Z\n' > msvcp.def
printf 'LIBRARY kernel32.dll\nEXPORTS\n ExitProcess\n' > kernel.def
for lib in msvcp kernel; do
    arch -arm64 "$linker" /lib /def:"$lib.def" /machine:x64 /out:"$lib.lib"
done
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -std=c++20 -fms-compatibility-version=19.44 -O2 -Wall -Wextra -Werror -fno-exceptions -fno-rtti -fno-stack-protector -D_DLL -D_MT -D_HAS_EXCEPTIONS=0 -nostdinc++ -isystem "$MS_STL_INCLUDE" -isystem "$MSVC_INCLUDE" -isystem "$WINDOWS_SDK_INCLUDE/ucrt" -isystem "$WINDOWS_SDK_INCLUDE/shared" -c "$here/msvc-iostream.cpp" -o guest.obj
arch -arm64 "$linker" /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:guest.exe guest.obj msvcp.lib kernel.lib
mkdir -p "$root/artifacts/msvc-iostream"
install -m 0644 guest.exe "$root/artifacts/msvc-iostream/msvc-iostream.exe"
printf 'Built %s\n' "$root/artifacts/msvc-iostream/msvc-iostream.exe"
