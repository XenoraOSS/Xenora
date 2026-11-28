# SPDX-License-Identifier: GPL-3.0-or-later
"""Thin gh wrapper. Arguments are always passed as lists, never through a shell."""
import json
import subprocess


def gh(*args):
    r = subprocess.run(["gh"] + list(args), capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError("gh %s failed: %s" % (" ".join(args[:2]), r.stderr.strip()[:300]))
    return r.stdout


def ndjson(out):
    return [json.loads(l) for l in out.splitlines() if l.strip()]


# Fields both the list and the single-pull-request query need. The commit date
# and the changed files are not in it: `gh pr list --json commits` exceeds
# GitHub's node limit with a handful of open pull requests, so `_pr_meta`
# gathers those two in one batched GraphQL request instead.
PR_FIELDS = "number,author,body,headRefOid,state"


def issue(repo, n):
    try:
        return json.loads(gh("issue", "view", str(n), "-R", repo, "--json", "number,title,body,state,labels"))
    except RuntimeError:
        return {"number": n, "title": "", "body": "", "state": "MISSING", "labels": []}


def comments(repo, n):
    jq = '.[] | {user: .user.login, body: .body, created_at: .created_at}'
    return ndjson(gh("api", "repos/%s/issues/%d/comments" % (repo, n), "--paginate", "--jq", jq))


def recent_comments(repo, since):
    """Issue and pull request comments updated since `since` (UTC ISO time), oldest first."""
    jq = ('.[] | {id: .id, user: .user.login, body: .body, created_at: .created_at, '
          'issue: (.issue_url | split("/") | last | tonumber), pull_request: (.html_url | test("/pull/"))}')
    return ndjson(gh("api", "repos/%s/issues/comments?sort=created&direction=asc&per_page=100&since=%s" % (repo, since),
                     "--paginate", "--jq", jq))


def open_issues(repo):
    return json.loads(gh("issue", "list", "-R", repo, "--state", "open", "--limit", "500",
                         "--json", "number,title,body,state,labels"))


def pr_data(repo, n, rows=None):
    """One pull request. `rows` reuses an already gathered `open_prs` result."""
    if rows:
        for row in rows:
            if row["number"] == n:
                return row
    p = json.loads(gh("pr", "view", str(n), "-R", repo, "--json", PR_FIELDS))
    meta = _pr_meta(repo, [n]).get(n, {"head_pushed_at": "", "files": []})
    return {"number": p["number"], "author": p["author"]["login"], "body": p["body"] or "",
            "head_sha": p["headRefOid"], "head_pushed_at": meta["head_pushed_at"],
            "files": meta["files"], "state": p["state"]}


def _pr_meta(repo, numbers):
    """Head commit date and changed files for `numbers`, in one GraphQL request.

    `gh pr list --json commits` asks for every commit author of every open pull
    request and trips GitHub's node limit once a handful are open, and one
    `gh api` call per pull request for the commit date and the changed files
    costs three requests each. Both fields are gathered for the whole list
    here instead. A pull request whose file list the API truncates falls back
    to the paginated REST call, so the returned lists stay complete.
    """
    if not numbers:
        return {}
    owner, name = repo.split("/", 1)
    parts = []
    for index, number in enumerate(numbers):
        parts.append(
            'p%d: repository(owner: %s, name: %s) { pullRequest(number: %d) { '
            'files(first: 100) { pageInfo { hasNextPage } nodes { path } } '
            'commits(last: 1) { nodes { commit { committedDate } } } } }' % (
                index, json.dumps(owner), json.dumps(name), number))
    data = json.loads(gh("api", "graphql", "-f", "query=query { %s }" % " ".join(parts)))["data"]
    meta = {}
    for index, number in enumerate(numbers):
        pr = (data.get("p%d" % index) or {}).get("pullRequest")
        if not pr:
            continue
        files = [node["path"] for node in pr["files"]["nodes"]]
        if pr["files"]["pageInfo"]["hasNextPage"]:
            files = gh("api", "repos/%s/pulls/%d/files" % (repo, number),
                       "--paginate", "--jq", ".[].filename").split()
        commits = pr["commits"]["nodes"]
        meta[number] = {"head_pushed_at": commits[0]["commit"]["committedDate"] if commits else "",
                        "files": files}
    return meta


def open_prs(repo):
    rows = json.loads(gh("pr", "list", "-R", repo, "--state", "open", "--limit", "100",
                         "--json", PR_FIELDS))
    meta = _pr_meta(repo, [p["number"] for p in rows])
    prs = []
    for p in rows:
        added = meta.get(p["number"])
        if added is None:
            # The batch refused or dropped this pull request: ask for its two
            # expensive fields alone rather than re-reading the cheap ones.
            added = _pr_meta(repo, [p["number"]]).get(
                p["number"], {"head_pushed_at": "", "files": []})
        prs.append({"number": p["number"], "author": p["author"]["login"], "body": p["body"] or "",
                    "head_sha": p["headRefOid"], "head_pushed_at": added["head_pushed_at"],
                    "files": added["files"], "state": p["state"]})
    return prs


def gather(repo, with_prs=True, with_comments=True, recent_since=""):
    """State for decide/audit/summary/replay: open tasks plus any dependency issues they name and,
    with `recent_since`, every comment since then."""
    from .tasks import parse_task
    issues = {i["number"]: i for i in open_issues(repo)}
    for i in list(issues.values()):
        for d in parse_task(i)["depends"]:
            if d not in issues:
                issues[d] = issue(repo, d)
    state = {"issues": list(issues.values())}
    if with_comments:
        state["task_comments"] = {str(n): comments(repo, n) for n, i in issues.items()
                                  if any((l["name"] if isinstance(l, dict) else l) == "agent:claimed" for l in i["labels"])}
    if with_prs:
        state["prs"] = open_prs(repo)
    if recent_since:
        state["recent_comments"] = recent_comments(repo, recent_since)
    return state


def apply_actions(repo, actions):
    for a in actions:
        n = str(a["issue"])
        for label in a["add_labels"]:
            gh("label", "create", label, "-R", repo, "--force", "--color", "BFD4F2")
        # Comment-only actions (for example claim refusals) must not call `gh issue edit`:
        # `["issue", "edit", n, "-R", repo]` is already five tokens, so a length check is not
        # enough to detect missing label flags and newer gh rejects a no-op edit.
        if a["add_labels"] or a["remove_labels"]:
            args = ["issue", "edit", n, "-R", repo]
            for l in a["add_labels"]:
                args += ["--add-label", l]
            for l in a["remove_labels"]:
                args += ["--remove-label", l]
            gh(*args)
        if a["comment"]:
            gh("issue", "comment", n, "-R", repo, "--body", a["comment"])
