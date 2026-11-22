"""Writes Penryn.icns: an isometric grass-topped block in pixel art, drawn here (no game
textures), as PNG-encoded ic07 (128) and ic08 (256) images, which Mac OS X 10.6 reads.

    python3 tools/make_icon.py OUT.icns
"""
import struct
import sys
import zlib


def png(w, h, px):
    raw = b"".join(b"\x00" + bytes(px[y * w * 4:(y + 1) * w * 4]) for y in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def noise(x, y, salt):
    v = (x * 73856093) ^ (y * 19349663) ^ (salt * 83492791)
    v = (v ^ (v >> 13)) * 1274126177 & 0xFFFFFFFF
    return (v >> 8) % 5


GRASS = [(84, 140, 52), (96, 156, 60), (108, 170, 66), (74, 126, 46), (118, 182, 72)]
DIRT = [(134, 96, 67), (121, 85, 58), (150, 108, 74), (106, 75, 51), (141, 101, 70)]


def face_colour(face, u, v):
    """Colour of a 16x16 face texel. face 0 top, 1 left, 2 right."""
    if face == 0:
        c = GRASS[noise(u, v, 1)]
    elif v < 3 + (noise(u, 0, 7) % 2):
        c = GRASS[noise(u, v, 2)]
    else:
        c = DIRT[noise(u, v, 3 + face)]
    shade = (1.0, 0.82, 0.64)[face]
    return tuple(int(k * shade) for k in c)


def draw(size):
    px = bytearray(size * size * 4)
    # An isometric cube on a 64-unit grid, scaled to the icon.
    s = size / 64.0
    cx, top, edge = 32.0, 6.0, 26.0  # centre x, top point y, half width
    hh = edge / 2.0                   # half height of the top rhombus
    side = 30.0                       # height of the side faces
    for py in range(size):
        for pxl in range(size):
            x, y = (pxl + 0.5) / s, (py + 0.5) / s
            dx = x - cx
            # Top face: the rhombus around (cx, top + hh).
            a = (dx / edge + (y - top - hh) / hh)
            b = (-dx / edge + (y - top - hh) / hh)
            col = None
            if abs(dx) / edge + abs(y - top - hh) / hh <= 1.0:
                u = int((a + 1) / 2 * 16) % 16
                v = int((b + 1) / 2 * 16) % 16
                col = face_colour(0, u, v)
            elif -edge <= dx < 0 and top + hh - dx * hh / edge * -1 <= y + 0 and \
                    y <= top + 2 * hh + side + dx * hh / edge * -1 - (0):
                # Left face: between x = cx - edge and cx.
                t = (dx + edge) / edge
                y0 = top + hh + t * hh
                if y0 <= y <= y0 + side:
                    u = int(t * 16) % 16
                    v = int((y - y0) / side * 16) % 16
                    col = face_colour(1, u, v)
            if col is None and 0 <= dx <= edge:
                t = dx / edge
                y0 = top + 2 * hh - t * hh
                if y0 <= y <= y0 + side:
                    u = int(t * 16) % 16
                    v = int((y - y0) / side * 16) % 16
                    col = face_colour(2, u, v)
            if col is not None:
                o = (py * size + pxl) * 4
                px[o:o + 4] = bytes((col[0], col[1], col[2], 255))
    return png(size, size, px)


def main():
    out = sys.argv[1]
    parts = [(b"ic07", draw(128)), (b"ic08", draw(256))]
    body = b"".join(t + struct.pack(">I", len(d) + 8) + d for t, d in parts)
    with open(out, "wb") as f:
        f.write(b"icns" + struct.pack(">I", len(body) + 8) + body)


if __name__ == "__main__":
    main()
