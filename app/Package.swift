// swift-tools-version: 6.0
// SPDX-License-Identifier: GPL-3.0-or-later
// Build with scripts/build-app.sh, which builds the Rust static library first.
import PackageDescription

let package = Package(
    name: "BetterXen",
    platforms: [.macOS(.v14)],
    targets: [
        .systemLibrary(name: "CXenora", path: "Sources/CXenora"),
        .executableTarget(name: "BetterXen", dependencies: ["CXenora"], path: "Sources/Xenora"),
    ]
)
