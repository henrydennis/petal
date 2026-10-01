#!/usr/bin/env python3
"""Draw Petal's app icon (a sunburst in a dark rounded square) and build AppIcon.icns.

Usage: scripts/make-icon.py <output-dir>   (needs Pillow and macOS's iconutil)
"""
import colorsys
import math
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFilter

SIZE = 1024
SCALE = 4  # draw large, then downsample, for smooth edges


def hsl(h, s, l):
    r, g, b = colorsys.hls_to_rgb(h % 1.0, l, s)
    return (int(r * 255), int(g * 255), int(b * 255), 255)


def draw_icon() -> Image.Image:
    n = SIZE * SCALE
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # macOS icon grid: an 824 px rounded square centred in 1024, corner radius ~185.
    inset, radius = 100 * SCALE, 185 * SCALE
    shadow = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((inset, inset + 12 * SCALE, n - inset, n - inset + 12 * SCALE), radius, fill=(0, 0, 0, 110))
    img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(18 * SCALE)))
    # Background: a subtle vertical gradient.
    bg = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    bd = ImageDraw.Draw(bg)
    for y in range(inset, n - inset):
        t = (y - inset) / (n - 2 * inset)
        c = tuple(int(a + (b - a) * t) for a, b in zip((44, 46, 54), (24, 25, 30))) + (255,)
        bd.line((inset, y, n - inset, y), fill=c)
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle((inset, inset, n - inset, n - inset), radius, fill=255)
    img.paste(bg, (0, 0), mask)

    cx = cy = n / 2
    gap = 5 * SCALE  # dark seams between segments

    def ring(r0, r1, segments):
        # segments: list of (start_turn, end_turn, color)
        for start, end, color in segments:
            box = (cx - r1, cy - r1, cx + r1, cy + r1)
            d.pieslice(box, start * 360 - 90, end * 360 - 90, fill=color)
        # carve the inner edge and the seams
        d.ellipse((cx - r0, cy - r0, cx + r0, cy + r0), fill=(30, 31, 36, 255))
        for start, _, _ in segments:
            a = start * 2 * math.pi - math.pi / 2
            d.line((cx + r0 * math.cos(a), cy + r0 * math.sin(a), cx + r1 * math.cos(a), cy + r1 * math.sin(a)), fill=(30, 31, 36, 255), width=gap)

    # Outer rings first, each inner one drawn over the last (pieslices fill to the centre).
    outer = [(0.00, 0.20), (0.22, 0.30), (0.34, 0.46), (0.50, 0.58), (0.62, 0.70), (0.74, 0.84)]
    ring(305 * SCALE, 345 * SCALE, [(a, b, hsl((a + b) / 2, 0.55, 0.72)) for a, b in outer])
    middle = [(0.00, 0.12), (0.12, 0.26), (0.26, 0.42), (0.42, 0.55), (0.55, 0.68), (0.68, 0.80), (0.80, 0.90), (0.90, 1.00)]
    ring(215 * SCALE, 295 * SCALE, [(a, b, hsl((a + b) / 2, 0.65, 0.62)) for a, b in middle])
    inner = [(0.00, 0.26), (0.26, 0.42), (0.42, 0.68), (0.68, 0.80), (0.80, 1.00)]
    ring(120 * SCALE, 205 * SCALE, [(a, b, hsl((a + b) / 2, 0.75, 0.55)) for a, b in inner])
    # Centre disc.
    d.ellipse((cx - 110 * SCALE, cy - 110 * SCALE, cx + 110 * SCALE, cy + 110 * SCALE), fill=(44, 46, 54, 255))

    return img.resize((SIZE, SIZE), Image.LANCZOS)


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(out_dir, exist_ok=True)
    icon = draw_icon()
    icon.save(os.path.join(out_dir, "AppIcon.png"))
    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "AppIcon.iconset")
        os.makedirs(iconset)
        for size in (16, 32, 128, 256, 512):
            icon.resize((size, size), Image.LANCZOS).save(os.path.join(iconset, f"icon_{size}x{size}.png"))
            icon.resize((size * 2, size * 2), Image.LANCZOS).save(os.path.join(iconset, f"icon_{size}x{size}@2x.png"))
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(out_dir, "AppIcon.icns")], check=True)
    print(f"wrote {out_dir}/AppIcon.icns")


if __name__ == "__main__":
    main()
