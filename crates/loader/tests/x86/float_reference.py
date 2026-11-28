#!/usr/bin/env python3
"""Reference vectors for SSE and SSE2 floating point, from exact fractions.

  generate   write float_vectors.txt and dot_vectors.txt next to this file
  --check    recompute in memory and fail if a committed file differs

Every finite result is computed as an exact `fractions.Fraction` (square roots by
integer comparison) and then rounded by definition, so nothing here shares code
or method with crates/loader/src/softfloat.rs, and no host floating point is
used. Rules: Intel SDM Vol. 1, 4.8, 4.9 and 11.5, and the Vol. 2 operation
sections. Cases whose flags those sections leave open are not generated:
a denormal dividend over zero, flushing an exact denormal result, and the
precision flag of a denormal converted to an integer under DAZ.

Also not generated: a DPPS or DPPD addition of two different NaNs, and one whose
multiplications stop differently taken together or one by one.

Line: <operation> <mxcsr> <a> <b> <result> <mxcsr flags>, all values hex. In
dot_vectors.txt the immediate follows the operation, and a result of "-" means an
unmasked exception was raised.
"""

import random
import sys
from fractions import Fraction
from math import isqrt
from pathlib import Path

HERE = Path(__file__).resolve().parent
IE, DE, ZE, OE, UE, PE = 1, 2, 4, 8, 16, 32
NEAREST, DOWN, UP, ZERO = range(4)


class Fmt:
    def __init__(self, bits, frac):
        self.bits, self.frac = bits, frac
        self.ebits = bits - 1 - frac
        self.emax = 2 ** (self.ebits - 1) - 1
        self.emin = 1 - self.emax
        self.inf = (2 ** self.ebits - 1) << frac
        self.quiet = 1 << (frac - 1)
        self.top = 1 << (bits - 1)

    def mag(self, v):
        return v % self.top

    def neg(self, v):
        return v >= self.top

    def nan(self, v):
        return self.mag(v) > self.inf

    def snan(self, v):
        return self.nan(v) and not v & self.quiet

    def isinf(self, v):
        return self.mag(v) == self.inf

    def denormal(self, v):
        return 0 < self.mag(v) < (1 << self.frac)

    def value(self, v):
        m = self.mag(v)
        field, fraction = m >> self.frac, m % (1 << self.frac)
        if field == 0:
            x = Fraction(fraction) * Fraction(2) ** (self.emin - self.frac)
        else:
            x = (1 + Fraction(fraction, 1 << self.frac)) * Fraction(2) ** (field - self.emax)
        return -x if self.neg(v) else x

    def encode(self, sign, x):
        """Bits of an exactly representable non-negative x."""
        if x == 0:
            m = 0
        elif x < Fraction(2) ** self.emin:
            m = int(x / Fraction(2) ** (self.emin - self.frac))
        else:
            e = exponent(x)
            m = ((e + self.emax) << self.frac) + int((x / Fraction(2) ** e - 1) * (1 << self.frac))
        return m + (self.top if sign else 0)


S, D = Fmt(32, 23), Fmt(64, 52)


def exponent(x):
    """floor(log2 x) for x > 0."""
    e = x.numerator.bit_length() - x.denominator.bit_length()
    return e if Fraction(2) ** e <= x else e - 1


def pick(below, above, position, sign, mode, even):
    """Choose between neighbours; position is -1, 0 or 1 against the midpoint,
    or None when exactly `below`."""
    if position is None:
        return below
    if mode == NEAREST:
        if position == 0:
            return below if even else above
        return above if position > 0 else below
    if mode == ZERO:
        return below
    return above if (mode == UP) != sign else below


def quantize(n, rest_sign, quantum, sign, mode):
    """n quanta below the value; rest_sign as in pick."""
    return pick(n * quantum, (n + 1) * quantum, rest_sign, sign, mode, n % 2 == 0)


def locate(x, quantum):
    """Quanta below the positive fraction x, and its side of the midpoint."""
    q = x / quantum
    n = q.numerator // q.denominator
    rest = q - n
    if rest == 0:
        return n, None
    half = Fraction(1, 2)
    return n, (rest > half) - (rest < half)


