#!/usr/bin/env python3
"""Generate OrcaShell app icon from the source logo.

Produces assets/AppIcon.png (1024x1024) from design/OrcaShellLogoNoText.png.
On macOS, also generates assets/AppIcon.icns via iconutil.

Parameters (edit these to tweak the icon):
  TILT_DEGREES  - rotation angle (positive = nose up)
  RECT_SIZE     - size of the ABYSS rounded rect within 1024 canvas
  ORCA_FILL     - how much of the rect the orca fills (0.0–1.0)
  CORNER_RATIO  - corner radius as fraction of rect size
  ORCA_OFFSET_X - horizontal nudge in pixels (negative = left)
  ORCA_OFFSET_Y - vertical nudge in pixels (negative = up)

Usage:
  python3 scripts/generate-icon.py
  ICON_BASENAME=AppIcon2 ORCA_FILL=0.98 ORCA_OFFSET_X=-8 ORCA_OFFSET_Y=-10 python3 scripts/generate-icon.py

Current shipped AppIcon values:
  ORCA_FILL=1.08
  ORCA_OFFSET_X=-24
  ORCA_OFFSET_Y=-44
"""

from PIL import Image, ImageDraw
import numpy as np
import os
import platform
import subprocess
import tempfile

# --- Tunable parameters ---
TILT_DEGREES = 25
RECT_SIZE = 1010       # 1024 canvas, ~7px padding per side
ORCA_FILL = 0.95       # original fill stays default; override via env for trials
CORNER_RATIO = 0.2237  # Apple squircle-ish corner radius
ORCA_OFFSET_X = 0
ORCA_OFFSET_Y = 0

# The currently adopted launcher icon was generated with:
#   ORCA_FILL=1.08
#   ORCA_OFFSET_X=-24
#   ORCA_OFFSET_Y=-44

ABYSS = (13, 17, 23)
CANVAS = 1024

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(REPO_ROOT, "design", "OrcaShellLogoNoText.png")
ICON_BASENAME = os.environ.get("ICON_BASENAME", "AppIcon")
OUT_PNG = os.path.join(REPO_ROOT, "assets", f"{ICON_BASENAME}.png")
OUT_ICNS = os.path.join(REPO_ROOT, "assets", f"{ICON_BASENAME}.icns")


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    return float(value) if value is not None else default


def env_int(name: str, default: int) -> int:
    value = os.environ.get(name)
    return int(value) if value is not None else default


