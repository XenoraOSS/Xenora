# SPDX-License-Identifier: GPL-3.0-or-later
"""Check the bundled PE session helper with an original guest package."""

import argparse
from hashlib import sha256
from pathlib import Path
import subprocess


INPUTS = (10, 21, 10)


def expected_transcript() -> str:
    lines = ["ready"]
    blue = 0x40  # The companion initializes this value in DLL_PROCESS_ATTACH.
    for sequence, value in enumerate(INPUTS, start=1):
        blue = (blue + value) % 256
        red = bytes((255, 0, 0, 255))
        clear = bytes((value, sequence, blue, 255))
        row = clear * 3 + red if value % 2 else red + clear * 3
        rgba = row * 3
        lines.append(f"frame {sequence} width=4 height=3 sha256={sha256(rgba).hexdigest()} rgba={rgba.hex()}")
    lines.append("done frames=3")
    return "\n".join(lines) + "\n"


def smoke(helper: Path, fixtures: Path) -> None:
    entry = fixtures / "authored-session-package.exe"
    companion = fixtures / "xenora-authored-companion.dll"
    command = [
        helper, "--pe", entry, "--sha256", sha256(entry.read_bytes()).hexdigest(),
        "--experimental", "--graphics-backend", "reference", "--pixels",
        "--package", companion,
    ]
    digest = sha256(companion.read_bytes()).hexdigest()
    wrong_pin = ("1" if digest[0] == "0" else "0") + digest[1:]
    requests = "".join(f"{value}\n" for value in INPUTS)

    completed = subprocess.run([*command, digest], input=requests, capture_output=True, text=True, timeout=60)
    if completed.returncode != 0 or completed.stdout != expected_transcript():
        raise RuntimeError(f"bundled session failed frame or shutdown verification (exit {completed.returncode})\n"
                           f"{completed.stdout}{completed.stderr}")

    rejected = subprocess.run([*command, wrong_pin], input=requests, capture_output=True, text=True, timeout=60)
    if rejected.returncode != 125 or rejected.stdout or "SHA-256 does not match" not in rejected.stderr:
        raise RuntimeError(f"bundled session did not refuse the wrong pin cleanly (exit {rejected.returncode})\n"
                           f"{rejected.stdout}{rejected.stderr}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("helper", type=Path)
    parser.add_argument("fixtures", type=Path)
    options = parser.parse_args()
    smoke(options.helper.resolve(strict=True), options.fixtures.resolve(strict=True))
    print("Bundled PE session verified 3 frames, clean shutdown and wrong-pin rejection")


if __name__ == "__main__":
    main()
