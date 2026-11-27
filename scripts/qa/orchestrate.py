# SPDX-License-Identifier: GPL-3.0-or-later
"""Independent test orchestration with structured JSON reports.

Runs listed commands, records durations and exit codes, and never promotes a
failed command to passed because output was filtered.
"""
from __future__ import annotations

import json
import time
from dataclasses import asdict, dataclass, field
from typing import Any, Sequence

from .exit_pipe import classify_status, run_preserving_status


@dataclass(frozen=True)
class SuiteSpec:
    name: str
    command: tuple[str, ...]
    kind: str = "cpu"  # cpu | gpu | other


@dataclass
class SuiteResult:
    name: str
    kind: str
    command: list[str]
    status: str
    returncode: int
    duration_seconds: float
    stdout: str
    stderr: str
    reproduction: str


@dataclass
class OrchestrationReport:
    suites: list[SuiteResult] = field(default_factory=list)
    started_at: float = 0.0
    finished_at: float = 0.0

    @property
    def passed(self) -> int:
        return sum(1 for s in self.suites if s.status == "passed")

    @property
    def failed(self) -> int:
        return sum(1 for s in self.suites if s.status == "failed")

    @property
    def ignored(self) -> int:
        return sum(1 for s in self.suites if s.status == "ignored")

    @property
    def unexecuted(self) -> int:
        return sum(1 for s in self.suites if s.status == "unexecuted")

    def overall_returncode(self) -> int:
        codes = [s.returncode for s in self.suites if s.status == "failed"]
        if not codes:
            return 0
        return codes[0] if codes[0] != 0 else 1

    def to_dict(self) -> dict[str, Any]:
        return {
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "duration_seconds": self.finished_at - self.started_at,
            "counts": {
                "passed": self.passed,
                "failed": self.failed,
                "ignored": self.ignored,
                "unexecuted": self.unexecuted,
            },
            "suites": [asdict(s) for s in self.suites],
            "overall_returncode": self.overall_returncode(),
        }


def shell_quote(part: str) -> str:
    if part == "":
        return "''"
    if all(c.isalnum() or c in "@%_+:=,./-" for c in part):
        return part
    return "'" + part.replace("'", "'\"'\"'") + "'"


def run_suites(
    specs: Sequence[SuiteSpec],
    *,
    stop_on_fail: bool = False,
    cwd: str | None = None,
) -> OrchestrationReport:
    report = OrchestrationReport(started_at=time.time())
    halted = False
    for spec in specs:
        if halted:
            report.suites.append(
                SuiteResult(
                    name=spec.name,
                    kind=spec.kind,
                    command=list(spec.command),
                    status="unexecuted",
                    returncode=-1,
                    duration_seconds=0.0,
                    stdout="",
                    stderr="not executed because a previous suite failed",
                    reproduction=" ".join(shell_quote(p) for p in spec.command),
                )
            )
            continue
        if spec.kind == "ignored":
            report.suites.append(
                SuiteResult(
                    name=spec.name,
                    kind=spec.kind,
                    command=list(spec.command),
                    status="ignored",
                    returncode=0,
                    duration_seconds=0.0,
                    stdout="",
                    stderr="suite marked ignored",
                    reproduction=" ".join(shell_quote(p) for p in spec.command),
                )
            )
            continue
        started = time.time()
        result = run_preserving_status(spec.command, cwd=cwd)
        finished = time.time()
        status = classify_status(result.returncode)
        report.suites.append(
            SuiteResult(
                name=spec.name,
                kind=spec.kind,
                command=list(spec.command),
                status=status,
                returncode=result.returncode,
                duration_seconds=finished - started,
                stdout=result.stdout,
                stderr=result.stderr,
                reproduction=" ".join(shell_quote(p) for p in spec.command),
            )
        )
        if stop_on_fail and status == "failed":
            halted = True
    report.finished_at = time.time()
    return report


def dumps_report(report: OrchestrationReport) -> str:
    return json.dumps(report.to_dict(), indent=2, sort_keys=True) + "\n"


def failure_log_excerpt(report: OrchestrationReport, *, limit: int = 40) -> str:
    lines: list[str] = []
    for suite in report.suites:
        if suite.status != "failed":
            continue
        lines.append(f"## {suite.name} (exit {suite.returncode})")
        lines.append(f"reproduce: {suite.reproduction}")
        err = (suite.stderr or suite.stdout).splitlines()
        lines.extend(err[:limit])
        lines.append("")
    return "\n".join(lines)


def separate_cpu_gpu(report: OrchestrationReport) -> dict[str, list[SuiteResult]]:
    out = {"cpu": [], "gpu": [], "other": []}
    for suite in report.suites:
        bucket = suite.kind if suite.kind in out else "other"
        out[bucket].append(suite)
    return out
