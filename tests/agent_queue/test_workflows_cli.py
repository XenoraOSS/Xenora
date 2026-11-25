# SPDX-License-Identifier: GPL-3.0-or-later
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest

from helpers import ROOT, issue

def dump(obj, path):
    with open(path, "w") as fh:
        json.dump(obj, fh)


WF = os.path.join(ROOT, ".github", "workflows")
QUEUE_WORKFLOWS = ["agent-claims.yml", "agent-pr-checks.yml"]
USER_TEXT = re.compile(r"\$\{\{\s*github\.event\.(comment|issue|pull_request)\.(body|title)|github\.head_ref")


def run_blocks(text):
    """Text of every `run:` block (literal block scalar or single line)."""
    out, lines, i = [], text.split("\n"), 0
    while i < len(lines):
        m = re.match(r"^(\s*)(?:- )?run:\s*(.*)$", lines[i])
        if m:
            indent, rest = len(m.group(1)), m.group(2)
            if rest in ("|", ">", "|-", ">-"):
                block = []
                i += 1
                while i < len(lines) and (not lines[i].strip() or len(lines[i]) - len(lines[i].lstrip()) > indent):
                    block.append(lines[i])
                    i += 1
                out.append("\n".join(block))
                continue
            out.append(rest)
        i += 1
    return out


class Workflows(unittest.TestCase):
    def read(self, name):
        with open(os.path.join(WF, name), encoding="utf-8") as fh:
            return fh.read()

    def test_top_level_permissions_and_no_user_text_in_run(self):
        for name in QUEUE_WORKFLOWS:
            text = self.read(name)
            self.assertRegex(text, r"(?m)^permissions:", name)
            blocks = run_blocks(text)
            self.assertTrue(blocks, name)
            for b in blocks:
                self.assertNotRegex(b, r"\$\{\{", "%s: expression inside run" % name)
                self.assertIsNone(USER_TEXT.search(b), name)

    def test_checker_detects_violation(self):
        bad = "jobs:\n  a:\n    steps:\n      - run: |\n          echo ${{ github.event.comment.body }}\n"
        self.assertTrue(any(re.search(r"\$\{\{", b) for b in run_blocks(bad)))

    def test_pr_workflow_never_checks_out_pr_code(self):
        text = self.read("agent-pr-checks.yml")
        self.assertNotRegex(text, r"github\.event\.pull_request\.head|ref:\s*\$\{\{")
        self.assertIn("pull_request_target", text)

    def job(self, name, job):
        """Text of one job block: from `  <job>:` to the next two-space-indented key."""
        m = re.search(r"(?ms)^  %s:\n(.*?)(?=^  \S|\Z)" % re.escape(job), self.read(name))
        self.assertIsNotNone(m, "%s: job %s" % (name, job))
        return m.group(1)

    def job_if(self, name, job):
        return re.search(r"(?m)^    if: (.*)$", self.job(name, job)).group(1)

    def test_only_command_comments_start_runs(self):
        # Ordinary comments must not start a claim run or a verification re-run.
        self.assertIn("startsWith(github.event.comment.body, '/verified')", self.job_if("agent-pr-checks.yml", "verify"))
        self.assertIn("startsWith(github.event.comment.body, '/')", self.job_if("agent-claims.yml", "command"))
        self.assertNotIn("issue_comment", self.job_if("agent-pr-checks.yml", "check"))

    def test_claim_jobs_share_one_group_and_replay(self):
        for job in ("command", "audit"):
            text = self.job("agent-claims.yml", job)
            self.assertRegex(text, r"group: agent-queue\n\s+cancel-in-progress: false", job)
            # Pending runs can be cancelled by newer ones, so every run replays unanswered commands.
            self.assertIn("agent_queue replay", text, job)
            self.assertIn("--recent-commands", text, job)
        self.assertNotIn("agent_queue decide", self.job("agent-claims.yml", "command"))

    def test_rerun_needs_an_accepted_verification(self):
        text = self.job("agent-pr-checks.yml", "verify")
        self.assertIn('if ! python3 -m agent_queue should-rerun --actions "$RUNNER_TEMP/actions.json"; then exit 0; fi', text)
        self.assertLess(text.index("agent_queue should-rerun"), text.index("agent_queue pr-check"))
        # The check is evaluated in this job: pull_request_target runs cannot be found by head SHA.
        self.assertNotIn("gh run", text)
        self.assertNotIn("actions: write", text)
        self.assertIn("::warning title=Agent PR check (advisory)::", text)

    def test_should_rerun_exit_codes(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "a.json")
            for actions, code in (([], 1), ([{"issue": 4, "add_labels": [], "remove_labels": [], "comment": "refused"}], 1),
                                  ([{"issue": 4, "add_labels": [], "remove_labels": [], "comment": "ok", "rerun_check": True}], 0)):
                dump(actions, p)
                self.assertEqual(Cli.run_cli(self, "should-rerun", "--actions", p).returncode, code, actions)

    def test_verify_runs_one_at_a_time_per_pull_request(self):
        text = self.job("agent-pr-checks.yml", "verify")
        self.assertRegex(text, r"group: agent-verify-\$\{\{ github\.event\.issue\.number \}\}\n\s+cancel-in-progress: false")

    def test_pr_check_is_advisory(self):
        # Exit status 1 (problems found) only warns until the live smoke test and issue migration.
        text = self.job("agent-pr-checks.yml", "check")
        found = re.search(r'if \[ "\$status" -eq 1 \]; then\n(.*?)\n\s+elif \[ "\$status" -ne 0 \]; then\n\s+exit "\$status"', text, re.S)
        self.assertIsNotNone(found, text)
        self.assertIn("::warning title=Agent PR check (advisory)::", found.group(1))
        self.assertNotIn("exit", found.group(1))
        self.assertEqual(text.count("exit "), 1)

    def test_actions_pinned_by_sha(self):
        for name in QUEUE_WORKFLOWS:
            for use in re.findall(r"uses:\s*(\S+)", self.read(name)):
                self.assertRegex(use, r"@[0-9a-f]{40}$", name)


