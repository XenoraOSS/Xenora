# SPDX-License-Identifier: GPL-3.0-or-later
"""Unified verification pipeline over existing Xenora commands.

Profiles invoke repository scripts/tests; they do not reimplement them.
GPU suites run serially. Exit codes are preserved exactly.
"""
from __future__ import annotations

import hashlib
import json
import subprocess
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Sequence

from .orchestrate import SuiteSpec, run_suites


PROFILES: dict[str, list[SuiteSpec]] = {
    "quick": [
        SuiteSpec("agent-queue-unit", ("python3", "-m", "unittest", "discover", "-s", "tests/agent_queue"), "cpu"),
        SuiteSpec("qa-unit", ("python3", "-m", "unittest", "discover", "-s", "tests/qa"), "cpu"),
        SuiteSpec("release-unit", ("python3", "-m", "unittest", "discover", "-s", "tests/release"), "cpu"),
    ],
    "cpu": [
        SuiteSpec("cargo-test-workspace", ("cargo", "test", "--workspace", "--locked"), "cpu"),
        SuiteSpec("semantic-review", ("cargo", "test", "--locked", "-p", "xenora-semantic-review"), "cpu"),
    ],
    "gpu": [
        SuiteSpec("test-gpu", ("scripts/test-gpu.sh",), "gpu"),
    ],
    "app": [
        SuiteSpec("build-app-smoke", ("scripts/build-app.sh",), "other"),
    ],
    "release": [
        SuiteSpec("native-audit", ("scripts/audit/native.sh",), "other"),
        SuiteSpec("qa-unit", ("python3", "-m", "unittest", "discover", "-s", "tests/qa"), "cpu"),
        SuiteSpec("release-unit", ("python3", "-m", "unittest", "discover", "-s", "tests/release"), "cpu"),
    ],
}

PROFILES["full"] = (
    list(PROFILES["quick"])
    + [s for s in PROFILES["cpu"]]
    + list(PROFILES["gpu"])
    + list(PROFILES["app"])
)


@dataclass
class Evidence:
    profile: str
    revision: str
    started_at: float
    finished_at: float
    source_fingerprint: str
    suites: list[dict[str, Any]] = field(default_factory=list)
    overall_returncode: int = 0
    skipped: dict[str, int] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def fingerprint_sources(root: Path, paths: Sequence[Path] | None = None) -> str:
    h = hashlib.sha256()
    if paths is None:
        files = sorted(
            p for p in root.rglob("*")
            if p.is_file()
            and not any(part in {"target", ".git", "__pycache__", "artifacts", "node_modules"} for part in p.parts)
            and (p.suffix in {".rs", ".py", ".sh", ".swift", ".toml"} or p.name == "Cargo.lock")
        )[:4000]
    else:
        files = list(paths)
    for path in files:
        try:
            rel = str(path.relative_to(root))
        except ValueError:
            rel = str(path)
        h.update(rel.encode())
        h.update(b"\0")
        try:
            h.update(path.read_bytes())
        except OSError:
            h.update(b"<missing>")
        h.update(b"\0")
    return h.hexdigest()


def run_profile(
    profile: str,
    *,
    root: Path,
    revision: str,
    stop_on_fail: bool = True,
    override_specs: Sequence[SuiteSpec] | None = None,
) -> Evidence:
    if override_specs is None and profile not in PROFILES:
        raise ValueError(f"unknown profile {profile!r}; choose from {sorted(PROFILES)}")
    specs = list(override_specs) if override_specs is not None else list(PROFILES[profile])
    started = time.time()
    # Suites run sequentially; GPU profiles therefore never overlap.
    report = run_suites(specs, stop_on_fail=stop_on_fail, cwd=str(root))
    finished = time.time()
    skipped = {
        "ignored": report.ignored,
        "unexecuted": report.unexecuted,
    }
    return Evidence(
        profile=profile,
        revision=revision,
        started_at=started,
        finished_at=finished,
        source_fingerprint=fingerprint_sources(root),
        suites=[asdict(s) for s in report.suites],
        overall_returncode=report.overall_returncode(),
        skipped=skipped,
    )


def evidence_is_stale(evidence: Evidence, root: Path) -> bool:
    return evidence.source_fingerprint != fingerprint_sources(root)


def current_revision(root: Path) -> str:
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()


def assess_evidence(
    evidence: Evidence,
    root: Path,
    *,
    requested_profile: str,
    actual_revision: str,
) -> dict[str, Any]:
    stale = evidence_is_stale(evidence, root)
    revision_matches = evidence.revision == actual_revision
    profile_matches = evidence.profile == requested_profile
    suites_passed = evidence.overall_returncode == 0
    return {
        "stale": stale,
        "revision": evidence.revision,
        "actual_revision": actual_revision,
        "revision_matches": revision_matches,
        "profile": evidence.profile,
        "requested_profile": requested_profile,
        "profile_matches": profile_matches,
        "evidence_returncode": evidence.overall_returncode,
        "suites_passed": suites_passed,
        "accepted": not stale and revision_matches and profile_matches and suites_passed,
    }


def load_evidence(text: str) -> Evidence:
    data = json.loads(text)
    return Evidence(
        profile=data["profile"],
        revision=data["revision"],
        started_at=float(data["started_at"]),
        finished_at=float(data["finished_at"]),
        source_fingerprint=data["source_fingerprint"],
        suites=list(data.get("suites", [])),
        overall_returncode=int(data.get("overall_returncode", 0)),
        skipped=dict(data.get("skipped", {})),
    )


def dumps_evidence(evidence: Evidence) -> str:
    return json.dumps(evidence.to_dict(), indent=2, sort_keys=True) + "\n"
