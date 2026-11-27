# SPDX-License-Identifier: GPL-3.0-or-later

"""Original fixture workloads selected by the compatibility runner."""


def loader_test(target: str, suite: str, description: str) -> dict:
    cargo_test = (
        "cargo", "test", "--locked", "--target", "aarch64-apple-darwin",
        "-p", "xenora-loader", "--test", target,
    )
    # --no-run builds the host test harness. Guest build and execution both
    # happen inside the test process and share the fixture command's result.
    return {
        "suite": suite,
        "description": description,
        "build_argv": (*cargo_test, "--no-run"),
        "argv": (*cargo_test, "--", "--nocapture", "--test-threads=1"),
    }


WORKLOADS = {
    "gate_a1": loader_test(
        "gate_a1", "c", "Freestanding C startup, relocations, and runtime services"
    ),
    "gate_a2": loader_test(
        "gate_a2", "c", "Hosted C startup, arguments, file I/O, and exit callbacks"
    ),
    "guest_cpp_path": loader_test(
        "guest_cpp_path", "cpp", "Hosted C++ object lifetime, threading, and file I/O"
    ),
    "guest_cpp_exceptions": loader_test(
        "guest_cpp_exceptions", "cpp", "C++ exception handling and stack unwinding"
    ),
    "modules": loader_test(
        "modules", "modules", "Guest image identity and built-in module services"
    ),
}
