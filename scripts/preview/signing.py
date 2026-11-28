# SPDX-License-Identifier: GPL-3.0-or-later
"""Sign Xenora's executables and verify Developer ID distributions on macOS."""

from __future__ import annotations

import json
from pathlib import Path
import plistlib
import re
import subprocess
from tempfile import TemporaryDirectory
from uuid import UUID
from xml.parsers.expat import ExpatError

from .package import HELPERS


# Apple Developer ID Application requirement, from TN3127.
_DEVELOPER_ID = (
    "anchor apple generic"
    " and certificate 1[field.1.2.840.113635.100.6.2.6] exists"
    " and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
)
_ALLOW_JIT = "com.apple.security.cs.allow-jit"
_JIT_ENTITLEMENTS = {_ALLOW_JIT: True}


def _signing_targets(app: Path) -> tuple[Path, ...]:
    return (*(app / "Contents" / "MacOS" / name for name in HELPERS), app)


def _signature_details(target: Path, *, run) -> str:
    return run(
        ["codesign", "--display", "--verbose=4", target],
        capture=True,
        stderr=subprocess.STDOUT,
    )


def verify_local(app: Path, *, run) -> None:
    """Verify every ad-hoc signature on a local preview."""
    for target in _signing_targets(app):
        run(["codesign", "--verify", "--strict", target])
        if "Signature=adhoc" not in _signature_details(target, run=run).splitlines():
            raise RuntimeError(f"{target.name} lacks a local ad-hoc signature")


def _verify_developer_id(app: Path, *, run) -> None:
    for target in _signing_targets(app):
        run(["codesign", "--verify", "--strict", f"-R={_DEVELOPER_ID}", target])
        details = _signature_details(target, run=run)
        flags = re.search(r"\bflags=0x([0-9a-fA-F]+)\b", details)
        if flags is None or not int(flags[1], 16) & 0x10000:
            raise RuntimeError(f"{target.name} signature lacks Hardened Runtime")
        if not re.search(r"^Timestamp=\S", details, re.MULTILINE):
            raise RuntimeError(f"{target.name} signature lacks a secure timestamp")

        entitlements = run(
            ["codesign", "--display", "--entitlements", "-", "--xml", target],
            capture=True,
        )
        try:
            actual = plistlib.loads(entitlements.encode("utf-8"))
        except (ValueError, plistlib.InvalidFileException, ExpatError):
            raise RuntimeError(f"{target.name} signature lacks valid entitlements") from None
        if not isinstance(actual, dict) or actual.keys() != _JIT_ENTITLEMENTS.keys() or actual[_ALLOW_JIT] is not True:
            raise RuntimeError(f"{target.name} signature has unexpected entitlements")


def sign_bundle(app: Path, *, identity: str | None, run) -> None:
    """Sign the helpers before sealing the app. None selects local ad-hoc signing."""
    if identity is None:
        for target in _signing_targets(app):
            run(["codesign", "--force", "--sign", "-", target])
        verify_local(app, run=run)
        return

    if not identity.strip() or identity.startswith("-"):
        raise ValueError("Provide a Developer ID Application identity")

    # The app runs original fixtures through the Rust FFI. The helpers run
    # guest programs. All invoke the native translator, which uses MAP_JIT.
    with TemporaryDirectory(prefix="xenora-sign-") as directory:
        entitlements = Path(directory) / "jit.plist"
        entitlements.write_bytes(plistlib.dumps(_JIT_ENTITLEMENTS))
        for target in _signing_targets(app):
            run([
                "codesign", "--force", "--sign", identity, "--options", "runtime",
                "--timestamp", "--entitlements", entitlements, target,
            ], sensitive=True)
    _verify_developer_id(app, run=run)


def notarize(app: Path, submission_zip: Path, *, profile: str, run) -> str:
    """Submit the signed archive, then staple its accepted ticket onto the app."""
    if not profile.strip() or profile.startswith("-"):
        raise ValueError("Provide an existing notarytool keychain profile")
    _verify_developer_id(app, run=run)
    try:
        response = run(
            [
                "xcrun", "notarytool", "submit", submission_zip, "--wait",
                "--output-format", "json", "--keychain-profile", profile,
            ],
            capture=True,
            sensitive=True,
        )
    except (OSError, RuntimeError, subprocess.SubprocessError):
        raise RuntimeError(
            "Notarization submission failed. Check the network and configured keychain profile."
        ) from None

    try:
        result = json.loads(response)
    except (TypeError, ValueError):
        raise RuntimeError("Notarization returned invalid JSON") from None
    if not isinstance(result, dict):
        raise RuntimeError("Notarization returned no submission record")
    submission_id = result.get("id")
    try:
        if not isinstance(submission_id, str) or str(UUID(submission_id)) != submission_id.lower():
            raise ValueError()
    except ValueError:
        submission_id = None
    if result.get("status") != "Accepted":
        diagnostic = f" (submission {submission_id})" if submission_id else ""
        raise RuntimeError(f"Apple did not accept the notarization submission{diagnostic}")
    if submission_id is None:
        raise RuntimeError("Notarization returned no valid submission ID")

    run(["xcrun", "stapler", "staple", app])
    run(["xcrun", "stapler", "validate", app])
    return submission_id


def verify_distribution(app: Path, *, run) -> None:
    """Verify an app from the final archive on the supported macOS host."""
    _verify_developer_id(app, run=run)
    run(["xcrun", "stapler", "validate", app])
    run(["syspolicy_check", "distribution", app])
