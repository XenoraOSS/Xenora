#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build Xenora.app (native ARM64) into build/Xenora.app and run its smoke check.
# Requires: macOS on Apple Silicon, Rust 1.100 (aarch64-apple-darwin), Swift 6 toolchain.
set -eu
cd "$(dirname "$0")/.."
root=$(pwd)

fail() { echo "build-app: $*" >&2; exit 1; }

usage="usage: scripts/build-app.sh [--check | --test]"
mode=${1:-}
case "$mode" in
    "") [ "$#" -eq 0 ] || fail "$usage" ;;
    --check|--test) [ "$#" -eq 1 ] || fail "$usage" ;;
    *) fail "$usage" ;;
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

if [ "$mode" = --check ]; then
    echo "build-app: macOS $macos_version, Swift $swift_version, Rust $tool_version, $target"
    echo "build-app: macOS SDK $sdk_path"
    exit 0
fi

cargo_metadata=$(cargo metadata --locked --format-version 1 --no-deps) ||
    fail "could not read Cargo workspace metadata"
cargo_target_dir=$(printf '%s\n' "$cargo_metadata" |
    /usr/bin/plutil -extract target_directory raw -expect string -o - -) ||
    fail "could not resolve Cargo target directory"
rust_release="$cargo_target_dir/$target/release"

# Match the app's LSMinimumSystemVersion so the static library links cleanly.
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release -p xenora-ffi --target "$target"
# Reuse the same bounded runners as the command line for the app's PE tools.
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release -p xenora --target "$target" \
    --bin xenora --bin xenora-pe-session
swift build --package-path app -c release --arch arm64 \
    -Xlinker -L -Xlinker "$rust_release"

bundle="$root/build/Xenora.app"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
cp "$(swift build --package-path app -c release --arch arm64 --show-bin-path)/Xenora" "$bundle/Contents/MacOS/Xenora"
cp "$rust_release/xenora" "$bundle/Contents/MacOS/xenora-cli"
cp "$rust_release/xenora-pe-session" "$bundle/Contents/MacOS/xenora-pe-session"
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
# The app and helpers use JIT memory. Local ad-hoc signing leaves Hardened
# Runtime off, so they need no JIT entitlement here.
worker="$bundle/Contents/MacOS/xenora-cli"
session="$bundle/Contents/MacOS/xenora-pe-session"
codesign --force --sign - "$worker"
codesign --force --sign - "$session"
codesign --force --sign - "$bundle"

binary="$bundle/Contents/MacOS/Xenora"
for executable in "$binary" "$worker" "$session"; do
    codesign --verify --strict "$executable"
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
codesign --verify --strict "$bundle"
"$binary" --smoke
"$binary" --smoke-inspection-order
"$binary" --smoke-import-result
"$binary" --smoke-hud
# A program run through the bundled worker, as the Run action does it: the
# fixture binds no ordinary import, so it stops after relocation. No GPU needed.
"$binary" --smoke-run-program "$root/fixtures/pe/delay-imports.exe" 125 relocated \
    "unresolved import dependency: fixture-ordinary.dll!OrdinaryByName"
if [ "$mode" = --test ]; then
    session_fixtures=$(mktemp -d "${TMPDIR:-/tmp}/xenora-bundle-session.XXXXXX")
    trap 'rm -rf "$session_fixtures"' EXIT
    sh fixtures/pe/authored-session/build-authored-session.sh "$session_fixtures"
    XENORA_PE_SESSION_TEST_DIRECTORY="$session_fixtures" XENORA_PE_SESSION="$session" \
        swift test --package-path app -c release --arch arm64 --jobs 2 \
        -Xlinker -L -Xlinker "$rust_release"
    "$binary" --smoke-loop-lifecycle
    "$binary" --smoke-audio-vfs-controls
    python3 scripts/smoke-pe-session.py "$session" "$session_fixtures"
fi
echo "build-app: built $bundle"
