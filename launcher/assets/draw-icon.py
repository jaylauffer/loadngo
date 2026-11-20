#!/usr/bin/env python3
"""Draws the launcher's icon: a dark tile with a grid of four squares, the
first of them lit and carrying a start mark.

    python3 launcher/assets/draw-icon.py launcher/assets/icon-1024.png
    python3 launcher/assets/draw-icon.py --full-bleed launcher/assets/icon-ios-1024.png

--full-bleed is the icon for an iPhone: the tile fills the whole square with
no transparency (iOS rounds the corners itself and shows transparency as
black), and what is on the tile grows with it.

Standard library only. Edges are antialiased from each shape's distance
function, so the picture is exact at 1024 pixels and is scaled down from
there (scripts/install-launcher.sh makes the other sizes).
"""

import struct
import sys
import zlib

SIZE = 1024

# The tile, to the proportions macOS app icons use: 824 of 1024, inset 100.
TILE = (100.0, 100.0, 924.0, 924.0, 185.0)
TILE_TOP = (0x30, 0x36, 0x42)
TILE_BOTTOM = (0x17, 0x1A, 0x20)

SQUARE = 250.0
GAP = 56.0
RADIUS = 58.0
FIRST = (SIZE - (2 * SQUARE + GAP)) / 2
LIT = (0x4F, 0x8F, 0xE8)
UNLIT = (0x4B, 0x53, 0x62)
MARK = (0xFF, 0xFF, 0xFF)


def rounded_box(x, y, left, top, right, bottom, radius):
    """Distance from (x, y) to a rounded box's edge, negative inside."""
    half_w = (right - left) / 2 - radius
    half_h = (bottom - top) / 2 - radius
    dx = abs(x - (left + right) / 2) - half_w
    dy = abs(y - (top + bottom) / 2) - half_h
    outside = (max(dx, 0.0) ** 2 + max(dy, 0.0) ** 2) ** 0.5
    return outside + min(max(dx, dy), 0.0) - radius


def convex(x, y, points):
    """Distance to a convex polygon given clockwise on screen, negative inside."""
    worst = -1e9
    count = len(points)
    for index in range(count):
        ax, ay = points[index]
        bx, by = points[(index + 1) % count]
        ex, ey = bx - ax, by - ay
        length = (ex * ex + ey * ey) ** 0.5
        # The outward normal of a clockwise edge (y grows downwards).
        worst = max(worst, ((x - ax) * ey - (y - ay) * ex) / length)
    return worst


def coverage(distance):
    return min(1.0, max(0.0, 0.5 - distance))


def over(base, colour, alpha):
    r, g, b, a = base
    out_a = alpha + a * (1 - alpha)
    if out_a <= 0:
        return (0.0, 0.0, 0.0, 0.0)
    mix = lambda top, under: (top * alpha + under * a * (1 - alpha)) / out_a
    return (mix(colour[0], r), mix(colour[1], g), mix(colour[2], b), out_a)


# How much the tile grows to fill the square.
ZOOM = SIZE / (TILE[2] - TILE[0])


def pixel(x, y, full_bleed=False):
    px, py = x + 0.5, y + 0.5
    colour = (0.0, 0.0, 0.0, 0.0)
    zoom = 1.0
    if full_bleed:
        # Look up the point of the ordinary icon that lands on this pixel
        # once the tile has grown about the centre; distances grow as much.
        zoom = ZOOM
        px = SIZE / 2 + (px - SIZE / 2) / zoom
        py = SIZE / 2 + (py - SIZE / 2) / zoom
        tile = 1.0
    else:
        tile = coverage(rounded_box(px, py, *TILE))
    if tile <= 0:
        return colour
    t = min(1.0, max(0.0, (py - TILE[1]) / (TILE[3] - TILE[1])))
    shade = tuple(TILE_TOP[i] + (TILE_BOTTOM[i] - TILE_TOP[i]) * t for i in range(3))
    colour = over(colour, shade, tile)
    for row in range(2):
        for column in range(2):
            left = FIRST + column * (SQUARE + GAP)
            top = FIRST + row * (SQUARE + GAP)
            inside = coverage(
                rounded_box(px, py, left, top, left + SQUARE, top + SQUARE, RADIUS) * zoom
            )
            if inside > 0:
                first = row == 0 and column == 0
                colour = over(colour, LIT if first else UNLIT, inside)
    # The start mark, centred by its centroid in the lit square.
    cx = FIRST + SQUARE / 2 + 8
    cy = FIRST + SQUARE / 2
    mark = [(cx - 44, cy - 60), (cx + 60, cy), (cx - 44, cy + 60)]
    inside = coverage(convex(px, py, mark) * zoom)
    if inside > 0:
        colour = over(colour, MARK, inside)
    return colour


def png(path, full_bleed=False):
    rows = bytearray()
    for y in range(SIZE):
        rows.append(0)
        for x in range(SIZE):
            r, g, b, a = pixel(x, y, full_bleed)
            if full_bleed:
                rows += bytes((round(r), round(g), round(b)))
            else:
                rows += bytes((round(r), round(g), round(b), round(a * 255)))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    # Colour type 2 is RGB with no alpha channel; 6 is RGBA.
    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 2 if full_bleed else 6, 0, 0, 0)
    with open(path, "wb") as out:
        out.write(b"\x89PNG\r\n\x1a\n")
        out.write(chunk(b"IHDR", header))
        out.write(chunk(b"IDAT", zlib.compress(bytes(rows), 9)))
        out.write(chunk(b"IEND", b""))


if __name__ == "__main__":
    args = sys.argv[1:]
    full_bleed = "--full-bleed" in args
    args = [arg for arg in args if arg != "--full-bleed"]
    if len(args) != 1 or args[0] in ("-h", "--help"):
        print(__doc__.strip())
        sys.exit(0 if args in (["-h"], ["--help"]) else 2)
    png(args[0], full_bleed)
