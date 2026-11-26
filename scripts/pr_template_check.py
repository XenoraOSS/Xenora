# SPDX-License-Identifier: GPL-3.0-or-later
"""Check that a pull request description follows .github/PULL_REQUEST_TEMPLATE.md.

Standard library only. The description is read as data from the GitHub event
file (or a plain text file with --body-file); it is never executed.
Exit status 0 when the description passes, 1 when something is missing.
"""
import argparse
import json
import re
import sys

SECTIONS = (
    "Summary",
    "Implementation and evidence",
    "Verification",
    "Compatibility and limitations",
    "Checklist",
    "AI assistance",
)
# Checklist items that must be ticked, found by the start of their text.
REQUIRED_TICKS = (
    ("I claimed the linked issue", "the claimed-first checkbox"),
    ("My commits are signed off under the DCO", "the DCO sign-off checkbox"),
)
ISSUE_LINK = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#\d+", re.I)
NO_ISSUE = re.compile(r"^\s*No linked issue:\s*\S", re.I | re.M)
EMPTY_LINK = re.compile(r"^\s*(?:close|fix|resolve)\w*\s*#\s*$", re.I)
HEADING = re.compile(r"##[ \t]+(.+?)[ \t]*")
FENCE = re.compile(r" {0,3}(`{3,}|~{3,})(.*)")
INLINE_CODE = re.compile(r"(`+)(.*?)\1")


def hide_inline_code(line):
    """Replace inline code spans with spaces, preserving offsets."""
    return INLINE_CODE.sub(lambda match: " " * len(match.group(0)), line)


def strip_fenced_blocks(text):
    """Return text outside top-level fenced blocks, preserving line breaks."""
    out = []
    fence = ""
    for raw in text.splitlines(keepends=True):
        line = raw.removesuffix("\n")
        newline = "\n" if raw.endswith("\n") else ""
        marker = FENCE.fullmatch(line)
        if fence:
            if marker:
                run, rest = marker.groups()
                if run.startswith(fence) and not rest.strip(" \t"):
                    fence = ""
            out.append(newline)
            continue
        if marker:
            run, rest = marker.groups()
            if run[0] == "~" or "`" not in rest:
                fence = run
                out.append(newline)
                continue
        out.append(line + newline)
    return "".join(out)


def strip_comments_outside_fences(body):
    """Remove HTML comments that are not inside a top-level code fence.

    A comment inside a fenced block is evidence text, not Markdown. A fence
    marker inside an HTML comment is part of that comment and does not open a
    fence. Removed comment characters are replaced by their line breaks so the
    remaining heading offsets stay stable.
    """
    text = body.replace("\r\n", "\n")
    out = []
    fence = ""
    in_comment = False
    for raw in text.splitlines(keepends=True):
        line = raw.removesuffix("\n")
        newline = "\n" if raw.endswith("\n") else ""
        if in_comment:
            end = line.find("-->")
            if end == -1:
                out.append(newline)
                continue
            in_comment = False
            line = line[end + 3:]
        if fence:
            out.append(line + newline)
            marker = FENCE.fullmatch(line)
            if marker:
                run, rest = marker.groups()
                if run.startswith(fence) and not rest.strip(" \t"):
                    fence = ""
            continue
        cleaned = []
        search = hide_inline_code(line)
        start = 0
        while True:
            comment = search.find("<!--", start)
            if comment == -1:
                cleaned.append(line[start:])
                break
            cleaned.append(line[start:comment])
            end = line.find("-->", comment + 4)
            if end == -1:
                in_comment = True
                break
            start = end + 3
        cleaned_line = "".join(cleaned)
        out.append(cleaned_line + newline)
        marker = FENCE.fullmatch(cleaned_line)
        if marker:
            run, rest = marker.groups()
            if run[0] == "~" or "`" not in rest:
                fence = run
    return "".join(out)


def section_headings(text):
    """Yield heading offsets and names outside top-level code fences."""
    fence = ""
    offset = 0
    for raw in text.splitlines(keepends=True):
        start, offset = offset, offset + len(raw)
        line = raw.rstrip("\n")
        marker = FENCE.fullmatch(line)
        if fence:
            if marker:
                run, rest = marker.groups()
                if run.startswith(fence) and not rest.strip(" \t"):
                    fence = ""
            continue
        if marker:
            run, rest = marker.groups()
            if run[0] == "~" or "`" not in rest:
                fence = run
                continue
        heading = HEADING.fullmatch(line)
        if heading:
            yield start, offset, heading[1].strip().lower()


def sections(body):
    """Map each level-2 heading to its text, with outside comments removed."""
    text = strip_comments_outside_fences(body)
    found = {}
    marks = list(section_headings(text))
    for i, (_, content_start, name) in enumerate(marks):
        end = marks[i + 1][0] if i + 1 < len(marks) else len(text)
        found[name] = text[content_start:end].strip()
    return text, found


def has_content(name, value):
    if name == "Verification":
        # The empty table from the template does not count: some cell must be filled.
        rows = [r for r in value.splitlines() if r.strip().startswith("|")]
        filled = [r for r in rows[2:] if [c for c in r.strip().strip("|").split("|")[1:] if c.strip()]]
        prose = [r for r in value.splitlines() if r.strip() and not r.strip().startswith("|")]
        return bool(filled or prose)
    if name == "Checklist":
        return "- [" in strip_fenced_blocks(value)
    # A bare "Closes #" left over from the template is not content.
    rest = [l for l in value.splitlines() if l.strip() and not EMPTY_LINK.match(l)]
    return bool(rest)


def check(body):
    """Return a list of human-readable problems; empty means the body passes."""
    if not body or not body.strip():
        return ["The description is empty. Copy .github/PULL_REQUEST_TEMPLATE.md into it and fill it in."]
    text, found = sections(body)
    problems = []
    for name in SECTIONS:
        if name.lower() not in found:
            problems.append('Missing section "## %s".' % name)
        elif not has_content(name, found[name.lower()]):
            problems.append('Section "## %s" is empty.' % name)
    if not ISSUE_LINK.search(text) and not NO_ISSUE.search(text):
        problems.append('No linked issue: add "Closes #<number>", or a line "No linked issue: <reason>".')
    checklist = strip_fenced_blocks(found.get("checklist", ""))
    for prefix, label in REQUIRED_TICKS:
        item = re.search(r"^\s*[-*]\s*\[( |x|X)\]\s*" + re.escape(prefix), checklist, re.M)
        if not item:
            problems.append("Checklist is missing %s." % label)
        elif item.group(1) == " ":
            problems.append("Tick %s." % label)
    return problems


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--event", help="GitHub event JSON file (pull_request or pull_request_target)")
    src.add_argument("--body-file", help="file holding the description text")
    args = ap.parse_args(argv)
    if args.event:
        with open(args.event, encoding="utf-8") as fh:
            body = (json.load(fh).get("pull_request") or {}).get("body") or ""
    else:
        with open(args.body_file, encoding="utf-8") as fh:
            body = fh.read()
    problems = check(body)
    if problems:
        print("The pull request description does not follow .github/PULL_REQUEST_TEMPLATE.md:")
        for p in problems:
            print("- " + p)
        print("Edit the description to fix this; the check runs again on every edit.")
        return 1
    print("The pull request description follows the template.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
