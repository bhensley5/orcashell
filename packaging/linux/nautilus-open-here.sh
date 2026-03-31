#!/usr/bin/env bash
# Nautilus script: "Open OrcaShell Here"
# Install to: ~/.local/share/nautilus/scripts/Open OrcaShell Here
#
# Nautilus passes selected paths via NAUTILUS_SCRIPT_SELECTED_FILE_PATHS
# (newline-separated).  If nothing is selected, fall back to the current
# directory via NAUTILUS_SCRIPT_CURRENT_URI.

set -euo pipefail

# Use the first selected path
if [ -n "${NAUTILUS_SCRIPT_SELECTED_FILE_PATHS:-}" ]; then
    DIR="$(echo "$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS" | head -n1)"
else
    # Decode file:// URI to a local path
    URI="${NAUTILUS_SCRIPT_CURRENT_URI:-}"
    if [ -n "$URI" ] && command -v python3 >/dev/null 2>&1; then
        DIR="$(python3 -c "import sys, urllib.parse; print(urllib.parse.unquote(urllib.parse.urlparse(sys.argv[1]).path))" "$URI")"
    else
        DIR="$HOME"
    fi
fi

# If selected item is a file, use its parent directory
if [ -f "$DIR" ]; then
    DIR="$(dirname "$DIR")"
fi

exec orcash open --dir "$DIR"
