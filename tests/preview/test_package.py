# SPDX-License-Identifier: GPL-3.0-or-later
import plistlib
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from zipfile import ZipFile, ZipInfo

from preview import package


class PreviewPackageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.archive = self.root / "preview.zip"
        self.destination = self.root / "extract"
        self.destination.mkdir()

    @staticmethod
    def app_files():
        return {
            "Xenora.app/Contents/Info.plist": plistlib.dumps({
                "CFBundleExecutable": "Xenora",
                "CFBundlePackageType": "APPL",
                "CFBundleVersion": "1234",
                "XenoraSourceRevision": "a" * 40,
                "IgnoredData": "omitted",
            }),
            "Xenora.app/Contents/MacOS/Xenora": b"app placeholder",
            "Xenora.app/Contents/MacOS/xenora-cli": b"worker placeholder",
            "Xenora.app/Contents/MacOS/xenora-pe-session": b"session helper placeholder",
        }

    def make_app(self, directory):
        for relative, body in self.app_files().items():
            file = directory / relative
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(body)
        return directory / "Xenora.app"

    def make_archive(self, extra=()):
        with ZipFile(self.archive, "w") as archive:
            for name, data in self.app_files().items():
                archive.writestr(name, data)
            for name, data in extra:
                archive.writestr(name, data)

    def mock_ditto(self, args):
        self.assertEqual(args, ["ditto", "-x", "-k", self.archive, self.destination])
        self.make_app(self.destination)

    def test_archive_uses_ditto_and_preserves_apple_metadata(self):
        app = self.make_app(self.root)
        commands = []
        package.archive_app(app, self.archive, run=commands.append)
        self.assertEqual(commands, [[
            "ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", app, self.archive
        ]])
        self.archive.touch()
        with self.assertRaisesRegex(ValueError, "already exists"):
            package.archive_app(app, self.archive, run=commands.append)
        self.assertEqual(len(commands), 1)

    def test_extract_accepts_matching_appledouble_and_ticket(self):
        self.make_archive([
            ("__MACOSX/", b""),
            ("__MACOSX/Xenora.app/._Contents", bytes.fromhex("00051607") + b"metadata"),
            ("Xenora.app/Contents/CodeResources", b"ticket"),
        ])
        self.assertEqual(
            package.extract_app(self.archive, self.destination, run=self.mock_ditto),
            self.destination / "Xenora.app",
        )

    def test_extract_rejects_unsafe_and_unexpected_entries_before_ditto(self):
        symlink = ZipInfo("Xenora.app/Contents/CodeResources")
        symlink.create_system = 3
        symlink.external_attr = (stat.S_IFLNK | 0o777) << 16
        for name, body in [
            ("Xenora.app/Contents/../Xenora", b"escape"),
            ("Xenora.app/Contents/MacOS/xenora", b"case collision"),
            ("SomeOther.app/Contents/Info.plist", b"other app"),
            ("Xenora.app/Contents/Resources/extra", b"extra resource"),
            ("__MACOSX/Xenora.app/Contents/._CodeResources", bytes.fromhex("00051607")),
            ("__MACOSX/Xenora.app/Contents/._Info.plist", b"bad magic"),
            (symlink, b"/tmp/outside"),
        ]:
            with self.subTest(name=name):
                self.make_archive([(name, body)])
                with self.assertRaises(ValueError):
                    package.extract_app(
                        self.archive, self.destination,
                        run=lambda args: self.fail("ditto ran for invalid input"),
                    )

    def test_extract_rejects_oversized_entries_before_ditto(self):
        self.make_archive()
        with patch.object(package, "MAX_MEMBER_BYTES", 5):
            with self.assertRaisesRegex(ValueError, "uncompressed size limit"):
                package.extract_app(
                    self.archive, self.destination,
                    run=lambda args: self.fail("ditto ran for oversized input"),
                )

    def test_archive_and_extraction_require_the_session_helper(self):
        member = "Xenora.app/Contents/MacOS/xenora-pe-session"
        app = self.make_app(self.root)
        (self.root / member).unlink()
        with self.assertRaisesRegex(ValueError, "missing bundle members: .*xenora-pe-session"):
            package.archive_app(app, self.archive, run=self.fail)

        with ZipFile(self.archive, "w") as archive:
            for name, data in self.app_files().items():
                if name != member:
                    archive.writestr(name, data)
        with self.assertRaisesRegex(ValueError, "missing archive members: .*xenora-pe-session"):
            package.extract_app(self.archive, self.destination, run=self.fail)

    def test_extract_rejects_oversized_zip_before_opening_it(self):
        self.archive.write_bytes(b"not a ZIP")
        with patch.object(package, "MAX_ARCHIVE_BYTES", 5):
            with self.assertRaisesRegex(ValueError, "on-disk size limit"):
                package.extract_app(self.archive, self.destination, run=self.fail)

    def test_inspect_records_all_binaries_without_paths_or_execution(self):
        app = self.make_app(self.root)
        calls = []

        def run(args, *, capture):
            self.assertTrue(capture)
            calls.append(args)
            return self.tool_output(args)

        evidence = package.inspect_app(app, run=run)
        self.assertEqual(evidence["info_plist"], {
            "CFBundleExecutable": "Xenora", "CFBundlePackageType": "APPL",
            "CFBundleVersion": "1234", "XenoraSourceRevision": "a" * 40,
        })
        self.assertEqual(set(evidence["executables"]), {"Xenora", "xenora-cli", "xenora-pe-session"})
        for binary in evidence["executables"].values():
            self.assertEqual(binary, {
                "file": "Mach-O 64-bit executable arm64",
                "architectures": ["arm64"],
                "dependencies": ["/usr/lib/libSystem.B.dylib"],
                "signature_verified": True,
            })
        self.assertTrue(evidence["signature_verified"])
        self.assertNotIn(str(self.root), str(evidence))
        self.assertEqual([args[-1] for args in calls if args[0] == "codesign"], [
            app / "Contents/MacOS/Xenora", app / "Contents/MacOS/xenora-cli",
            app / "Contents/MacOS/xenora-pe-session", app,
        ])

    def test_inspect_refuses_wrong_helper_architecture_linkage_and_signature(self):
        app = self.make_app(self.root)
        for (tool, output, error) in [
            ("lipo", "x86_64 arm64", ValueError),
            (
                "otool",
                "worker:\n\t/opt/local/libfoo.dylib (compatibility version 1, current version 1)\n",
                ValueError,
            ),
            ("codesign", None, subprocess.CalledProcessError),
        ]:
            for helper in ("xenora-cli", "xenora-pe-session"):
                with self.subTest(tool=tool, helper=helper):
                    def run(args, *, capture):
                        if args[0] == tool and args[-1].name == helper:
                            if output is None:
                                raise subprocess.CalledProcessError(1, args)
                            return output
                        return self.tool_output(args)

                    with self.assertRaises(error):
                        package.inspect_app(app, run=run)

    @staticmethod
    def tool_output(args):
        match = {
            "file": "Mach-O 64-bit executable arm64\n",
            "lipo": "arm64\n",
            "otool": f"{args[-1]}:\n\t/usr/lib/libSystem.B.dylib "
                     "(compatibility version 1.0.0, current version 1359.40.3)\n",
            "codesign": "",
        }
        return match[args[0]]
