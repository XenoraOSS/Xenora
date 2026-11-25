# SPDX-License-Identifier: GPL-3.0-or-later
"""Read-only repository health checks for contributors."""
from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable


@dataclass(frozen=True)
class HealthFinding:
    code: str
    ok: bool
    detail: str


def check_ignored_artifacts_tracked(tracked_paths: Iterable[str]) -> HealthFinding:
    bad = []
    for path in tracked_paths:
        parts = path.split("/")
        if "__pycache__" in parts or path.endswith(".pyc"):
            bad.append(path)
        if "/target/" in f"/{path}/" or path.startswith("target/"):
            bad.append(path)
    ok = not bad
    detail = "no tracked build artifacts" if ok else "tracked artifacts: " + ", ".join(bad[:10])
    return HealthFinding("tracked-artifacts", ok, detail)


def check_large_generated_paths(tracked_paths: Iterable[str], *, suspicious_prefixes: tuple[str, ...] = ("artifacts/",)) -> HealthFinding:
    hits = [p for p in tracked_paths if any(p.startswith(pref) for pref in suspicious_prefixes)]
    ok = not hits
    detail = "no generated artifact paths tracked" if ok else "suspicious paths: " + ", ".join(hits[:10])
    return HealthFinding("generated-paths", ok, detail)


def check_upstream_configured(remotes: Iterable[str], *, expected: str = "MacosPorters/betterxen") -> HealthFinding:
    names = set(remotes)
    # Accept either a remote named upstream or origin pointing at expected.
    ok = "upstream" in names or "origin" in names
    detail = f"remotes={sorted(names)}; expected repo {expected}"
    return HealthFinding("remotes", ok, detail)


def format_health(findings: Iterable[HealthFinding]) -> str:
    lines = ["# Repository health", ""]
    for finding in findings:
        mark = "ok" if finding.ok else "FAIL"
        lines.append(f"- [{mark}] {finding.code}: {finding.detail}")
    lines.append("")
    return "\n".join(lines)
