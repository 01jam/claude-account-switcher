#!/usr/bin/env python3
"""Render every icon the app ships, with no image library involved.

Two families:

* **Tray** (`tray.png`, `tray-alert.png`) — Tabler's `arrows-exchange`, drawn
  from its two SVG paths (`M7 10h14l-4 -4`, `M17 14h-14l4 4`) with stroke-width
  2 and round caps, so each segment is a capsule of radius 1 in viewBox units.
  Scaled to the stroked bounding box rather than the 24x24 viewBox, otherwise it
  reads as small in a panel. The `-alert` variant adds a warning badge.

* **App** (`32x32`, `128x128`, `128x128@2x`, `icon`, and `icon.icns` for the
  macOS bundle) — a terracotta squircle with the same arrows glyph centred on
  it in white, tilted 15°. Nothing is borrowed from anyone else's mark, so
  these regenerate on any machine and carry no redistribution caveat.

Run: python3 scripts/generate-icons.py
"""

import math
import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "src-tauri", "icons")
# Claude's terracotta, sampled from the plate of the icon Claude Desktop ships:
# the tray glyph and the whole app plate are this one colour.
ACCENT = (217, 119, 87)
GLYPH = (255, 255, 255)  # the arrows on the plate
BADGE = (230, 145, 45)  # amber, distinct from the glyph at a glance

# --- arrows-exchange geometry -------------------------------------------

SEGMENTS = [
    ((7, 10), (21, 10)),
    ((21, 10), (17, 6)),
    ((17, 14), (3, 14)),
    ((3, 14), (7, 18)),
]
STROKE_R = 1.0
GX0, GX1 = 3 - STROKE_R, 21 + STROKE_R
GY0, GY1 = 6 - STROKE_R, 18 + STROKE_R


def dist_to_segment(p, a, b):
    dx, dy = b[0] - a[0], b[1] - a[1]
    length2 = dx * dx + dy * dy
    if length2 == 0:
        return math.hypot(p[0] - a[0], p[1] - a[1])
    t = max(0.0, min(1.0, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / length2))
    return math.hypot(p[0] - (a[0] + t * dx), p[1] - (a[1] + t * dy))


def mix(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


# --- PNG ------------------------------------------------------------------


def write_png(path, size, pixels):
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        for x in range(size):
            raw.extend(pixels[y * size + x])

    def chunk(tag, body):
        return (
            struct.pack(">I", len(body))
            + tag
            + body
            + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF)
        )

    blob = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as f:
        f.write(blob)
    print(f"  {os.path.basename(path):18} {size:>4}px  {len(blob):>6} bytes")


# --- tray icons -----------------------------------------------------------

TRAY_N = 128
TRAY_MARGIN = 6
BADGE_R = TRAY_N * 0.21
BADGE_CX = TRAY_N - BADGE_R - 2
BADGE_CY = TRAY_N - BADGE_R - 2
BADGE_CLEAR = BADGE_R * 1.16
TRI = [
    (BADGE_CX, BADGE_CY - BADGE_R),
    (BADGE_CX - BADGE_R * 0.94, BADGE_CY + BADGE_R * 0.72),
    (BADGE_CX + BADGE_R * 0.94, BADGE_CY + BADGE_R * 0.72),
]


def in_triangle(p, a, b, c):
    def side(o, u, v):
        return (u[0] - o[0]) * (v[1] - o[1]) - (u[1] - o[1]) * (v[0] - o[0])

    d1, d2, d3 = side(p, a, b), side(p, b, c), side(p, c, a)
    return not ((d1 < 0 or d2 < 0 or d3 < 0) and (d1 > 0 or d2 > 0 or d3 > 0))


def render_tray(alert):
    scale = (TRAY_N - 2 * TRAY_MARGIN) / (GX1 - GX0)
    off_x = TRAY_MARGIN
    off_y = (TRAY_N - (GY1 - GY0) * scale) / 2
    segs = [
        (
            ((a[0] - GX0) * scale + off_x, (a[1] - GY0) * scale + off_y),
            ((b[0] - GX0) * scale + off_x, (b[1] - GY0) * scale + off_y),
        )
        for a, b in SEGMENTS
    ]
    radius = STROKE_R * scale

    def on_badge(p):
        if not in_triangle(p, *TRI):
            return False
        dx = abs(p[0] - BADGE_CX)
        dy = p[1] - BADGE_CY
        bar = dx <= BADGE_R * 0.11 and -BADGE_R * 0.22 <= dy <= BADGE_R * 0.30
        dot = dx <= BADGE_R * 0.12 and BADGE_R * 0.42 <= dy <= BADGE_R * 0.58
        return not (bar or dot)

    S = 4
    total = S * S
    pixels = []
    for y in range(TRAY_N):
        for x in range(TRAY_N):
            glyph = badge = 0
            for sy in range(S):
                for sx in range(S):
                    p = (x + (sx + 0.5) / S, y + (sy + 0.5) / S)
                    if alert and on_badge(p):
                        badge += 1
                        continue
                    if min(dist_to_segment(p, a, b) for a, b in segs) <= radius:
                        if alert and math.hypot(p[0] - BADGE_CX, p[1] - BADGE_CY) <= BADGE_CLEAR:
                            continue
                        glyph += 1
            if badge:
                pixels.append((*BADGE, round(255 * badge / total)))
            elif glyph:
                pixels.append((*ACCENT, round(255 * glyph / total)))
            else:
                pixels.append((0, 0, 0, 0))
    return pixels


