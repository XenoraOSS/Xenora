#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Validate and reconcile the Xenora campaign tracker.

`--check` is deterministic and offline. `--sync` uses the installed `gh` CLI to
refresh PR/issue states from upstream; it never changes ownership, evidence
classification, test evidence, or limitations.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TRACKER = ROOT / "docs" / "coordination" / "ascension-tracker.json"
REPO = "ranveersoni98/Xenora"

STATES = {
    "UNCLAIMED",
    "CLAIMED",
    "RESEARCHING",
    "IMPLEMENTING",
    "TESTING",
    "PR_OPEN",
    "REVIEW_REQUIRED",
    "MERGED",
    "VERIFIED",
    "BLOCKED",
    "SUPERSEDED",
}
EVIDENCE = {"A", "B", "C", "D", "E"}
PRIORITIES = {"P0", "P1", "P2", "P3"}
REQUIRED = {
    "id",
    "subsystem",
    "description",
    "priority",
    "dependencies",
    "owner",
    "status",
    "evidence_class",
    "issue",
    "pr",
    "test_status",
    "limitations",
}


def load() -> dict:
    return json.loads(TRACKER.read_text(encoding="utf-8"))


def save(data: dict) -> None:
    TRACKER.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def validate(data: dict) -> None:
    if data.get("schema_version") != 1:
        raise ValueError("schema_version must be 1")
    packages = data.get("work_packages")
    if not isinstance(packages, list):
        raise ValueError("work_packages must be a list")
    seen: set[str] = set()
    ids: set[str] = set()
    for index, package in enumerate(packages):
        label = f"work_packages[{index}]"
        missing = REQUIRED - package.keys()
        if missing:
            raise ValueError(f"{label} missing {sorted(missing)}")
        identifier = package["id"]
        if identifier in seen:
            raise ValueError(f"duplicate work package id {identifier}")
        seen.add(identifier)
        ids.add(identifier)
        if package["status"] not in STATES:
            raise ValueError(f"{identifier}: unknown status {package['status']}")
        if package["evidence_class"] not in EVIDENCE:
            raise ValueError(f"{identifier}: unknown evidence class {package['evidence_class']}")
        if package["priority"] not in PRIORITIES:
            raise ValueError(f"{identifier}: unknown priority {package['priority']}")
        for key in ("issue", "pr"):
            value = package[key]
            if value is not None and (not isinstance(value, int) or value <= 0):
                raise ValueError(f"{identifier}: {key} must be null or a positive integer")
        if not isinstance(package["dependencies"], list):
            raise ValueError(f"{identifier}: dependencies must be a list")
        for key in ("subsystem", "description", "test_status", "limitations"):
            if not isinstance(package[key], str):
                raise ValueError(f"{identifier}: {key} must be a string")
        if package["owner"] is not None and not isinstance(package["owner"], str):
            raise ValueError(f"{identifier}: owner must be null or a string")
    for package in packages:
        for dependency in package["dependencies"]:
            if dependency not in ids:
                raise ValueError(f"{package['id']}: unknown dependency {dependency}")


def api_objects(endpoint: str, jq: str) -> dict[int, dict]:
    command = ["gh", "api", "--paginate", endpoint, "--jq", jq]
    try:
        output = subprocess.check_output(command, cwd=ROOT, text=True)
    except FileNotFoundError as error:
        raise SystemExit("gh is required for --sync") from error
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"gh api failed for {endpoint}: {error}") from error
    result: dict[int, dict] = {}
    for line in output.splitlines():
        if not line.strip():
            continue
        item = json.loads(line)
        result[int(item["number"])] = item
    return result


def sync(data: dict) -> list[str]:
    pulls = api_objects(
        f"repos/{REPO}/pulls?state=all&per_page=100",
        '.[] | {number, state, merged_at}',
    )
    issues = api_objects(
        f"repos/{REPO}/issues?state=all&per_page=100",
        '.[] | {number, state}',
    )
    changes: list[str] = []
    for package in data["work_packages"]:
        current = package["status"]
        pull = pulls.get(package["pr"]) if package["pr"] is not None else None
        issue = issues.get(package["issue"]) if package["issue"] is not None else None
        if pull is not None:
            if pull.get("merged_at"):
                desired = "MERGED"
            elif pull["state"] == "open":
                desired = "PR_OPEN"
            else:
                desired = "SUPERSEDED"
        elif issue is not None:
            if issue["state"] == "closed" and current not in {"MERGED", "VERIFIED", "SUPERSEDED"}:
                desired = "SUPERSEDED"
            elif current in STATES:
                desired = current
            else:
                desired = "UNCLAIMED"
        else:
            desired = current
        if desired != current:
            package["status"] = desired
            changes.append(f"{package['id']}: {current} -> {desired}")
    if changes:
        data["updated_at"] = datetime.now(timezone.utc).date().isoformat()
        save(data)
    return changes


def dashboard(data: dict) -> None:
    packages = data["work_packages"]
    print(f"work packages: {len(packages)}")
    for state in sorted(STATES):
        count = sum(package["status"] == state for package in packages)
        if count:
            print(f"  {state}: {count}")
    for subsystem in sorted({package["subsystem"] for package in packages}):
        count = sum(package["subsystem"] == subsystem for package in packages)
        print(f"  subsystem {subsystem}: {count}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="validate the tracker only")
    parser.add_argument("--sync", action="store_true", help="reconcile states with upstream via gh")
    parser.add_argument("--dashboard", action="store_true", help="print status counts")
    args = parser.parse_args()
    data = load()
    validate(data)
    if args.sync:
        changes = sync(data)
        for change in changes:
            print(change)
        validate(data)
    if args.check or not (args.sync or args.dashboard):
        print(f"tracker valid: {TRACKER.relative_to(ROOT)}")
    if args.dashboard or not (args.sync or args.check):
        dashboard(data)
    return 0


if __name__ == "__main__":
    sys.exit(main())
