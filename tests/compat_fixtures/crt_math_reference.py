#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Reference values for the host math.h functions of #468, computed with mpmath
# at 200 bits and rounded once to the nearest double or float. It prints the
# HOST64 and HOST32 tables of crates/loader/tests/runtime_services.rs; the same
# values are in tests/compat_fixtures/crt_math.c. Needs `pip install mpmath`
# (1.4.1 was used). Xenora's code plays no part in these values.
import struct, mpmath as M
M.mp.prec = 200
def b64(x): return struct.unpack('<Q', struct.pack('<d', x))[0]
def b32(x): return struct.unpack('<I', struct.pack('<f', x))[0]
from fractions import Fraction
def r64(v):
    # float(Fraction) rounds correctly, subnormals included.
    m, e = M.frexp(v)
    return float(Fraction(int(M.ldexp(m, 200))) * Fraction(2) ** (e - 200))
def r32(v):
    with M.workprec(24): return float(+v)
F = {
 'acos': M.acos, 'asin': M.asin, 'atan': M.atan, 'cbrt': lambda x: M.sign(x) * M.cbrt(abs(x)), 'cos': M.cos, 'cosh': M.cosh,
 'erf': M.erf, 'exp': M.exp, 'exp2': lambda x: M.mpf(2)**x, 'expm1': M.expm1, 'log': M.log,
 'log10': M.log10, 'log1p': M.log1p, 'log2': lambda x: M.log(x, 2), 'sin': M.sin, 'sinh': M.sinh,
 'tan': M.tan, 'tanh': M.tanh, 'tgamma': M.gamma, 'atan2': M.atan2, 'hypot': M.hypot, 'pow': M.power,
}
D = {
 'acos': [0.3, -0.7, 0.99], 'asin': [0.3, -0.5, 1e-10], 'atan': [0.5, -3.0, 1e6],
 'cbrt': [2.0, -27.5, 1e-300], 'cos': [1.0, 100.0, 1e22], 'cosh': [0.5, -3.0, 700.0],
 'erf': [0.1, -1.5, 3.0], 'exp': [1.0, -10.0, 700.0], 'exp2': [0.5, -1070.5, 10.3],
 'expm1': [1e-10, 0.5, -2.0], 'log': [2.0, 1e-300, 0.999], 'log10': [2.0, 7e-5, 1e22],
 'log1p': [1e-15, 0.5, -0.5], 'log2': [3.0, 1e100, 0.1], 'sin': [0.5, 3.141592653589793, 1e22],
 'sinh': [1e-5, 2.0, -700.0], 'tan': [1.0, 1.5707963267948966, -0.5], 'tanh': [0.5, -2.0, 1e-8],
 'tgamma': [0.5, 5.5, -2.5, 170.5],
 'atan2': [(1.0, 2.0), (-1.0, -2.0), (0.5, -0.0001)], 'hypot': [(3.0, 4.0), (1e300, 1e300), (1e-310, 2e-310)],
 'pow': [(2.0, 0.5), (1.5, 100.0), (0.9, -1000.0)],
}
FF = {'cosf': 'cos', 'expf': 'exp', 'logf': 'log', 'sinf': 'sin', 'tanf': 'tan', 'atan2f': 'atan2', 'powf': 'pow'}
DF = {'cosf': [0.5, 100.0], 'expf': [1.0, -20.0], 'logf': [2.0, 1e-30], 'sinf': [0.5, 1e10], 'tanf': [1.0, -0.25],
      'atan2f': [(1.0, 2.0), (-3.0, 0.5)], 'powf': [(2.0, 0.5), (1.1, 50.0)]}
print("const HOST64: &[(&str, u64, u64, u64)] = &[")
for n, xs in D.items():
    for x in xs:
        a, y = (x if isinstance(x, tuple) else (x, None))
        v = F[n](M.mpf(a)) if y is None else F[n](M.mpf(a), M.mpf(y))
        print(f"    (\"{n}\", 0x{b64(a):016X}, 0x{b64(y if y is not None else 0.0):016X}, 0x{b64(r64(v)):016X}), // {n}({a}{'' if y is None else ', '+repr(y)})")
print("];")
print("const HOST32: &[(&str, u32, u32, u32)] = &[")
for n, xs in DF.items():
    for x in xs:
        a, y = (x if isinstance(x, tuple) else (x, None))
        a = r32(M.mpf(a)); y = None if y is None else r32(M.mpf(y))
        v = F[FF[n]](M.mpf(a)) if y is None else F[FF[n]](M.mpf(a), M.mpf(y))
        print(f"    (\"{n}\", 0x{b32(a):08X}, 0x{b32(y if y is not None else 0.0):08X}, 0x{b32(r32(v)):08X}), // {n}({a!r}f{'' if y is None else ', '+repr(y)+'f'})")
print("];")
