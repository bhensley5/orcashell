#!/usr/bin/env bash
set -euo pipefail

# Package OrcaShell as a macOS .app bundle.
# Usage: ./scripts/package-macos.sh [--target TARGET]
# Expects cargo build --release to have been run already.

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="${1:-}"

if [ "$TARGET" = "--target" ]; then
    TARGET="$2"
    BINARY="$REPO_ROOT/target/$TARGET/release/orcashell"
    CLI_BINARY="$REPO_ROOT/target/$TARGET/release/orcash"
else
    BINARY="$REPO_ROOT/target/release/orcashell"
    CLI_BINARY="$REPO_ROOT/target/release/orcash"
fi

# Derive version from git tag (e.g. v0.2.0 -> 0.2.0), fallback to 0.0.0
VERSION="$(git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')"
VERSION="${VERSION:-0.0.0}"

if [ ! -f "$BINARY" ]; then
    echo "Error: orcashell binary not found at $BINARY"
    echo "Run 'cargo build --release' first."
    exit 1
fi

if [ ! -f "$CLI_BINARY" ]; then
    echo "Error: orcash binary not found at $CLI_BINARY"
    echo "Run 'cargo build --release' first."
    exit 1
fi

APP_DIR="$REPO_ROOT/target/OrcaShell.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"
mkdir -p "$APP_DIR/Contents/Library/Services"

cp "$BINARY" "$APP_DIR/Contents/MacOS/orcashell"
cp "$CLI_BINARY" "$APP_DIR/Contents/MacOS/orcash"
cp "$REPO_ROOT/packaging/macos/Info.plist" "$APP_DIR/Contents/"
cp "$REPO_ROOT/assets/AppIcon.icns" "$APP_DIR/Contents/Resources/"

# Bundle Quick Action workflows. macOS auto-registers them when the app is in
# any Applications directory (no launchctl or manual registration needed).
cp -r "$REPO_ROOT/packaging/macos/Services/New OrcaShell Tab Here.workflow" \
      "$APP_DIR/Contents/Library/Services/"
cp -r "$REPO_ROOT/packaging/macos/Services/New OrcaShell Window Here.workflow" \
      "$APP_DIR/Contents/Library/Services/"

# Inject version from git tag
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$APP_DIR/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP_DIR/Contents/Info.plist"

echo "Built: $APP_DIR ($VERSION)"
