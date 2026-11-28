# SPDX-License-Identifier: GPL-3.0-or-later
"""The agent checks must not spend three GitHub calls per open pull request."""
import json
import unittest
from unittest import mock

from agent_queue import ghio


def row(number, sha="sha%d", body="body", author="alice"):
    return {"number": number, "author": {"login": author}, "body": body,
            "headRefOid": sha % number, "state": "OPEN"}


def graphql(number, files=("src/a.rs",), truncated=False, when="2026-01-10T09:00:00Z"):
    return {"pullRequest": {
        "files": {"pageInfo": {"hasNextPage": truncated}, "nodes": [{"path": f} for f in files]},
        "commits": {"nodes": [{"commit": {"committedDate": when}}]},
    }}


class PrBatching(unittest.TestCase):
    def run_with(self, rows, answers, rest=None):
        calls = []

        def fake_gh(*args):
            calls.append(list(args))
            if args[:2] == ("pr", "list"):
                return json.dumps(rows)
            if args[:2] == ("api", "graphql"):
                return json.dumps({"data": answers})
            if args[:2] == ("pr", "view"):
                return json.dumps({r["number"]: r for r in rows}[int(args[2])])
            if rest is not None and args[0] == "api" and "/files" in args[1]:
                return rest(args[1])
            raise AssertionError("unexpected gh call: %r" % (args,))

        with mock.patch.object(ghio, "gh", side_effect=fake_gh):
            return ghio.open_prs("o/r"), calls

    def test_two_calls_for_the_whole_list(self):
        prs, calls = self.run_with(
            [row(1), row(2), row(3)],
            {"p0": graphql(1, ["src/a.rs", "src/b.rs"]), "p1": graphql(2), "p2": graphql(3)})
        self.assertEqual(len(calls), 2, calls)
        self.assertEqual(tuple(calls[0][:2]), ("pr", "list"))
        self.assertEqual(tuple(calls[1][:2]), ("api", "graphql"))
        self.assertEqual([p["number"] for p in prs], [1, 2, 3])
        self.assertEqual(prs[0]["files"], ["src/a.rs", "src/b.rs"])
        self.assertEqual(prs[0]["head_sha"], "sha1")
        self.assertEqual(prs[0]["head_pushed_at"], "2026-01-10T09:00:00Z")
        self.assertEqual(prs[2]["author"], "alice")

    def test_query_names_the_repository_and_one_alias_per_pull_request(self):
        _, calls = self.run_with([row(7), row(9)], {"p0": graphql(7), "p1": graphql(9)})
        query = calls[1][-1]
        self.assertIn('owner: "o"', query)
        self.assertIn('name: "r"', query)
        self.assertIn("pullRequest(number: 7)", query)
        self.assertIn("pullRequest(number: 9)", query)
        self.assertIn("p1: repository", query)

    def test_truncated_file_list_falls_back_to_the_paginated_call(self):
        prs, calls = self.run_with(
            [row(4)],
            {"p0": graphql(4, ["src/a.rs"], truncated=True)},
            rest=lambda path: "src/a.rs\nsrc/b.rs\n")
        self.assertEqual(len(calls), 3, calls)
        self.assertEqual(calls[2][0], "api")
        self.assertIn("/pulls/4/files", calls[2][1])
        self.assertEqual(prs[0]["files"], ["src/a.rs", "src/b.rs"])

    def test_missing_pull_request_retries_only_the_expensive_fields(self):
        prs, calls = self.run_with([row(5)], {})
        self.assertEqual(len(calls), 3, calls)
        self.assertEqual(tuple(calls[1][:2]), ("api", "graphql"))
        self.assertEqual(tuple(calls[2][:2]), ("api", "graphql"))
        self.assertIn("pullRequest(number: 5)", calls[2][-1])
        self.assertNotIn("p1:", calls[2][-1])
        self.assertEqual(prs[0]["number"], 5)
        self.assertEqual(prs[0]["files"], [])
        self.assertEqual(prs[0]["head_pushed_at"], "")

    def test_empty_list_costs_one_call(self):
        prs, calls = self.run_with([], {})
        self.assertEqual(calls, [["pr", "list", "-R", "o/r", "--state", "open", "--limit", "100",
                                  "--json", ghio.PR_FIELDS]])
        self.assertEqual(prs, [])

    def test_pr_data_reuses_a_gathered_row_without_any_call(self):
        rows = [{"number": 3, "author": "alice", "body": "b", "head_sha": "sha3",
                 "head_pushed_at": "t", "files": ["src/a.rs"], "state": "OPEN"}]
        calls = []

        def fake_gh(*args):
            calls.append(list(args))
            raise AssertionError("unexpected gh call: %r" % (args,))

        with mock.patch.object(ghio, "gh", side_effect=fake_gh):
            got = ghio.pr_data("o/r", 3, rows)
        self.assertEqual(calls, [])
        self.assertEqual(got, rows[0])

    def test_pr_data_without_rows_costs_two_calls(self):
        calls = []

        def fake_gh(*args):
            calls.append(list(args))
            if args[:2] == ("pr", "view"):
                return json.dumps(row(8))
            if args[:2] == ("api", "graphql"):
                return json.dumps({"data": {"p0": graphql(8, ["src/x.rs"])}})
            raise AssertionError("unexpected gh call: %r" % (args,))

        with mock.patch.object(ghio, "gh", side_effect=fake_gh):
            got = ghio.pr_data("o/r", 8)
        self.assertEqual(len(calls), 2, calls)
        self.assertEqual(got["files"], ["src/x.rs"])
        self.assertEqual(got["head_sha"], "sha8")


if __name__ == "__main__":
    unittest.main()
