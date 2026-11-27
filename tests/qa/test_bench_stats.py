# SPDX-License-Identifier: GPL-3.0-or-later
import json
import unittest

from qa.bench_stats import compare_medians, export_json, human_report, summarize


class BenchStats(unittest.TestCase):
    def test_percentiles_and_export(self):
        samples = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        stats = summarize(samples)
        self.assertEqual(stats.count, 10)
        self.assertEqual(stats.median, 5.5)
        self.assertGreaterEqual(stats.p95, stats.median)
        self.assertGreaterEqual(stats.p99, stats.p95)
        payload = json.loads(export_json(stats, host="m5", toolchain="rustc", revision="abc", metric="gpu"))
        self.assertEqual(payload["host"], "m5")
        self.assertIn("median", human_report(stats, metric="gpu"))

    def test_regression_threshold(self):
        ok = compare_medians([10, 10, 10], [10.5, 10.4, 10.6], threshold=1.10)
        self.assertFalse(ok.regressing)
        bad = compare_medians([10, 10, 10], [12, 12, 12], threshold=1.10)
        self.assertTrue(bad.regressing)
        self.assertGreater(bad.ratio, 1.10)

    def test_rejects_non_finite_samples(self):
        for value in (float("nan"), float("inf"), float("-inf")):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    summarize([1.0, value, 2.0])
                with self.assertRaises(ValueError):
                    compare_medians([1.0, 1.0, 1.0], [1.0, value, 1.0])


if __name__ == "__main__":
    unittest.main()
