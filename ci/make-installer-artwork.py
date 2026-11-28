#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Draws the two bitmaps of the Windows installer from the app icon, with the standard library only.

  crates/puddle-app/wix/artwork/dialog.bmp  493 x 312  first and last page: water panel with the duck on the left, white on the right
  crates/puddle-app/wix/artwork/banner.bmp  493 x  58  strip at the top of the middle pages: white, the duck at the right edge

Windows Installer wants plain 24-bit BMP files, so the generated files are committed; run this again
after changing the icon or the colours (python3 -I ci/make-installer-artwork.py). The bitmaps are
stretched on high-DPI screens and have no dark mode, which is the limit of the classic installer.
`--check` exits 1 when the committed files differ from what this script draws.
"""
import math
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ICON = ROOT / "crates/puddle-app/icons/icon.png"
OUT = ROOT / "crates/puddle-app/wix/artwork"

WATER_TOP = (0x0F, 0x5E, 0x9C)
WATER_BOTTOM = (0x4F, 0xB6, 0xE6)
RIPPLE = (0xFF, 0xFF, 0xFF)
WHITE = (0xFF, 0xFF, 0xFF)


def read_png(path):
    """Width, height and rows of (r, g, b, a) from an 8-bit RGBA PNG."""
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat = 8, b""
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert (depth, color, interlace) == (8, 6, 0), "the icon must be 8-bit RGBA, not interlaced"
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * 4
    rows, prev = [], bytearray(stride)
    for y in range(height):
        base = y * (stride + 1)
        filt, line = raw[base], bytearray(raw[base + 1 : base + 1 + stride])
        for i in range(stride):
            left = line[i - 4] if i >= 4 else 0
            up = prev[i]
            upleft = prev[i - 4] if i >= 4 else 0
            if filt == 1:
                line[i] = (line[i] + left) & 255
            elif filt == 2:
                line[i] = (line[i] + up) & 255
            elif filt == 3:
                line[i] = (line[i] + (left + up) // 2) & 255
            elif filt == 4:
                p = left + up - upleft
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - upleft)
                pred = left if pa <= pb and pa <= pc else (up if pb <= pc else upleft)
                line[i] = (line[i] + pred) & 255
        rows.append([tuple(line[x * 4 : x * 4 + 4]) for x in range(width)])
        prev = line
    return width, height, rows


def scaled(icon, size):
    """The icon as a size x size grid of premultiplied-free (r, g, b, a) by box averaging."""
    width, height, rows = icon
    out = []
    for y in range(size):
        y0, y1 = y * height // size, max((y + 1) * height // size, y * height // size + 1)
        line = []
        for x in range(size):
            x0, x1 = x * width // size, max((x + 1) * width // size, x * width // size + 1)
            total = [0, 0, 0, 0]
            for yy in range(y0, y1):
                for xx in range(x0, x1):
                    r, g, b, a = rows[yy][xx]
                    total[0] += r * a
                    total[1] += g * a
                    total[2] += b * a
                    total[3] += a
            n = (y1 - y0) * (x1 - x0)
            alpha = total[3] / n
            color = tuple(c / total[3] if total[3] else 0 for c in total[:3])
            line.append((*color, alpha / 255))
        out.append(line)
    return out


def blend(base, top, alpha):
    return tuple(round(b * (1 - alpha) + t * alpha) for b, t in zip(base, top))


def paste(canvas, sprite, left, top):
    for y, line in enumerate(sprite):
        for x, (r, g, b, a) in enumerate(line):
            if a > 0:
                canvas[top + y][left + x] = blend(canvas[top + y][left + x], (r, g, b), a)


def dialog(icon):
    width, height, panel = 493, 312, 164
    canvas = [[WHITE] * width for _ in range(height)]
    for y in range(height):
        row = blend(WATER_TOP, WATER_BOTTOM, y / (height - 1))
        for x in range(panel):
            canvas[y][x] = row
    # Soft rings around the spot the duck sits on, clipped to the panel.
    cx, cy = panel // 2, 205
    for y in range(height):
        for x in range(panel):
            d = math.hypot((x - cx) / 1.0, (y - cy) / 0.32)
            for radius in (46, 66, 86):
                edge = abs(d - radius)
                if edge < 1.6:
                    canvas[y][x] = blend(canvas[y][x], RIPPLE, 0.35 * (1 - edge / 1.6))
    paste(canvas, scaled(icon, 120), (panel - 120) // 2, 80)
    return canvas


def banner(icon):
    width, height = 493, 58
    canvas = [[WHITE] * width for _ in range(height)]
    paste(canvas, scaled(icon, 44), width - 44 - 12, 7)
    for x in range(width):  # the thin water line under the strip
        canvas[height - 2][x] = WATER_TOP
        canvas[height - 1][x] = WATER_BOTTOM
    return canvas


def bmp(canvas):
    height, width = len(canvas), len(canvas[0])
    pad = (4 - (width * 3) % 4) % 4
    body = b"".join(
        b"".join(bytes((b, g, r)) for r, g, b in row) + b"\x00" * pad for row in reversed(canvas)
    )
    header = struct.pack("<2sIHHI", b"BM", 54 + len(body), 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0, len(body), 2835, 2835, 0, 0)
    return header + info + body


def main():
    icon = read_png(ICON)
    files = {"dialog.bmp": bmp(dialog(icon)), "banner.bmp": bmp(banner(icon))}
    if "--check" in sys.argv:
        stale = [n for n, b in files.items() if not (OUT / n).is_file() or (OUT / n).read_bytes() != b]
        if stale:
            print("make-installer-artwork: out of date: " + ", ".join(stale), file=sys.stderr)
            return 1
        return 0
    OUT.mkdir(parents=True, exist_ok=True)
    for name, body in files.items():
        (OUT / name).write_bytes(body)
        print(f"wrote {OUT / name} ({len(body)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
