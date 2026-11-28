#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later

"""Compare the same wanted PE imports across two saved runtime registry snapshots."""

import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import sys

from runtime_inventory import (
    MAX_SNAPSHOT_BYTES,
    MAX_WANTED_BYTES,
    WANTED_STATUSES,
    InventoryError,
    code,
    first_unique,
    match_wanted,
    md_table,
    normalized_key,
    parse_snapshot,
    read_wanted,
    render_json,
    service_key,
    target_key,
)


class SavedInput:
    """Read a bounded regular-file snapshot for the existing inventory parsers."""

    def __init__(self, path, limit):
        self.path = path
        try:
            with open(path, "rb", opener=self.open_nonblocking) as source:
                if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                    raise InventoryError(f"{path}: expected a regular file")
                self.data = source.read(limit + 1)
        except OSError as error:
            raise InventoryError(f"{path}: {error}") from error
        if len(self.data) > limit:
            raise InventoryError(f"{path}: input exceeds {limit} bytes")

    @staticmethod
    def open_nonblocking(path, flags):
        return os.open(path, flags | getattr(os, "O_NONBLOCK", 0))

    def open(self, _mode):
        return io.BytesIO(self.data)

    def __str__(self):
        return str(self.path)


def unique_json_object(pairs):
    row = dict(pairs)
    if len(row) != len(pairs):
        raise InventoryError("duplicate member in a snapshot JSON object")
    return row


def binding_for(match, services):
    status = match["status"]
    if status == "missing":
        return {"status": status, "resolution": "missing", "canonical": None, "data_import": None}

    library, kind, symbol = target_key(match)
    return {
        "status": status,
        "resolution": match["resolution"],
        "canonical": {"library": library, "kind": kind, "symbol": symbol},
        "data_import": services[(library, kind, symbol)]["data_import"],
    }


def read_snapshot(path, label):
    try:
        data = SavedInput(path, MAX_SNAPSHOT_BYTES).data
        json.loads(data, object_pairs_hook=unique_json_object)
        snapshot = parse_snapshot(data)
        services = first_unique(snapshot["services"], service_key)
        keys = first_unique(snapshot["keys"], service_key)
        if len(services) != len(snapshot["services"]) or len(keys) != len(snapshot["keys"]):
            raise InventoryError("duplicate canonical services or resolution keys")
    except (ValueError, TypeError, RecursionError) as error:
        raise InventoryError(f"{label}: {error}") from error
    return services, keys, hashlib.sha256(data).hexdigest()


def read_wanted_csv(path):
    source = SavedInput(path, MAX_WANTED_BYTES)
    try:
        text = source.data.decode("utf-8-sig")
        # Validate quoting on the same bytes used for exact import matching.
        for _ in csv.reader(io.StringIO(text, newline=""), strict=True):
            pass
    except (csv.Error, UnicodeDecodeError) as error:
        raise InventoryError(f"{path}: invalid wanted-import CSV: {error}") from error
    return read_wanted(source)


def compare_imports(baseline, candidate, wanted):
    baseline_services, baseline_keys, baseline_hash = baseline
    candidate_services, candidate_keys, candidate_hash = candidate
    before = match_wanted(wanted, baseline_services, baseline_keys)
    after = match_wanted(wanted, candidate_services, candidate_keys)
    libraries = {}
    changes = []
    # Both match lists are emitted in the same wanted-entry order.
    for previous, current in zip(before["matches"], after["matches"]):
        library, kind, symbol = normalized_key(current["library"], current["kind"], current["symbol"])
        if library not in libraries:
            libraries[library] = {
                "library": library,
                "total": 0,
                "baseline": dict.fromkeys(WANTED_STATUSES, 0),
                "candidate": dict.fromkeys(WANTED_STATUSES, 0),
            }
        totals = libraries[library]
        totals["total"] += 1
        totals["baseline"][previous["status"]] += 1
        totals["candidate"][current["status"]] += 1

        old_binding = binding_for(previous, baseline_services)
        new_binding = binding_for(current, candidate_services)
        if old_binding != new_binding:
            changes.append(
                {
                    "library": library,
                    "kind": kind,
                    "symbol": symbol,
                    "baseline": old_binding,
                    "candidate": new_binding,
                }
            )
    return {
        "schema": 1,
        "snapshot_sha256": {"baseline": baseline_hash, "candidate": candidate_hash},
        "distinct_imports": after["distinct_imports"],
        "changed_imports": len(changes),
        "totals": {"baseline": before["status_counts"], "candidate": after["status_counts"]},
        "libraries": [libraries[name] for name in sorted(libraries)],
        "changes": sorted(changes, key=service_key),
    }


def render_markdown(report):
    def counts(values):
        return "/".join(str(values[status]) for status in WANTED_STATUSES)

    def binding(value):
        target = value["canonical"]
        if target is None:
            return value["status"]
        interface = "data" if value["data_import"] else "callable"
        return (
            f"{value['status']} ({value['resolution']}) · "
            f"{target['library']}!{target['symbol']} ({target['kind']}, {interface})"
        )

    lines = [
        "# Wanted import binding comparison",
        "",
        f"Compared {report['distinct_imports']} distinct imports. {report['changed_imports']} changed.",
        "",
        f"- Baseline snapshot SHA-256 {code(report['snapshot_sha256']['baseline'])}",
        f"- Candidate snapshot SHA-256 {code(report['snapshot_sha256']['candidate'])}",
        "",
        "Counts are ordered implemented / partial / stub / missing.",
        "",
    ]
    library_rows = [
        (
            "All DLLs",
            report["distinct_imports"],
            counts(report["totals"]["baseline"]),
            counts(report["totals"]["candidate"]),
        ),
    ]
    library_rows.extend(
        (code(row["library"]), row["total"], counts(row["baseline"]), counts(row["candidate"]))
        for row in report["libraries"]
    )
    lines.extend(md_table(("DLL", "Imports", "Baseline", "Candidate"), library_rows))
    lines.extend(["", "## Binding changes", ""])
    if report["changes"]:
        lines.extend(
            md_table(
                ("DLL", "Kind", "Symbol", "Baseline", "Candidate"),
                [
                    (
                        code(row["library"]),
                        row["kind"],
                        code(row["symbol"]),
                        code(binding(row["baseline"])),
                        code(binding(row["candidate"])),
                    )
                    for row in report["changes"]
                ],
            )
        )
    else:
        lines.append("No bindings changed.")
    return ("\n".join(lines) + "\n").encode("utf-8")


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True, help="saved baseline registry snapshot")
    parser.add_argument("--candidate", type=Path, required=True, help="saved candidate registry snapshot")
    parser.add_argument("--wanted", type=Path, required=True, help="probe imports.csv or wanted-import CSV")
    parser.add_argument("--format", choices=("json", "markdown"), default="json")
    options = parser.parse_args(argv)
    try:
        baseline = read_snapshot(options.baseline, "baseline")
        candidate = read_snapshot(options.candidate, "candidate")
        report = compare_imports(baseline, candidate, read_wanted_csv(options.wanted))
    except InventoryError as error:
        print(f"import binding comparison: {error}", file=sys.stderr)
        return 2
    render = render_json if options.format == "json" else render_markdown
    sys.stdout.buffer.write(render(report))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