def main():
    orca_fill = env_float("ORCA_FILL", ORCA_FILL)
    orca_offset_x = env_int("ORCA_OFFSET_X", ORCA_OFFSET_X)
    orca_offset_y = env_int("ORCA_OFFSET_Y", ORCA_OFFSET_Y)

    src = Image.open(SOURCE)
    arr = np.array(src)

    # Fix transparent pixel RGB to ABYSS for clean anti-aliasing
    mask_zero = arr[:, :, 3] == 0
    arr[mask_zero, 0] = ABYSS[0]
    arr[mask_zero, 1] = ABYSS[1]
    arr[mask_zero, 2] = ABYSS[2]
    fixed = Image.fromarray(arr)

    # Crop to orca content
    alpha = arr[:, :, 3]
    rows = np.where(np.any(alpha > 10, axis=1))[0]
    cols = np.where(np.any(alpha > 10, axis=0))[0]
    cropped = fixed.crop((cols[0], rows[0], cols[-1] + 1, rows[-1] + 1))

    # Rotate
    rotated = cropped.rotate(TILT_DEGREES, resample=Image.BICUBIC, expand=True)
    rw, rh = rotated.size

    # Scale orca to fill the rect
    pad = (CANVAS - RECT_SIZE) // 2
    radius = int(RECT_SIZE * CORNER_RATIO)
    target = int(RECT_SIZE * orca_fill)
    scale = min(target / rw, target / rh)
    new_w = int(rw * scale)
    new_h = int(rh * scale)
    rotated = rotated.resize((new_w, new_h), Image.LANCZOS)

    # Transparent canvas
    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))

    # Draw ABYSS rounded rect
    bg_mask = Image.new("L", (CANVAS, CANVAS), 0)
    draw = ImageDraw.Draw(bg_mask)
    draw.rounded_rectangle(
        [(pad, pad), (pad + RECT_SIZE - 1, pad + RECT_SIZE - 1)],
        radius=radius,
        fill=255,
    )
    bg = Image.new("RGBA", (CANVAS, CANVAS), ABYSS + (255,))
    canvas.paste(bg, (0, 0), bg_mask)

    # Center orca
    orca_x = pad + (RECT_SIZE - new_w) // 2 + orca_offset_x
    orca_y = pad + (RECT_SIZE - new_h) // 2 + orca_offset_y
    canvas.paste(rotated, (orca_x, orca_y), rotated)

    os.makedirs(os.path.dirname(OUT_PNG), exist_ok=True)
    canvas.save(OUT_PNG)
    print(f"Saved {OUT_PNG} (orca {new_w}x{new_h} in {RECT_SIZE}x{RECT_SIZE} rect)")

    # Generate .icns on macOS
    if platform.system() == "Darwin":
        iconset = f"/tmp/{ICON_BASENAME}.iconset"
        subprocess.run(["rm", "-rf", iconset], check=False)
        os.makedirs(iconset, exist_ok=True)

        mac_icon_sizes = [16, 32, 64, 128, 256, 512, 1024]
        for s in mac_icon_sizes:
            subprocess.run(
                [
                    "sips",
                    "-z",
                    str(s),
                    str(s),
                    OUT_PNG,
                    "--out",
                    f"{iconset}/icon_{s}x{s}.png",
                ],
                check=True,
                capture_output=True,
            )
            if s < 1024:
                retina = s * 2
                subprocess.run(
                    [
                        "sips",
                        "-z",
                        str(retina),
                        str(retina),
                        OUT_PNG,
                        "--out",
                        f"{iconset}/icon_{s}x{s}@2x.png",
                    ],
                    check=True,
                    capture_output=True,
                )

        iconutil = subprocess.run(
            ["iconutil", "-c", "icns", iconset, "-o", OUT_ICNS],
            check=False,
            capture_output=True,
            text=True,
        )
        subprocess.run(["rm", "-rf", iconset])
        if iconutil.returncode != 0:
            # Some macOS environments reject otherwise-valid iconsets; fall back to
            # tiff2icns using a multi-image TIFF assembled from standard icon sizes.
            with tempfile.TemporaryDirectory(prefix=f"{ICON_BASENAME}-tiff-") as temp_dir:
                tiff_sizes = [16, 32, 48, 128, 256, 512, 1024]
                tiff_paths = []
                for size in tiff_sizes:
                    out_tiff = os.path.join(temp_dir, f"icon_{size}.tiff")
                    subprocess.run(
                        [
                            "sips",
                            "-s",
                            "format",
                            "tiff",
                            "-z",
                            str(size),
                            str(size),
                            OUT_PNG,
                            "--out",
                            out_tiff,
                        ],
                        check=True,
                        capture_output=True,
                    )
                    tiff_paths.append(out_tiff)

                merged_tiff = os.path.join(temp_dir, f"{ICON_BASENAME}.tiff")
                subprocess.run(
                    ["tiffutil", "-cat", *tiff_paths, "-out", merged_tiff],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                subprocess.run(
                    ["tiff2icns", merged_tiff, OUT_ICNS],
                    check=True,
                    capture_output=True,
                    text=True,
                )
        print(f"Saved {OUT_ICNS}")

    # Generate .ico (all platforms)
    OUT_ICO = os.path.join(REPO_ROOT, "assets", f"{ICON_BASENAME}.ico")
    ico_sizes = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)]
    ico_images = [canvas.resize(size, Image.LANCZOS) for size in ico_sizes]
    ico_images[0].save(OUT_ICO, format="ICO", sizes=ico_sizes, append_images=ico_images[1:])
    print(f"Saved {OUT_ICO}")


if __name__ == "__main__":
    main()
