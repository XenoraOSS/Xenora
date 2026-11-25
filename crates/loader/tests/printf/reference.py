#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Reference outputs for the printf formatting engine, from the host C library.

  generate   write vectors.txt beside this script
  --check    regenerate in memory; exit 1 if the committed file differs

The cases are generated here, compiled into a small C program with the host
`clang`, and formatted by the host C library's `snprintf`. That library is an
implementation of ISO C 7.21.6.1 that shares no code with Xenora, so the file
is an independent answer for every case whose result ISO C defines.

Left out on purpose, because the host and Windows differ or ISO C leaves the
result undefined; `crt_format.rs` tests these by hand from Microsoft's page:

  - the `l` size (a long is 64 bits on the host and 32 on Windows), `I`, `I32`
    and `I64`, which the host does not know
  - `%p`, and a null pointer for `%s`
  - `#` with `d`, `i`, `u`, `c` or `s`, `0` with `c` or `s`, and a precision
    with `c`, which ISO C leaves undefined
  - wide characters and `%n`, which the engine refuses
  - for floating point: infinities and NaNs (Windows writes them differently),
    `%a` with fewer than 13 digits (Windows uses 13 where the host uses as
    few as it can, and the host does not round the dropped digits correctly:
    it looks at two of them), `%a` of a subnormal number (the host normalizes
    it), and `L`

Each line of vectors.txt is three tab-separated fields:

  format    the format string, in hexadecimal
  arguments comma-separated, in order; `i:<16 hex digits>` is an 8-byte
            argument slot as the Win64 calling convention passes it (for an
            int the upper half is junk that must be ignored); `s:<hex>` is a
            string argument's bytes; `f:<16 hex digits>` is the slot of a
            `double`, its IEEE 754 bits
  output    what snprintf wrote, in hexadecimal

