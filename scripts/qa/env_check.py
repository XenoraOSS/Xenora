# SPDX-License-Identifier: GPL-3.0-or-later
"""Development environment diagnostics for Apple Silicon Xenora contributors."""
from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable


@dataclass
class EnvFinding:
    name: str
    ok: bool
    detail: str


@dataclass
class EnvReport:
    findings: list[EnvFinding] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return all(f.ok for f in self.findings)


def check_uname_machine(machine: str) -> EnvFinding:
    ok = machine.strip() == "arm64"
    return EnvFinding("host-arch", ok, f"uname -m => {machine.strip()}" + ("" if ok else " (want arm64)"))


def check_rosetta_translated(proc_translated: str) -> EnvFinding:
    ok = proc_translated.strip() == "0"
    return EnvFinding(
        "rosetta",
        ok,
        f"sysctl.proc_translated => {proc_translated.strip()}" + ("" if ok else " (Rosetta translation active)"),
    )


def check_rustc_host(rustc_vv: str) -> EnvFinding:
    host = ""
    for line in rustc_vv.splitlines():
        if line.startswith("host:"):
            host = line.split(":", 1)[1].strip()
    ok = host == "aarch64-apple-darwin"
    return EnvFinding("rustc-host", ok, f"host={host or 'unknown'}" + ("" if ok else " (want aarch64-apple-darwin)"))


def check_swift_version(swift_version: str) -> EnvFinding:
    ok = "Swift version" in swift_version
    return EnvFinding("swift", ok, swift_version.strip().splitlines()[0] if swift_version.strip() else "missing")


_LLVM_RE = re.compile(r"LLVM|clang", re.IGNORECASE)


def check_llvm_tool(which_path: str, version_text: str) -> EnvFinding:
    if not which_path.strip():
        return EnvFinding("llvm", False, "tool not found on PATH")
    ok = bool(_LLVM_RE.search(version_text) or version_text.strip())
    return EnvFinding("llvm", ok, f"{which_path.strip()}: {version_text.strip().splitlines()[0] if version_text.strip() else 'no version'}")


def check_git_worktree_clean(porcelain: str) -> EnvFinding:
    ok = porcelain.strip() == ""
    return EnvFinding("worktree-clean", ok, "clean" if ok else "dirty worktree")


def check_branch_sync(*, ahead: int, behind: int) -> EnvFinding:
    ok = ahead == 0 and behind == 0
    detail = f"ahead={ahead} behind={behind}"
    return EnvFinding("branch-sync", ok, detail)


def measure_branch_sync(root: Path) -> EnvFinding:
    try:
        output = subprocess.check_output(
            ["git", "rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
            cwd=root,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return EnvFinding("branch-sync", False, "upstream unavailable")
    try:
        ahead_text, behind_text = output.split()
        return check_branch_sync(ahead=int(ahead_text), behind=int(behind_text))
    except ValueError:
        return EnvFinding("branch-sync", False, "upstream counts unavailable")


def summarize(findings: Iterable[EnvFinding]) -> EnvReport:
    return EnvReport(findings=list(findings))


def format_report(report: EnvReport) -> str:
    lines = ["# Environment diagnostics", ""]
    for finding in report.findings:
        mark = "ok" if finding.ok else "FAIL"
        lines.append(f"- [{mark}] {finding.name}: {finding.detail}")
    lines.append("")
    lines.append("overall: " + ("ok" if report.ok else "problems found"))
    lines.append("")
    return "\n".join(lines)
