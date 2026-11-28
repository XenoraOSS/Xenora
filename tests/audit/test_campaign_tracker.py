#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later

import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("campaign_tracker", ROOT / "scripts/audit/campaign_tracker.py")
tracker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tracker)


class RuntimeEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.data = tracker.load()

    def test_evidence_preserves_all_inventory_binding_fields(self):
        self.data["runtime_service_evidence"][0]["status"] = "stub"
        projection = tracker.runtime_evidence(self.data)
        inventory = json.loads(tracker.RUNTIME_INVENTORY.read_text(encoding="utf-8"))
        self.assertEqual(len(projection["services"]), len(inventory["services"]))
        self.assertEqual(projection["aliases"], inventory["aliases"])
        for binding, projected in zip(inventory["services"], projection["services"]):
            self.assertEqual(binding, {key: projected[key] for key in binding})
            self.assertIn(projected["evidence_class"], tracker.EVIDENCE)

    def test_duplicate_inventory_summary_keys_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "duplicate JSON key 'canonical_services'"):
            json.loads('{"summary":{"canonical_services":3,"canonical_services":2}}',
                       object_pairs_hook=tracker.unique_object)

    def test_stale_inventory_summary_is_reported_without_losing_rows(self):
        inventory = json.loads(tracker.RUNTIME_INVENTORY.read_text(encoding="utf-8"))
        count = len(inventory["services"])
        inventory["summary"]["canonical_services"] = count - 1
        with patch.object(Path, "read_text", return_value=json.dumps(inventory)):
            projection = tracker.runtime_evidence(self.data)
        self.assertEqual(len(projection["services"]), count)
        self.assertEqual(projection["inventory_summary_mismatches"]["canonical_services"],
                         {"declared": count - 1, "rows": count})

    def test_verified_milestone_requires_two_distinct_reviewers(self):
        milestone = self.data["runtime_acceptance"][0]
        milestone["status"] = "VERIFIED"
        milestone["independent_reviews"] = True
        with self.assertRaisesRegex(ValueError, "independent review records required"):
            tracker.validate(self.data)
        milestone["independent_reviews"] = [
            {"reviewer": "account", "source": "https://example.com/1"},
            {"reviewer": "Account", "source": "https://example.com/2"},
        ]
        with self.assertRaisesRegex(ValueError, "two distinct reviewers required"):
            tracker.validate(self.data)
        milestone["independent_reviews"][1]["reviewer"] = "other-account"
        tracker.validate(self.data)