class Cli(unittest.TestCase):
    def run_cli(self, *args, stdin=None):
        env = dict(os.environ, PYTHONPATH=os.path.join(ROOT, "scripts"))
        return subprocess.run([sys.executable, "-m", "agent_queue", "--config", os.path.join(ROOT, ".github", "agent-queue.json")] + list(args),
                              input=stdin, capture_output=True, text=True, env=env)

    def test_shipped_config_parses(self):
        r = self.run_cli("config-get", "upstream")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "MacosPorters/betterxen")

    def test_summary_accepts_now_after_subcommand(self):
        # Actions passes --now after `summary`; parent-only --now used to reject that.
        with tempfile.TemporaryDirectory() as d:
            state = os.path.join(d, "state.json")
            dump({"issues": [], "prs": []}, state)
            r = self.run_cli("summary", "--state", state, "--now", "2026-01-10T12:00:00Z")
            self.assertEqual(r.returncode, 0, r.stderr)
            payload = json.loads(r.stdout)
            self.assertEqual(payload["generated_at"], "2026-01-10T12:00:00Z")

    def test_acceptance_authorization_end_to_end(self):
        from agent_queue import lease as L, tasks as T
        i = issue(1)
        digest = T.acceptance_hash("cargo test -p gpu")
        with tempfile.TemporaryDirectory() as d:
            ip, cp = os.path.join(d, "i.json"), os.path.join(d, "c.json")
            dump(i, ip)
            dump([], cp)
            r = self.run_cli("acceptance", "--issue", ip, "--comments", cp)
            self.assertEqual(r.returncode, 3)
            self.assertEqual(r.stdout, "")
            dump([{"user": "github-actions[bot]", "body": L.acceptance_marker(digest), "created_at": "2026-01-10T10:00:00Z"}], cp)
            r = self.run_cli("acceptance", "--issue", ip, "--comments", cp)
            self.assertEqual((r.returncode, r.stdout.strip()), (0, "cargo test -p gpu"))
            i["body"] = i["body"].replace("cargo test -p gpu", "cargo test -p gpu && evil")
            dump(i, ip)
            self.assertEqual(self.run_cli("acceptance", "--issue", ip, "--comments", cp).returncode, 3)

    def test_scripts_parse(self):
        for s in ("agent-queue.sh", "agent-sync.sh"):
            r = subprocess.run(["sh", "-n", os.path.join(ROOT, "scripts", s)], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_event_normalisation(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "e.json")
            dump({"issue": {"number": 4, "pull_request": {}}, "comment": {"body": "/verified abc1234", "user": {"login": "q"}}}, p)
            out = json.loads(self.run_cli("event", "--path", p).stdout)
            self.assertEqual(out, {"kind": "pr", "number": 4, "actor": "q", "body": "/verified abc1234"})


if __name__ == "__main__":
    unittest.main()
