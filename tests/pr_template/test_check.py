# SPDX-License-Identifier: GPL-3.0-or-later
import json
import os
import subprocess
import sys
import tempfile
import unittest

import pr_template_check as c

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TEMPLATE = open(os.path.join(ROOT, ".github", "PULL_REQUEST_TEMPLATE.md"), encoding="utf-8").read()

GOOD = """## Summary

Adds X.

Closes #12

## Implementation and evidence

Touches crates/texture only.

## Verification

| Check | Result / evidence |
| --- | --- |
| Formatting and lint | cargo fmt --check: clean |
| Unit / integration tests | cargo test -p xenora-texture: 40 passed |

## Compatibility and limitations

None known.

## Checklist

- [x] I claimed the linked issue before starting, and the claim was still mine when I opened this pull request.
- [x] I reviewed the changes and disclosed material AI assistance below, if applicable.
- [x] My commits are signed off under the DCO.

## AI assistance

None.
"""


class Check(unittest.TestCase):
    def test_good_body_passes(self):
        self.assertEqual(c.check(GOOD), [])

    def test_unfilled_template_fails_on_every_rule(self):
        problems = c.check(TEMPLATE)
        text = "\n".join(problems)
        for want in ('"## Summary" is empty', '"## Implementation and evidence" is empty',
                     '"## Verification" is empty', '"## Compatibility and limitations" is empty',
                     '"## AI assistance" is empty', "No linked issue", "claimed-first", "DCO"):
            self.assertIn(want, text)

    def test_empty_and_freeform_bodies_fail(self):
        self.assertTrue(c.check(""))
        self.assertTrue(c.check("Fixes a bug. Closes #3"))

    def test_missing_section_is_named(self):
        body = GOOD.replace("## Compatibility and limitations\n\nNone known.\n", "")
        self.assertEqual(c.check(body), ['Missing section "## Compatibility and limitations".'])

    def test_comment_marker_inside_fence_does_not_hide_following_sections(self):
        body = GOOD.replace(
            "## Compatibility and limitations",
            "```\nliteral <!-- inside fence\n```\n<!-- template note -->\n## Compatibility and limitations",
        )
        self.assertEqual(c.check(body), [])
        text, found = c.sections(body)
        self.assertIn("literal <!-- inside fence", found["verification"])
        for name in ("compatibility and limitations", "checklist", "ai assistance"):
            self.assertIn(name, found)

    def test_comment_around_fence_marker_does_not_hide_section(self):
        body = GOOD.replace(
            "## Compatibility and limitations",
            "<!--\n```\n-->\n## Compatibility and limitations",
        )
        self.assertEqual(c.check(body), [])

    def test_comment_inside_fence_is_preserved(self):
        body = GOOD.replace(
            "## Compatibility and limitations",
            "```\n<!-- evidence -->\n```\n## Compatibility and limitations",
        )
        self.assertEqual(c.check(body), [])
        self.assertIn("<!-- evidence -->", c.sections(body)[1]["verification"])

    def test_inline_code_comment_marker_does_not_hide_sections(self):
        body = GOOD.replace(
            "Touches crates/texture only.",
            "Touches crates/texture only. A literal `<!--` in inline code.",
        )
        self.assertEqual(c.check(body), [])

    def test_inline_code_comment_marker_keeps_real_comment_removed(self):
        body = GOOD.replace(
            "Touches crates/texture only.",
            "Touches crates/texture only. A literal `<!--` in inline code.\n<!-- template note -->",
        )
        self.assertEqual(c.check(body), [])
        self.assertNotIn("template note", c.sections(body)[0])

    def test_checklist_boxes_only_inside_fence_fail(self):
        body = GOOD.replace(
            "- [x] I claimed the linked issue before starting, and the claim was still mine when I opened this pull request.\n"
            "- [x] I reviewed the changes and disclosed material AI assistance below, if applicable.\n"
            "- [x] My commits are signed off under the DCO.",
            "```\n"
            "- [x] I claimed the linked issue before starting, and the claim was still mine when I opened this pull request.\n"
            "- [x] My commits are signed off under the DCO.\n"
            "```",
        )
        problems = c.check(body)
        self.assertIn('Section "## Checklist" is empty.', problems)
        self.assertIn("Checklist is missing the claimed-first checkbox.", problems)
        self.assertIn("Checklist is missing the DCO sign-off checkbox.", problems)

    def test_fenced_report_heading_does_not_replace_checklist(self):
        for fence in ("```", "~~~"):
            with self.subTest(fence=fence):
                body = GOOD + f"\n{fence}text\n## Checklist\nAll tests passed.\n{fence}\n"
                self.assertEqual(c.check(body), [])

    def test_fenced_heading_does_not_supply_missing_section(self):
        body = GOOD.replace(
            "## Compatibility and limitations\n\nNone known.",
            "```markdown\n## Compatibility and limitations\nNone known.\n```",
        )
        self.assertEqual(c.check(body), ['Missing section "## Compatibility and limitations".'])

    def test_real_heading_after_fence_and_code_only_verification(self):
        end = GOOD.index("## Compatibility and limitations")
        start = GOOD.index("| Check |")
        report = (
            "   ````text\n"
            "~~~\n"
            "```\n"
            "```` not a closing fence\n"
            "## Checklist\n"
            "All tests passed.\n"
            "  ````` \t\n\n"
        )
        body = GOOD[:start] + report + GOOD[end:]
        self.assertEqual(c.check(body), [])
        self.assertEqual(c.sections(body)[1]["verification"], report.strip())
        self.assertEqual(c.check(GOOD[:start] + GOOD[end:]), ['Section "## Verification" is empty.'])

    def test_unticked_boxes_fail(self):
        body = GOOD.replace("- [x] I claimed", "- [ ] I claimed").replace("- [x] My commits", "- [ ] My commits")
        problems = c.check(body)
        self.assertEqual(len(problems), 2)
        self.assertIn("claimed-first", problems[0])
        self.assertIn("DCO", problems[1])

    def test_issue_link_forms(self):
        for link in ("Fixes #4", "resolves #4", "Closed #4", "No linked issue: requested by the maintainer"):
            self.assertEqual(c.check(GOOD.replace("Closes #12", link)), [], link)
        self.assertIn("No linked issue", "\n".join(c.check(GOOD.replace("Closes #12", "See issue 12"))))

    def test_link_inside_comment_does_not_count(self):
        self.assertTrue(c.check(GOOD.replace("Closes #12", "<!-- Closes #12 -->")))

    def test_new_claim_checkbox_wording_passes(self):
        body = GOOD.replace("I claimed the linked issue before starting, and the claim was still mine when I opened this pull request.",
                            "I claimed the linked issue with a reply before starting, and nobody else held a live claim on it.")
        self.assertEqual(c.check(body), [])

    def test_cli_exit_codes_from_event_file(self):
        script = os.path.join(ROOT, "scripts", "pr_template_check.py")
        with tempfile.TemporaryDirectory() as d:
            for body, want in ((GOOD, 0), (TEMPLATE, 1), (None, 1)):
                path = os.path.join(d, "event.json")
                with open(path, "w", encoding="utf-8") as fh:
                    json.dump({"pull_request": {"body": body}}, fh)
                run = subprocess.run([sys.executable, script, "--event", path], capture_output=True, text=True)
                self.assertEqual(run.returncode, want, run.stdout)


if __name__ == "__main__":
    unittest.main()
