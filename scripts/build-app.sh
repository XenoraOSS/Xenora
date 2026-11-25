#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build BetterXen.app (native ARM64) into build/BetterXen.app and run its smoke check.
# Requires: macOS on Apple Silicon, Rust 1.100 (aarch64-apple-darwin), Swift 6 toolchain.
set -eu
cd "$(dirname "$0")/.."
root=$(pwd)

fail() { echo "build-app: $*" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || fail "macOS is required"
[ "$(uname -m)" = arm64 ] || fail "a native arm64 host is required"
[ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 0 ] || fail "running under Rosetta is not supported"
command -v cargo >/dev/null || fail "cargo not found; install Rust 1.100"
command -v swift >/dev/null || fail "swift not found; install the Xcode command line tools"

target=aarch64-apple-darwin
# Match the app's LSMinimumSystemVersion so the static library links cleanly.
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release -p xenora-ffi --target "$target"
swift build --package-path app -c release --arch arm64 \
    -Xlinker -L -Xlinker "$root/target/$target/release"

bundle="$root/build/BetterXen.app"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
cp "$(swift build --package-path app -c release --arch arm64 --show-bin-path)/BetterXen" "$bundle/Contents/MacOS/BetterXen"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>BetterXen</string>
    <key>CFBundleIdentifier</key><string>org.macosporters.betterxen</string>
    <key>CFBundleName</key><string>BetterXen</string>
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
# Ad-hoc signature for local development only. No entitlements are requested:
# this build executes no guest code, so it needs no JIT entitlement.
codesign --force --sign - "$bundle"

binary="$bundle/Contents/MacOS/BetterXen"
[ "$(lipo -archs "$binary")" = arm64 ] || fail "executable is not arm64-only: $(lipo -archs "$binary")"
"$binary" --smoke
echo "build-app: built $bundle"
