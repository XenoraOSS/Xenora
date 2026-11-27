# SPDX-License-Identifier: GPL-3.0-or-later
import unittest

from qa.bench_regress import (
    BenchIdentity,
    BenchRecord,
    IncompatibleComparison,
    compare_records,
)


def _rec(samples, **kwargs):
    ident = BenchIdentity(
        metric=kwargs.get("metric", "latency"),
        unit=kwargs.get("unit", "ns"),
        workload=kwargs.get("workload", "add"),
        host=kwargs.get("host", "m5"),
        toolchain=kwargs.get("toolchain", "rustc"),
    )
    return BenchRecord(identity=ident, samples=tuple(samples), revision=kwargs.get("rev", "a"))


class BenchRegress(unittest.TestCase):
    def test_rejects_incompatible_identity(self):
        a = _rec([10, 10, 10], workload="add")
        b = _rec([10, 10, 10], workload="mul")
        with self.assertRaises(IncompatibleComparison):
            compare_records(a, b)

    def test_noise_not_regression(self):
        base = _rec([100, 100, 100, 100])
        cur = _rec([101, 101, 102, 100], rev="b")
        payload = compare_records(base, cur, threshold=1.10, noise_floor_ratio=0.05)
        self.assertFalse(payload["regressing"])
        self.assertFalse(payload["meaningful"])

    def test_clear_regression(self):
        base = _rec([100, 100, 100, 100])
        cur = _rec([130, 131, 129, 130], rev="b")
        payload = compare_records(base, cur, threshold=1.10)
        self.assertTrue(payload["regressing"])
        self.assertTrue(payload["meaningful"])

    def test_requires_samples(self):
        with self.assertRaises(ValueError):
            compare_records(_rec([1]), _rec([1, 2, 3]), min_samples=3)

    def test_rejects_non_finite_record_samples(self):
        base = _rec([100, 100, 100])
        cur = _rec([100, float("nan"), 100], rev="b")
        with self.assertRaises(ValueError):
            compare_records(base, cur)


if __name__ == "__main__":
    unittest.main()
