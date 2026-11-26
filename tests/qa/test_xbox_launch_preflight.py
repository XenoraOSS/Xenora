# SPDX-License-Identifier: GPL-3.0-or-later
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/xbox-launch-preflight.py"
SPEC = importlib.util.spec_from_file_location("xbox_launch_preflight", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
preflight = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(preflight)


def manifest(*executables: str, fields: str = "") -> bytes:
    return (
        '<Game configVersion="1">' + fields + "<ExecutableList>"
        + "".join(executables) + "</ExecutableList></Game>"
    ).encode()


SCARLETT = '<Executable Name="large-startup.exe" TargetDeviceFamily="Scarlett"/>'


class ManifestTests(unittest.TestCase):
    def test_fields_are_declarations_and_omissions_stay_null(self):
        config = preflight.parse_config(
            manifest(
                '<Executable Name="bin\\Launch.EXE" TargetDeviceFamily="Scarlett"'
                ' Id="Fixture.Launch" IsDevOnly="1" />',
                fields='<Identity Name="Local&amp;Fixture" /><TitleId>FFFFFFFF</TitleId>',
            )
        )
        selected = preflight.select_executable(config, "bin/launch.exe")
        self.assertEqual(config["identity"], {"Name": "Local&Fixture"})
        self.assertEqual(config["title_id"], "FFFFFFFF")
        self.assertEqual(
            selected,
            {
                "name": "bin/Launch.EXE",
                "target_device_family": "Scarlett",
                "id": "Fixture.Launch",
                "is_dev_only": True,
                "architecture": None,
            },
        )
        absent = preflight.parse_config(manifest(SCARLETT))
        self.assertIsNone(absent["identity"])
        self.assertIsNone(absent["title_id"])
        self.assertIsNone(preflight.select_executable(absent, None)["id"])

    def test_selection_requires_one_declared_scarlett_image(self):
        config = preflight.parse_config(
            manifest(SCARLETT, '<Executable Name="other.exe" TargetDeviceFamily="PC"/>')
        )
        for name, code in (
            (None, "ambiguous_executable"),
            ("absent.exe", "executable_not_declared"),
            ("other.exe", "not_scarlett"),
        ):
            with self.subTest(name=name), self.assertRaises(preflight.PreflightError) as failure:
                preflight.select_executable(config, name)
                self.fail("selection succeeded")
            self.assertEqual(failure.exception.code, code)
        self.assertEqual(
            preflight.select_executable(config, "large-startup.exe")["name"], "large-startup.exe"
        )
        for declaration, code in (
            ('<Executable Name="game.exe"/>', "not_scarlett"),
            ('<Executable Name="game.exe" TargetDeviceFamily="XboxOne"/>', "not_scarlett"),
            (
                '<Executable Name="game.exe" TargetDeviceFamily="Scarlett" Architecture="ARM64"/>',
                "not_x64",
            ),
        ):
            config = preflight.parse_config(manifest(declaration))
            with self.assertRaises(preflight.PreflightError) as failure:
                preflight.select_executable(config, None)
            self.assertEqual(failure.exception.code, code)

    def test_rejects_invalid_paths_xml_and_ambiguous_declarations(self):
        cases = [
            (b"<Game>", "unsupported_config_version"),
            (b'<Game configVersion="1">', "invalid_xml"),
            (b'<!DOCTYPE Game SYSTEM "file:///somewhere"><Game/>', "unsupported_xml"),
            (manifest(SCARLETT, '<Executable Name="LARGE-STARTUP.EXE"/>'), "duplicate_executable"),
            (manifest(fields="<TitleId>123</TitleId>"), "invalid_title_id"),
            (manifest(fields="<Identity/><Identity/>"), "duplicate_element"),
            (manifest(fields="<A>" * 33 + "</A>" * 33), "xml_too_deep"),
        ]
        for name in ("../game.exe", "/game.exe", "C:\\game.exe", "abc/./game.exe", "abc//game.exe"):
            cases.append((manifest(f'<Executable Name="{name}"/>'), "invalid_executable_name"))
        for contents, code in cases:
            with self.subTest(code=code, contents=contents), self.assertRaises(
                preflight.PreflightError
            ) as failure:
                preflight.parse_config(contents)
                self.fail("invalid declaration was accepted")
            self.assertEqual(failure.exception.code, code)

    def test_refuses_symlink_escape_and_reports_first_failed_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "fixture/MicrosoftGame.config"
            config.parent.mkdir()
            config.write_bytes(manifest(SCARLETT))
            outside = root / "outside.exe"
            outside.write_bytes(b"MZ")
            (config.parent / "large-startup.exe").symlink_to(outside)
            report, status = preflight.preflight(config, Path("/unused"), None)
            self.assertEqual(status, 1)
            self.assertEqual(report["events"], ["manifest", "executable_selection"])
            self.assertEqual(report["outcome"]["stage"], "executable_read")
            self.assertEqual(report["outcome"]["code"], "outside_fixture_root")


@unittest.skipUnless(
    os.getenv("XENORA_STARTUP_FIXTURES") and os.getenv("XENORA_STARTUP_CLI"),
    "set XENORA_STARTUP_FIXTURES and XENORA_STARTUP_CLI for native integration",
)
class NativeStartupTests(unittest.TestCase):
    fixtures = Path(os.getenv("XENORA_STARTUP_FIXTURES", ".")).resolve()
    xenora = Path(os.getenv("XENORA_STARTUP_CLI", ".")).resolve()

    def run_guest(self, fixture: Path, *options: str) -> tuple[subprocess.CompletedProcess, dict]:
        result = subprocess.run(
            [self.xenora, "run", "--experimental", "--pe", fixture, "--startup-report",
             "--trace", *options],
            capture_output=True, text=True, timeout=25, check=False,
        )
        lines = [line.removeprefix("xenora-startup ") for line in result.stderr.splitlines()
                 if line.startswith("xenora-startup ")]
        self.assertEqual(len(lines), 1, result.stderr)
        report = json.loads(lines[0])
        self.assertEqual(report["protocol"], "complete", result.stderr)
        return result, report

    def test_large_image_callbacks_and_independent_guest_status(self):
        for name, expected in (("large-startup", 0), ("large-startup-control", 12)):
            with self.subTest(name=name):
                result, report = self.run_guest(self.fixtures / f"{name}.exe")
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(report["outcome"]["guest_status"], expected)
                self.assertEqual(report["last_completed_stage"], "entry_returned")
                self.assertEqual(
                    [item["stage"] for item in report["events"]],
                    ["parsed", "mapped", "relocated", "imports_bound", "tls_completed",
                     "entry_started", "entry_returned"],
                )
                self.assertIn("dir64_applied=8198", result.stderr)
                self.assertIn("tls_callbacks=2", result.stderr)

    def test_scarlett_runtime_fixture_reports_missing_initialization(self):
        result = subprocess.run(
            [sys.executable, SCRIPT, "--config", self.fixtures / "MicrosoftGame.config",
             "--xenora", self.xenora],
            capture_output=True, text=True, timeout=40, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report["selected_executable"]["id"], "RuntimeFixture")
        self.assertEqual(report["selected_executable"]["target_device_family"], "Scarlett")
        self.assertEqual(report["outcome"]["code"], "unverified_console_contract")
        image = report["static_pe"]
        self.assertEqual(image["total_imports"], 2)
        self.assertEqual(image["unresolved_imports"], 2)
        first = image["first_listed_unresolved_ordinary_import"]
        self.assertEqual(first["module"], "xgameruntime.dll")
        self.assertEqual(first["symbol_or_ordinal"], "XGameRuntimeInitialize")

        result, startup = self.run_guest(self.fixtures / "scarlett-runtime.exe")
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertEqual(startup["last_completed_stage"], "relocated")
        self.assertEqual(startup["outcome"]["phase"], "startup")
        self.assertIn("xgameruntime.dll!XGameRuntimeInitialize", startup["outcome"]["message"])

    def test_adjacent_dlls_do_not_supply_runtime_imports(self):
        for name in ("xenora-launch-one.dll", "xenora-launch-two.dll"):
            self.assertTrue((self.fixtures / name).is_file())
        result, report = self.run_guest(self.fixtures / "dependent-startup.exe")
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertEqual(report["last_completed_stage"], "relocated")
        self.assertEqual(report["outcome"]["phase"], "startup")
        self.assertIn("xenora-launch-one.dll!first_action", report["outcome"]["message"])

    def test_ordinary_import_terminator_must_fit_the_directory(self):
        data = bytearray((self.fixtures / "dependent-startup.exe").read_bytes())
        pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
        # Two 20-byte descriptors followed by a 20-byte zero terminator.
        import_size_offset = pe_offset + 24 + 124
        struct.pack_into("<I", data, import_size_offset, 59)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "short-import-directory.exe"
            path.write_bytes(data)
            result, report = self.run_guest(path)
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertEqual(report["last_completed_stage"], "relocated")
        self.assertEqual(report["outcome"]["phase"], "startup")
        self.assertIn("before its empty descriptor", report["outcome"]["message"])

    def test_rejected_images_and_entry_budget_come_from_different_stages(self):
        data = (self.fixtures / "large-startup.exe").read_bytes()
        pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
        bad_headers = bytearray(data)
        # PE signature (4) + COFF header (20) + SizeOfHeaders offset (60).
        struct.pack_into("<I", bad_headers, pe_offset + 84, 0)
        with tempfile.TemporaryDirectory() as directory:
            for name, contents, stage, message in (
                ("truncated", data[:32], None, "smaller than required headers"),
                ("mapping", bad_headers, "parsed", "SizeOfHeaders"),
            ):
                with self.subTest(name=name):
                    path = Path(directory) / f"{name}.exe"
                    path.write_bytes(contents)
                    result, report = self.run_guest(path)
                    self.assertEqual(result.returncode, 125, result.stderr)
                    self.assertEqual(report["last_completed_stage"], stage)
                    self.assertIn(message, report["outcome"]["message"])

        result, report = self.run_guest(self.fixtures / "large-startup.exe", "--max-steps", "64")
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertEqual(report["last_completed_stage"], "entry_returned")
        self.assertEqual(report["outcome"]["phase"], "execution")
        self.assertIn("StepLimitExceeded", report["outcome"]["message"])

    def test_relocation_and_tls_rejections_preserve_stage_prefix(self):
        data = (self.fixtures / "large-startup.exe").read_bytes()
        coff = struct.unpack_from("<I", data, 0x3C)[0] + 4
        section_count = struct.unpack_from("<H", data, coff + 2)[0]
        optional = coff + 20
        sections = optional + struct.unpack_from("<H", data, coff + 16)[0]

        def directory_offset(index: int) -> int:
            rva = struct.unpack_from("<I", data, optional + 112 + index * 8)[0]
            for record in range(sections, sections + section_count * 40, 40):
                start, size, offset = struct.unpack_from("<III", data, record + 12)
                if start <= rva < start + size:
                    return offset + rva - start
            self.fail(f"fixture directory {index} has no file offset")

        bad_relocations = bytearray(data)
        struct.pack_into("<I", bad_relocations, directory_offset(5) + 4, 7)
        bad_tls = bytearray(data)
        tls = directory_offset(9)
        template_start = struct.unpack_from("<Q", data, tls)[0]
        struct.pack_into("<Q", bad_tls, tls + 8, template_start - 1)

        with tempfile.TemporaryDirectory() as directory:
            for name, contents, stage, message in (
                ("relocations", bad_relocations, "mapped", "malformed relocation block"),
                ("tls", bad_tls, "imports_bound", "invalid TLS template range"),
            ):
                with self.subTest(name=name):
                    path = Path(directory) / f"{name}.exe"
                    path.write_bytes(contents)
                    result, report = self.run_guest(path)
                    self.assertEqual(result.returncode, 125, result.stderr)
                    self.assertEqual(report["last_completed_stage"], stage)
                    self.assertEqual(report["outcome"]["phase"], "startup")
                    self.assertIn(message, report["outcome"]["message"])

    def test_static_probe_preserves_findings_without_guest_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "MicrosoftGame.config"
            for name, missing in (
                ("large-startup.exe", None),
                ("dependent-startup.exe", "xenora-launch-one.dll"),
            ):
                with self.subTest(name=name):
                    (root / name).write_bytes((self.fixtures / name).read_bytes())
                    config.write_bytes(manifest(
                        f'<Executable Name="{name}" TargetDeviceFamily="Scarlett"/>'
                    ))
                    result = subprocess.run(
                        [sys.executable, SCRIPT, "--config", config, "--xenora", self.xenora],
                        capture_output=True, text=True, timeout=40, check=False,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    report = json.loads(result.stdout)
                    self.assertEqual(report["events"], [
                        "manifest", "executable_selection", "executable_read", "static_pe_probe",
                    ])
                    self.assertEqual(report["outcome"]["stage"], "console_bootstrap")
                    self.assertEqual(report["outcome"]["code"], "unverified_console_contract")
                    self.assertNotIn(directory, result.stdout)
                    image = report["static_pe"]
                    first = image["first_listed_unresolved_ordinary_import"]
                    if missing:
                        self.assertEqual(image["unresolved_imports"], 2)
                        self.assertEqual(first["module"], missing)
                        self.assertEqual(first["symbol_or_ordinal"], "first_action")
                    else:
                        self.assertIsNone(first)
                        self.assertEqual(image["dir64_relocations"], 8198)
                        self.assertEqual(image["tls_callbacks_count"], 2)
                        self.assertGreater(int(image["size_of_image"], 16), 128 * 1024 * 1024)


    def test_delayed_imports_are_reported_separately(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "delay-imports.exe"
            path.write_bytes((ROOT / "fixtures/pe/delay-imports.exe").read_bytes())
            config = root / "MicrosoftGame.config"
            config.write_bytes(manifest(
                '<Executable Name="delay-imports.exe" TargetDeviceFamily="Scarlett"/>'
            ))
            report, status = preflight.preflight(config, self.xenora, None)
            self.assertEqual(status, 0, report["outcome"])
            self.assertGreater(report["static_pe"]["delayed_imports"], 0)
            self.assertEqual(report["static_pe"]["delay_import_error"], "none")


if __name__ == "__main__":
    unittest.main()
