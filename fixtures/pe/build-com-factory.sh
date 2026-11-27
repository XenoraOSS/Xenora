#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
clang=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
linker=${LLD_LINK:-/opt/homebrew/opt/lld/bin/lld-link}
for tool in "$clang" "$linker"; do
    [ -x "$tool" ] && lipo -verify_arch arm64 "$tool" || { echo "Native ARM64 tool required: $tool" >&2; exit 1; }
done
scratch=$(mktemp -d "${TMPDIR:-/tmp}/xenora-com-factory.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
printf 'LIBRARY kernel32.dll\nEXPORTS\n    ExitProcess\n' > kernel32.def
printf 'LIBRARY ole32.dll\nEXPORTS\n    CoInitializeEx\n    CoUninitialize\n    CoGetClassObject\n    CoCreateInstance\n    CoCreateInstanceEx\n' > ole32.def
for module in kernel32 ole32; do arch -arm64 "$linker" /lib "/def:$module.def" /machine:x64 "/out:$module.lib"; done
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize -c "$here/com-factory.c" -o guest.obj
arch -arm64 "$linker" /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:com-factory.exe guest.obj kernel32.lib ole32.lib
arch -arm64 "$clang" --target=x86_64-pc-windows-msvc -O1 -Wall -Wextra -Werror -ffreestanding -fno-builtin -fno-stack-protector -fno-vectorize -fno-slp-vectorize -DTEST_STALE -c "$here/com-factory.c" -o stale.obj
arch -arm64 "$linker" /entry:entry /subsystem:console /nodefaultlib /timestamp:0 /machine:x64 /out:com-factory-stale.exe stale.obj kernel32.lib ole32.lib
if [ "${1:-}" = --check ]; then
    cmp com-factory.exe "$here/com-factory.exe"
    cmp com-factory-stale.exe "$here/com-factory-stale.exe"
else
    install -m 0644 com-factory.exe "$here/com-factory.exe"
    install -m 0644 com-factory-stale.exe "$here/com-factory-stale.exe"
fi
