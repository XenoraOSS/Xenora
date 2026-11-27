# SPDX-License-Identifier: GPL-3.0-or-later
"""Check GPU-gate orchestration with stubs, not native GPU correctness."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
LOADER_TARGETS = (
    "graphics_fences",
    "guest_graphics_fences",
    "guest_graphics_path",
    "guest_textured_path",
    "hosted_graphics_path",
    "shared_resource_lifecycle",
    "threaded_guest_application",
)
VALIDATION_VARIABLES = (
    "MTL_DEBUG_LAYER",
    "MTL_SHADER_VALIDATION",
    "MTL_SHADER_VALIDATION_REPORT_TO_STDERR",
    "MTL_SHADER_VALIDATION_ABORT_ON_FAULT",
)

COMMAND_STUB = """import json
import os
import sys

kind, *args = sys.argv[1:]
with open(os.environ["TEST_GPU_LOG"], "a", encoding="utf-8") as stream:
    json.dump({
        "kind": kind,
        "args": args,
        "cwd": os.getcwd(),
        "env": {key: value for key, value in os.environ.items()
                if key.startswith("MTL_") or key == "CARGO_BUILD_JOBS"},
    }, stream)
    stream.write("\\n")

target = args[args.index("--test") + 1] if "--test" in args else ""
phase = "timeout" if kind == "timeout" else (
    "build" if "--no-run" in args else "run"
)
if target == os.environ["TEST_GPU_FAIL_TARGET"] and phase == os.environ["TEST_GPU_FAIL_PHASE"]:
    sys.exit(int(os.environ["TEST_GPU_FAIL_STATUS"]))
if kind == "timeout":
    os.execvp(args[1], args[1:])
"""


class GpuGateTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name).resolve()
        self.checkout = self.root / "checkout with spaces"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.checkout / "scripts/lib").mkdir(parents=True)
        for relative in ("scripts/test-gpu.sh", "scripts/lib/timeout.sh"):
            shutil.copyfile(ROOT / relative, self.checkout / relative)
        self.log = self.root / "commands.jsonl"
        helper = self.root / "record_command.py"
        helper.write_text(COMMAND_STUB, encoding="utf-8")
        for name in ("cargo", "timeout"):
            tool = self.bin / name
            command = shlex.join([sys.executable, str(helper), name])
            tool.write_text(f'#!/bin/sh\nexec {command} "$@"\n', encoding="utf-8")
            tool.chmod(0o755)

    def run_gate(self, *, fail_target="", fail_phase="", fail_status=1,
                 jobs="", semantic_timeout=""):
        self.log.unlink(missing_ok=True)
        result = subprocess.run(
            ["sh", str(self.checkout / "scripts/test-gpu.sh")],
            cwd=self.root,
            env={
                **os.environ,
                **{key: "0" for key in VALIDATION_VARIABLES},
                "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
                "TEST_GPU_LOG": str(self.log),
                "TEST_GPU_FAIL_TARGET": fail_target,
                "TEST_GPU_FAIL_PHASE": fail_phase,
                "TEST_GPU_FAIL_STATUS": str(fail_status),
                "CARGO_BUILD_JOBS": jobs,
                "XENORA_SEMANTIC_TIMEOUT": semantic_timeout,
            },
            capture_output=True,
            encoding="utf-8",
            timeout=30,
            check=False,
        )
        records = [json.loads(line) for line in self.log.read_text().splitlines()]
        return result, records

    def cargo_args(self, target, *, build=False):
        args = ["test", "--locked", "-p", "xenora-loader",
                "--features", "gpu-tests", "--test", target]
        return args + (["--no-run"] if build else
                       ["--", "--nocapture", "--test-threads=1"])

    def test_native_loader_targets_are_built_then_run_serially(self):
        result, records = self.run_gate()

        self.assertEqual(result.returncode, 0, result.stderr)
        for target in LOADER_TARGETS:
            with self.subTest(target=target):
                invocations = [
                    (index, record) for index, record in enumerate(records)
                    if record["kind"] == "cargo" and target in record["args"]
                ]
                self.assertEqual(len(invocations), 2)
                (build_index, build), (run_index, run) = invocations
                self.assertEqual(build["args"], self.cargo_args(target, build=True))
                self.assertEqual(run["args"], self.cargo_args(target))
                self.assertEqual(run_index, build_index + 2)
                timeout = records[run_index - 1]
                self.assertEqual(timeout["kind"], "timeout")
                self.assertEqual(timeout["args"], ["30s", "cargo"] + run["args"])
        for record in records:
            self.assertEqual(record["cwd"], str(self.checkout))
            self.assertEqual(record["env"]["CARGO_BUILD_JOBS"], "4")
            for variable in VALIDATION_VARIABLES:
                self.assertEqual(record["env"][variable], "1")
        self.assertEqual(records[-2]["args"][0], "300s")

    def test_build_jobs_and_semantic_timeout_remain_configurable(self):
        result, records = self.run_gate(jobs="2", semantic_timeout="75s")

        self.assertEqual(result.returncode, 0, result.stderr)
        for record in records:
            self.assertEqual(record["env"]["CARGO_BUILD_JOBS"], "2")
        self.assertEqual(records[-2]["kind"], "timeout")
        self.assertEqual(records[-2]["args"][0], "75s")

    def test_compile_failure_stops_the_gate_before_running_target(self):
        for target in LOADER_TARGETS:
            with self.subTest(target=target):
                result, records = self.run_gate(
                    fail_target=target, fail_phase="build", fail_status=37,
                )
                self.assertEqual(result.returncode, 37, result.stderr)
                self.assertEqual(records[-1]["kind"], "cargo")
                self.assertEqual(records[-1]["args"], self.cargo_args(target, build=True))

    def test_native_test_failure_stops_the_gate(self):
        for target in LOADER_TARGETS:
            with self.subTest(target=target):
                result, records = self.run_gate(
                    fail_target=target, fail_phase="run", fail_status=42,
                )
                self.assertEqual(result.returncode, 42, result.stderr)
                self.assertEqual(records[-1]["kind"], "cargo")
                self.assertEqual(records[-1]["args"], self.cargo_args(target))

    def test_timeout_failure_stops_the_gate(self):
        target = "shared_resource_lifecycle"
        result, records = self.run_gate(
            fail_target=target, fail_phase="timeout", fail_status=124,
        )

        self.assertEqual(result.returncode, 124, result.stderr)
        self.assertEqual(records[-1]["kind"], "timeout")
        self.assertEqual(records[-1]["args"], ["30s", "cargo"] + self.cargo_args(target))