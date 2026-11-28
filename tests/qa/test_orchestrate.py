# SPDX-License-Identifier: GPL-3.0-or-later
import json
import sys
import tempfile
import unittest
from pathlib import Path

from qa.orchestrate import (
    SuiteSpec,
    dumps_report,
    failure_log_excerpt,
    run_suites,
    separate_cpu_gpu,
)


class Orchestrate(unittest.TestCase):
    def run_unittest(self, source):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "test_fixture.py").write_text(source, encoding="utf-8")
            return run_suites(
                [SuiteSpec("unittest", (sys.executable, "-m", "unittest", "test_fixture"))],
                cwd=tmp,
            )

    def test_unittest_records_counts_without_changing_suite_meaning(self):
        report = self.run_unittest("""
import unittest

class Fixture(unittest.TestCase):
    def test_pass(self):
        print("standalone output")

    @unittest.skip("fixture skip")
    def test_skip(self):
        pass
""")
        payload = json.loads(dumps_report(report))
        self.assertEqual(payload["counts"], {
            "passed": 1, "failed": 0, "ignored": 0, "unexecuted": 0,
        })
        self.assertEqual(report.overall_returncode(), 0)
        suite = payload["suites"][0]
        self.assertEqual(suite["tests_total"], 2)
        self.assertEqual(suite["tests_skipped"], 1)
        self.assertIn("standalone output", suite["stdout"])
        self.assertIn("OK (skipped=1)", suite["stderr"])

    def test_failed_unittest_reports_counts_and_preserves_exit_code(self):
        report = self.run_unittest("""
import unittest

class Fixture(unittest.TestCase):
    def test_fail(self):
        self.fail("fixture failure")

    def test_pass(self):
        pass

    @unittest.skip("fixture skip")
    def test_skip(self):
        pass
""")
        suite = report.suites[0]
        self.assertEqual((suite.tests_total, suite.tests_skipped), (3, 1))
        self.assertEqual(suite.status, "failed")
        self.assertEqual(suite.returncode, 1)
        self.assertEqual(report.overall_returncode(), 1)
        self.assertIn("FAILED (failures=1, skipped=1)", suite.stderr)

    def test_unittest_without_skips_reports_zero_skips(self):
        report = self.run_unittest("""
import unittest

class Fixture(unittest.TestCase):
    def test_pass(self):
        pass
""")
        suite = report.suites[0]
        self.assertEqual((suite.tests_total, suite.tests_skipped), (1, 0))

    def test_counts_require_unittest_stderr_terminal_summary(self):
        summary = "-" * 70 + "\nRan 1 test in 0.001s\n\nOK\n"
        commands = [
            (sys.executable, "-m", "unittest", "--help"),
            (sys.executable, "-c", "import sys; sys.stderr.write(sys.argv[1])", summary),
        ]
        report = run_suites([SuiteSpec("unknown", command) for command in commands])
        self.assertEqual(report.passed, 2)
        for suite in report.suites:
            self.assertIsNone(suite.tests_total)
            self.assertIsNone(suite.tests_skipped)

        trailing = self.run_unittest("""
import atexit
import sys
import unittest

atexit.register(lambda: sys.stderr.write("trailing output\\n"))

class Fixture(unittest.TestCase):
    def test_pass(self):
        pass
""").suites[0]
        self.assertTrue(trailing.stderr.endswith("trailing output\n"))
        self.assertIsNone(trailing.tests_total)
        self.assertIsNone(trailing.tests_skipped)

    def test_records_pass_and_fail_with_durations(self):
        report = run_suites([
            SuiteSpec("ok", ("python3", "-c", "print(1)"), "cpu"),
            SuiteSpec("bad", ("python3", "-c", "import sys; sys.exit(2)"), "cpu"),
        ])
        self.assertEqual(report.passed, 1)
        self.assertEqual(report.failed, 1)
        self.assertGreaterEqual(report.suites[0].duration_seconds, 0.0)
        self.assertEqual(report.overall_returncode(), 2)
        payload = json.loads(dumps_report(report))
        self.assertEqual(payload["counts"]["failed"], 1)

    def test_stop_on_fail_marks_unexecuted(self):
        report = run_suites(
            [
                SuiteSpec("bad", ("python3", "-c", "import sys; sys.exit(5)"), "cpu"),
                SuiteSpec("later", ("python3", "-c", "print('nope')"), "gpu"),
            ],
            stop_on_fail=True,
        )
        self.assertEqual(report.suites[1].status, "unexecuted")
        self.assertEqual(report.unexecuted, 1)

    def test_ignored_and_cpu_gpu_split(self):
        report = run_suites([
            SuiteSpec("skip", ("true",), "ignored"),
            SuiteSpec("cpu", ("python3", "-c", "print('c')"), "cpu"),
            SuiteSpec("gpu", ("python3", "-c", "print('g')"), "gpu"),
        ])
        self.assertEqual(report.ignored, 1)
        buckets = separate_cpu_gpu(report)
        self.assertEqual(len(buckets["cpu"]), 1)
        self.assertEqual(len(buckets["gpu"]), 1)

    def test_failure_excerpt_includes_reproduction(self):
        report = run_suites([
            SuiteSpec("bad", ("python3", "-c", "import sys; sys.stderr.write('boom\\n'); sys.exit(1)"), "cpu"),
        ])
        text = failure_log_excerpt(report)
        self.assertIn("reproduce:", text)
        self.assertIn("boom", text)


if __name__ == "__main__":
    unittest.main()
