#!/usr/bin/env python3
"""Independent bit-exact reference vectors for F16C.

  generate   write f16c_vectors.txt next to this file
  --check    recompute in memory and fail if the committed file differs

Every finite conversion is computed as an exact `fractions.Fraction` and then
rounded by definition, using the independent SSE reference in
`float_reference.py`. Nothing here shares code with `softfloat.rs`, the decoder
or the executor, and no host floating point is used. The rules are the
operation sections of Intel SDM Vol. 2, VCVTPH2PS and VCVTPS2PH: a widening
conversion ignores DAZ and reports no denormal exception; a narrowing one
ignores FTZ, takes its rounding mode from imm8[1:0] or MXCSR.RC, and reports
DE, UE and PE for a denormal source when DM is masked and PM or UM is unmasked.
NaN payloads are shifted, quieted, and signal invalid only when signaling.

Line: <op> <width> <imm8> <mxcsr> <src0> <src1> <res0> <res1> <flags>. The
width is 128 or 256; all fields after it are hexadecimal. The source and
result words are printed as 128-bit values; unused lanes are zero.
"""

import random
import sys
from pathlib import Path

from float_reference import DOWN, NEAREST, UP, ZERO, Fmt, rounded

HERE = Path(__file__).resolve().parent
H, S = Fmt(16, 10), Fmt(32, 23)
IE, DE, OE, UE, PE = 1, 2, 8, 16, 32


def finite(fmt, value, mode, masked):
    """Round one finite lane to bits and flags, or (0, flags) if it aborts."""
    result = rounded(fmt, value, {
        "mode": mode, "ftz": False, "daz": False,
        "om": masked & OE != 0, "um": masked & UE != 0,
    })
    # The generated cases keep underflow masked: an unmasked underflow has no
    # delivered result, and the independent SSE reference leaves that result
    # open rather than choosing bits.
    assert result is not None
    return result


def h2s(value):
    """VCVTPH2PS on one binary16 lane: bits and flags."""
    sign = S.top if H.neg(value) else 0
    if H.nan(value):
        moved = (value & ((1 << H.frac) - 1)) << (S.frac - H.frac)
        return S.inf | moved | S.quiet | sign, IE if H.snan(value) else 0
    if H.isinf(value):
        return S.inf | sign, 0
    if H.mag(value) == 0:
        return sign, 0
    bits, flags = finite(S, H.value(value), NEAREST, 0x3F)
    return bits, flags


def s2h(value, mode, mxcsr):
    """VCVTPS2PH on one binary32 lane: bits and flags."""
    sign = H.top if S.neg(value) else 0
    if S.nan(value):
        moved = (value & ((1 << S.frac) - 1)) >> (S.frac - H.frac)
        return H.inf | moved | H.quiet | sign, IE if S.snan(value) else 0
    if S.isinf(value):
        return H.inf | sign, 0
    if S.mag(value) == 0:
        return sign, 0
    bits, flags = finite(H, S.value(value), mode, mxcsr >> 7 & 0x3F)
    if S.denormal(value):
        # MXCSR mask bits, not the flag bits defined above.
        dm, pm, um = mxcsr & 1 << 8, mxcsr & 1 << 12, mxcsr & 1 << 11
        if not dm:
            flags |= DE
        elif not pm or not um:
            flags |= DE | UE | PE
    return bits, flags


