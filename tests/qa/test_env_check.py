# SPDX-License-Identifier: GPL-3.0-or-later
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

from qa.env_check import (
    check_branch_sync,
    measure_branch_sync,
    check_git_worktree_clean,
    check_llvm_tool,
    check_rosetta_translated,
    check_rustc_host,
    check_swift_version,
    check_uname_machine,
    format_report,
    summarize,
)


class EnvCheck(unittest.TestCase):
    def test_positive_and_negative_host_checks(self):
        self.assertTrue(check_uname_machine("arm64").ok)
        self.assertFalse(check_uname_machine("x86_64").ok)
        self.assertTrue(check_rosetta_translated("0").ok)
        self.assertFalse(check_rosetta_translated("1").ok)
        rust = check_rustc_host("host: aarch64-apple-darwin\n")
        self.assertTrue(rust.ok)
        self.assertFalse(check_rustc_host("host: x86_64-apple-darwin\n").ok)

    def test_toolchain_worktree_and_report(self):
        self.assertTrue(check_swift_version("Swift version 6.4\n").ok)
        self.assertTrue(check_llvm_tool("/usr/bin/clang", "Apple clang version 17").ok)
        self.assertFalse(check_llvm_tool("", "").ok)
        self.assertTrue(check_git_worktree_clean("").ok)
        self.assertFalse(check_git_worktree_clean(" M file").ok)
        self.assertTrue(check_branch_sync(ahead=0, behind=0).ok)
        self.assertFalse(check_branch_sync(ahead=2, behind=1).ok)
        report = summarize([
            check_uname_machine("arm64"),
            check_branch_sync(ahead=1, behind=0),
        ])
        self.assertFalse(report.ok)
        self.assertIn("FAIL", format_report(report))

    def test_measured_branch_sync_uses_git_counts(self):
        with patch("qa.env_check.subprocess.check_output", return_value="2\t1\n") as run:
            finding = measure_branch_sync(Path("/repo"))
        self.assertFalse(finding.ok)
        self.assertIn("ahead=2", finding.detail)
        self.assertIn("behind=1", finding.detail)
        self.assertEqual(run.call_args.args[0], ["git", "rev-list", "--left-right", "--count", "HEAD...@{upstream}"])

    def test_missing_upstream_is_not_reported_as_synchronized(self):
        with patch(
            "qa.env_check.subprocess.check_output",
            side_effect=subprocess.CalledProcessError(128, ["git"]),
        ):
            finding = measure_branch_sync(Path("/repo"))
        self.assertFalse(finding.ok)
        self.assertIn("unavailable", finding.detail)


if __name__ == "__main__":
    unittest.main()
