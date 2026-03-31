#!/usr/bin/env python3
"""Generate OrcaShell app icon from the source logo.

Produces assets/AppIcon.png (1024x1024) from design/OrcaShellLogoNoText.png.
On macOS, also generates assets/AppIcon.icns via iconutil.

Parameters (edit these to tweak the icon):
  TILT_DEGREES  - rotation angle (positive = nose up)
  RECT_SIZE     - size of the ABYSS rounded rect within 1024 canvas
  ORCA_FILL     - how much of the rect the orca fills (0.0–1.0)
  CORNER_RATIO  - corner radius as fraction of rect size

Usage:
  python3 scripts/generate-icon.py
"""

from PIL import Image, ImageDraw
import numpy as np
import os
import platform
import subprocess

# --- Tunable parameters ---
TILT_DEGREES = 25
RECT_SIZE = 1010       # 1024 canvas, ~7px padding per side
ORCA_FILL = 0.95       # orca fills 95% of the rect
CORNER_RATIO = 0.2237  # Apple squircle-ish corner radius

ABYSS = (13, 17, 23)
CANVAS = 1024

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(REPO_ROOT, "design", "OrcaShellLogoNoText.png")
OUT_PNG = os.path.join(REPO_ROOT, "assets", "AppIcon.png")
OUT_ICNS = os.path.join(REPO_ROOT, "assets", "AppIcon.icns")


def main():
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
    target = int(RECT_SIZE * ORCA_FILL)
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
    orca_x = pad + (RECT_SIZE - new_w) // 2
    orca_y = pad + (RECT_SIZE - new_h) // 2
    canvas.paste(rotated, (orca_x, orca_y), rotated)

    os.makedirs(os.path.dirname(OUT_PNG), exist_ok=True)
    canvas.save(OUT_PNG)
    print(f"Saved {OUT_PNG} (orca {new_w}x{new_h} in {RECT_SIZE}x{RECT_SIZE} rect)")

    # Generate .icns on macOS
    if platform.system() == "Darwin":
        iconset = "/tmp/AppIcon.iconset"
        os.makedirs(iconset, exist_ok=True)
        for s in [16, 32, 64, 128, 256, 512, 1024]:
            subprocess.run(
                ["sips", "-z", str(s), str(s), OUT_PNG,
                 "--out", f"{iconset}/icon_{s}x{s}.png"],
                capture_output=True,
            )
        for s in [16, 32, 128, 256, 512]:
            d = s * 2
            os.replace(f"{iconset}/icon_{d}x{d}.png", f"{iconset}/icon_{s}x{s}@2x.png")
            # Re-generate the non-retina version since we just moved it
            subprocess.run(
                ["sips", "-z", str(d), str(d), OUT_PNG,
                 "--out", f"{iconset}/icon_{d}x{d}.png"],
                capture_output=True,
            )
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", OUT_ICNS])
        subprocess.run(["rm", "-rf", iconset])
        print(f"Saved {OUT_ICNS}")

    # Generate .ico (all platforms)
    OUT_ICO = os.path.join(REPO_ROOT, "assets", "AppIcon.ico")
    ico_sizes = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)]
    ico_images = [canvas.resize(size, Image.LANCZOS) for size in ico_sizes]
    ico_images[0].save(OUT_ICO, format="ICO", sizes=ico_sizes, append_images=ico_images[1:])
    print(f"Saved {OUT_ICO}")


if __name__ == "__main__":
    main()
