# SPDX-License-Identifier: GPL-3.0-or-later
import argparse
import contextlib
import fcntl
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "compat"))
import runner


class RunnerTests(unittest.TestCase):
    def test_capture_preserves_exit_and_both_ends_of_truncated_streams(self):
        half = runner.OUTPUT_LIMIT // 2
        script = r"""
import sys

half = int(sys.argv[1])
sys.stdout.write("a" * half + "discarded stdout" + "b" * (half - 1) + "\n")
diagnostic = "fixture check failed\n"
sys.stderr.write("c" * half + "discarded stderr" + "d" * (half - len(diagnostic)) + diagnostic)
sys.exit(7)
"""
        result = runner.capture([sys.executable, "-c", script, str(half)], 10, os.environ.copy())

        self.assertEqual((result["status"], result["returncode"]), ("failed", 7))
        self.assertTrue(result["stdout_truncated"])
        self.assertTrue(result["stderr_truncated"])
        self.assertEqual(result["stdout"], "a" * half + "b" * (half - 1) + "\n")
        diagnostic = "fixture check failed\n"
        self.assertEqual(result["stderr"], "c" * half + "d" * (half - len(diagnostic)) + diagnostic)

    def test_capture_preserves_untruncated_streams_exactly(self):
        for size in (runner.OUTPUT_LIMIT - 1, runner.OUTPUT_LIMIT):
            with self.subTest(size=size):
                result = runner.capture(
                    [sys.executable, "-c",
                     "import sys; sys.stdout.write('a'*int(sys.argv[1])); print('last line', file=sys.stderr)",
                     str(size)],
                    10, os.environ.copy(),
                )

                self.assertEqual((result["status"], result["returncode"]), ("passed", 0))
                self.assertFalse(result["stdout_truncated"])
                self.assertFalse(result["stderr_truncated"])
                self.assertEqual(result["stdout"], "a" * size)
                self.assertEqual(result["stderr"], "last line\n")

    def test_deadline_terminates_child(self):
        result = runner.capture([sys.executable, "-c", "import time; time.sleep(30)"], 0.1, os.environ.copy())
        self.assertEqual(result["status"], "timeout")
        self.assertLess(result["elapsed_seconds"], 5)

    def test_deadline_terminates_descendant_retaining_pipes(self):
        descendant = """
import fcntl
import sys
import time
with open(sys.argv[1], "w") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    print("descendant ready", flush=True)
    time.sleep(10)
"""
        parent = """
import subprocess
import sys
subprocess.Popen([sys.executable, "-c", sys.argv[1], sys.argv[2]])
"""
        with tempfile.TemporaryDirectory() as directory:
            lock_path = Path(directory) / "descendant.lock"
            result = runner.capture(
                [sys.executable, "-c", parent, descendant, str(lock_path)],
                2, os.environ.copy(),
            )
            self.assertEqual(result["status"], "timeout")
            self.assertLess(result["elapsed_seconds"], 5)
            self.assertIn("descendant ready", result["stdout"])
            # SIGKILL can be delivered before the descendant releases its file lock.
            deadline = time.monotonic() + 2
            with lock_path.open() as lock:
                while True:
                    try:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        if time.monotonic() >= deadline:
                            self.fail("descendant still holds its lock after the deadline")
                        time.sleep(0.01)

    def test_missing_program_is_not_a_pass(self):
        result = runner.capture(["xenora-nonexistent-compat-executable"], 1, os.environ.copy())
        self.assertEqual(result["status"], "missing-tool")

    def test_invalid_timeouts_are_rejected(self):
        for value in ("0", "-1", "nan", "inf", "3601"):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                runner.timeout_seconds(value)

    def test_compare_identifies_a_regression(self):
        def result(status):
            return {"workloads": [{"id": "a", "stages": [{"name": "fixture-command", "status": status}]}]}
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertNotEqual(runner.compare(result("passed"), result("failed")), 0)
            self.assertEqual(runner.compare(result("failed"), result("passed")), 0)
            with self.assertRaises(ValueError):
                runner.compare(result("passed"), {"workloads": []})

    def test_output_cannot_escape_artifacts(self):
        with self.assertRaises(ValueError):
            runner.output_path(ROOT / "CHANGELOG.md")

    def test_report_reader_rejects_duplicate_workloads(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text('{"schema_version":1,"workloads":[{"id":"x","status":"passed","stages":[{"name":"one","status":"passed"}]},{"id":"x","status":"passed","stages":[{"name":"one","status":"passed"}]}]}')
            with self.assertRaises(ValueError):
                runner.read_result(path)

    def test_compare_rejects_different_stage_scope_for_same_workload(self):
        def result(stage):
            return {"workloads": [{"id": "a", "stages": [{"name": stage, "status": "passed"}]}]}

        with self.assertRaisesRegex(
            ValueError, r"missing: a/host-build; extra: a/fixture-command"
        ):
            runner.compare(result("host-build"), result("fixture-command"))

    def test_output_rejects_existing_and_symlink_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            results_dir = Path(directory).resolve() / "results"
            results_dir.mkdir()
            existing = results_dir / "existing.json"
            existing.write_text("keep")
            dangling = results_dir / "dangling.json"
            dangling.symlink_to(results_dir / "missing.json")
            escape = results_dir / "escape"
            escape.symlink_to(results_dir.parent, target_is_directory=True)
            with patch.object(runner, "RESULTS_DIR", results_dir):
                for path in (existing, dangling):
                    with self.subTest(path=path), self.assertRaises(FileExistsError):
                        runner.output_path(path)
                with self.assertRaises(ValueError):
                    runner.output_path(escape / "result.json")
            self.assertEqual(existing.read_text(), "keep")

    def test_write_result_preserves_file_created_after_output_check(self):
        with tempfile.TemporaryDirectory() as directory:
            results_dir = Path(directory).resolve()
            with patch.object(runner, "RESULTS_DIR", results_dir):
                path = runner.output_path(results_dir / "result.json")
                path.write_text("concurrent writer")
                with self.assertRaises(FileExistsError):
                    runner.write_result(path, {"workloads": []})
            self.assertEqual(path.read_text(), "concurrent writer")
            self.assertEqual(list(results_dir.iterdir()), [path])

    def test_report_reader_rejects_malformed_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            for expected, changes, stage_changes in (
                ("Unsupported result schema", {"schema_version": True}, {}),
                ("Invalid context", {"context": []}, {}),
                ("Invalid commit metadata", {"context": {"commit": {"status": "passed"}}}, {}),
                ("Invalid status", {}, {"status": "made-up"}),
                ("Invalid detail", {}, {"detail": {"message": "bad"}}),
                ("Invalid return code", {}, {"returncode": True}),
                ("Inconsistent stage outcome", {}, {"status": "failed"}),
            ):
                with self.subTest(expected=expected), self.assertRaisesRegex(ValueError, expected):
                    stage = {"name": "fixture-command", "status": "passed", **stage_changes}
                    result = {
                        "schema_version": runner.SCHEMA_VERSION,
                        "workloads": [{"id": "a", "status": "passed", "stages": [stage]}],
                        **changes,
                    }
                    path.write_text(json.dumps(result))
                    runner.read_result(path)

    def report_output(self, result, *options):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text(json.dumps({"schema_version": runner.SCHEMA_VERSION, **result}))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(runner.main(["report", str(path), *options]), 0)
        return output.getvalue()

    def test_markdown_reports_stage_outcomes_and_excludes_captured_output(self):
        commit_hash = "7" * 40
        result = {
            "context": {
                "commit": {"status": "passed", "stdout": commit_hash + "\n"},
                "worktree_status": {"status": "passed", "stdout": " M private-file-marker\n"},
            },
            "workloads": [
                {
                    "id": "built",
                    "status": "timeout",
                    "stages": [
                        {"name": "host-build", "status": "passed", "returncode": 0},
                        {"name": "fixture-command", "status": "timeout", "timeout_seconds": 3},
                    ],
                },
                {
                    "id": "blocked",
                    "status": "failed",
                    "stages": [
                        {
                            "name": "host-build",
                            "status": "failed",
                            "returncode": 7,
                            "stdout": "captured-stdout-marker",
                            "stderr": "captured-stderr-marker",
                            "detail": "stage-detail-marker",
                        },
                        {"name": "fixture-command", "status": "skipped"},
                    ],
                },
            ],
        }

        markdown = self.report_output(result, "--format", "markdown")

        self.assertTrue(markdown.startswith("# Xenora fixture report\n"))
        self.assertIn(f"- Commit: {commit_hash}\n", markdown)
        self.assertIn("- Worktree: dirty\n", markdown)
        for row in (
            ("`built`", "host-build", "passed", "0", "unavailable"),
            ("`built`", "fixture-command", "timeout", "unavailable", "3"),
            ("`blocked`", "host-build", "failed", "7", "unavailable"),
            ("`blocked`", "fixture-command", "skipped", "unavailable", "unavailable"),
        ):
            self.assertIn("| " + " | ".join(row) + " |", markdown)
        self.assertNotIn("marker", markdown)

    def test_markdown_handles_legacy_report_without_context(self):
        result = {
            "workloads": [
                {
                    "id": "legacy",
                    "status": "missing-tool",
                    "stages": [{"name": "fixture-command", "status": "missing-tool"}],
                },
            ],
        }

        markdown = self.report_output(result, "--format", "markdown")

        self.assertIn("- Commit: unavailable\n", markdown)
        self.assertIn("- Worktree: unavailable\n", markdown)
        self.assertIn("| `legacy` | fixture-command | missing-tool | unavailable | unavailable |", markdown)
        self.assertNotIn("host-build", markdown)
        self.assertEqual(
            self.report_output(result),
            "legacy: missing-tool\n  fixture-command: missing-tool\n",
        )

    def test_markdown_blocks_untrusted_workload_and_metadata_markup(self):
        payload = "unsafe|value\n# injection\n<script>injected</script>"
        result = {
            "context": {"commit": {"status": "passed", "stdout": payload}},
            "workloads": [
                {
                    "id": payload,
                    "status": "passed",
                    "stages": [{"name": "fixture-command", "status": "passed", "returncode": 0}],
                },
            ],
        }

        markdown = self.report_output(result, "--format", "markdown")

        self.assertIn("- Commit: unavailable\n", markdown)
        self.assertIn("| unavailable | fixture-command | passed | 0 | unavailable |", markdown)
        self.assertNotIn("unsafe|value", markdown)
        self.assertNotIn("\n# injection", markdown)
        self.assertNotIn("<script>", markdown)

    def test_build_failure_skips_fixture_and_survives_report_round_trip(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture_path = Path(directory) / "fixture-ran"
            workload = {
                "suite": "test",
                "build_argv": [sys.executable, "-c", "raise SystemExit(7)"],
                "argv": [
                    sys.executable, "-c",
                    "from pathlib import Path; import sys; Path(sys.argv[1]).touch()",
                    str(fixture_path),
                ],
            }
            output = runner.run_workload("example", workload, 5, os.environ.copy())
            self.assertFalse(fixture_path.exists())
            self.assertEqual(output["status"], "failed")
            build, fixture = output["stages"]
            self.assertEqual(
                (build["name"], build["status"], build["returncode"]),
                ("host-build", "failed", 7),
            )
            self.assertEqual((fixture["name"], fixture["status"]), ("fixture-command", "skipped"))
            self.assertIsNone(fixture.get("returncode"))
            path = Path(directory) / "result.json"
            result = {"schema_version": runner.SCHEMA_VERSION, "workloads": [output]}
            runner.write_result(path, result)
            self.assertEqual(runner.read_result(path), result)

    def test_successful_build_preserves_fixture_failure(self):
        workload = {
            "suite": "test",
            "build_argv": [sys.executable, "-c", "print('compiled')"],
            "argv": [sys.executable, "-c", "raise SystemExit(9)"],
        }
        output = runner.run_workload("example", workload, 5, os.environ.copy())
        self.assertEqual(output["status"], "failed")
        self.assertEqual(
            [(stage["name"], stage["status"], stage["returncode"]) for stage in output["stages"]],
            [("host-build", "passed", 0), ("fixture-command", "failed", 9)],
        )


if __name__ == "__main__":
    unittest.main()
