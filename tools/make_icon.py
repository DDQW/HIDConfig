"""Writes packaging/openrazer.ico and packaging/openrazer.png (stdlib only).

Same design as the runtime tray icon (apps/razer-tray/src/icon.rs): a green ring
with a centre dot, anti-aliased, on a transparent background.

Usage: python -I tools/make_icon.py <repo root>
"""

import math
import struct
import sys
import zlib

GREEN = (0x44, 0xD6, 0x2C)
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def coverage(d, inner, outer, aa):
    a = min(max((d - inner) / aa + 0.5, 0.0), 1.0)
    b = min(max((outer - d) / aa + 0.5, 0.0), 1.0)
    return min(a, b)


def render(size):
    """RGBA rows, scaled from the 32 px design (ring 9.5..15.5, dot 4)."""
    k = size / 32.0
    c = (size - 1) / 2.0
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            # 4x4 supersampling for small sizes, exact edges for large ones.
            acc = 0.0
            for sy in range(4):
                for sx in range(4):
                    px = x + (sx + 0.5) / 4 - 0.5
                    py = y + (sy + 0.5) / 4 - 0.5
                    d = math.hypot(px - c, py - c)
                    ring = coverage(d, 9.5 * k, 15.5 * k, 0.25)
                    dot = coverage(d, -1.0, 4.0 * k, 0.25)
                    acc += max(ring, dot)
            a = acc / 16
            row += bytes((*GREEN, round(a * 255)))
        rows.append(bytes(row))
    return rows


def png(size, rows):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    raw = b"".join(b"\x00" + r for r in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(images):
    """ICO with PNG-encoded entries (supported since Windows Vista)."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, blobs = b"", b""
    for size, data in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    return header + entries + blobs


def main():
    root = sys.argv[1]
    images = [(s, png(s, render(s))) for s in SIZES]
    with open(f"{root}/packaging/openrazer.ico", "wb") as f:
        f.write(ico(images))
    with open(f"{root}/packaging/openrazer.png", "wb") as f:
        f.write(dict(images)[256])
    print("wrote packaging/openrazer.ico and packaging/openrazer.png")


if __name__ == "__main__":
    main()