def locate_root(x, quantum):
    """As locate, for the square root of x."""
    q = x / (quantum * quantum)
    n = isqrt(q.numerator // q.denominator)
    if n * n == q:
        return n, None
    middle = (Fraction(n) + Fraction(1, 2)) ** 2
    return n, (q > middle) - (q < middle)


def finish(fmt, sign, e, locator, control):
    """Round a positive exact value with floor(log2) = e, given its locator."""
    mode, ftz, flags = control["mode"], control["ftz"], 0
    n, side = locator(Fraction(2) ** (e - fmt.frac))
    unbounded = quantize(n, side, Fraction(2) ** (e - fmt.frac), sign, mode)
    tiny = unbounded < Fraction(2) ** fmt.emin
    quantum = Fraction(2) ** (max(e, fmt.emin) - fmt.frac)
    inexact = side is not None
    n, side = locator(quantum)
    if tiny and not control.get("um", True):
        # No result. Precision is open if only denormalizing is inexact.
        return None if inexact != (side is not None) else (0, UE | (PE if inexact else 0))
    result = quantize(n, side, quantum, sign, mode)
    if tiny and ftz:
        if side is None:
            return None  # flushing an exact denormal is left open
        return fmt.encode(sign, Fraction(0)), UE | PE
    if side is not None:
        flags |= PE | (UE if tiny else 0)
    if result >= Fraction(2) ** (fmt.emax + 1):
        if not control.get("om", True):
            return 0, OE | (PE if inexact else 0)
        infinite = mode == NEAREST or (mode == UP and not sign) or (mode == DOWN and sign)
        return (fmt.inf if infinite else fmt.inf - 1) + (fmt.top if sign else 0), OE | PE
    return fmt.encode(sign, result), flags


def rounded(fmt, x, control):
    sign = x < 0
    x = abs(x)
    return finish(fmt, sign, exponent(x), lambda q: locate(x, q), control)


def default(fmt):
    return fmt.top + fmt.inf + fmt.quiet


def prepare(fmt, v, control):
    if control["daz"] and fmt.denormal(v):
        return fmt.top if fmt.neg(v) else 0
    return v


def binary(op, fmt, a, b, control):
    a, b = prepare(fmt, a, control), prepare(fmt, b, control)
    de = DE if fmt.denormal(a) or fmt.denormal(b) else 0
    if op in ("min", "max"):
        if fmt.nan(a) or fmt.nan(b):
            return b, IE
        order = compare(fmt, a, b)
        return (a if order == (-1 if op == "min" else 1) else b), de
    if fmt.nan(a) or fmt.nan(b):
        chosen = a if fmt.nan(a) else b
        return chosen | fmt.quiet, IE if fmt.snan(a) or fmt.snan(b) else 0
    if op == "sub":
        b ^= fmt.top
    sa, sb = fmt.neg(a), fmt.neg(b)
    sign = sa != sb
    signed = lambda m: m + (fmt.top if sign else 0)
    if op in ("add", "sub"):
        if fmt.isinf(a) and fmt.isinf(b) and sign:
            return default(fmt), IE
        if fmt.isinf(a):
            return a, de
        if fmt.isinf(b):
            return b, de
        x = fmt.value(a) + fmt.value(b)
        if x == 0:
            both = fmt.mag(a) == 0 and not sign
            return (fmt.top if (sa if both else control["mode"] == DOWN) else 0), de
    elif op == "mul":
        if fmt.isinf(a) or fmt.isinf(b):
            if fmt.mag(a) == 0 or fmt.mag(b) == 0:
                return default(fmt), IE
            return signed(fmt.inf), de
        x = fmt.value(a) * fmt.value(b)
        if x == 0:
            return signed(0), de
    else:
        if (fmt.isinf(a) and fmt.isinf(b)) or (fmt.mag(a) == 0 and fmt.mag(b) == 0):
            return default(fmt), IE
        if fmt.isinf(a):
            return signed(fmt.inf), de
        if fmt.isinf(b):
            return signed(0), de
        if fmt.mag(b) == 0:
            return None if de else (signed(fmt.inf), ZE)
        x = fmt.value(a) / fmt.value(b)
        if x == 0:
            return signed(0), de
    out = rounded(fmt, x, control)
    return out and (out[0], out[1] | de)


def horizontal(name, fmt, a, b, control):
    """128-bit packed result of a horizontal or add-subtract operation, as the
    lane composition of exact binary lanes. The flags of every lane are OR-ed
    together, which is how the processor accumulates them. A lane whose outcome
    the rules leave open discards the whole vector, as for the scalar forms."""
    count = 128 // fmt.bits
    lane = lambda v, i: (v >> (fmt.bits * i)) % (1 << fmt.bits)
    pairwise = name.startswith("h")
    subtract = "sub" in name
    result = 0
    flags = 0
    for index in range(count):
        if pairwise:
            if index < count // 2:
                vector, pair = a, index * 2
            else:
                vector, pair = b, (index - count // 2) * 2
            x, y = lane(vector, pair), lane(vector, pair + 1)
            op = "sub" if subtract else "add"
        else:
            x, y = lane(a, index), lane(b, index)
            op = "sub" if index % 2 == 0 else "add"
        out = binary(op, fmt, x, y, control)
        if out is None:
            return None
        result |= out[0] << (fmt.bits * index)
        flags |= out[1]
    return result, flags


def root(fmt, v, control):
    v = prepare(fmt, v, control)
    if fmt.nan(v):
        return v | fmt.quiet, IE if fmt.snan(v) else 0
    if fmt.mag(v) == 0:
        return v, 0
    if fmt.neg(v):
        return default(fmt), IE
    if fmt.isinf(v):
        return v, 0
    x = fmt.value(v)
    bits, flags = finish(fmt, False, exponent(x) // 2, lambda q: locate_root(x, q), control)
    return bits, flags | (DE if fmt.denormal(v) else 0)


def compare(fmt, a, b):
    def key(v):
        if fmt.isinf(v):
            return Fraction(2) ** (fmt.emax + 2) * (-1 if fmt.neg(v) else 1)
        return fmt.value(v)
    return (key(a) > key(b)) - (key(a) < key(b))


def ordering(fmt, a, b, signaling, control):
    """(-1, 0, 1 or None for unordered), flags."""
    a, b = prepare(fmt, a, control), prepare(fmt, b, control)
    if fmt.nan(a) or fmt.nan(b):
        return None, IE if signaling or fmt.snan(a) or fmt.snan(b) else 0
    return compare(fmt, a, b), DE if fmt.denormal(a) or fmt.denormal(b) else 0


def predicate(code, fmt, a, b, control):
    order, flags = ordering(fmt, a, b, code in (1, 2, 5, 6), control)
    truth = [order == 0, order == -1, order in (-1, 0), order is None,
             order != 0, order != -1, order not in (-1, 0), order is not None][code]
    return (2 ** fmt.bits - 1 if truth else 0), flags


def comparison(fmt, a, b, signaling, control):
    """RFLAGS as ZF=4, PF=2, CF=1."""
    order, flags = ordering(fmt, a, b, signaling, control)
    return {None: 7, 1: 0, -1: 1, 0: 4}[order], flags


def widen(src, dst, v, control):
    v = prepare(src, v, control)
    sign = dst.top if src.neg(v) else 0
    if src.nan(v):
        payload = Fraction(src.mag(v) - src.inf) * Fraction(2) ** (dst.frac - src.frac)
        return sign + dst.inf + (int(payload) | dst.quiet), IE if src.snan(v) else 0
    if src.isinf(v):
        return sign + dst.inf, 0
    x = src.value(v)
    if x == 0:
        return sign, 0
    out = rounded(dst, x, control)
    return out and (out[0], out[1] | (DE if src.denormal(v) else 0))


def to_int(fmt, v, bits, truncate, control):
    if control["daz"] and fmt.denormal(v):
        return None
    indefinite = 2 ** (bits - 1), IE
    if fmt.nan(v) or fmt.isinf(v):
        return indefinite
    x = fmt.value(v)
    n, side = locate(abs(x), Fraction(1))
    r = int(quantize(n, side, Fraction(1), x < 0, ZERO if truncate else control["mode"]))
    r = -r if x < 0 else r
    if not -(2 ** (bits - 1)) <= r < 2 ** (bits - 1):
        return indefinite
    return r % 2 ** bits, 0 if side is None else PE


def integral(fmt, v, imm, control):
    """ROUND: mode in imm 0-1 or, by bit 2, MXCSR; bit 3 hides inexactness."""
    v = prepare(fmt, v, control)
    if fmt.nan(v):
        return v | fmt.quiet, IE if fmt.snan(v) else 0
    if fmt.isinf(v) or fmt.mag(v) == 0:
        return v, 0
    x = fmt.value(v)
    n, side = locate(abs(x), Fraction(1))
    r = quantize(n, side, Fraction(1), x < 0, control["mode"] if imm // 4 % 2 else imm % 4)
    return fmt.encode(fmt.neg(v), r), PE if side is not None and imm // 8 % 2 == 0 else 0


def dot(fmt, a, b, imm, control, masks):
    """Lanes of a DPPS or DPPD, and its flags; None instead of lanes if an
    exception outside `masks` was raised. None if a rule is left open."""
    flags = 0

    def step(results):
        """Flags of one step, and whether it stops the instruction."""
        raised = 0
        for _, f in results:
            raised |= f
        early = raised & (IE | DE | ZE)
        if early & ~masks:
            return early, True
        return raised, bool(raised & ~masks)

    count = len(a)
    results = []
    for i in range(count):
        out = binary("mul", fmt, a[i], b[i], control) if imm // 2 ** (4 + i) % 2 else (0, 0)
        if out is None:
            return None
        results.append(out)
    raised, stop = step(results)
    flags |= raised
    if stop:
        alone = 0
        for out in results:
            f, ended = step([out])
            alone |= f
            if ended:
                break
        return (None, flags) if alone == flags else None
    while len(results) > 1:
        pairs = [(results[i][0], results[i + 1][0]) for i in range(0, len(results), 2)]
        results = []
        for x, y in pairs:
            if fmt.nan(x) and fmt.nan(y) and x != y:
                return None
            out = binary("add", fmt, x, y, control)
            if out is None:
                return None
            f, stop = step([out])
            flags |= f
            if stop:
                return None, flags
            results.append(out)
    total = results[0][0]
    return [total if imm // 2 ** i % 2 else 0 for i in range(count)], flags


def fma(fmt, a, b, c, form, control):
    """A fused multiply-add: one rounding of a*b, negated for `nm`, then c.

    `form` is "add", "sub", "nmadd" or "nmsub". The product and the sum are
    exact `Fraction`s, so the single rounding in `rounded` is the only one.
    """
    a, b, c = prepare(fmt, a, control), prepare(fmt, b, control), prepare(fmt, c, control)
    de = DE if fmt.denormal(a) or fmt.denormal(b) or fmt.denormal(c) else 0
    negate = form in ("nmadd", "nmsub")
    subtract = form in ("sub", "nmsub")
    product_sign = (fmt.neg(a) != fmt.neg(b)) != negate
    addend_sign = fmt.neg(c) != subtract
    zero_product = fmt.mag(a) == 0 or fmt.mag(b) == 0
    infinite_product = fmt.isinf(a) or fmt.isinf(b)
    invalid_product = zero_product and infinite_product
    if fmt.nan(a) or fmt.nan(b) or fmt.nan(c):
        chosen = a if fmt.nan(a) else b if fmt.nan(b) else c
        signaling = fmt.snan(a) or fmt.snan(b) or fmt.snan(c)
        return chosen | fmt.quiet, IE if signaling or invalid_product else 0
    if invalid_product:
        return default(fmt), IE
    if infinite_product:
        if fmt.isinf(c) and addend_sign != product_sign:
            return default(fmt), IE
        return fmt.inf + (fmt.top if product_sign else 0), de
    if fmt.isinf(c):
        return fmt.inf + (fmt.top if addend_sign else 0), de
    x = fmt.value(a) * fmt.value(b)
    if negate:
        x = -x
    y = fmt.value(c)
    if subtract:
        y = -y
    total = x + y
    if total == 0:
        # An exact zero takes the common sign of two zeros of the same sign,
        # and is negative for opposite signs only when rounding down.
        sign = product_sign if product_sign == addend_sign else control["mode"] == DOWN
        return fmt.encode(sign, Fraction(0)), de
    out = rounded(fmt, total, control)
    return out and (out[0], out[1] | de)


def vector_fma(fmt, a, b, c, form, ordering, control, scalar):
    """Lanes of a packed or scalar fused multiply-add, and the flags they raise.

    The ordering is the position of the destination among the three sources:
    132 multiplies the destination by the third, 213 the second by the
    destination, and 231 the second by the third. The alternating families
    subtract in even lanes and add in odd ones, or the other way round.
    """
    count = 1 if scalar else 128 // fmt.bits
    mask = (1 << fmt.bits) - 1
    result, flags = a, 0
    for i in range(count):
        lane_form = form
        if form == "maddsub":
            lane_form = "sub" if i % 2 == 0 else "add"
        elif form == "msubadd":
            lane_form = "add" if i % 2 == 0 else "sub"
        av = (a >> (fmt.bits * i)) & mask
        bv = (b >> (fmt.bits * i)) & mask
        cv = (c >> (fmt.bits * i)) & mask
        if ordering == "132":
            x, y, z = av, cv, bv
        elif ordering == "213":
            x, y, z = bv, av, cv
        else:
            x, y, z = bv, cv, av
        out = fma(fmt, x, y, z, lane_form, control)
        if out is None:
            return None
        flags |= out[1]
        result = (result & ~(mask << (fmt.bits * i))) | (out[0] << (fmt.bits * i))
    return result, flags


def build_fma():
    """FMA3 vectors: every family, ordering, precision and operand form."""
    rng = random.Random(0x464D4133)
    lines = []
    packed = [("fmadd", "add"), ("fmsub", "sub"), ("fnmadd", "nmadd"), ("fnmsub", "nmsub"),
              ("fmaddsub", "maddsub"), ("fmsubadd", "msubadd")]
    plain = packed[:4]
    orderings = ("132", "213", "231")

    def control(mode):
        daz, ftz = rng.random() < 0.15, rng.random() < 0.15
        return {"mode": mode, "daz": daz, "ftz": ftz,
                "mxcsr": 0x1F80 | mode << 13 | daz << 6 | ftz << 15}

    def pack(fmt, lanes):
        return sum(v << (fmt.bits * i) for i, v in enumerate(lanes))

    def emit(name, fmt, form, ordering, mode, a, b, c, scalar=False):
        rules = control(mode)
        out = vector_fma(fmt, a, b, c, form, ordering, rules, scalar)
        if out is None:
            return
        result, flags = out
        lines.append(f"{name} {rules['mxcsr']:x} {a:x} {b:x} {c:x} {result:x} {flags:x}\n")

    for fmt, kind in ((S, "ps"), (D, "pd")):
        count = 128 // fmt.bits
        for stem, form in packed:
            for ordering in orderings:
                name = stem + ordering + kind
                for _ in range(32):
                    mode = rng.randrange(4)
                    emit(name, fmt, form, ordering, mode,
                         pack(fmt, [operands(fmt, rng) for _ in range(count)]),
                         pack(fmt, [operands(fmt, rng) for _ in range(count)]),
                         pack(fmt, [operands(fmt, rng) for _ in range(count)]))
                # Every pair of special operands, with the addend cycling. The
                # grid is kept to two families to bound the suite's run time.
                if stem not in ("fmadd", "fnmsub"):
                    continue
                one, least = fmt.emax << fmt.frac, 1 << fmt.frac
                special = [0, fmt.top, 1, least - 1, least, one - 1, one, one + least,
                           fmt.inf - 1, fmt.inf, fmt.top + fmt.inf, fmt.inf + fmt.quiet + 5]
                addends = [0, one, fmt.top + one, fmt.inf, fmt.top + fmt.inf, least - 1]
                for n, x in enumerate(special):
                    for m, y in enumerate(special):
                        z = addends[(n + m) % len(addends)]
                        emit(name, fmt, form, ordering, 0,
                             pack(fmt, [x] * count), pack(fmt, [y] * count),
                             pack(fmt, [z] * count))
                # The product of two values either side of one is not exactly
                # one: a fused result keeps the difference, a separately rounded
                # product loses it.
                near = one + (1 << (fmt.frac - 1))
                below = one - (1 << (fmt.frac - 1))
                if form in ("add", "sub", "nmadd", "nmsub"):
                    for mode in range(4):
                        for sign in (0, fmt.top):
                            emit(name, fmt, form, ordering, mode,
                                 pack(fmt, [near] * count), pack(fmt, [below | sign] * count),
                                 pack(fmt, [one | fmt.top] * count))
    for fmt, kind in ((S, "ss"), (D, "sd")):
        count = 128 // fmt.bits
        for stem, form in plain:
            for ordering in orderings:
                name = stem + ordering + kind
                for _ in range(32):
                    mode = rng.randrange(4)
                    emit(name, fmt, form, ordering, mode,
                         pack(fmt, [operands(fmt, rng) for _ in range(count)]),
                         pack(fmt, [operands(fmt, rng) for _ in range(count)]),
                         pack(fmt, [operands(fmt, rng) for _ in range(count)]), scalar=True)
                if stem not in ("fmadd", "fnmsub"):
                    continue
                one, least = fmt.emax << fmt.frac, 1 << fmt.frac
                special = [0, fmt.top, 1, least, one, fmt.inf]
                for x in special:
                    for y in special:
                        for z in (0, one, fmt.inf):
                            emit(name, fmt, form, ordering, 0,
                                 pack(fmt, [x] * count), pack(fmt, [y] * count),
                                 pack(fmt, [z] * count), scalar=True)
                near = one + (1 << (fmt.frac - 1))
                below = one - (1 << (fmt.frac - 1))
                for mode in range(4):
                    emit(name, fmt, form, ordering, mode,
                         pack(fmt, [near] * count), pack(fmt, [below] * count),
                         pack(fmt, [one | fmt.top] * count), scalar=True)
    return "".join(lines)


def from_int(fmt, v, bits, control):
    x = v - 2 ** bits if v >= 2 ** (bits - 1) else v
    return rounded(fmt, Fraction(x), control) if x else (0, 0)


def operands(fmt, rng):
    one = fmt.emax << fmt.frac
    least = 1 << fmt.frac
    edge = [0, 1, 2, least - 1, least, least + 1, 2 * least, one - 1, one, one + 1, one + least,
            one + fmt.quiet, one + (fmt.frac << fmt.frac), one + (fmt.frac << fmt.frac) + 1,
            fmt.inf - 1, fmt.inf - least, fmt.inf, fmt.inf + fmt.quiet, fmt.inf + fmt.quiet + 5,
            fmt.inf + 1, fmt.inf + fmt.quiet - 1]
    choice = rng.randrange(6)
    if choice < 2:
        v = rng.choice(edge)
    elif choice == 2:
        v = rng.randrange(least)  # denormal
    elif choice == 3:  # near one, few significant bits
        v = one + (rng.randrange(-40, 41) << fmt.frac) + (rng.randrange(16) << (fmt.frac - 4))
    elif choice == 4:  # near the smallest and largest normals
        v = rng.choice([least * rng.randrange(1, fmt.frac + 4), fmt.inf - least * rng.randrange(1, 4)]) + rng.randrange(least)
    else:
        v = rng.randrange(fmt.top)
    v = min(v, fmt.top - 1)
    return v + (fmt.top if rng.random() < 0.5 else 0)


def related(fmt, a, rng):
    """A second operand, often within a few exponents of the first."""
    if rng.random() < 0.5 or fmt.mag(a) >= fmt.inf:
        return operands(fmt, rng)
    m = fmt.mag(a) + (rng.randrange(-fmt.frac - 3, fmt.frac + 4) << fmt.frac) + rng.randrange(-3, 4)
    return max(0, min(m, fmt.inf)) + (fmt.top if rng.random() < 0.5 else 0)


def integers(bits, rng):
    c = rng.randrange(4)
    if c == 0:
        return rng.choice([0, 1, 2 ** (bits - 1), 2 ** (bits - 1) - 1, 2 ** bits - 1])
    if c == 1:
        return (2 ** rng.randrange(bits) + rng.randrange(-2, 3)) % 2 ** bits
    return rng.randrange(2 ** bits) >> rng.randrange(bits)


def near_int(fmt, bits, rng):
    """Values around halves, integers and the integer range limits."""
    if rng.random() < 0.3:
        return operands(fmt, rng)
    base = rng.choice([Fraction(rng.randrange(-9, 10), 2), Fraction(rng.randrange(-2 ** 24, 2 ** 24), 4),
                       Fraction(2 ** (bits - 1)), Fraction(-(2 ** (bits - 1))), Fraction(rng.randrange(1, 2 ** 20)) * 2 ** rng.randrange(40)])
    v = fmt.top if base == 0 and rng.random() < 0.5 else 0
    if base:
        v = rounded(fmt, base, {"mode": NEAREST, "ftz": False})[0]
    m = max(0, fmt.mag(v) + rng.randrange(-2, 3))
    return m + (fmt.top if fmt.neg(v) else 0)


def build():
    rng = random.Random(0x58454E4F5241)
    lines = []

    def emit(name, make, compute, count):
        done = 0
        while done < count:
            mode = rng.randrange(4)
            control = {"mode": mode, "daz": rng.random() < 0.2, "ftz": rng.random() < 0.2}
            mxcsr = 0x1F80 | mode << 13 | control["daz"] << 6 | control["ftz"] << 15
            a, b = make()
            out = compute(a, b, control)
            if out is None:
                continue
            lines.append(f"{name} {mxcsr:x} {a:x} {b:x} {out[0]:x} {out[1]:x}\n")
            done += 1

    def direct(name, mode, a, b, out):
        if out is not None:
            lines.append(f"{name} {0x1F80 | mode << 13:x} {a:x} {b:x} {out[0]:x} {out[1]:x}\n")

    for fmt, s in ((S, "ss"), (D, "sd")):
        # Every pair of special operands, for every two-operand rule.
        one, least = fmt.emax << fmt.frac, 1 << fmt.frac
        special = [0, fmt.top, 1, one, fmt.top + one, fmt.inf - 1, fmt.inf, fmt.top + fmt.inf,
                   fmt.inf + fmt.quiet + 5, fmt.top + fmt.inf + 3]
        for a in special:
            for b in special:
                for op in ("add", "sub", "mul", "div", "min", "max"):
                    mode = rng.randrange(4)
                    control = {"mode": mode, "daz": False, "ftz": False}
                    direct(op + s, mode, a, b, binary(op, fmt, a, b, control))
        # Products within a rounding step of the smallest normal: (1 + t)(1 - t)
        # times 2^emin. Tininess is judged after rounding, so some are not tiny.
        root_of_least = fmt.emin // 2
        for shift in range(fmt.frac // 2 - 3, fmt.frac // 2 + 6):
            t = 1 << (fmt.frac - shift)
            a = ((root_of_least + fmt.emax) << fmt.frac) + t
            rest = fmt.emin - root_of_least
            # 1 - t is in the binade below one, with twice the resolution.
            b = ((rest - 1 + fmt.emax) << fmt.frac) + (least - 2 * t)
            for mode in range(4):
                for x, y in ((a, b), (a + fmt.top, b)):
                    control = {"mode": mode, "daz": False, "ftz": False}
                    direct("mul" + s, mode, x, y, binary("mul", fmt, x, y, control))
    # The same neighbourhood for narrowing: 2^-126 (1 - 2^-k).
    for k in range(22, 30):
        b = D.encode(False, Fraction(2) ** S.emin * (1 - Fraction(1, 2 ** k)))
        for mode in range(4):
            for x in (b, b + D.top):
                direct("cvtsd2ss", mode, 0, x, widen(D, S, x, {"mode": mode, "daz": False, "ftz": False}))

    for fmt, s in ((S, "ss"), (D, "sd")):
        def pair(fmt=fmt):
            a = operands(fmt, rng)
            return a, related(fmt, a, rng)
        for op in ("add", "sub", "mul", "div", "min", "max"):
            emit(op + s, pair, lambda a, b, c, op=op, fmt=fmt: binary(op, fmt, a, b, c),
                 260 if op in ("add", "sub", "mul", "div") else 100)
        emit("sqrt" + s, lambda fmt=fmt: (0, operands(fmt, rng)), lambda a, b, c, fmt=fmt: root(fmt, b, c), 200)
        for code in range(8):
            emit(f"cmp{s}{code}", pair, lambda a, b, c, code=code, fmt=fmt: predicate(code, fmt, a, b, c), 40)
        emit("comi" + s, pair, lambda a, b, c, fmt=fmt: comparison(fmt, a, b, True, c), 80)
        emit("ucomi" + s, pair, lambda a, b, c, fmt=fmt: comparison(fmt, a, b, False, c), 80)
        for bits, w in ((32, "d"), (64, "q")):
            for name, truncate in (("cvt", False), ("cvtt", True)):
                emit(f"{name}{s}2si{w}", lambda fmt=fmt, bits=bits: (0, near_int(fmt, bits, rng)),
                     lambda a, b, c, fmt=fmt, bits=bits, t=truncate: to_int(fmt, b, bits, t, c), 110)
            emit(f"cvtsi2{s}{w}", lambda bits=bits: (0, integers(bits, rng)),
                 lambda a, b, c, fmt=fmt, bits=bits: from_int(fmt, b, bits, c), 80)
    emit("cvtss2sd", lambda: (0, operands(S, rng)), lambda a, b, c: widen(S, D, b, c), 120)

    def narrowing():
        if rng.random() < 0.4:
            return 0, operands(D, rng)
        # A binary32 neighbourhood, with bits beyond its precision.
        v = operands(S, rng)
        x = widen(S, D, v, {"mode": NEAREST, "daz": False, "ftz": False})[0]
        return 0, x ^ (rng.choice([0, 1, 1 << 28, (1 << 28) + 1, (1 << 29) - 1]) if D.mag(x) else 0)
    emit("cvtsd2ss", narrowing, lambda a, b, c: widen(D, S, b, c), 260)
    for fmt, s in ((S, "ss"), (D, "sd")):
        for imm in (*range(16), 244):
            emit(f"round{s}{imm}", lambda fmt=fmt: (0, near_int(fmt, fmt.frac + 2, rng)),
                 lambda a, b, c, fmt=fmt, imm=imm: integral(fmt, b, imm, c), 48)
    return "".join(lines)


def build_dots():
    """Every immediate of DPPS and DPPD, some with one exception unmasked."""
    rng = random.Random(0x444F5453)
    lines = []

    def lane(fmt):
        if rng.random() < 0.7:  # near one, so that sums round and cancel
            value = (fmt.emax + rng.randrange(-3, 4)) << fmt.frac
            value += rng.randrange(1 << fmt.frac) if rng.random() < 0.5 else 0
            return value + (fmt.top if rng.random() < 0.5 else 0)
        return operands(fmt, rng)

    def number(fmt, lanes):
        return sum(v << (fmt.bits * i) for i, v in enumerate(lanes))

    for fmt, name in ((S, "dpps"), (D, "dppd")):
        count = 128 // fmt.bits
        for imm in range(256):
            done = 0
            while done < 14:
                mode = rng.randrange(4)
                control = {"mode": mode, "daz": rng.random() < 0.2, "ftz": rng.random() < 0.2}
                masks = 0x3F - (rng.choice([IE, DE, OE, UE, PE]) if done >= 8 else 0)
                control["om"], control["um"] = bool(masks & OE), bool(masks & UE)
                mxcsr = masks << 7 | mode << 13 | control["daz"] << 6 | control["ftz"] << 15
                a = [lane(fmt) for _ in range(count)]
                b = [lane(fmt) for _ in range(count)]
                if rng.random() < 0.15 and done >= 8:  # near the ends of the range
                    scale = rng.choice([1, fmt.emax * 2 - 2])
                    a = [v % (1 << fmt.frac) + (scale << fmt.frac) for v in a]
                if rng.random() < 0.2:  # products that cancel exactly
                    a[1], b[1] = a[0], b[0] ^ fmt.top
                out = dot(fmt, a, b, imm, control, masks)
                if out is None:
                    continue
                result = "-" if out[0] is None else f"{number(fmt, out[0]):x}"
                lines.append(f"{name} {imm:x} {mxcsr:x} {number(fmt, a):x} {number(fmt, b):x} {result} {out[1]:x}\n")
                done += 1
    return "".join(lines)


def build_horizontal():
    """128-bit vectors of HADDPS/PD, HSUBPS/PD and ADDSUBPS/PD: random operands
    and a grid of special values in every pairing position, so that NaN
    propagation, denormal handling and rounding are covered lane by lane."""
    rng = random.Random(0x484F52495A)
    lines = []

    def pack(fmt, lanes):
        return sum(v << (fmt.bits * i) for i, v in enumerate(lanes))

    def emit(name, fmt, control, a, b):
        out = horizontal(name, fmt, a, b, control)
        if out is None:
            return
        result, flags = out
        mxcsr = 0x1F80 | control["mode"] << 13 | control["daz"] << 6 | control["ftz"] << 15
        lines.append(f"{name} {mxcsr:x} {a:x} {b:x} {result:x} {flags:x}\n")

    for fmt, kind in ((S, "ps"), (D, "pd")):
        count = 128 // fmt.bits
        for stem in ("hadd", "hsub", "addsub"):
            name = stem + kind
            for _ in range(160):
                control = {"mode": rng.randrange(4), "daz": rng.random() < 0.2,
                           "ftz": rng.random() < 0.2}
                a = pack(fmt, [operands(fmt, rng) for _ in range(count)])
                b = pack(fmt, [operands(fmt, rng) for _ in range(count)])
                emit(name, fmt, control, a, b)
            # Special values rotating through every lane of both sources, so
            # each pairing position meets each special case.
            one, least = fmt.emax << fmt.frac, 1 << fmt.frac
            special = [0, fmt.top, 1, least - 1, least, one - 1, one,
                       fmt.inf - 1, fmt.inf, fmt.top + fmt.inf,
                       fmt.inf + fmt.quiet + 5, fmt.top + fmt.inf + 3]
            base_a = pack(fmt, [one] * count)
            base_b = pack(fmt, [one] * count)
            for x in special:
                for position in range(count):
                    lanes_a = [one] * count
                    lanes_b = [one] * count
                    lanes_a[position] = x
                    lanes_b[count - 1 - position] = x
                    control = {"mode": 0, "daz": False, "ftz": False}
                    emit(name, fmt, control, pack(fmt, lanes_a), base_b)
                    emit(name, fmt, control, base_a, pack(fmt, lanes_b))
    return "".join(lines)


def main(argv):
    if len(argv) != 2 or argv[1] not in ("generate", "--check"):
        sys.exit("usage: float_reference.py generate|--check")
    for name, text in (("float_vectors.txt", build()), ("dot_vectors.txt", build_dots()),
                       ("fma_vectors.txt", build_fma()),
                       ("horizontal_vectors.txt", build_horizontal())):
        if argv[1] == "generate":
            (HERE / name).write_text(text)
        elif (HERE / name).read_text() != text:
            sys.exit(f"{name} is out of date")
        print(f"{name}: {text.count(chr(10))} vectors")


if __name__ == "__main__":
    main(sys.argv)
