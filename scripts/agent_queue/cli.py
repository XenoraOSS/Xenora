# SPDX-License-Identifier: GPL-3.0-or-later
"""Command line entry: PYTHONPATH=scripts python3 -m agent_queue <subcommand>. JSON in, JSON out."""
import argparse
import json
import sys
from datetime import datetime, timedelta, timezone

from . import config as cfgmod
from . import ghio
from . import lease as L
from . import prcheck, queue, replay
from . import tasks as T
from . import decide as decide_mod
from .decide import decide
from .patterns import path_matches


def _load(path):
    with open(path, encoding="utf-8") if path != "-" else sys.stdin as fh:
        return json.load(fh)


def _now(arg):
    return T.parse_time(arg) if arg else datetime.now(timezone.utc)


def _out(obj):
    json.dump(obj, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")


def main(argv):
    p = argparse.ArgumentParser(prog="agent_queue")
    p.add_argument("--config", default=".github/agent-queue.json")
    p.add_argument("--now", default="", help="UTC time YYYY-MM-DDTHH:MM:SSZ (tests)")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("gather"); s.add_argument("--repo", required=True); s.add_argument("--no-prs", action="store_true"); s.add_argument("--pr", type=int, default=0)
    s.add_argument("--recent-commands", action="store_true", help="include comments from the last 2 x lease_hours for replay")
    s = sub.add_parser("replay"); s.add_argument("--state", required=True)
    s = sub.add_parser("decide"); s.add_argument("--event", required=True); s.add_argument("--state", required=True)
    s = sub.add_parser("should-rerun"); s.add_argument("--actions", required=True)
    s = sub.add_parser("apply"); s.add_argument("--repo", required=True); s.add_argument("--actions", required=True)
    s = sub.add_parser("audit"); s.add_argument("--state", required=True)
    s = sub.add_parser("summary"); s.add_argument("--state", required=True); s.add_argument("--text", action="store_true")
    s.add_argument("--now", default="", help="UTC time YYYY-MM-DDTHH:MM:SSZ (overrides parent --now)")
    s = sub.add_parser("pr-check"); s.add_argument("--state", required=True)
    s = sub.add_parser("acceptance"); s.add_argument("--issue", required=True); s.add_argument("--comments", required=True)
    s = sub.add_parser("task"); s.add_argument("--issue", required=True)
    s = sub.add_parser("touch"); s.add_argument("--state", required=True); s.add_argument("--login", required=True)
    s = sub.add_parser("fetch-issue"); s.add_argument("--repo", required=True); s.add_argument("--issue", type=int, required=True)
    s = sub.add_parser("claim-status"); s.add_argument("--data", required=True); s.add_argument("--login", required=True); s.add_argument("--skip", type=int, default=0)
    s = sub.add_parser("list"); s.add_argument("--state", required=True); s.add_argument("--area", default=""); s.add_argument("--json", action="store_true")
    s = sub.add_parser("next"); s.add_argument("--state", required=True); s.add_argument("--area", required=True); s.add_argument("--login", required=True)
    s = sub.add_parser("check-body"); s.add_argument("--file", required=True)
    s = sub.add_parser("event"); s.add_argument("--path", required=True)
    s = sub.add_parser("config-get"); s.add_argument("key")
    a = p.parse_args(argv)
    cfg = cfgmod.load(a.config)

    if a.cmd == "event":
        # normalise a GitHub webhook payload file (issue_comment or pull_request_target)
        ev = _load(a.path)
        item = ev.get("issue") or ev.get("pull_request") or {}
        who = (ev.get("comment") or {}).get("user") or item.get("user") or {}
        _out({"kind": "pr" if ("pull_request" in ev or "pull_request" in item) else "issue",
              "number": item.get("number"), "actor": who.get("login", ""),
              "body": (ev.get("comment") or {}).get("body", "")})
    elif a.cmd == "config-get":
        v = cfg[a.key]
        print(v if isinstance(v, (str, int, float)) else json.dumps(v))
    elif a.cmd == "gather":
        since = ""
        if a.recent_commands:
            since = (_now(a.now) - timedelta(hours=replay.window_hours(cfg))).strftime("%Y-%m-%dT%H:%M:%SZ")
        st = ghio.gather(a.repo, with_prs=not a.no_prs, recent_since=since)
        if a.pr:
            st["pr"] = ghio.pr_data(a.repo, a.pr, st.get("prs"))
            st["pr_comments"] = ghio.comments(a.repo, a.pr)
        _out(st)
    elif a.cmd == "decide":
        _out(decide(_load(a.event), _load(a.state), cfg))
    elif a.cmd == "replay":
        _out(replay.replay(_load(a.state), cfg, _now(a.now)))
    elif a.cmd == "should-rerun":
        # exit 0 only for an accepted /verified of the current head
        return 0 if decide_mod.rerun_check(_load(a.actions)) else 1
    elif a.cmd == "apply":
        ghio.apply_actions(a.repo, _load(a.actions))
    elif a.cmd == "audit":
        _out(queue.audit(_load(a.state), cfg, _now(a.now)))
    elif a.cmd == "summary":
        res = queue.summary(_load(a.state), a.now)
        if a.text:
            for r in res["tasks"]:
                print("#%d\t%s\t%s\t%s\t%s\t%s" % (r["number"], r["state"], r["area"], r["priority"], r["holder"] or "-", r["title"]))
        else:
            _out(res)
    elif a.cmd == "pr-check":
        res = prcheck.check_pr(_load(a.state), cfg, _now(a.now))
        _out(res)
        return 0 if res["ok"] else 1
    elif a.cmd == "task":
        _out(T.parse_task(_load(a.issue)))
    elif a.cmd == "acceptance":
        task = T.parse_task(_load(a.issue))
        want = L.approved_hash(_load(a.comments))
        if not T.normalise_acceptance(task["acceptance"]) or want != T.acceptance_hash(task["acceptance"]):
            sys.stderr.write("not authorized: no maintainer approval matches the current acceptance command\n")
            return 3
        print(T.normalise_acceptance(task["acceptance"]))
    elif a.cmd == "fetch-issue":
        _out({"issues": [ghio.issue(a.repo, a.issue)], "comments": ghio.comments(a.repo, a.issue)})
    elif a.cmd == "claim-status":
        data = _load(a.data)
        task = T.parse_task(data["issues"][0])
        comments = data["comments"]
        if task["holder"] == a.login and "agent:claimed" in task["labels"] and L.latest_grant(comments, task["number"], a.login):
            print("granted")
            return 0
        for c in reversed(comments[a.skip:]):
            if c["user"] in L.BOT and c["body"].startswith("Claim refused for %s:" % a.login):
                print(c["body"].split("\n", 1)[0])
                return 1
        print("pending")
        return 2
    elif a.cmd == "list":
        tasks = decide_mod.task_index(_load(a.state))
        rows = [t for t in tasks.values() if t["state"] == "OPEN" and "agent:ready" in t["labels"]
                and (not a.area or t["area"] == a.area)]
        rows.sort(key=lambda t: (cfgmod.PRIORITIES.index(t["priority"]), t["number"]))
        if a.json:
            _out([{k: t[k] for k in ("number", "title", "area", "priority", "depends", "files")} for t in rows])
        else:
            for t in rows:
                print("#%d\t%s\t%s\t%s" % (t["number"], t["area"], t["priority"], t["title"]))
    elif a.cmd == "next":
        tasks = decide_mod.task_index(_load(a.state))
        rows = sorted((t for t in tasks.values() if t["state"] == "OPEN" and t["area"] == a.area),
                      key=lambda t: (cfgmod.PRIORITIES.index(t["priority"]), t["number"]))
        for t in rows:
            if not decide_mod.claim_problem(t, tasks, cfg, a.login):
                print(t["number"])
                return 0
        sys.stderr.write("no claimable task in area %s\n" % a.area)
        return 1
    elif a.cmd == "check-body":
        with open(a.file, encoding="utf-8") as fh:
            t = T.parse_task({"number": 0, "body": fh.read()})
        if not t["area"] or not t["files"] or not T.normalise_acceptance(t["acceptance"]):
            sys.stderr.write("body needs a valid Area, Files and Acceptance command section\n")
            return 1
        print("area:" + t["area"])
        print("priority:" + t["priority"])
    elif a.cmd == "touch":
        # stdin lines "<sha>\t<path>"; report those touching patterns of tasks held by login
        state, hits = _load(a.state), []
        held = [T.parse_task(i) for i in state["issues"]]
        held = [t for t in held if t["holder"] == a.login and t["state"] == "OPEN"]
        for line in sys.stdin.read().splitlines():
            sha, _, path = line.partition("\t")
            for t in held:
                if any(path_matches(path, pat) for pat in t["files"]):
                    hits.append({"commit": sha, "path": path, "task": t["number"]})
        _out(hits)
    return 0
