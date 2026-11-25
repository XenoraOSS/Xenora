#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build Xenora.app (native ARM64) into build/Xenora.app and run its smoke check.
# Requires: macOS on Apple Silicon, Rust 1.100 (aarch64-apple-darwin), Swift 6 toolchain.
set -eu
cd "$(dirname "$0")/.."
root=$(pwd)

fail() { echo "build-app: $*" >&2; exit 1; }

case "${1:-}" in
    "") [ "$#" -eq 0 ] || fail "usage: scripts/build-app.sh [--check]" ;;
    --check) [ "$#" -eq 1 ] || fail "usage: scripts/build-app.sh [--check]" ;;
    *) fail "usage: scripts/build-app.sh [--check]" ;;
esac

[ "$(uname -s)" = Darwin ] || fail "macOS is required"
[ "$(uname -m)" = arm64 ] || fail "run from a native ARM64 shell on Apple Silicon"
[ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 0 ] ||
    fail "run from a native ARM64 shell on Apple Silicon"
macos_version=$(sw_vers -productVersion)
[ "${macos_version%%.*}" -ge 14 ] || fail "macOS 14 or later is required (found $macos_version)"

# The SDK/toolchain may be newer than the oldest version the bundle runs on.
sdk_path=$(xcrun --sdk macosx --show-sdk-path 2>/dev/null) &&
    [ -d "$sdk_path" ] || fail "macOS SDK unavailable; run xcode-select --install or select a working Xcode"
command -v swift >/dev/null || fail "Swift unavailable; install Xcode or its Command Line Tools"
swift_info=$(swift --version 2>&1) ||
    fail "cannot run Swift; check xcode-select -p and finish Xcode setup"
swift_version=$(printf '%s\n' "$swift_info" | sed -nE 's/.*Swift version ([0-9]+[.][0-9]+([.][0-9]+)?).*/\1/p')
case "$swift_version" in
    '') fail "could not determine the Swift toolchain version (need Swift 6 or later)" ;;
    *) [ "${swift_version%%.*}" -ge 6 ] ||
        fail "Swift 6 or later is required (found $swift_version); update the selected Xcode tools" ;;
esac
printf '%s\n' "$swift_info" | grep -q '^Target: arm64-apple-macos' ||
    fail "the selected Swift toolchain must target arm64-apple-macos"

target=aarch64-apple-darwin
rust_minimum=$(sed -n 's/^rust-version = "\([0-9][0-9.]*\)"$/\1/p' Cargo.toml)
[ -n "$rust_minimum" ] || fail "Cargo.toml is missing the workspace Rust version"
for tool in cargo rustc; do
    command -v "$tool" >/dev/null ||
        fail "$tool unavailable; install Rust $rust_minimum or later for $target and add it to PATH"
    tool_info=$("$tool" -vV 2>&1) ||
        fail "cannot run $tool; select an installed Rust $rust_minimum or later toolchain for $target"
    tool_version=$(printf '%s\n' "$tool_info" | sed -n 's/^release: //p')
    printf '%s\n' "$tool_version" | awk -F. -v minimum="$rust_minimum" '
        BEGIN { split(minimum, required, ".") }
        $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ &&
            ($1 + 0 > required[1] + 0 || ($1 + 0 == required[1] + 0 && $2 + 0 >= required[2] + 0)) {
                supported = 1
            }
        END { exit !supported }
    ' || fail "$tool $rust_minimum or later is required (found ${tool_version:-unknown})"
    printf '%s\n' "$tool_info" | grep -q "^host: $target$" ||
        fail "the selected $tool must run natively on $target"
done

if [ "${1:-}" = --check ]; then
    echo "build-app: macOS $macos_version, Swift $swift_version, Rust $tool_version, $target"
    echo "build-app: macOS SDK $sdk_path"
    exit 0
fi

# Match the app's LSMinimumSystemVersion so the static library links cleanly.
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release -p xenora-ffi --target "$target"
# The `xenora run` worker the app's Run action launches, so a run from the app
# gets the same worker process, wall deadline and step limit as the command line.
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release -p xenora --target "$target"
swift build --package-path app -c release --arch arm64 \
    -Xlinker -L -Xlinker "$root/target/$target/release"

bundle="$root/build/Xenora.app"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
cp "$(swift build --package-path app -c release --arch arm64 --show-bin-path)/Xenora" "$bundle/Contents/MacOS/Xenora"
cp "$root/target/$target/release/xenora" "$bundle/Contents/MacOS/xenora-cli"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>Xenora</string>
    <key>CFBundleIdentifier</key><string>local.xenora.Xenora</string>
    <key>CFBundleName</key><string>Xenora</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>Windows Executable</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array><string>com.microsoft.windows-executable</string></array>
        </dict>
    </array>
</dict>
</plist>
PLIST
# Ad-hoc signature for local development only. No entitlements are requested.
# The bundled worker translates guest code into JIT memory; without the hardened
# runtime an ad-hoc signed executable needs no JIT entitlement for that.
codesign --force --sign - "$bundle"

binary="$bundle/Contents/MacOS/Xenora"
for executable in "$binary" "$bundle/Contents/MacOS/xenora-cli"; do
    [ "$(lipo -archs "$executable")" = arm64 ] || fail "executable is not arm64-only: $(lipo -archs "$executable")"
    file "$executable"
    otool -L "$executable"
    # Only system libraries may be linked: anything else would be a host dependency
    # shipped or expected outside macOS, which this bundle does not carry.
    for dependency in $(otool -L "$executable" | tail -n +2 | awk '{print $1}'); do
        case "$dependency" in
            /System/Library/*|/usr/lib/*) ;;
            *) fail "$executable links a non-system library: $dependency" ;;
        esac
    done
done
"$binary" --smoke
"$binary" --smoke-inspection-order
"$binary" --smoke-import-result
"$binary" --smoke-hud
# A program run through the bundled worker, as the Run action does it: the
# fixture binds no ordinary import, so it stops after relocation. No GPU needed.
"$binary" --smoke-run-program "$root/fixtures/pe/delay-imports.exe" 125 relocated \
    "unresolved import dependency: fixture-ordinary.dll!OrdinaryByName"
echo "build-app: built $bundle"
