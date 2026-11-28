# SPDX-License-Identifier: GPL-3.0-or-later
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import qa.__main__ as cli
from qa.orchestrate import SuiteSpec
from qa.verify import (
    Evidence,
    assess_evidence,
    dumps_evidence,
    evidence_is_stale,
    fingerprint_sources,
    load_evidence,
    run_profile,
)

ROOT = Path(__file__).resolve().parents[2]


class VerifyPipeline(unittest.TestCase):
    def init_git(self, root):
        subprocess.run(["git", "init", "--quiet", root], check=True)

    def test_fingerprint_tracks_checkout_inputs_and_honors_git_ignores(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            self.init_git(root)
            (root / ".gitignore").write_text("*.bin\n/artifacts/\n", encoding="utf-8")
            tracked = root / "fixture with spaces\nand newline.bin"
            tracked.write_bytes(b"original")
            subprocess.run(["git", "add", "-f", "--", tracked.name], cwd=root, check=True)
            baseline = fingerprint_sources(root)

            ignored = root / "artifacts"
            ignored.mkdir()
            (ignored / "report.py").write_bytes(b"local output")
            self.assertEqual(fingerprint_sources(root), baseline)

            header = root / "new header.h"
            header.write_bytes(b"new checkout input")
            self.assertNotEqual(fingerprint_sources(root), baseline)
            header.unlink()
            self.assertEqual(fingerprint_sources(root), baseline)

            tracked.write_bytes(b"modified")
            self.assertNotEqual(fingerprint_sources(root), baseline)
            tracked.unlink()
            self.assertNotEqual(fingerprint_sources(root), baseline)

    def test_fingerprint_detects_symlink_retargeted_between_equal_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            self.init_git(root)
            for name in ("first", "second"):
                (root / name).write_bytes(b"same bytes")
            link = root / "linked-input"
            link.symlink_to("first")
            baseline = fingerprint_sources(root)
            link.unlink()
            link.symlink_to("second")
            self.assertNotEqual(fingerprint_sources(root), baseline)

    def test_mid_run_edit_invalidates_evidence_even_after_revert(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            self.init_git(root)
            fixture = root / "guest.bin"
            fixture.write_bytes(b"original")

            for code in (0, 9):
                with self.subTest(suite_exit=code):
                    evidence = run_profile(
                        "quick",
                        root=root,
                        revision="testrev",
                        override_specs=[
                            SuiteSpec("edit", (
                                sys.executable, "-c",
                                "from pathlib import Path; import sys; "
                                "Path('guest.bin').write_bytes(b'changed'); sys.exit(int(sys.argv[1]))",
                                str(code),
                            )),
                        ],
                    )
                    self.assertEqual(evidence.overall_returncode, code)
                    self.assertEqual(evidence.suites[0]["returncode"], code)
                    loaded = load_evidence(dumps_evidence(evidence))
                    self.assertTrue(evidence_is_stale(loaded, root))
                    fixture.write_bytes(b"original")
                    self.assertTrue(evidence_is_stale(loaded, root))
                    assessment = assess_evidence(
                        loaded, root, requested_profile="quick", actual_revision="testrev"
                    )
                    self.assertFalse(assessment["accepted"])

    def test_real_unittest_profile_slice(self):
        # Real Xenora command, not a mock: run a tiny existing unittest module.
        evidence = run_profile(
            "custom",
            root=ROOT,
            revision="testrev",
            override_specs=[
                SuiteSpec(
                    "env-check-tests",
                    ("python3", "-m", "unittest", "tests.qa.test_env_check"),
                    "cpu",
                ),
                SuiteSpec(
                    "deliberate-fail",
                    ("python3", "-c", "import sys; sys.exit(9)"),
                    "cpu",
                ),
                SuiteSpec(
                    "should-skip",
                    ("python3", "-c", "print('nope')"),
                    "cpu",
                ),
            ],
            stop_on_fail=True,
        )
        self.assertEqual(evidence.suites[0]["status"], "passed")
        self.assertEqual(evidence.suites[1]["status"], "failed")
        self.assertEqual(evidence.suites[1]["returncode"], 9)
        self.assertEqual(evidence.suites[2]["status"], "unexecuted")
        self.assertEqual(evidence.overall_returncode, 9)
        self.assertEqual(evidence.skipped["unexecuted"], 1)
        text = dumps_evidence(evidence)
        loaded = load_evidence(text)
        self.assertEqual(loaded.revision, "testrev")

    def test_stale_evidence_when_sources_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.init_git(root)
            f = root / "scripts" / "a.py"
            f.parent.mkdir(parents=True)
            f.write_text("print(1)\n", encoding="utf-8")
            evidence = run_profile(
                "custom",
                root=root,
                revision="r1",
                override_specs=[SuiteSpec("ok", ("python3", "-c", "print(1)"), "cpu")],
            )
            self.assertFalse(evidence_is_stale(evidence, root))
            f.write_text("print(2)\n", encoding="utf-8")
            self.assertTrue(evidence_is_stale(evidence, root))
            self.assertNotEqual(evidence.source_fingerprint, fingerprint_sources(root))

    def test_profile_suites_run_in_the_selected_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            self.init_git(root)
            evidence = run_profile(
                "custom",
                root=root,
                revision="r1",
                override_specs=[
                    SuiteSpec(
                        "cwd",
                        (
                            "python3",
                            "-c",
                            "from pathlib import Path; import sys; sys.exit(0 if Path.cwd().resolve() == Path(sys.argv[1]).resolve() else 3)",
                            str(root),
                        ),
                        "cpu",
                    )
                ],
            )
        self.assertEqual(evidence.overall_returncode, 0, evidence.suites[0]["stderr"])

    def test_assessment_separates_staleness_metadata_and_suite_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.init_git(root)
            source = root / "scripts" / "a.py"
            source.parent.mkdir(parents=True)
            source.write_text("print(1)\n", encoding="utf-8")
            evidence = Evidence(
                profile="quick",
                revision="rev1",
                started_at=0.0,
                finished_at=1.0,
                source_fingerprint=fingerprint_sources(root),
                overall_returncode=9,
            )

            assessment = assess_evidence(
                evidence, root, requested_profile="quick", actual_revision="rev1"
            )
            self.assertFalse(assessment["stale"])
            self.assertTrue(assessment["revision_matches"])
            self.assertTrue(assessment["profile_matches"])
            self.assertFalse(assessment["suites_passed"])
            self.assertFalse(assessment["accepted"])

            mismatched = assess_evidence(
                evidence, root, requested_profile="cpu", actual_revision="other"
            )
            self.assertFalse(mismatched["stale"])
            self.assertFalse(mismatched["revision_matches"])
            self.assertFalse(mismatched["profile_matches"])
            self.assertFalse(mismatched["accepted"])

            evidence.overall_returncode = 0
            source.write_text("print(2)\n", encoding="utf-8")
            stale = assess_evidence(
                evidence, root, requested_profile="quick", actual_revision="rev1"
            )
            self.assertTrue(stale["stale"])
            self.assertTrue(stale["suites_passed"])
            self.assertFalse(stale["accepted"])

    def test_cli_rejects_a_revision_other_than_the_checkout(self):
        with (
            patch("qa.__main__.current_revision", return_value="actualrev"),
            patch("qa.__main__.run_profile") as run_profile_mock,
            patch("sys.stderr", new_callable=io.StringIO) as stderr,
        ):
            rc = cli.main(["verify", "quick", "--revision", "otherrev"])
        self.assertEqual(rc, 2)
        self.assertIn("does not match", stderr.getvalue())
        run_profile_mock.assert_not_called()


if __name__ == "__main__":
    unittest.main()
