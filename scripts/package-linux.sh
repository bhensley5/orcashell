#!/usr/bin/env bash
set -euo pipefail

# Package OrcaShell for Linux as a tar.gz with binary, icon, and .desktop file.
# Usage: ./scripts/package-linux.sh

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BINARY="$REPO_ROOT/target/release/orcashell"
CLI_BINARY="$REPO_ROOT/target/release/orcash"
VERSION="${ORCASHELL_RELEASE_VERSION:-$(git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')}"
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

STAGING="$REPO_ROOT/target/orcashell-linux"
rm -rf "$STAGING"
mkdir -p "$STAGING"

cp "$BINARY" "$STAGING/orcashell"
cp "$CLI_BINARY" "$STAGING/orcash"
cp "$REPO_ROOT/assets/AppIcon.png" "$STAGING/orcashell.png"
cp "$REPO_ROOT/packaging/linux/orcashell.desktop" "$STAGING/"
cp "$REPO_ROOT/packaging/linux/nautilus-open-here.sh" "$STAGING/"
cp "$REPO_ROOT/packaging/linux/orcashell-open-here.desktop" "$STAGING/orcashell-open-here.desktop"
cp "$REPO_ROOT/packaging/linux/uninstall.sh" "$STAGING/"

# Create install script
cat > "$STAGING/install.sh" << 'INSTALL'
#!/usr/bin/env bash
set -euo pipefail
PREFIX="${1:-$HOME/.local}"
install -Dm755 orcashell "$PREFIX/bin/orcashell"
install -Dm755 orcash "$PREFIX/bin/orcash"
install -Dm644 orcashell.png "$PREFIX/share/icons/hicolor/1024x1024/apps/orcashell.png"
install -Dm644 orcashell.desktop "$PREFIX/share/applications/orcashell.desktop"

ORCASH_BIN="$PREFIX/bin/orcash"

# Nautilus "Scripts" integration (right-click → Scripts → Open OrcaShell Here)
# Patch the absolute path so the script works even when ~/.local/bin is not on
# the desktop session's PATH.
if command -v nautilus >/dev/null 2>&1; then
    NAUTILUS_DEST="$HOME/.local/share/nautilus/scripts/Open OrcaShell Here"
    sed "s|exec orcash |exec \"$ORCASH_BIN\" |" nautilus-open-here.sh > /tmp/orcashell-nautilus-install.tmp
    install -Dm755 /tmp/orcashell-nautilus-install.tmp "$NAUTILUS_DEST"
    rm -f /tmp/orcashell-nautilus-install.tmp
    echo "Installed Nautilus script: right-click → Scripts → Open OrcaShell Here"
fi

# Dolphin/KDE service menu integration (right-click → Open OrcaShell Here)
# Same absolute-path patching for desktop session compatibility.
if [ -d "$HOME/.local/share/kio" ] || command -v dolphin >/dev/null 2>&1; then
    DOLPHIN_DEST="$HOME/.local/share/kio/servicemenus/orcashell-open-here.desktop"
    sed "s|Exec=orcash |Exec=\"$ORCASH_BIN\" |" orcashell-open-here.desktop > /tmp/orcashell-dolphin-install.tmp
    install -Dm644 /tmp/orcashell-dolphin-install.tmp "$DOLPHIN_DEST"
    rm -f /tmp/orcashell-dolphin-install.tmp
    echo "Installed Dolphin service menu: right-click → Open OrcaShell Here"
fi

echo "Installed to $PREFIX (make sure $PREFIX/bin is on your PATH)"
INSTALL
chmod +x "$STAGING/install.sh"

tar -czf "$REPO_ROOT/target/orcashell-${VERSION}-linux-x86_64.tar.gz" -C "$REPO_ROOT/target" orcashell-linux

echo "Built: target/orcashell-${VERSION}-linux-x86_64.tar.gz"
