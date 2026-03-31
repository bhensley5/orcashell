#!/usr/bin/env bash
# Uninstall OrcaShell and its file manager integrations.
# Reverses what install.sh creates.
#
# Usage: ./uninstall.sh [PREFIX]
#   PREFIX defaults to ~/.local

set -euo pipefail
PREFIX="${1:-$HOME/.local}"

removed=0

for f in \
    "$PREFIX/bin/orcashell" \
    "$PREFIX/bin/orcash" \
    "$PREFIX/share/icons/hicolor/1024x1024/apps/orcashell.png" \
    "$PREFIX/share/applications/orcashell.desktop" \
    "$HOME/.local/share/nautilus/scripts/Open OrcaShell Here" \
    "$HOME/.local/share/kio/servicemenus/orcashell-open-here.desktop"
do
    if [ -f "$f" ]; then
        rm -f "$f"
        echo "Removed: $f"
        removed=$((removed + 1))
    fi
done

if [ "$removed" -gt 0 ]; then
    echo "Uninstalled OrcaShell ($removed files removed)."
else
    echo "No OrcaShell files found to remove."
fi