def make(op, imm, mxcsr, src0, src1, width):
    mode = (mxcsr >> 13) & 3 if imm & 4 else imm & 3
    count = 4 if width == 128 else 8
    if op == "vcvtph2ps":
        words = [src0] + ([src1] if width == 256 else [])
        values = [(words[i // 4] >> (16 * (i % 4))) & 0xFFFF for i in range(count)]
    else:
        values = [((src0 if i < 4 else src1) >> (32 * (i % 4))) & 0xFFFFFFFF
                  for i in range(count)]
    results, flags = [], 0
    for value in values:
        bits, raised = h2s(value) if op == "vcvtph2ps" else s2h(value, mode, mxcsr)
        flags |= raised
        results.append(bits)
    if op == "vcvtph2ps":
        out = [sum(results[i] << (32 * (i % 4)) for i in range(min(count, 4))),
               sum(results[i] << (32 * (i - 4)) for i in range(4, count))]
    else:
        out = [sum(results[i] << (16 * i) for i in range(count)), 0]
    return out, flags


def cases():
    rng = random.Random(0x4C16F)
    half_special = [0x0000, 0x8000, 0x0001, 0x03FF, 0x0400, 0x3C00, 0xBC00,
                    0x7BFF, 0xFBFF, 0x7C00, 0xFC00, 0x7E00, 0x7C01, 0xFE01]
    single_special = [0x00000000, 0x80000000, 0x00000001, 0x007FFFFF, 0x00800000,
                      0x3F800000, 0xBF800000, 0x477FE000, 0x477FFFFF, 0x7F800000,
                      0xFF800000, 0x7FC00001, 0x7F800001, 0xFF800001,
                      0x33800000, 0x38800000, 0x34000000, 0x3F800001, 0x3F7FFFFF]
    configs = [(0x1F80, imm) for imm in (0, 1, 2, 3, 4, 7, 8, 255)]
    configs += [(0x1FC0, 0), (0x9F80, 0), (0x1F80 | (2 << 13), 4),
                (0x1F80 | (3 << 13), 7), (0x1F80 & ~(1 << 10), 0),
                (0x1F80 & ~(1 << 12), 0), (0x1F80 & ~(1 << 8), 0),
                (0x1F80 & ~(1 << 8) & ~(1 << 12), 0)]
    rows = []
    for op in ("vcvtph2ps", "vcvtps2ph"):
        for width in (128, 256):
            for mxcsr, imm in configs:
                for _ in range(6):
                    if op == "vcvtph2ps":
                        pool = half_special if rng.randrange(3) else [rng.randrange(1 << 16) for _ in range(8)]
                    else:
                        pool = single_special if rng.randrange(3) else [rng.randrange(1 << 32) for _ in range(8)]
                    src0 = sum(pool[rng.randrange(len(pool))] << (16 * i if op == "vcvtph2ps" else 32 * i)
                               for i in range(4))
                    src1 = sum(pool[rng.randrange(len(pool))] << (16 * i if op == "vcvtph2ps" else 32 * i)
                               for i in range(4))
                    if width == 128:
                        # Keep the unused source word zero: the same vectors are
                        # also checked on the 256-bit form.
                        src1 = 0
                    out, flags = make(op, imm, mxcsr, src0, src1, width)
                    rows.append(f"{op} {width} {imm:02x} {mxcsr:08x} {src0:032x} {src1:032x} "
                                f"{out[0]:032x} {out[1]:032x} {flags:02x}")
    # One lane per special value, in every rounding mode and several exception
    # masks, so boundary cases are not left to chance.
    for value in single_special:
        for mode in range(4):
            for mxcsr in (0x1F80, 0x9F80, 0x1FC0, 0x1F80 & ~(1 << 8),
                          0x1F80 & ~(1 << 12), 0x1F80 & ~(1 << 10)):
                out, flags = make("vcvtps2ph", mode, mxcsr, value, 0, 128)
                rows.append(f"vcvtps2ph 128 {mode:02x} {mxcsr:08x} {value:032x} 0 "
                            f"{out[0]:032x} 0 {flags:02x}")
    for value in half_special:
        for mxcsr in (0x1F80, 0x1FC0, 0x9F80):
            out, flags = make("vcvtph2ps", 0, mxcsr, value, 0, 128)
            rows.append(f"vcvtph2ps 128 00 {mxcsr:08x} {value:032x} 0 "
                        f"{out[0]:032x} 0 {flags:02x}")
    return list(dict.fromkeys(rows))


def text():
    return "\n".join(cases()) + "\n"


def main(argv):
    if len(argv) != 2 or argv[1] not in ("generate", "--check"):
        sys.exit("usage: f16c_reference.py generate|--check")
    path = HERE / "f16c_vectors.txt"
    if argv[1] == "generate":
        path.write_text(text())
    elif path.read_text() != text():
        sys.exit("f16c_vectors.txt differs from the reference")
    print(f"f16c_vectors.txt: {len(cases())} vectors")


if __name__ == "__main__":
    main(sys.argv)
