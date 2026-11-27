#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later

"""Run Xenora's registered original fixtures with a host deadline."""

import argparse
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import subprocess
import sys
import tempfile
import time

if __package__:
    from .manifest import WORKLOADS
else:
    from manifest import WORKLOADS


ROOT = Path(__file__).resolve().parents[2]
OUTPUT_LIMIT = 32 * 1024
OUTPUT_HALF = OUTPUT_LIMIT // 2
DEFAULT_TIMEOUT = 300
RESULTS_DIR = ROOT / "artifacts" / "compat"
SCHEMA_VERSION = 1
STATUSES = {"passed", "failed", "timeout", "missing-tool", "launch-error"}


def capture(argv, timeout, env):
    """Drain both streams, retaining the beginning and end if over the limit."""
    started = time.monotonic()
    output = {name: bytearray() for name in ("stdout", "stderr")}
    truncated = dict.fromkeys(output, False)
    returncode = None
    detail = None
    try:
        process = subprocess.Popen(
            argv,
            cwd=ROOT,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except FileNotFoundError as error:
        status, detail = "missing-tool", str(error)
    except OSError as error:
        status, detail = "launch-error", str(error)
    else:
        deadline = started + timeout
        status = "timeout"
        try:
            with selectors.DefaultSelector() as selector:
                for name in output:
                    pipe = getattr(process, name)
                    os.set_blocking(pipe.fileno(), False)
                    selector.register(pipe, selectors.EVENT_READ, name)
                while time.monotonic() < deadline:
                    returncode = process.poll()
                    if not selector.get_map():
                        if returncode is not None:
                            status = "passed" if returncode == 0 else "failed"
                            break
                        remaining = max(0, deadline - time.monotonic())
                        try:
                            returncode = process.wait(timeout=remaining)
                            status = "passed" if returncode == 0 else "failed"
                        except subprocess.TimeoutExpired:
                            pass
                        break
                    remaining = max(0, deadline - time.monotonic())
                    for key, _ in selector.select(remaining):
                        try:
                            chunk = os.read(key.fd, 65536)
                        except BlockingIOError:
                            continue
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        name = key.data
                        retained = output[name]
                        retained.extend(chunk)
                        if len(retained) > OUTPUT_LIMIT:
                            del retained[OUTPUT_HALF:-OUTPUT_HALF]
                            truncated[name] = True
        finally:
            # Reap the direct child and terminate any surviving descendants still
            # in the command's process group, including those retaining a pipe.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.stdout.close()
            process.stderr.close()
            try:
                returncode = process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                detail = "Child did not exit within one second after SIGKILL"
        if status == "timeout" and detail is None:
            detail = "Host deadline expired"

    decoded = {}
    for name, data in output.items():
        if truncated[name]:
            # Decode the retained regions separately so bytes across the gap
            # cannot combine into a character that was absent from the stream.
            decoded[name] = data[:OUTPUT_HALF].decode("utf-8", errors="replace")
            decoded[name] += data[OUTPUT_HALF:].decode("utf-8", errors="replace")
        else:
            decoded[name] = data.decode("utf-8", errors="replace")
    return {
        "status": status,
        "returncode": returncode,
        "elapsed_seconds": round(time.monotonic() - started, 6),
        "detail": detail,
        **decoded,
        **{f"{name}_truncated": value for name, value in truncated.items()},
    }


def run_stage(name, argv, timeout, env, blocked_by=None):
    if blocked_by is None:
        outcome = capture(argv, timeout, env)
    else:
        outcome = {
            "status": "skipped",
            "returncode": None,
            "elapsed_seconds": 0,
            "detail": f"{blocked_by['name']} {blocked_by['status']}",
            "stdout": "",
            "stderr": "",
            "stdout_truncated": False,
            "stderr_truncated": False,
        }
    return {
        "name": name,
        "argv": list(argv),
        "timeout_seconds": timeout,
        **outcome,
    }


def run_workload(workload_id, workload, override, env):
    timeout = DEFAULT_TIMEOUT if override is None else override
    build = run_stage("host-build", workload["build_argv"], timeout, env)
    blocked_by = None if build["status"] == "passed" else build
    fixture = run_stage("fixture-command", workload["argv"], timeout, env, blocked_by)
    return {
        "id": workload_id,
        "suite": workload["suite"],
        "status": (blocked_by or fixture)["status"],
        "stages": [build, fixture],
    }


def context(env):
    commands = {
        "commit": ("git", "rev-parse", "HEAD"),
        "worktree_status": ("git", "status", "--porcelain", "--untracked-files=normal"),
        "cargo": ("cargo", "--version"),
        "rustc": ("rustc", "--version", "--verbose"),
    }
    return {
        "host": {"system": platform.system(), "version": platform.mac_ver()[0] or platform.release(),
                 "machine": platform.machine()},
        "python": platform.python_version(),
        "cargo_build_jobs": env["CARGO_BUILD_JOBS"],
        **{key: capture(command, 5, env) for key, command in commands.items()},
    }


def output_path(path):
    resolved = path.resolve()
    if RESULTS_DIR.resolve() != RESULTS_DIR or RESULTS_DIR not in resolved.parents:
        raise ValueError(f"Result output must be under {RESULTS_DIR}")
    if path.is_symlink() or resolved.exists():
        raise FileExistsError(f"Result output already exists: {path}")
    return resolved


def write_result(path, result):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=path.parent, delete=False
    ) as handle:
        temporary = Path(handle.name)
        try:
            json.dump(result, handle, indent=2, ensure_ascii=False, allow_nan=False)
            handle.write("\n")
            handle.flush()
            os.link(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def validate_item(item, key, allowed_statuses=STATUSES):
    if not isinstance(item, dict) or not isinstance(item.get(key), str) or not item[key]:
        raise ValueError(f"Invalid {key}")
    status = item.get("status")
    if not isinstance(status, str) or status not in allowed_statuses:
        raise ValueError(f"Invalid status for {item[key]}")


def read_result(path):
    with path.open(encoding="utf-8") as handle:
        result = json.load(handle)
    if (
        not isinstance(result, dict)
        or type(result.get("schema_version")) is not int
        or result["schema_version"] != SCHEMA_VERSION
    ):
        raise ValueError(f"Unsupported result schema in {path}")
    workloads = result.get("workloads")
    if not isinstance(workloads, list) or not workloads:
        raise ValueError(f"No workloads in {path}")
    metadata = result.get("context", {})
    if not isinstance(metadata, dict):
        raise ValueError(f"Invalid context in {path}")
    commit = metadata.get("commit", {})
    if not isinstance(commit, dict) or (
        commit.get("status") == "passed" and not isinstance(commit.get("stdout"), str)
    ):
        raise ValueError(f"Invalid commit metadata in {path}")
    workload_ids = set()
    for workload in workloads:
        validate_item(workload, "id")
        if workload["id"] in workload_ids:
            raise ValueError(f"Duplicate workload {workload['id']}")
        workload_ids.add(workload["id"])
        stages = workload.get("stages")
        if not isinstance(stages, list) or not stages:
            raise ValueError(f"No stages for {workload['id']}")
        stage_names = set()
        for stage in stages:
            validate_item(stage, "name", STATUSES | {"skipped"})
            if stage.get("detail") is not None and not isinstance(stage["detail"], str):
                raise ValueError(f"Invalid detail for {stage['name']}")
            if stage.get("returncode") is not None and type(stage["returncode"]) is not int:
                raise ValueError(f"Invalid return code for {stage['name']}")
            if stage["name"] in stage_names:
                raise ValueError(f"Duplicate stage {stage['name']}")
            stage_names.add(stage["name"])
        if [stage["name"] for stage in stages] == ["host-build", "fixture-command"]:
            build, fixture = (stage["status"] for stage in stages)
            if build == "skipped" or (build == "passed") == (fixture == "skipped"):
                raise ValueError(f"Inconsistent stage outcome for {workload['id']}")
            expected_status = fixture if build == "passed" else build
        elif len(stages) == 1 and stages[0]["name"] == "fixture-command":
            expected_status = stages[0]["status"]
        else:
            raise ValueError(f"Invalid stage sequence for {workload['id']}")
        if workload["status"] != expected_status:
            raise ValueError(f"Inconsistent stage outcome for {workload['id']}")
    return result


def report(result):
    commit = result.get("context", {}).get("commit", {})
    if commit.get("status") == "passed":
        print(f"Commit {commit['stdout'].strip()}")
    for workload in result["workloads"]:
        print(f"{workload['id']}: {workload['status']}")
        for stage in workload["stages"]:
            detail = stage.get("detail")
            if detail is None and stage.get("returncode") is not None:
                detail = f"host exit {stage['returncode']}"
            suffix = f" ({detail})" if detail else ""
            print(f"  {stage['name']}: {stage['status']}{suffix}")


def report_markdown(result):
    metadata = result.get("context", {})
    commit = metadata.get("commit", {})
    revision = commit["stdout"].strip() if commit.get("status") == "passed" else ""
    commit_label = "unavailable"
    if re.fullmatch(r"[a-f0-9]{40}(?:[a-f0-9]{24})?", revision):
        commit_label = revision

    worktree = metadata.get("worktree_status")
    worktree_label = "unavailable"
    if isinstance(worktree, dict) and worktree.get("status") == "passed":
        status_output = worktree.get("stdout")
        if isinstance(status_output, str):
            worktree_label = "dirty" if status_output else "clean"

    print("# Xenora fixture report\n")
    print(f"- Commit: {commit_label}")
    print(f"- Worktree: {worktree_label}\n")
    print("The fixture-command status describes the host test command as a whole.\n")
    print("| Workload | Stage | Status | Host exit code | Timeout (s) |")
    print("| --- | --- | --- | ---: | ---: |")
    for workload in result["workloads"]:
        workload_id = workload["id"]
        workload_label = "unavailable"
        if re.fullmatch(r"[a-zA-Z0-9_-]+", workload_id):
            workload_label = f"`{workload_id}`"
        for stage in workload["stages"]:
            returncode = stage.get("returncode")
            exit_label = "unavailable" if returncode is None else str(returncode)
            timeout = stage.get("timeout_seconds")
            timeout_label = "unavailable"
            if type(timeout) in (float, int) and 0 < timeout <= 3600:
                timeout_label = str(timeout)
            print(
                f"| {workload_label} | {stage['name']} | {stage['status']} "
                f"| {exit_label} | {timeout_label} |"
            )


def compare(baseline, candidate):
    def statuses(result):
        return {
            (workload["id"], stage["name"]): stage["status"]
            for workload in result["workloads"]
            for stage in workload["stages"]
        }

    before, after = statuses(baseline), statuses(candidate)
    if before.keys() != after.keys():
        missing = ", ".join(f"{workload}/{stage}" for workload, stage in sorted(before.keys() - after.keys()))
        extra = ", ".join(f"{workload}/{stage}" for workload, stage in sorted(after.keys() - before.keys()))
        raise ValueError(f"Result scope differs (missing: {missing or 'none'}; extra: {extra or 'none'})")
    regression = False
    changes = 0
    for key in sorted(before):
        old, new = before[key], after[key]
        if old == new:
            continue
        changes += 1
        regressed = old == "passed" and new != "passed"
        regression |= regressed
        suffix = " (regression)" if regressed else ""
        print(f"{key[0]}/{key[1]}: {old} -> {new}{suffix}")
    if not changes:
        print("No stage status changes")
    return int(regression)


def timeout_seconds(raw):
    try:
        value = float(raw)
    except ValueError:
        raise argparse.ArgumentTypeError("expected seconds as a number") from None
    if not math.isfinite(value) or not 0 < value <= 3600:
        raise argparse.ArgumentTypeError("expected a timeout greater than 0 and at most 3600 seconds")
    return value


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    suites = sorted({workload["suite"] for workload in WORKLOADS.values()})
    listing = actions.add_parser("list", help="list registered fixtures")
    listing.add_argument("--suite", choices=suites)
    run = actions.add_parser("run", help="run selected fixtures and save a result")
    run.add_argument("workloads", metavar="WORKLOAD", nargs="*")
    run.add_argument("--suite", choices=suites)
    run.add_argument(
        "--timeout", type=timeout_seconds, metavar="SECONDS", help="deadline per stage"
    )
    run.add_argument("--output", type=Path, metavar="FILE")
    summary = actions.add_parser("report", help="summarize a saved result")
    summary.add_argument("result", type=Path)
    summary.add_argument("--format", choices=("text", "markdown"), default="text")
    comparison = actions.add_parser("compare", help="compare stage outcomes")
    comparison.add_argument("baseline", type=Path)
    comparison.add_argument("candidate", type=Path)
    args = parser.parse_args(argv)

    try:
        if args.action == "report":
            result = read_result(args.result)
            if args.format == "markdown":
                report_markdown(result)
            else:
                report(result)
            return 0
        if args.action == "compare":
            return compare(read_result(args.baseline), read_result(args.candidate))

        selected = {
            key: workload for key, workload in WORKLOADS.items()
            if args.suite is None or args.suite == workload["suite"]
        }
        if args.action == "list":
            for key, workload in selected.items():
                print(f"{key} [{workload['suite']}]  {workload['description']}")
            return 0
        if args.workloads:
            unknown = sorted(set(args.workloads) - selected.keys())
            if unknown:
                parser.error(f"unknown workload in selection: {', '.join(unknown)}")
            selected = {key: selected[key] for key in args.workloads}

        started = datetime.now(timezone.utc)
        result_path = output_path(
            args.output or RESULTS_DIR / started.strftime("%Y%m%dT%H%M%S.%fZ.json")
        )
        env = os.environ.copy()
        try:
            build_jobs = int(env.get("CARGO_BUILD_JOBS", "2"))
        except ValueError:
            raise ValueError("CARGO_BUILD_JOBS must be a positive integer") from None
        if build_jobs < 1:
            raise ValueError("CARGO_BUILD_JOBS must be a positive integer")
        env["CARGO_BUILD_JOBS"] = str(min(build_jobs, 4))
        result = {
            "schema_version": SCHEMA_VERSION,
            "started_at": started.isoformat(),
            "context": context(env),
            "workloads": [
                run_workload(key, workload, args.timeout, env) for key, workload in selected.items()
            ],
        }
        write_result(result_path, result)
        report(result)
        print(f"Result saved to {result_path}")
        return int(any(workload["status"] != "passed" for workload in result["workloads"]))
    except (OSError, ValueError) as error:
        parser.exit(2, f"{parser.prog}: {error}\n")


if __name__ == "__main__":
    sys.exit(main())
