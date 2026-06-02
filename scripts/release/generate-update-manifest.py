#!/usr/bin/env python3
"""Generate OrcaShell signed-update manifest JSON."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


PLATFORMS = {
    "macos_arm64": "OrcaShell-{version}-macos-arm64.dmg",
    "linux_x86_64": "orcashell-{version}-linux-x86_64.tar.gz",
    "windows_x86_64": "orcashell-{version}-windows-x64.zip",
}
DEFAULT_DOWNLOAD_BASE_URL = "https://orcashell.com/downloads"
DEFAULT_RELEASE_NOTES_URL_TEMPLATE = "https://orcashell.com/releases/#{version}"


def artifact_path(artifacts_dir: Path, filename: str) -> Path:
    matches = [path for path in artifacts_dir.glob(f"**/{filename}") if path.is_file()]
    if len(matches) != 1:
        raise SystemExit(
            f"expected exactly one artifact named {filename}, found {len(matches)}"
        )
    return matches[0]


def sha256_hex(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts-dir", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--published-at", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--download-base-url")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    download_base_url = args.download_base_url or DEFAULT_DOWNLOAD_BASE_URL
    release_notes_url = DEFAULT_RELEASE_NOTES_URL_TEMPLATE.format(version=args.version)

    downloads = {}
    artifacts = {}
    for platform, template in PLATFORMS.items():
        filename = template.format(version=args.version)
        path = artifact_path(args.artifacts_dir, filename)
        downloads[platform] = f"{download_base_url}/{filename}"
        artifacts[platform] = {
            "sha256": sha256_hex(path),
            "size_bytes": path.stat().st_size,
        }

    manifest = {
        "manifest_version": 1,
        "version": args.version,
        "published_at": args.published_at,
        "release_notes_url": release_notes_url,
        "downloads": downloads,
        "artifacts": artifacts,
    }

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
