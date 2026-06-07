#!/usr/bin/env python3
"""Generate the LEMON record app icon (pure stdlib — no PIL/ImageMagick).

Renders a 1024x1024 RGBA PNG: a near-black rounded "squircle" tile with a
solid lemon-yellow record disc centered on it. Antialiasing is analytic
(signed-distance coverage), so edges stay crisp without supersampling.

Then `iconutil` (called by the Makefile / make-icon target) turns the
sips-resized iconset into icons/icon.icns. This script only emits the base PNG.

Usage: python3 scripts/gen-icon.py icons/icon-1024.png
"""
import struct
import sys
import zlib

SIZE = 1024
TILE = (0x16, 0x16, 0x18)        # near-black tile, terminal/Ableton dark
LEMON = (0xF4, 0xD0, 0x3A)       # Lemon Audio brand accent
CORNER = SIZE * 0.225            # squircle-ish corner radius
DISC_R = SIZE * 0.30             # record disc radius


def rounded_rect_sdf(x, y, w, h, r):
    """Signed distance to a rounded rectangle centered at origin (negative inside)."""
    qx = abs(x) - (w / 2 - r)
    qy = abs(y) - (h / 2 - r)
    ax, ay = max(qx, 0.0), max(qy, 0.0)
    outside = (ax * ax + ay * ay) ** 0.5
    inside = min(max(qx, qy), 0.0)
    return outside + inside - r


def coverage(sdf):
    """1px analytic antialiasing: full inside, 0 outside, linear across the edge."""
    return min(max(0.5 - sdf, 0.0), 1.0)


def blend(dst, src, a):
    return tuple(round(d + (s - d) * a) for d, s in zip(dst, src))


def build():
    cx = cy = SIZE / 2
    rows = []
    for py in range(SIZE):
        row = bytearray()
        y = py + 0.5 - cy
        for px in range(SIZE):
            x = px + 0.5 - cx
            tile_a = coverage(rounded_rect_sdf(x, y, SIZE, SIZE, CORNER))
            disc_a = coverage(((x * x + y * y) ** 0.5) - DISC_R)
            rgb = blend(TILE, LEMON, disc_a)
            row += bytes((*rgb, round(255 * tile_a)))
        rows.append(bytes(row))
    return rows


def write_png(path, rows):
    raw = bytearray()
    for r in rows:
        raw.append(0)            # filter type 0 (None)
        raw += r

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", ihdr)
           + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
           + chunk(b"IEND", b""))
    with open(path, "wb") as f:
        f.write(png)


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "icons/icon-1024.png"
    write_png(out, build())
    print(f"wrote {out} ({SIZE}x{SIZE})")