# --- app icon -------------------------------------------------------------

GLYPH_ANGLE = math.radians(-15)  # screen coords: negative reads anticlockwise
GLYPH_SPAN = 0.60  # share of the icon side the glyph spans before rotating
PLATE_MARGIN = 0.03
PLATE_N = 4.5  # superellipse exponent: squircle, not a rounded rectangle


def render_app(size):
    scale = (size * GLYPH_SPAN) / (GX1 - GX0)
    stroke_r = STROKE_R * scale

    # Rotate around the glyph's own centre, then centre the *rotated* bounding
    # box on the plate. Centring the unrotated box instead would leave the mark
    # visibly low and to one side, because the tilt is what sets its extents.
    cx, cy = (GX0 + GX1) / 2, (GY0 + GY1) / 2
    cos_a, sin_a = math.cos(GLYPH_ANGLE), math.sin(GLYPH_ANGLE)

    def place(p):
        x, y = (p[0] - cx) * scale, (p[1] - cy) * scale
        return (x * cos_a - y * sin_a, x * sin_a + y * cos_a)

    rotated = [(place(a), place(b)) for a, b in SEGMENTS]
    xs = [p[0] for seg in rotated for p in seg]
    ys = [p[1] for seg in rotated for p in seg]
    dx = size / 2 - (min(xs) + max(xs)) / 2
    dy = size / 2 - (min(ys) + max(ys)) / 2
    segs = [((a[0] + dx, a[1] + dy), (b[0] + dx, b[1] + dy)) for a, b in rotated]

    half = size * (1 - 2 * PLATE_MARGIN) / 2
    pc = size / 2

    def on_plate(p):
        u, v = abs(p[0] - pc) / half, abs(p[1] - pc) / half
        return u <= 1 and v <= 1 and u**PLATE_N + v**PLATE_N <= 1.0

    S = 3
    total = S * S
    pixels = []
    for y in range(size):
        for x in range(size):
            arrow = plate = 0
            for sy in range(S):
                for sx in range(S):
                    p = (x + (sx + 0.5) / S, y + (sy + 0.5) / S)
                    if not on_plate(p):
                        continue
                    plate += 1
                    if min(dist_to_segment(p, a, b) for a, b in segs) <= stroke_r:
                        arrow += 1

            if not plate:
                pixels.append((0, 0, 0, 0))
                continue

            rgb = mix(ACCENT, GLYPH, arrow / total)
            # The plate's own coverage sets alpha, keeping the squircle smooth.
            pixels.append((*rgb, round(255 * plate / total)))
    return pixels


# --- macOS bundle icon ---------------------------------------------------

# An .icns is a container: a header, then one record per size holding the PNG
# verbatim. Which sizes go in is fixed by the format; these are the ones the
# app already renders. `ic09`/`ic14` and `ic08`/`ic13` carry the same pixels at
# a nominal size and its retina equivalent, which is how macOS wants them.
ICNS_ENTRIES = [
    (b"icp5", "32x32.png"),
    (b"ic07", "128x128.png"),
    (b"ic08", "128x128@2x.png"),
    (b"ic13", "128x128@2x.png"),
    (b"ic09", "icon.png"),
    (b"ic14", "icon.png"),
]


def write_icns(path):
    body = b""
    for kind, name in ICNS_ENTRIES:
        src = os.path.join(OUT, name)
        if not os.path.exists(src):
            continue
        with open(src, "rb") as f:
            data = f.read()
        body += kind + struct.pack(">I", len(data) + 8) + data

    with open(path, "wb") as f:
        f.write(b"icns" + struct.pack(">I", len(body) + 8) + body)
    print(f"  {os.path.basename(path)}")


def main():
    print("tray:")
    write_png(os.path.join(OUT, "tray.png"), TRAY_N, render_tray(alert=False))
    write_png(os.path.join(OUT, "tray-alert.png"), TRAY_N, render_tray(alert=True))

    print("app:")
    for name, size in [
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("icon.png", 512),
    ]:
        write_png(os.path.join(OUT, name), size, render_app(size))

    print("macOS bundle:")
    write_icns(os.path.join(OUT, "icon.icns"))


if __name__ == "__main__":
    main()
