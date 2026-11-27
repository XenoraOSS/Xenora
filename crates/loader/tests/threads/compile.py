#!/usr/bin/env python3
"""Compile the guest thread programs with clang and list their code.

  generate [program|fls|taskqueue]  write the selected listing (default: program)
  --check [program|fls|taskqueue]   recompile and fail if the selected listing differs

Each listing holds one line per item: `code <hex>`, `entry <name> <offset>` and
`import <name> <offset>` for each 32-bit RIP-relative reference to an import
slot. Nothing is executed and no linker is needed: the test places the code and
fills in the references. Only clang and the Python standard library are used.
"""

import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
FLAGS = ["-target", "x86_64-pc-windows-msvc", "-O1", "-ffreestanding", "-fno-builtin", "-nostdinc",
         "-fno-stack-protector", "-fno-asynchronous-unwind-tables", "-mno-implicit-float",
         "-fno-jump-tables", "-fno-optimize-sibling-calls", "-fno-ident", "-Wno-infinite-recursion"]
REL32 = range(4, 10)  # IMAGE_REL_AMD64_REL32 to REL32_5


def listing(source):
    with tempfile.TemporaryDirectory() as directory:
        obj = Path(directory, "program.obj")
        subprocess.run(["clang", *FLAGS, "-c", source, "-o", obj], check=True)
        data = obj.read_bytes()
    machine, sections, _, symbols_at, symbol_count, optional, _ = struct.unpack_from("<HHIIIHH", data)
    assert machine == 0x8664
    strings = symbols_at + 18 * symbol_count

    def name_at(raw):
        if raw[:4] == b"\0\0\0\0":
            start = strings + struct.unpack_from("<I", raw, 4)[0]
            return data[start:data.index(b"\0", start)].decode()
        return raw.rstrip(b"\0").decode()

    symbols, index = {}, 0
    while index < symbol_count:
        raw = data[symbols_at + 18 * index:symbols_at + 18 * (index + 1)]
        value, section, _, storage, extra = struct.unpack_from("<IhHBB", raw, 8)
        symbols[index] = (name_at(raw[:8]), value, section, storage)
        index += 1 + extra
    text = None
    for number in range(sections):
        fields = struct.unpack_from("<8sIIIIIIHHI", data, 20 + optional + 40 * number)
        name, size = fields[0].rstrip(b"\0").decode(), fields[3]
        if name == ".text":
            text = (number + 1, size, fields[4], fields[5], fields[7])
        elif size and name in (".data", ".rdata", ".bss"):
            sys.exit(f"unexpected {name} of {size} bytes")
    number, size, at, relocations_at, count = text
    code = bytearray(data[at:at + size])
    imports = []
    for index in range(count):
        offset, symbol, kind = struct.unpack_from("<IIH", data, relocations_at + 10 * index)
        target, value, section, _ = symbols[symbol]
        if kind not in REL32:
            sys.exit(f"relocation type {kind} against {target}")
        trailing = kind - 4
        if section == number:
            addend = struct.unpack_from("<i", code, offset)[0]
            struct.pack_into("<i", code, offset, value + addend - (offset + 4 + trailing))
        elif target.startswith("__imp_") and trailing == 0:
            imports.append((target[6:], offset))
        else:
            sys.exit(f"unresolved reference to {target}")
    lines = [f"entry {name} {value}" for name, value, section, storage in sorted(symbols.values())
             if section == number and storage == 2 and name.startswith("main_")]
    lines += [f"import {name} {offset}" for name, offset in imports]
    lines += [f"code {code[i:i + 32].hex()}" for i in range(0, len(code), 32)]
    return "\n".join(lines) + "\n"


def main(argv):
    if not 2 <= len(argv) <= 3 or argv[1] not in ("generate", "--check"):
        sys.exit("usage: compile.py generate|--check [program|fls|taskqueue]")
    fixture = argv[2] if len(argv) == 3 else "program"
    if fixture not in ("program", "fls", "taskqueue"):
        sys.exit("usage: compile.py generate|--check [program|fls|taskqueue]")
    output = HERE / f"{fixture}.txt"
    text = listing(HERE / f"{fixture}.c")
    if argv[1] == "generate":
        output.write_text(text)
    elif output.read_text() != text:
        sys.exit(f"{output.name} differs from the compiler output")
    print(f"{text.count('entry ')} programs, {text.count('import ')} import references, {32 * text.count('code ')} bytes")


if __name__ == "__main__":
    main(sys.argv)
