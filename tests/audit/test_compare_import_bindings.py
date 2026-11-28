# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the offline import comparison through its public CLI."""

import csv
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/audit/compare_import_bindings.py"


def service(library, symbol, status, *, kind="name", data=False):
    return {
        "library": library,
        "kind": kind,
        "symbol": symbol,
        "status": status,
        "data_import": data,
        "source": "manual",
        "status_explicit": True,
    }


def alias(library, symbol, target, *, kind="name", canonical=False):
    return {
        "library": library,
        "kind": kind,
        "symbol": symbol,
        "canonical_library": target["library"],
        "canonical_kind": target["kind"],
        "canonical_symbol": target["symbol"],
        "api_set": library.lower().startswith("api-ms-"),
        "canonical": canonical,
    }


def snapshot(services=(), aliases=()):
    rows = list(services) or [service("unrelated.dll", "Anchor", "implemented")]
    keys = [
        alias(row["library"], row["symbol"], row, kind=row["kind"], canonical=True)
        for row in rows
    ]
    return {"schema": 1, "services": rows, "keys": keys + list(aliases), "typed": []}


class CompareImportBindingsTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        directory = Path(temporary.name)
        self.baseline = directory / "baseline.json"
        self.candidate = directory / "candidate.json"
        self.wanted = directory / "wanted.csv"

    @staticmethod
    def write_snapshot(path, data):
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")

    def write_wanted(self, rows):
        with self.wanted.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.writer(handle)
            writer.writerow(["library", "symbol", "kind"])
            writer.writerows(rows)

    def compare(self, *extra, expected=0):
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--baseline",
                str(self.baseline),
                "--candidate",
                str(self.candidate),
                "--wanted",
                str(self.wanted),
                *extra,
            ],
            cwd=ROOT,
            capture_output=True,
            check=False,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, expected, (result.stdout, result.stderr))
        return result

    def test_exact_identity_retarget_removal_addition_and_data_flag(self):
        code = service("provider.dll", "First", "partial")
        data = service("provider.dll", "Second", "partial", data=True)
        literal = service("Win.DLL", "Ordinal#7", "implemented")
        removed = service("WIN.dll", "Symbol", "stub")
        added = service("Win.DLL", "symbol", "implemented")
        old_data = service("Win.dll", "DataFlip", "partial")
        new_data = service("win.dll", "DataFlip", "partial", data=True)
        old_ordinal = alias("Win.DLL", "Ordinal#7", code, kind="ordinal")
        new_ordinal = alias("win.dll", "Ordinal#7", data, kind="ordinal")
        self.write_snapshot(
            self.baseline, snapshot([code, data, literal, removed, old_data], [old_ordinal])
        )
        self.write_snapshot(
            self.candidate, snapshot([code, data, literal, added, new_data], [new_ordinal])
        )
        wanted_rows = [
            ("WIN.dll", "Ordinal#7", "ordinal"),
            ("win.DLL", "Ordinal#7", "name"),
            ("Win.dll", "DataFlip", "name"),
            ("Win.dll", "Symbol", "name"),
            ("Win.dll", "symbol", "name"),
            ("other.dll", "Absent", "name"),
            ("win.DLL", "Ordinal#7", "ordinal"),
        ]
        self.write_wanted(wanted_rows)
        raw = self.compare().stdout
        self.write_wanted(reversed(wanted_rows))
        self.assertEqual(self.compare().stdout, raw)
        report = json.loads(raw)
        self.assertEqual(report["schema"], 1)
        self.assertEqual(report["distinct_imports"], 6)
        self.assertEqual(report["changed_imports"], 4)
        self.assertEqual(
            report["snapshot_sha256"],
            {
                "baseline": hashlib.sha256(self.baseline.read_bytes()).hexdigest(),
                "candidate": hashlib.sha256(self.candidate.read_bytes()).hexdigest(),
            },
        )
        self.assertEqual(
            report["totals"],
            {
                "baseline": {"implemented": 1, "partial": 2, "stub": 1, "missing": 2},
                "candidate": {"implemented": 2, "partial": 2, "stub": 0, "missing": 2},
            },
        )
        libraries = {item["library"]: item for item in report["libraries"]}
        self.assertEqual(set(libraries), {"other.dll", "win.dll"})
        self.assertEqual(libraries["win.dll"]["total"], 5)
        self.assertEqual(libraries["other.dll"]["total"], 1)
        self.assertEqual(libraries["other.dll"]["baseline"]["missing"], 1)
        self.assertEqual(libraries["other.dll"]["candidate"]["missing"], 1)

        changes = {
            (row["library"], row["kind"], row["symbol"]): row for row in report["changes"]
        }
        self.assertEqual(
            set(changes),
            {
                ("win.dll", "ordinal", "Ordinal#7"),
                ("win.dll", "name", "DataFlip"),
                ("win.dll", "name", "Symbol"),
                ("win.dll", "name", "symbol"),
            },
        )
        retarget = changes[("win.dll", "ordinal", "Ordinal#7")]
        self.assertEqual(retarget["baseline"]["status"], "partial")
        self.assertEqual(retarget["candidate"]["status"], "partial")
        for side in ("baseline", "candidate"):
            self.assertEqual(retarget[side]["resolution"], "ordinal-alias")
        self.assertEqual(
            retarget["baseline"]["canonical"],
            {"library": "provider.dll", "kind": "name", "symbol": "First"},
        )
        self.assertEqual(
            retarget["candidate"]["canonical"],
            {"library": "provider.dll", "kind": "name", "symbol": "Second"},
        )
        self.assertFalse(retarget["baseline"]["data_import"])
        self.assertTrue(retarget["candidate"]["data_import"])
        flip = changes[("win.dll", "name", "DataFlip")]
        self.assertEqual(flip["baseline"]["canonical"], flip["candidate"]["canonical"])
        for side in ("baseline", "candidate"):
            self.assertEqual(flip[side]["resolution"], "canonical")
        self.assertFalse(flip["baseline"]["data_import"])
        self.assertTrue(flip["candidate"]["data_import"])
        for name, side in [("Symbol", "candidate"), ("symbol", "baseline")]:
            binding = changes[("win.dll", "name", name)][side]
            self.assertEqual(binding["status"], "missing")
            self.assertEqual(binding["resolution"], "missing")
            self.assertIsNone(binding["canonical"])
            self.assertIsNone(binding["data_import"])

    def test_named_module_alias_and_api_set_alias_remain_distinct(self):
        canonical = service("provider.dll", "Function", "implemented")
        ordinary = alias("compatible.dll", "Function", canonical)
        api_set = alias("api-ms-example.dll", "Function", canonical)
        self.write_snapshot(self.baseline, snapshot([canonical]))
        self.write_snapshot(self.candidate, snapshot([canonical], [ordinary, api_set]))
        self.write_wanted(
            [
                ("API-MS-EXAMPLE.DLL", "Function", "name"),
                ("Compatible.DLL", "Function", "name"),
            ]
        )
        comparison = json.loads(self.compare().stdout)
        found = {
            row["library"]: row["candidate"]["resolution"]
            for row in comparison["changes"]
        }
        self.assertEqual(
            found,
            {"api-ms-example.dll": "api-set-alias", "compatible.dll": "module-alias"},
        )

    def test_malformed_and_ambiguous_snapshots_fail_without_output(self):
        self.write_wanted([("x.dll", "Function", "name")])
        first = service("x.dll", "Function", "partial")
        duplicate = service("X.DLL", "Function", "implemented")
        second = service("x.dll", "Another", "partial")
        self.write_snapshot(self.baseline, snapshot([first]))
        for label, bad in [
            ("duplicate normalized service", snapshot([first, duplicate])),
            ("canonical and alias collide", snapshot([first], [alias("X.DLL", "Function", first)])),
            (
                "alias has two targets",
                snapshot([first, second], [alias("Y.DLL", "Value", first), alias("y.dll", "Value", second)]),
            ),
            ("unknown schema", {"schema": 17, "services": [], "aliases": []}),
            ("nonboolean data flag", snapshot([{**first, "data_import": "false"}])),
            ("nonscalar service source", snapshot([{**first, "source": []}])),
        ]:
            with self.subTest(label=label):
                self.write_snapshot(self.candidate, bad)
                result = self.compare(expected=2)
                self.assertEqual(result.stdout, "")
                self.assertTrue(result.stderr.strip())
                self.assertNotIn("Traceback", result.stderr)

    def test_malformed_wanted_and_bounded_snapshot_fail(self):
        self.write_snapshot(self.baseline, snapshot())
        self.write_snapshot(self.candidate, snapshot())
        self.write_wanted([("x.dll", "Ordinal#65536", "ordinal")])
        rejected = self.compare(expected=2)
        self.assertEqual(rejected.stdout, "")
        self.assertIn("ordinal", rejected.stderr.lower())

        self.write_wanted([("x.dll", "Function", "name")])
        with self.candidate.open("wb") as output:
            output.truncate(8 * 1024 * 1024 + 1)
        result = self.compare(expected=2)
        self.assertEqual(result.stdout, "")
        self.assertIn("exceeds", result.stderr.lower())

    def test_duplicate_json_members_and_unterminated_csv_fields_are_errors(self):
        payload = json.dumps(snapshot())
        self.write_snapshot(self.baseline, snapshot())
        self.write_wanted([("unrelated.dll", "Anchor", "name")])
        self.candidate.write_text(
            payload.replace('"status": "implemented"', '"status": "stub", "status": "implemented"'),
            encoding="utf-8",
        )
        result = self.compare(expected=2)
        self.assertIn("duplicate member", result.stderr)
        self.assertEqual(result.stdout, "")

        self.write_snapshot(self.candidate, snapshot())
        self.wanted.write_text('library,symbol\nunrelated.dll,"Anchor', encoding="utf-8")
        result = self.compare(expected=2)
        self.assertIn("CSV", result.stderr)
        self.assertEqual(result.stdout, "")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "named pipes require POSIX")
    def test_rejects_fifo_paths_without_waiting_for_a_writer(self):
        self.write_snapshot(self.baseline, snapshot())
        self.write_wanted([("unrelated.dll", "Anchor", "name")])
        os.mkfifo(self.candidate)
        self.assertIn("regular file", self.compare(expected=2).stderr)

        self.candidate.unlink()
        self.write_snapshot(self.candidate, snapshot())
        self.wanted.unlink()
        os.mkfifo(self.wanted)
        self.assertIn("regular file", self.compare(expected=2).stderr)

    def test_markdown_reports_counts_and_escapes_table_separators(self):
        symbol = "Odd|Name`<Tail>&"
        self.write_snapshot(self.baseline, snapshot())
        self.write_snapshot(self.candidate, snapshot([service("X.DLL", symbol, "partial")]))
        self.write_wanted([("x.dll", symbol, "name")])
        markdown = self.compare("--format", "markdown").stdout
        self.assertIn("x.dll", markdown)
        self.assertIn("missing", markdown.lower())
        self.assertIn("partial", markdown.lower())
        self.assertIn("| <code>x.dll</code> | 1 | 0/0/0/1 | 0/1/0/0 |", markdown)
        self.assertIn("<code>Odd&#124;Name`&lt;Tail&gt;&amp;</code>", markdown)
        self.assertNotIn(symbol, markdown)


if __name__ == "__main__":
    unittest.main()
