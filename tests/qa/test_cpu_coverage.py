# SPDX-License-Identifier: GPL-3.0-or-later
import json
import tempfile
import unittest
from pathlib import Path

from qa.cpu_coverage import (
    blocker_manifest,
    build_report,
    compare_reports,
    dumps_report,
    scan_imports,
    scan_opcodes,
)

ROOT = Path(__file__).resolve().parents[2]


class CpuCoverage(unittest.TestCase):
    def test_scan_opcodes_reads_the_declarative_registry(self):
        text = """
        schema = 1
        architecture = "x86-64"
        [operation]
        add = { ref = "sdm ADD" }
        syscall = { ref = "sdm SYSCALL" }
        [alu]
        operations = ["add"]
        [[form]]
        op = "syscall"
        map = "0f"
        opcode = 0x05
        enc = "none"
        size = "n"
        """
        entries = {e.opcode: e.status for e in scan_opcodes(text, source="instructions.toml")}
        self.assertEqual(entries.get("legacy:1:05"), "decodable")
        self.assertEqual(entries.get("legacy:0f:05"), "decodable")
        self.assertNotIn("05", entries)

    def test_scan_opcodes_distinguishes_vex_and_legacy_spaces(self):
        registry = (ROOT / "crates/loader/x86/instructions.toml").read_text(encoding="utf-8")
        entries = {e.opcode: e.status for e in scan_opcodes(registry, source="instructions.toml")}
        self.assertEqual(entries.get("legacy:1:05"), "decodable")
        self.assertEqual(entries.get("legacy:0f:05"), "decodable")
        self.assertTrue(any(key.startswith("vex:0f") for key in entries))


    def test_compare_treats_decodable_registry_entries_as_coverage(self):
        before = {"opcodes": [], "counts": {"interpreter_only": []}}
        after = {
            "opcodes": [{"opcode": "legacy:1:05", "status": "decodable"}],
            "counts": {"interpreter_only": []},
        }
        self.assertEqual(compare_reports(before, after)["newly_supported"], ["legacy:1:05"])

    def test_scan_imports_registration_kinds(self):
        text = '''
        const KERNEL32: &str = "kernel32.dll";
        registry.register(KERNEL32, "CreateFileW", |_, _, _| Ok(0));
        registry.register_with_status(
            "kernel32.dll",
            "Sleep",
            ServiceStatus::Stub,
            |_, _, _| Ok(0),
        );
        registry.register_with_status(
            "kernel32.dll", "WaitForIt", ServiceStatus::Partial, |_, _, _| Ok(0),
        );
        registry.alias("api-ms-win-core-file-l1-1-0.dll", KERNEL32, "CreateFileW");
        '''
        entries = {(e.dll, e.name): e.kind for e in scan_imports(text, source="imports.rs")}
        self.assertEqual(entries.get(("kernel32.dll", "CreateFileW")), "functional")
        self.assertEqual(entries.get(("kernel32.dll", "Sleep")), "stub")
        self.assertEqual(entries.get(("kernel32.dll", "WaitForIt")), "partial")
        self.assertEqual(
            entries.get(("api-ms-win-core-file-l1-1-0.dll", "CreateFileW")),
            "alias",
        )

    def test_scan_imports_ignores_arbitrary_literals_and_test_modules(self):
        text = '''
        enum ImportName { Name(String) }
        let name = "ImportName";
        let helper = "not_a_guest_import";
        #[cfg(test)]
        mod tests {
            fn register_tests() {
                registry.register("kernel32.dll", "TestOnly", |_, _, _| Ok(0));
            }
        }
        '''
        entries = scan_imports(text, source="imports.rs")
        self.assertEqual(entries, [])

    def test_scan_imports_reads_generated_service_metadata(self):
        text = '''
        ServiceMetadata {
            library: "kernel32.dll",
            symbol: "GetCurrentProcess",
            parameters: &[],
            result: "handle",
            status: ServiceStatus::Implemented,
        }
        ServiceMetadata {
            library: "ucrtbase.dll",
            symbol: "fopen",
            parameters: &[],
            result: "u64",
            status: ServiceStatus::Partial,
        }
        '''
        entries = {(e.dll, e.name): e.kind for e in scan_imports(text, source="runtime_table.rs")}
        self.assertEqual(entries[("kernel32.dll", "GetCurrentProcess")], "functional")
        self.assertEqual(entries[("ucrtbase.dll", "fopen")], "partial")

    def test_scan_imports_keeps_same_symbol_in_distinct_modules(self):
        text = '''
        registry.register("kernel32.dll", "SharedName", |_, _, _| Ok(0));
        registry.register("ucrtbase.dll", "SharedName", |_, _, _| Ok(0));
        '''
        entries = scan_imports(text, source="services.rs")
        self.assertEqual(
            sorted((e.dll, e.name) for e in entries),
            [("kernel32.dll", "SharedName"), ("ucrtbase.dll", "SharedName")],
        )

    def test_repo_report_and_blockers(self):
        report = build_report(ROOT)
        self.assertTrue(report.opcodes or report.imports or report.notes)
        imports = {(entry.dll, entry.name) for entry in report.imports}
        self.assertIn(("kernel32.dll", "GetCurrentProcess"), imports)
        self.assertNotIn(("imports", "ImportName"), imports)
        self.assertNotIn(("imports", "Defaulted"), imports)
        self.assertNotIn(("imports_sync", "CARGO_BIN_EXE_xenora"), imports)
        self.assertNotIn("05", {entry.opcode for entry in report.opcodes})
        self.assertIn("Op::Add", report.interpreter_symbols)
        self.assertIn("Op::Paddb", report.interpreter_symbols)
        self.assertIn("Op::Popcnt", report.interpreter_symbols)
        self.assertIn("Op::Add", report.dbt_symbols)
        self.assertIn("Op::Paddb", report.dbt_symbols)
        self.assertNotIn("Op::Popcnt", report.dbt_symbols)
        self.assertNotIn("emit_fault_guard", report.dbt_symbols)
        self.assertNotIn("execute_helper", report.dbt_symbols)
        payload = json.loads(dumps_report(report))
        blockers = blocker_manifest(report, limit=10)
        self.assertIsInstance(blockers, list)
        delta = compare_reports(payload, payload)
        self.assertEqual(delta["newly_supported"], [])

    def test_blocker_manifest_includes_partial_imports(self):
        report = build_report(ROOT)
        blockers = blocker_manifest(report, limit=500)
        partial = [b for b in blockers if b["kind"] == "import" and "partial" in b["task"]]
        self.assertTrue(partial)


if __name__ == "__main__":
    unittest.main()
