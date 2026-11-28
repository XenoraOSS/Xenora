# SPDX-License-Identifier: GPL-3.0-or-later
"""Command-level previews tests. No signing or notarization service runs."""

import json
from pathlib import Path
import plistlib
import subprocess
import unittest

from preview.signing import notarize, sign_bundle, verify_distribution, verify_local


APP = Path("/staged/Xenora.app")
WORKER = APP / "Contents" / "MacOS" / "xenora-cli"
SESSION = APP / "Contents" / "MacOS" / "xenora-pe-session"
ARCHIVE = Path("/staged/Xenora-notary.zip")
SUBMISSION = "2efe2717-52ef-43a5-96dc-0797e4ca1041"
ENTITLEMENTS = {"com.apple.security.cs.allow-jit": True}
DEVELOPER_DETAILS = "CodeDirectory v=20500 flags=0x10000(runtime)\nTimestamp=Nov 25, 2026\n"


class Recorder:
    def __init__(self):
        self.calls = []
        self.embedded_entitlements = []
        self.details = DEVELOPER_DETAILS
        self.entitlements = plistlib.dumps(ENTITLEMENTS).decode()
        self.response = json.dumps({"status": "Accepted", "id": SUBMISSION})
        self.submission_error = None

    def run(self, args, **kwargs):
        self.calls.append((args, kwargs))
        if "--sign" in args and "--entitlements" in args:
            path = args[args.index("--entitlements") + 1]
            self.embedded_entitlements.append(plistlib.loads(path.read_bytes()))
        if args[:3] == ["codesign", "--display", "--verbose=4"]:
            return self.details
        if args[:3] == ["codesign", "--display", "--entitlements"]:
            return self.entitlements
        if args[:3] == ["xcrun", "notarytool", "submit"]:
            if self.submission_error is not None:
                raise self.submission_error
            return self.response
        return ""


class SigningTests(unittest.TestCase):
    def setUp(self):
        self.commands = Recorder()

    def test_local_signs_the_helpers_before_the_app(self):
        self.commands.details = "CodeDirectory v=20400 flags=0x2(adhoc)\nSignature=adhoc\n"
        sign_bundle(APP, identity=None, run=self.commands.run)
        args = [args for args, _ in self.commands.calls]
        self.assertEqual(args[:3], [
            ["codesign", "--force", "--sign", "-", WORKER],
            ["codesign", "--force", "--sign", "-", SESSION],
            ["codesign", "--force", "--sign", "-", APP],
        ])
        self.assertIn(["codesign", "--verify", "--strict", WORKER], args)
        self.assertIn(["codesign", "--verify", "--strict", SESSION], args)
        self.assertIn(["codesign", "--verify", "--strict", APP], args)

    def test_local_verification_requires_adhoc_signatures(self):
        with self.assertRaisesRegex(RuntimeError, "lacks a local ad-hoc signature"):
            verify_local(APP, run=self.commands.run)

    def test_developer_id_signs_all_jit_processes_and_checks_signatures(self):
        identity = "Developer ID Application: Xenora (TEAM)"
        sign_bundle(APP, identity=identity, run=self.commands.run)
        signing = self.commands.calls[:3]
        self.assertEqual([args[-1] for args, _ in signing], [WORKER, SESSION, APP])
        self.assertEqual(self.commands.embedded_entitlements, [ENTITLEMENTS] * 3)
        for args, kwargs in signing:
            self.assertEqual(args[:8], [
                "codesign", "--force", "--sign", identity, "--options", "runtime",
                "--timestamp", "--entitlements",
            ])
            self.assertEqual(kwargs, {"sensitive": True})
        verification = [
            args for args, _ in self.commands.calls if args[:3] == ["codesign", "--verify", "--strict"]
        ]
        self.assertEqual([args[-1] for args in verification], [WORKER, SESSION, APP])
        for args in verification:
            requirement = args[3]
            self.assertIn("anchor apple generic", requirement)
            self.assertIn("1.2.840.113635.100.6.1.13", requirement)
        for args, kwargs in self.commands.calls:
            if args[:3] == ["codesign", "--display", "--verbose=4"]:
                self.assertEqual(kwargs, {"capture": True, "stderr": subprocess.STDOUT})

    def test_notarize_submits_then_staples_and_validates_the_app(self):
        result = notarize(APP, ARCHIVE, profile="local-profile", run=self.commands.run)
        self.assertEqual(result, SUBMISSION)
        self.assertEqual(self.commands.calls[-3:], [
            ([
                "xcrun", "notarytool", "submit", ARCHIVE, "--wait",
                "--output-format", "json", "--keychain-profile", "local-profile",
            ], {"capture": True, "sensitive": True}),
            (["xcrun", "stapler", "staple", APP], {}),
            (["xcrun", "stapler", "validate", APP], {}),
        ])

    def test_notarize_requires_verified_hardening_timestamp_and_jit_entitlements(self):
        for details, entitlements, expected in [
            ("flags=0x2\nTimestamp=Nov 25, 2026\n", ENTITLEMENTS, "Hardened Runtime"),
            ("flags=0x10000(runtime)\nSigned Time=Nov 25, 2026\n", ENTITLEMENTS, "secure timestamp"),
            (DEVELOPER_DETAILS, {"com.apple.security.get-task-allow": True}, "unexpected entitlements"),
            (DEVELOPER_DETAILS, {"com.apple.security.cs.allow-jit": 1}, "unexpected entitlements"),
        ]:
            with self.subTest(expected=expected):
                commands = Recorder()
                commands.details = details
                commands.entitlements = plistlib.dumps(entitlements).decode()
                with self.assertRaisesRegex(RuntimeError, expected):
                    notarize(APP, ARCHIVE, profile="local-profile", run=commands.run)
                self.assertTrue(all(args[0] == "codesign" for args, _ in commands.calls))

    def test_rejected_or_malformed_notary_response_is_never_stapled(self):
        for response, expected in [
            ("unexpected raw output", "invalid JSON"),
            ('{"status": "Invalid"}', "did not accept"),
            (json.dumps({"status": "Invalid", "id": SUBMISSION}), SUBMISSION),
            ('{"status": "Accepted"}', "no valid submission ID"),
            ('{"status": "Accepted", "id": "wrong"}', "no valid submission ID"),
        ]:
            with self.subTest(expected=expected):
                commands = Recorder()
                commands.response = response
                with self.assertRaisesRegex(RuntimeError, expected):
                    notarize(APP, ARCHIVE, profile="local-profile", run=commands.run)
                self.assertFalse(any(args[:2] == ["xcrun", "stapler"] for args, _ in commands.calls))

    def test_notary_error_does_not_expose_profile_or_child_output(self):
        self.commands.submission_error = subprocess.CalledProcessError(
            1, ["notarytool", "sensitive-profile"], output="sensitive output",
        )
        with self.assertRaisesRegex(RuntimeError, "submission failed") as error:
            notarize(APP, ARCHIVE, profile="sensitive-profile", run=self.commands.run)
        self.assertNotIn("sensitive", str(error.exception))
        self.assertTrue(error.exception.__suppress_context__)

    def test_final_verification_validates_ticket_and_gatekeeper(self):
        verify_distribution(APP, run=self.commands.run)
        self.assertEqual(self.commands.calls[-2:], [
            (["xcrun", "stapler", "validate", APP], {}),
            (["syspolicy_check", "distribution", APP], {}),
        ])
