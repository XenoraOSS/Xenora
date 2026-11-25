#!/bin/sh
# Build the crt_stdin compatibility fixture with mingw-w64 GCC: the normal
# program at two optimisation levels, and the control variant that must be
# caught. The output directory is the script's first argument.
set -eu
cd "$(dirname "$0")/../.."
out="$1"
mkdir -p "$out"
for level in 0 2; do
    x86_64-w64-mingw32-gcc -O$level -Wall -Wextra -Werror -fno-builtin \
        -o "$out/crt-stdin-O$level.exe" tests/compat_fixtures/crt_stdin.c
done
x86_64-w64-mingw32-gcc -O1 -DCRT_STDIN_BREAK -Wall -Wextra -Werror -fno-builtin \
    -o "$out/crt-stdin-break.exe" tests/compat_fixtures/crt_stdin.c
echo "built crt-stdin fixtures in $out"
