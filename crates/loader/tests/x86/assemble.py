#!/usr/bin/env python3
"""Assemble the original x86-64 fixtures with the LLVM assembler.

  generate   write integer.txt, vector.txt and float.txt next to this file
  --check    reassemble in memory and fail if a committed fixture differs

Each output line is `<hex bytes> | <Intel-syntax text>`. The assembler is
`clang -target x86_64-apple-macos` and the bytes are read with `objdump -d`,
so no Xenora code takes part in choosing an encoding. Nothing is executed.
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
LOW = "a c d b sp bp si di".split()
CONDITIONS = "o no b ae e ne be a s ns p np l ge le g".split()
PTR = {8: "byte", 16: "word", 32: "dword", 64: "qword"}


def reg(index, bits):
    if index >= 8:
        return f"r{index}" + {64: "", 32: "d", 16: "w", 8: "b"}[bits]
    name = LOW[index]
    if bits == 8:
        return name + "l"
    full = name + "x" if index < 4 else name
    return {64: "r", 32: "e", 16: ""}[bits] + full


def mem(bits):
    return f"{PTR[bits]} ptr [rsi + 4*rdi + 16]"


def cases():
    out = []
    for op in "add or adc sbb and sub xor cmp test".split():
        for bits in (8, 16, 32, 64):
            out += [f"{op} {reg(3, bits)}, {reg(1, bits)}", f"{op} {reg(9, bits)}, {reg(12, bits)}",
                    f"{op} {reg(0, bits)}, {reg(0, bits)}", f"{op} {mem(bits)}, {reg(2, bits)}"]
            if op != "test":
                # The reversed encoding of a register pair is load-form only.
                out.append(f"{op} {reg(10, bits)}, {mem(bits)}")
            imm = {8: "0x7f", 16: "0x1234", 32: "0x12345678", 64: "0x12345678"}[bits]
            out += [f"{op} {reg(0, bits)}, {imm}", f"{op} {reg(3, bits)}, {imm}",
                    f"{op} {mem(bits)}, {imm}"]
            if bits != 8:
                out += [f"{op} {reg(11, bits)}, -3", f"{op} {mem(bits)}, -3"]
        out += [f"{op} ah, bl", f"{op} cl, dh", f"{op} bh, 0x55"]
    for op in "inc dec neg not mul imul div idiv".split():
        for bits in (8, 16, 32, 64):
            out += [f"{op} {reg(1, bits)}", f"{op} {reg(9, bits)}", f"{op} {mem(bits)}"]
        out.append(f"{op} ch")
    for op in "rol ror rcl rcr shl shr sar".split():
        for bits in (8, 16, 32, 64):
            for count in ("1", "3", "7", "cl"):
                out += [f"{op} {reg(3, bits)}, {count}", f"{op} {mem(bits)}, {count}"]
            out += [f"{op} {reg(13, bits)}, {bits - 1}", f"{op} {reg(13, bits)}, 0"]
    for op in ("shld", "shrd"):
        for bits in (16, 32, 64):
            for count in ("1", "3", "7", "cl"):
                out += [f"{op} {reg(3, bits)}, {reg(9, bits)}, {count}",
                        f"{op} {mem(bits)}, {reg(2, bits)}, {count}"]
            out += [f"{op} {reg(13, bits)}, {reg(0, bits)}, {bits - 1}", f"{op} {reg(13, bits)}, {reg(1, bits)}, 0",
                    f"{op} {reg(3, bits)}, {reg(3, bits)}, 5", f"{op} {reg(12, bits)}, {reg(10, bits)}, {bits}"]
    for bits in (16, 32, 64):
        out += [f"imul {reg(3, bits)}, {reg(9, bits)}", f"imul {reg(3, bits)}, {mem(bits)}",
                f"imul {reg(8, bits)}, {reg(1, bits)}, -7", f"imul {reg(8, bits)}, {reg(1, bits)}, 0x1234",
                f"imul {reg(8, bits)}, {mem(bits)}, 0x1234"]
        for op in "bsf bsr popcnt tzcnt lzcnt".split():
            out += [f"{op} {reg(3, bits)}, {reg(9, bits)}", f"{op} {reg(10, bits)}, {mem(bits)}"]
        for op in "bt bts btr btc".split():
            out += [f"{op} {reg(3, bits)}, {reg(9, bits)}", f"{op} {reg(10, bits)}, 5",
                    f"{op} {mem(bits)}, {bits - 1}"]
        for cc in CONDITIONS:
            out += [f"cmov{cc} {reg(3, bits)}, {reg(9, bits)}", f"cmov{cc} {reg(10, bits)}, {mem(bits)}"]
        for src in (8, 16):
            if src < bits:
                for op in ("movzx", "movsx"):
                    out += [f"{op} {reg(3, bits)}, {reg(9, src)}", f"{op} {reg(10, bits)}, {mem(src)}"]
        out += [f"lea {reg(3, bits)}, [rsi + 4*rdi + 16]", f"lea {reg(12, bits)}, [r9 - 5]",
                f"lea {reg(0, bits)}, [8*r10 + 64]"]
    out += ["movsxd rbx, r9d", f"movsxd r10, {mem(32)}", "movzx ebx, ah", "movsx ecx, bh"]
    for cc in CONDITIONS:
        out += [f"set{cc} bl", f"set{cc} {mem(8)}", f"set{cc} ah"]
    for bits in (8, 16, 32, 64):
        out += [f"mov {reg(3, bits)}, {reg(9, bits)}", f"mov {reg(10, bits)}, {mem(bits)}",
                f"mov {mem(bits)}, {reg(2, bits)}", f"mov {reg(14, bits)}, 0x5a",
                f"mov {mem(bits)}, 0x5a", f"xchg {reg(3, bits)}, {reg(9, bits)}",
                f"xchg {mem(bits)}, {reg(2, bits)}"]
        # A0-A3: the accumulator and an absolute address, the memory operand's.
        out += [f"movabs {reg(0, bits)}, {PTR[bits]} ptr [0x2020]",
                f"movabs {PTR[bits]} ptr [0x2020], {reg(0, bits)}"]
    for op in ("xadd", "cmpxchg"):
        for bits in (8, 16, 32, 64):
            # Destination RAX always matches the accumulator; a register with
            # itself keeps the sum. LOCK is valid only with a memory destination.
            out += [f"{op} {reg(3, bits)}, {reg(1, bits)}", f"{op} {reg(9, bits)}, {reg(12, bits)}",
                    f"{op} {reg(0, bits)}, {reg(2, bits)}", f"{op} {reg(3, bits)}, {reg(3, bits)}",
                    f"{op} {mem(bits)}, {reg(2, bits)}", f"lock {op} {mem(bits)}, {reg(2, bits)}",
                    f"lock {op} {mem(bits)}, {reg(0, bits)}"]
        out += [f"{op} ah, bl", f"{op} cl, dh", f"{op} {mem(8)}, ch"]
    for bits in (8, 16, 32, 64):
        imm = {8: "0x7f", 16: "0x1234", 32: "0x12345678", 64: "0x12345678"}[bits]
        for op in "add or adc sbb and sub xor".split():
            out += [f"lock {op} {mem(bits)}, {reg(2, bits)}", f"lock {op} {mem(bits)}, {reg(12, bits)}",
                    f"lock {op} {mem(bits)}, {imm}", f"lock {op} {mem(bits)}, -3"]
        for op in "inc dec neg not".split():
            out.append(f"lock {op} {mem(bits)}")
        out.append(f"lock xchg {mem(bits)}, {reg(2, bits)}")
        if bits != 8:
            for op in "bts btr btc".split():
                out += [f"lock {op} {mem(bits)}, {bits - 1}", f"lock {op} {mem(bits)}, 3"]
    out += [f"lock add {mem(8)}, ch", f"lock xchg {mem(8)}, ah"]
    out += ["mov ah, bl", "mov dl, ch", "mov bh, 0x21", "xchg ah, bl", "mov sil, dil",
            "movabs r15, 0x8877665544332211", "mov rcx, -2", "bswap ebx", "bswap r9",
            "cbw", "cwde", "cdqe", "cwd", "cdq", "cqo", "clc", "stc", "cmc", "lahf", "sahf",
            "nop", "cpuid", "nop dword ptr [rax + 8]", "pause", "lfence", "mfence", "sfence", "fninit",
            f"movnti {mem(32)}, ebx", f"movnti {mem(64)}, rbx",
            f"movnti {mem(32)}, r9d", f"movnti {mem(64)}, r14", "push r9", "push rbx", "push -5", "push 0x12345678",
            f"push {mem(64)}", "pop rbx", "pop r12", f"pop {mem(64)}", "leave"]
    # The accumulator short form of XCHG: 91..97, and 90 with REX.B.
    for index in range(1, 16):
        bits = (16, 32, 64)[index % 3]
        out.append(f"xchg {reg(0, bits)}, {reg(index, bits)}")
    out += ["xchg r8w, ax", "xchg r8, rax", "xchg r8d, eax", "xchg ecx, eax", "xchg r15w, ax"]
    return list(dict.fromkeys(out))


PACKED = """punpcklbw punpcklwd punpckldq punpcklqdq punpckhbw punpckhwd punpckhdq punpckhqdq
packsswb packssdw packuswb pcmpgtb pcmpgtw pcmpgtd pcmpeqb pcmpeqw pcmpeqd paddb paddw paddd paddq
psubb psubw psubd psubq paddsb paddsw paddusb paddusw psubsb psubsw psubusb psubusw pmullw pmulhw
pmulhuw pmuludq pmaddwd psadbw pavgb pavgw pminub pmaxub pminsw pmaxsw pand pandn por pxor andps
andpd andnps andnpd orps orpd xorps xorpd unpcklps unpcklpd unpckhps unpckhpd psllw pslld psllq
psrlw psrld psrlq psraw psrad pshufb phaddw phaddd phaddsw pmaddubsw phsubw phsubd phsubsw
psignb psignw psignd pmulhrsw pabsb pabsw pabsd movaps movapd movups movupd movdqa movdqu""".split()


def vector_cases():
    wide, quad, dword = (f"{w} ptr [rsi + 4*rdi + 16]" for w in ("xmmword", "qword", "dword"))
    word = "word ptr [rsi + 4*rdi + 16]"
    out = []
    for op in PACKED:
        out += [f"{op} xmm3, xmm9", f"{op} xmm10, {wide}", f"{op} xmm2, xmm2"]
    for op in "movaps movapd movups movupd movdqa movdqu movntdq movntps movntpd".split():
        out.append(f"{op} {wide}, xmm11")
    for op, width in (("movss", dword), ("movsd", quad), ("movq", quad)):
        out += [f"{op} xmm3, xmm9", f"{op} xmm10, {width}", f"{op} {width}, xmm11"]
    for op in "movlps movlpd movhps movhpd".split():
        out += [f"{op} xmm3, {quad}", f"{op} {quad}, xmm9"]
    out += ["movhlps xmm3, xmm9", "movlhps xmm10, xmm1", "movhlps xmm2, xmm2", "movlhps xmm2, xmm2"]
    out += ["movd xmm3, r9d", "movd xmm12, eax", "movq xmm3, r9", "movd r9d, xmm3", "movd ebx, xmm12",
            "movq r9, xmm3", f"movd xmm3, {dword}", f"movd {dword}, xmm9"]
    for op in "movmskps movmskpd pmovmskb".split():
        out += [f"{op} ebx, xmm9", f"{op} r10d, xmm3"]
    for op, bits in (("w", 16), ("d", 32), ("q", 64)):
        for name in ("psll", "psrl", "psra"):
            if name + op != "psraq":
                out += [f"{name}{op} xmm{3 + 6 * (n % 2)}, {n}" for n in (0, 1, 7, bits - 1, bits, 255)]
    for op in ("pslldq", "psrldq"):
        out += [f"{op} xmm9, {n}" for n in (0, 1, 5, 15, 16, 255)]
    for op in "pshufd pshufhw pshuflw shufps shufpd".split():
        out += [f"{op} xmm3, xmm9, {n}" for n in (0, 27, 78, 177, 255)]
        out += [f"{op} xmm10, {wide}, 114", f"{op} xmm2, xmm2, 57"]
    for n in (0, 1, 5, 15, 16, 17, 31, 32, 255):
        out += [f"palignr xmm3, xmm9, {n}", f"palignr xmm10, {wide}, {n}", f"palignr xmm2, xmm2, {n}"]
    for op in """pmuldq pcmpeqq packusdw pminsb pminsd pminuw pminud pmaxsb pmaxsd pmaxuw pmaxud pmulld
            phminposuw ptest""".split():
        out += [f"{op} xmm3, xmm9", f"{op} xmm10, {wide}", f"{op} xmm2, xmm2"]
    # XMM0 selects; it may also be the source or the destination.
    for op in "pblendvb blendvps blendvpd".split():
        out += [f"{op} xmm3, xmm9, xmm0", f"{op} xmm10, {wide}, xmm0", f"{op} xmm2, xmm0, xmm0",
                f"{op} xmm0, xmm9, xmm0"]
    for kind, width in (("bw", quad), ("bd", dword), ("bq", word), ("wd", quad), ("wq", dword), ("dq", quad)):
        for op in ("pmovsx", "pmovzx"):
            out += [f"{op}{kind} xmm3, xmm9", f"{op}{kind} xmm10, {width}", f"{op}{kind} xmm2, xmm2"]
    out.append(f"movntdqa xmm10, {wide}")
    for op in ("pblendw", "mpsadbw"):
        out += [f"{op} xmm3, xmm9, {n}" for n in (0, 1, 2, 3, 4, 5, 6, 7, 129, 255)]
        out += [f"{op} xmm10, {wide}, 90", f"{op} xmm2, xmm2, 165"]
    for op, most in (("blendps", 16), ("blendpd", 4)):
        out += [f"{op} xmm3, xmm9, {n}" for n in (*range(most), 255)]
        out += [f"{op} xmm10, {wide}, {most - 6 if most > 6 else 2}", f"{op} xmm2, xmm2, 1"]
    # Source lane, destination lane and zero mask; memory has no source lane.
    for n in (0, 16, 32, 48, 64, 81, 98, 152, 160, 199, 228, 254, 255):
        out += [f"insertps xmm3, xmm9, {n}", f"insertps xmm10, {dword}, {n}", f"insertps xmm2, xmm2, {n}"]
    for n in (0, 1, 2, 3, 4, 255):
        out += [f"extractps {r}, xmm9, {n}" for r in ("ebx", "r9d", "ebp")] + [f"extractps {dword}, xmm3, {n}"]
    # ebp would be read as CH if its byte were a legacy byte register.
    byte = "byte ptr [rsi + 4*rdi + 16]"
    for op, regs, place in (("b", ("ebx", "r9d", "ebp"), byte), ("w", ("ebx", "r9d", "ebp"), word),
                            ("d", ("ebx", "r9d"), dword), ("q", ("rbx", "r9"), quad)):
        for n in (0, 1, 2, 7, 15, 31, 255):
            for general in regs:
                if op != "w":
                    out.append(f"pinsr{op} xmm9, {general}, {n}")
                    out.append(f"pextr{op} {general}, xmm9, {n}")
            if op != "w":
                out.append(f"pinsr{op} xmm3, {place}, {n}")
            out.append(f"pextr{op} {place}, xmm3, {n}")
    for n in (0, 3, 7):
        out += [f"pinsrw xmm9, ebx, {n}", f"pinsrw xmm3, {word}, {n}", f"pextrw r9d, xmm3, {n}"]
    out += [f"stmxcsr {dword}"]
    return list(dict.fromkeys(out))


def float_cases():
    out = []
    memory = {"ss": "dword", "sd": "qword", "ps": "xmmword", "pd": "xmmword"}
    place = " ptr [rsi + 4*rdi + 16]"
    for kind, width in memory.items():
        for op in "add sub mul div min max sqrt".split():
            out += [f"{op}{kind} xmm3, xmm9", f"{op}{kind} xmm3, {width}{place}"]
        for code in range(8):
            out += [f"cmp{kind} xmm3, xmm9, {code}", f"cmp{kind} xmm3, {width}{place}, {code}"]
        for n in (*range(16), 244):
            out += [f"round{kind} xmm3, xmm9, {n}", f"round{kind} xmm3, {width}{place}, {n}"]
    # The tests put every other immediate in the last byte.
    for op in ("dpps", "dppd"):
        out += [f"{op} xmm3, xmm9, 255", f"{op} xmm3, xmmword{place}, 255", f"{op} xmm3, xmm3, 255"]
    for kind in ("ss", "sd"):
        for op in ("comi", "ucomi"):
            out += [f"{op}{kind} xmm3, xmm9", f"{op}{kind} xmm3, {memory[kind]}{place}"]
        for op in ("cvt", "cvtt"):
            for reg in ("r9d", "r9"):
                out += [f"{op}{kind}2si {reg}, xmm9", f"{op}{kind}2si {reg}, {memory[kind]}{place}"]
        for reg, width in (("r9d", "dword"), ("r9", "qword")):
            out += [f"cvtsi2{kind} xmm3, {reg}", f"cvtsi2{kind} xmm3, {width}{place}"]
    for op, width in (("cvtss2sd", "dword"), ("cvtsd2ss", "qword"), ("cvtps2pd", "qword"), ("cvtpd2ps", "xmmword"),
                      ("cvtdq2ps", "xmmword"), ("cvtdq2pd", "qword"), ("cvtps2dq", "xmmword"), ("cvttps2dq", "xmmword"),
                      ("cvtpd2dq", "xmmword"), ("cvttpd2dq", "xmmword")):
        out += [f"{op} xmm3, xmm9", f"{op} xmm3, {width}{place}"]
    return out


def assemble(lines):
    with tempfile.TemporaryDirectory() as directory:
        source, obj = Path(directory, "a.s"), Path(directory, "a.o")
        source.write_text(".intel_syntax noprefix\n" + "\n".join(lines) + "\n")
        subprocess.run(["clang", "-target", "x86_64-apple-macos", "-c", source, "-o", obj], check=True)
        # One line per instruction: objdump never wraps its raw bytes.
        listing = subprocess.run(["objdump", "-d", obj], check=True, capture_output=True, text=True).stdout
    rows, prefix = [], []
    for match in re.finditer(r"^\s*[0-9a-f]+:\s((?:[0-9a-f]{2}\s)+)(.*)$", listing, re.M):
        row = prefix + match.group(1).split()
        # objdump lists a leading LOCK prefix on a row of its own.
        prefix = row if match.group(2).strip() == "lock" else []
        if not prefix:
            rows.append(row)
    if len(rows) != len(lines):
        sys.exit(f"{len(lines)} instructions assembled to {len(rows)} rows")
    return "".join(f"{' '.join(row)} | {line}\n" for row, line in zip(rows, lines))


def main(argv):
    if len(argv) != 2 or argv[1] not in ("generate", "--check"):
        sys.exit("usage: assemble.py generate|--check")
    for name, lines in (("integer.txt", cases()), ("vector.txt", vector_cases()),
                        ("float.txt", float_cases())):
        text = assemble(lines)
        if argv[1] == "generate":
            (HERE / name).write_text(text)
        elif (HERE / name).read_text() != text:
            sys.exit(f"{name} differs from the assembler output")
        print(f"{name}: {len(lines)} instructions")


if __name__ == "__main__":
    main(sys.argv)