Standard library only (Python 3.9+). Needs `clang` on PATH.
"""

import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
VECTORS = HERE / "vectors.txt"
MASK64 = (1 << 64) - 1


class Random:
    """xorshift64*, so the cases do not depend on the Python version."""

    def __init__(self, seed):
        self.state = seed

    def next(self):
        s = self.state
        s ^= s >> 12
        s ^= (s << 25) & MASK64
        s ^= s >> 27
        self.state = s
        return (s * 0x2545F4914F6CDD1D) & MASK64

    def below(self, bound):
        return self.next() % bound

    def pick(self, items):
        return items[self.below(len(items))]


EDGES = [0, 1, 2, 7, 8, 9, 10, 15, 16, 99, 100, 127, 128, 255, 256, 4095, 32767, 32768, 65535,
         65536, 2**31 - 1, 2**31, 2**32 - 1, 2**32, 2**63 - 1, 2**63, 2**64 - 1]

# size prefix -> (bits, C type for a signed conversion, C type for an unsigned one)
SIZES = {
    "": (32, "int", "unsigned"),
    "hh": (8, "int", "unsigned"),
    "h": (16, "int", "unsigned"),
    "ll": (64, "long long", "unsigned long long"),
    "j": (64, "intmax_t", "uintmax_t"),
    "z": (64, "ssize_t", "size_t"),
    "t": (64, "ptrdiff_t", "size_t"),
}
STRINGS = [b"", b"a", b"ab", b"hello", b"hello, world", b"  padded  ", b"100%", b"tab\there",
           bytes([0xC3, 0xA9, 0x7E, 0x80, 0xFF]), b"x" * 40]


def value(random):
    kind = random.below(4)
    if kind == 0:
        return random.pick(EDGES)
    if kind == 1:
        return (-random.pick(EDGES)) & MASK64
    if kind == 2:
        return random.below(1000)
    return random.next()


def junk(random):
    """Upper half of a slot that holds a 32-bit argument."""
    return random.pick([0, 0xFFFFFFFF, 0xDEADBEEF, random.next() >> 32]) << 32


def int_argument(random, number):
    """An `int` argument: its C text and its slot."""
    low = number & 0xFFFFFFFF
    signed = low - (1 << 32) if low >> 31 else low
    return f"(int)({signed})", f"i:{junk(random) | low:016x}"


def spec(random, kinds):
    """One conversion specification: (text, [(C argument, slot text)])."""
    kind = random.pick(kinds)
    arguments = []
    # + and blank ask for a sign on a signed conversion. On an unsigned one they
    # are defined and have no effect, which is worth checking.
    allowed = {"d": "-+ 0", "i": "-+ 0", "u": "-+ 0", "o": "-+ #0", "x": "-+ #0", "X": "-+ #0",
               "c": "-", "s": "-"}[kind]
    flags = "".join(flag for flag in allowed if random.below(3) == 0)
    if flags and random.below(2):
        flags = flags[::-1]
    text = "%" + flags

    choice = random.below(6)
    if choice in (1, 2):
        text += str(random.pick([1, 2, 3, 5, 8, 12, 20, 33]))
    elif choice == 3:
        text += "*"
        width = random.pick([0, 1, 4, 9, 17, -1, -6, -15])
        arguments.append(int_argument(random, width))

    if kind != "c":
        choice = random.below(7)
        if choice == 1:
            text += "."
        elif choice in (2, 3):
            text += "." + str(random.pick([0, 1, 2, 4, 7, 11, 25]))
        elif choice == 4:
            text += ".*"
            precision = random.pick([0, 1, 3, 6, 14, -1, -5])
            arguments.append(int_argument(random, precision))

    if kind == "c":
        byte = random.pick([0x41, 0x7A, 0x20, 0x25, 0x01, 0x7F, 0x80, 0xFF, 0x00, random.below(256)])
        # The argument is an int; only its low byte is the character.
        number = byte | (random.below(3) << 8)
        arguments.append(int_argument(random, number))
    elif kind == "s":
        string = random.pick(STRINGS)
        literal = "".join(f"\\{byte:03o}" for byte in string)
        arguments.append((f'"{literal}"', "s:" + string.hex()))
    else:
        size = random.pick(list(SIZES))
        text += size
        bits, signed_type, unsigned_type = SIZES[size]
        number = value(random)
        if bits == 64:
            if kind in "di":
                signed = number - (1 << 64) if number >> 63 else number
                # LLONG_MIN has no literal of its own.
                literal = "(-9223372036854775807LL - 1)" if signed == -(1 << 63) else f"{signed}LL"
                arguments.append((f"({signed_type})({literal})", f"i:{number:016x}"))
            else:
                arguments.append((f"({unsigned_type})({number}ULL)", f"i:{number:016x}"))
        else:
            # hh and h arguments are passed as an int that need not be in
            # range: the library converts it before printing.
            arguments.append(int_argument(random, number))
    return text + kind, arguments


FLOATS = [0.0, -0.0, 1.0, -1.0, 0.5, 1.5, 2.5, 3.5, 0.125, 0.375, 0.1, 0.2, 0.3, 123.456, -123.456,
          1e-5, 1e-4, 9.9995e-5, 0.00012345, 9.9995, 9.99995, 99999.95, 999999.5, 1e5, 1e6, 1e7,
          123456789.0, 1e15, 1e16, 1e17, 1e21, 1e22, 1e100, 1e-100, 0.1 + 0.2, 1 / 3, 2 / 3,
          2.0**-1074, 2.0**-1022, 2.2250738585072014e-308, 1.7976931348623157e308, 4.35, 8.345,
          0.05, 0.15, 0.25, 0.35, 0.45, 1e23, 5e-324, 2.5e-5, 7.0, 10.0, 100.0, 1000.0, 0.999999,
          0.9999995, 99.5, 999.5, 0.0001, 0.00001]


def float_bits(random):
    """The bits of a finite double: a chosen value or a random one."""
    import struct
    kind = random.below(4)
    if kind < 2:
        number = random.pick(FLOATS)
        if kind == 1:
            number = -number
        return struct.unpack("<Q", struct.pack("<d", number))[0]
    while True:
        bits = random.next()
        if kind == 3:
            # Keep the exponent near 1 so that the f style stays short.
            bits = (bits & ~(0x7FF << 52)) | ((1023 - 40 + random.below(80)) << 52)
        if (bits >> 52) & 0x7FF != 0x7FF:
            return bits


def float_spec(random):
    """One floating-point specification: (text, [(C argument, slot text)])."""
    kind = random.pick("fFeEgGaA")
    arguments = []
    flags = "".join(flag for flag in "-+ #0" if random.below(3) == 0)
    if flags and random.below(2):
        flags = flags[::-1]
    text = "%" + flags
    choice = random.below(6)
    if choice in (1, 2):
        text += str(random.pick([1, 2, 3, 5, 8, 12, 20, 33]))
    elif choice == 3:
        text += "*"
        arguments.append(int_argument(random, random.pick([0, 1, 4, 9, 17, -1, -6, -15])))
    bits = float_bits(random)
    if kind in "aA":
        # The host is a reference only where nothing is rounded and off the
        # subnormal numbers.
        text += "." + str(random.pick([13, 14, 20, 25]))
        if (bits >> 52) & 0x7FF == 0 and bits & ((1 << 52) - 1):
            bits |= 1 << 62
    else:
        choice = random.below(7)
        if choice == 1:
            text += "."
        elif choice in (2, 3):
            text += "." + str(random.pick([0, 1, 2, 4, 7, 11, 17, 25]))
        elif choice == 4:
            text += ".*"
            arguments.append(int_argument(random, random.pick([0, 1, 3, 6, 14, -1, -5])))
    text += random.pick(["", "", "l"])
    arguments.append((f"d(0x{bits:016x}ULL)", f"f:{bits:016x}"))
    return text + kind, arguments


def cases():
    random = Random(0x9E3779B97F4A7C15)
    out = []
    literals = ["", "a", " ", "x=", "[", "]", "%%", "100%% ", "\n", "end"]
    for index in range(2600):
        if index < 1500:
            kinds = "diuoxX"
        elif index < 1800:
            kinds = "cs"
        else:
            kinds = "diuoxXcs"
        count = 1 if index < 1800 else 1 + random.below(3)
        text, arguments = random.pick(literals), []
        for _ in range(count):
            piece, more = spec(random, kinds)
            text += piece + random.pick(literals)
            arguments += more
        out.append((text.encode(), arguments))
    # Floating point has its own generator, after the others, so that adding
    # it left every earlier case as it was.
    random = Random(0xD1B54A32D192ED03)
    for index in range(2400):
        count = 1 if index < 2000 else 1 + random.below(3)
        text, arguments = random.pick(literals), []
        for _ in range(count):
            piece, more = float_spec(random) if index < 2000 or random.below(2) else spec(
                random, "diuoxXcs")
            text += piece + random.pick(literals)
            arguments += more
        out.append((text.encode(), arguments))
    return out


def reference(all_cases):
    lines = ["#include <stdio.h>", "#include <stdint.h>", "#include <stddef.h>",
             "#include <string.h>", "#include <sys/types.h>",
             "static char b[8192];",
             "static double d(uint64_t bits) { double v; memcpy(&v, &bits, 8); return v; }",
             "static void emit(int n) {",
             "    if (n < 0 || n >= (int)sizeof b) { puts(\"!\"); return; }",
             "    for (int i = 0; i < n; i++) printf(\"%02x\", (unsigned char)b[i]);",
             "    putchar('\\n');",
             "}",
             "int main(void) {"]
    for text, arguments in all_cases:
        literal = "".join(f"\\{byte:03o}" for byte in text)
        call = ", ".join([f'"{literal}"'] + [argument for argument, _ in arguments])
        lines.append(f"    emit(snprintf(b, sizeof b, {call}));")
    lines += ["    return 0;", "}"]
    with tempfile.TemporaryDirectory() as directory:
        source, binary = Path(directory, "reference.c"), Path(directory, "reference")
        source.write_text("\n".join(lines) + "\n")
        subprocess.run(["clang", "-w", "-O0", source, "-o", binary], check=True)
        output = subprocess.run([binary], check=True, capture_output=True, text=True).stdout
    results = output.splitlines()
    if len(results) != len(all_cases) or "!" in results:
        sys.exit("the reference program did not format every case")
    return results


def main(argv):
    if len(argv) != 2 or argv[1] not in ("generate", "--check"):
        sys.exit("usage: reference.py generate|--check")
    all_cases = cases()
    results = reference(all_cases)
    header = ["# Generated by crates/loader/tests/printf/reference.py from the host C library's",
              "# snprintf. Do not edit. Fields: format, arguments, output; see that script."]
    rows = [f"{text.hex()}\t{','.join(slot for _, slot in arguments)}\t{result}"
            for (text, arguments), result in zip(all_cases, results)]
    content = "\n".join(header + rows) + "\n"
    if argv[1] == "generate":
        VECTORS.write_text(content)
    elif not VECTORS.exists() or VECTORS.read_text() != content:
        sys.exit("vectors.txt differs from the host C library's output; run reference.py generate")
    print(f"vectors.txt: {len(rows)} cases")


if __name__ == "__main__":
    main(sys.argv)
