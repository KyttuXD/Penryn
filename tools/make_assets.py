#!/usr/bin/env python3
"""Penryn's game data, made from the player's own copy of Minecraft 26.3.

    tools/make_assets.py PREPARED_DIR [--jar CLIENT_JAR] OUT_DIR

PREPARED_DIR is what Horizon's `horizon-prepare` writes from the player's game
(.local/horizon-client/26.3): the block registry order, every state's properties, hardness,
outline shapes and tags. Block models, block states and textures are read from CLIENT_JAR
(the 26.3 client the launcher downloaded for the signed-in account) when given, else from
PREPARED_DIR/assets.

Writes OUT_DIR/blocks.bin, OUT_DIR/atlas.bin and OUT_DIR/items.bin (the packs' item
definitions, "PNI1": see below). Nothing from Mojang is kept in this
repository: these files are made on the player's machine and stay with their copy of the game.

blocks.bin (little-endian), "PNBC":
    u32 states, u32 blocks, u32 boxes, u32 quads, u32 items, u32 collision boxes
    per state: u16 block, u16 hardness x100 (65535: unbreakable), u8 flags (1 needs the right
               tool, 2 full collision, 4 replaceable, 8 opaque full cube, 16 a bubble column
               that drags down, 32 holds a fluid, 64 open), u8 box count,
               u16 first box, u32 first quad, u16 quad count, u16 facing (0 north 1 south 2 west
               3 east 4 down 5 up, 255 none), u32 first collision
               box, u8 collision box count, u8 dynamic collision rule (DYNAMIC_COLLISION;
               0: the boxes), u16 pad
    per box: u8 x0 y0 z0 x1 y1 z1 in sixteenths (the outline shape)
    per collision box: i8 x0 y0 z0 x1 y1 z1 in 1/32 block (vanilla's collision shape)
    per quad (48 bytes): 4 x (i16 x, y, z in 1/1024 block; u16 u, v in 1/65535 of the atlas),
               u8 cull (0 down 1 up 2 north 3 south 4 west 5 east, 6 none), u8 flags (1 tinted,
               2 cutout, 4 translucent, 8 unshaded, 16 on the cull face's boundary), u8 light
               (shade x255), u8 pad, i32 pad
    per block: u8 kind, u8 tint (0 none, 1 grass, 2 foliage, 3 birch, 4 spruce, 5 water,
               6 redstone, 7 stem), u8 mineable, u8 incorrect-for tiers, u8[3] map colour,
               u8 name length, name, u16[5] sound events, u8 volume, u8 pitch (x100),
               f32 friction, f32 speed factor, f32 jump factor, u8 offset (1 model, 2 outline,
               4 also vertical), f32 largest horizontal offset, f32 largest vertical offset
    per item: u8 name length, name
    then: 4 x f32, the atlas rectangle of water_still, and 4 x f32 of lava_still
kind: 0 air, 1 solid, 2 cutout, 3 plant (no collision), 4 water, 5 lava, 6 invisible solid

atlas.bin: "PNA2", u16 width, u16 height, then width x height RGBA (16x16 tiles).
items.bin: see the writer in main (the packs' item definitions, keyed by ns:path).
"""
import json
import math
import os
import struct
import sys
import zipfile
import zlib

TILE = 16


# ------------------------------------------------------------------ files

RESOURCE_FORMAT = (97, 1)  # 26.3's pack_version.resource_major/minor


def pack_format_value(v):
    """A pack.mcmeta format: an int (major) or [major, minor]."""
    if isinstance(v, list):
        return (int(v[0]) if v else 0, int(v[1]) if len(v) > 1 else 0)
    return (int(v), 0)


def formats_include(spec, fmt=RESOURCE_FORMAT, minor_max=False):
    """Whether an overlay's or a pack's formats (int, [min, max], {min_inclusive, max_inclusive},
    or min_format / max_format) take in 26.3's format."""
    if spec is None:
        return False
    if isinstance(spec, dict) and ("min_format" in spec or "max_format" in spec):
        lo = pack_format_value(spec.get("min_format", 0))
        hi = spec.get("max_format")
        hi = pack_format_value(hi) if hi is not None else (1 << 30, 0)
        if not isinstance(spec.get("max_format"), list):
            hi = (hi[0], 1 << 30)  # an int max takes every minor version of it
        return lo <= fmt <= hi
    if isinstance(spec, dict):
        lo, hi = spec.get("min_inclusive", 0), spec.get("max_inclusive", 1 << 30)
    elif isinstance(spec, list):
        lo, hi = (spec[0], spec[-1]) if spec else (0, -1)
    else:
        lo = hi = spec
    return int(lo) <= fmt[0] <= int(hi)


class Pack:
    """One resource pack (a zip or a folder) as vanilla's pack stack sees it: pack.mcmeta, its
    overlays that apply to 26.3 (later entries on top), and its filter, which hides the files of
    the packs below it."""

    def __init__(self, path):
        import re
        self.path = path
        self.zip = zipfile.ZipFile(path) if os.path.isfile(path) else None
        meta = self._raw("pack.mcmeta")
        try:
            meta = json.loads(meta.decode("utf-8-sig")) if meta else {}
        except ValueError:
            meta = {}
        self.meta = meta
        pk = meta.get("pack", {}) if isinstance(meta, dict) else {}
        self.description = pk.get("description", "")
        fmt = pk.get("pack_format")
        if "min_format" in pk or "max_format" in pk:
            self.compatible = formats_include(pk)
        else:
            sup = pk.get("supported_formats")
            self.compatible = formats_include(sup if sup is not None else fmt)
        self.roots = [""]
        for e in (meta.get("overlays", {}) or {}).get("entries", []) if isinstance(meta, dict) else []:
            d = e.get("directory", "")
            if not re.match(r"^[a-z0-9_.-]+$", d or ""):
                continue
            if formats_include(e if ("min_format" in e or "max_format" in e) else e.get("formats")):
                self.roots.append(d + "/")
        self.filters = []
        for f in ((meta.get("filter", {}) or {}).get("block", []) if isinstance(meta, dict) else []):
            try:
                self.filters.append((re.compile(f["namespace"]) if f.get("namespace") is not None else None,
                                     re.compile(f["path"]) if f.get("path") is not None else None))
            except (re.error, KeyError, TypeError):
                pass
        self._names = None

    def _raw(self, name):
        # Server packs are untrusted: no path out of the pack, no member that inflates past
        # PACK_MEMBER_LIMIT (a zip bomb).
        if not safe_name(name):
            return None
        if self.zip:
            try:
                info = self.zip.getinfo(name)
            except KeyError:
                return None
            if info.file_size > PACK_MEMBER_LIMIT:
                return None
            try:
                return self.zip.read(info)
            except (zipfile.BadZipFile, NotImplementedError, RuntimeError, OSError, EOFError):
                return None
        p = os.path.join(self.path, *name.split("/"))
        try:
            with open(p, "rb") as f:
                return f.read()
        except OSError:
            return None

    def read(self, full):
        for root in reversed(self.roots):
            d = self._raw(root + full)
            if d is not None:
                return d
        return None

    def names(self):
        """Every assets/... file it has (overlay folders folded in)."""
        if self._names is None:
            raw = []
            if self.zip:
                raw = self.zip.namelist()
            else:
                for r, _, files in os.walk(self.path):
                    for fn in files:
                        raw.append(os.path.relpath(os.path.join(r, fn), self.path).replace(os.sep, "/"))
            out = set()
            for n in raw:
                for root in self.roots:
                    if root and n.startswith(root):
                        n = n[len(root):]
                        break
                if n.startswith("assets/") and not n.endswith("/"):
                    out.add(n)
            self._names = out
        return self._names

    def compat_word(self):
        """Vanilla's Pack.Compatibility for the pack list: "1" (compatible), "new" (made for a
        newer version), "old" (for an older one) or "unknown" (no format, or a broken one)."""
        if self.compatible:
            return "1"
        pk = self.meta.get("pack", {}) if isinstance(self.meta, dict) else {}
        try:
            if "min_format" in pk or "max_format" in pk:
                lo = pack_format_value(pk.get("min_format", 0))
                hi = pk.get("max_format")
                hi = pack_format_value(hi) if hi is not None else (1 << 30, 0)
            else:
                sup = pk.get("supported_formats", pk.get("pack_format"))
                if isinstance(sup, list) and sup:
                    lo, hi = pack_format_value(sup[0]), pack_format_value(sup[-1])
                elif sup is None or isinstance(sup, (dict, list)):
                    return "unknown"
                else:
                    lo = hi = pack_format_value(sup)
            if lo > RESOURCE_FORMAT:
                return "new"
            if hi < RESOURCE_FORMAT:
                return "old"
        except (TypeError, ValueError, IndexError):
            pass
        return "unknown"

    def hides(self, full):
        """The pack's filter hides `full` (assets/ns/path) of the packs below it."""
        parts = full.split("/", 2)
        if len(parts) < 3:
            return False
        ns, p = parts[1], parts[2]
        for nre, pre in self.filters:
            if (nre is None or nre.fullmatch(ns)) and (pre is None or pre.fullmatch(p)):
                return True
        return False


PACK_MEMBER_LIMIT = 64 << 20   # bytes of one file of a pack, inflated
PACK_IMAGE_LIMIT = 4096 * 4096  # pixels of one PNG of a pack


def safe_name(name):
    """A file name inside a pack or the game: no empty, "." or ".." part, nothing absolute."""
    return isinstance(name, str) and "\\" not in name and "\0" not in name and \
        all(p not in ("", ".", "..") for p in name.split("/"))


def full_name(rel):
    """textures/block/stone.png -> assets/minecraft/textures/block/stone.png; a namespaced
    reference kept in the path (textures/custom:item/x.png) -> assets/custom/textures/item/x.png."""
    i = rel.find(":")
    if i < 0:
        return "assets/minecraft/" + rel
    j = rel.rfind("/", 0, i) + 1
    return "assets/%s/%s%s" % (rel[j:i], rel[:j], rel[i + 1:])


def nsref(s):
    """A resource location as the asset reader takes it: minecraft's without its namespace,
    others' with it (custom:block/x)."""
    if not isinstance(s, str):
        return s
    return s[len("minecraft:"):] if s.startswith("minecraft:") else s


class Assets:
    """assets/... from a jar or a folder, with resource packs layered over it as vanilla's pack
    stack does: `packs` bottom to top, the last one's files winning."""

    def __init__(self, jar, folder, packs=()):
        self.zip = zipfile.ZipFile(jar) if jar else None
        self.folder = folder
        self.packs = [Pack(p) for p in packs]

    def base(self, full):
        if not safe_name(full):
            return None
        # The fonts: the client jar's include/unifont.json lists no providers (the game gets the
        # Unifont definitions and its zips from the downloaded assets), so with a jar the prepared
        # folder, which has both, answers first.
        if self.folder and self.zip and full.startswith("assets/minecraft/font/"):
            try:
                with open(os.path.join(self.folder, *full.split("/")), "rb") as f:
                    return f.read()
            except OSError:
                pass
        if self.zip:
            try:
                return self.zip.read(full)
            except KeyError:
                if not self.folder:
                    return None
        if not self.folder:
            return None
        p = os.path.join(self.folder, *full.split("/"))
        try:
            with open(p, "rb") as f:
                return f.read()
        except OSError:
            return None

    def layers(self, full, with_base=True):
        """The file in each layer that has it, bottom (the game) to top, as getResourceStack."""
        out = []
        for pk in reversed(self.packs):
            d = pk.read(full)
            if d is not None:
                out.append(d)
            if pk.hides(full):
                return out[::-1]
        d = self.base(full) if with_base else None
        if d is not None:
            out.append(d)
        return out[::-1]

    def read(self, rel):
        full = full_name(rel)
        for pk in reversed(self.packs):
            d = pk.read(full)
            if d is not None:
                return d
            if pk.hides(full):
                return None
        return self.base(full)

    def meta(self, rel):
        """`rel`'s .mcmeta from the layer `rel` itself comes from (vanilla reads a resource's
        metadata from its own pack): None when that layer has none."""
        full = full_name(rel)
        for pk in reversed(self.packs):
            if pk.read(full) is not None:
                return pk.read(full + ".mcmeta")
            if pk.hides(full):
                return None
        return self.base(full + ".mcmeta")

    def from_pack(self, rel):
        """The top pack's own copy of `rel` (None: the game's, or nobody's)."""
        full = full_name(rel)
        for pk in reversed(self.packs):
            d = pk.read(full)
            if d is not None:
                return d
            if pk.hides(full):
                return None
        return None

    def json(self, rel):
        d = self.read(rel)
        if d is None:
            return None
        try:
            return json.loads(d.decode("utf-8-sig"))
        except ValueError:
            return None

    def json_stack(self, rel, packs_only=False):
        out = []
        for d in self.layers(full_name(rel), with_base=not packs_only):
            try:
                out.append(json.loads(d.decode("utf-8-sig")))
            except ValueError:
                pass
        return out

    def image(self, rel):
        """A PNG, first frame chosen by its animation (.mcmeta frames[0]); a pack's higher
        resolution copy of a game texture scaled down to the game's size (Penryn draws the
        game's sizes)."""
        if not isinstance(rel, str):
            return None
        own = self.from_pack(rel) if self.packs else None
        data = self.read(rel)
        size = png_header(data)
        if own is not None and size and size[0] * size[1] > PACK_IMAGE_LIMIT:
            print("%s: %dx%d is too large: left out" % (rel, size[0], size[1]))
            return None
        img = png_decode(data)
        if img is None:
            return None
        k = 1
        if own is not None:
            base = self.base(full_name(rel))
            bimg = png_header(base) if base else None
            w, h, px = img
            if bimg and (bimg[0], bimg[1]) != (w, h) and w % bimg[0] == 0 and h % bimg[1] == 0 \
                    and w // bimg[0] == h // bimg[1]:
                k = w // bimg[0]
                img = scale_down(img, k)
        if rel.endswith(".png"):
            meta = self.meta(rel)
            anim = None
            if meta:
                try:
                    anim = json.loads(meta.decode("utf-8-sig")).get("animation")
                except (ValueError, AttributeError):
                    anim = None
            if isinstance(anim, dict):
                img = first_frame(img, anim, k)
        return img

    def names(self):
        """Every assets/... file of the stack."""
        out = set(self.zip.namelist()) if self.zip else set()
        if not self.zip:
            base = os.path.join(self.folder, "assets")
            for r, _, files in os.walk(base):
                for fn in files:
                    out.add("assets/" + os.path.relpath(os.path.join(r, fn), base).replace(os.sep, "/"))
        for pk in self.packs:
            if pk.filters:
                out = set(n for n in out if not pk.hides(n))
            out |= pk.names()
        return sorted(out)


def png_header(data):
    """(width, height) of a PNG without decoding it."""
    if not data or data[:8] != b"\x89PNG\r\n\x1a\n" or len(data) < 24:
        return None
    return struct.unpack(">II", data[16:24])


def first_frame(img, anim, k=1):
    """The frame an .mcmeta animation shows first (frames[0], else frame 0), as SpriteContents:
    frames of width x height (default: square, the shorter side), left to right, top to bottom."""
    w, h, px = img
    side = min(w, h)
    fw = max(1, int(anim.get("width", side * k)) // k)
    fh = max(1, int(anim.get("height", side * k)) // k)
    if "width" in anim and "height" not in anim:
        fh = h
    if "height" in anim and "width" not in anim:
        fw = w
    fw, fh = min(fw, w), min(fh, h)
    cols, rows = max(1, w // fw), max(1, h // fh)
    frames = anim.get("frames") or [0]
    first = frames[0].get("index", 0) if isinstance(frames[0], dict) else frames[0]
    try:
        first = int(first)
    except (TypeError, ValueError):
        first = 0
    if not 0 <= first < cols * rows:
        first = 0
    if (fw, fh) == (w, h):
        return img
    x0, y0 = (first % cols) * fw, (first // cols) * fh
    out = bytearray()
    for y in range(fh):
        o = ((y0 + y) * w + x0) * 4
        out += px[o:o + fw * 4]
    return fw, fh, bytes(out)


def scale_down(img, k):
    w, h, px = img
    nw, nh = w // k, h // k
    out = bytearray(nw * nh * 4)
    for y in range(nh):
        row = (y * k) * w
        for x in range(nw):
            o = (row + x * k) * 4
            out[(y * nw + x) * 4:(y * nw + x) * 4 + 4] = px[o:o + 4]
    return nw, nh, bytes(out)


def png_decode(data):
    """(width, height, RGBA bytes) of a non-interlaced PNG, or None."""
    if not data or data[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    pos, idat, plte, trns = 8, b"", None, None
    w = h = depth = ctype = 0
    while pos < len(data):
        n = struct.unpack(">I", data[pos:pos + 4])[0]
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + n]
        pos += 12 + n
        if kind == b"IHDR":
            w, h, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if interlace:
                return None
        elif kind == b"PLTE":
            plte = body
        elif kind == b"tRNS":
            trns = body
        elif kind == b"IDAT":
            idat += body
    ch = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(ctype)
    if ch is None or depth not in (1, 2, 4, 8, 16):
        return None
    raw = zlib.decompress(idat)
    bpp = ch * depth
    stride = (w * bpp + 7) // 8
    unit = max(1, bpp // 8)
    prev = bytearray(stride)
    out = bytearray(w * h * 4)
    # tRNS for grey and truecolour images: one colour that is transparent (compared after
    # the samples are scaled to 8 bits, as they are stored here).
    mask_ = (1 << depth) - 1
    scale_ = (lambda v: v * 255 // mask_) if depth < 8 else ((lambda v: v >> 8) if depth == 16 else (lambda v: v))
    grey_key = scale_(struct.unpack(">H", trns[:2])[0]) if ctype == 0 and trns and len(trns) >= 2 else None
    rgb_key = tuple(scale_(v) for v in struct.unpack(">HHH", trns[:6])) if ctype == 2 and trns and len(trns) >= 6 else None
    p = 0
    for y in range(h):
        f = raw[p]
        line = bytearray(raw[p + 1:p + 1 + stride])
        p += 1 + stride
        for i in range(stride):
            a = line[i - unit] if i >= unit else 0
            b = prev[i]
            c = prev[i - unit] if i >= unit else 0
            if f == 1:
                line[i] = (line[i] + a) & 255
            elif f == 2:
                line[i] = (line[i] + b) & 255
            elif f == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 255
        prev = line
        if depth == 8:
            s = line
        elif depth == 16:
            s = line[0::2]
        else:
            s = bytearray()
            per = 8 // depth
            mask = (1 << depth) - 1
            for i in range(w * ch):
                v = (line[i // per] >> (8 - depth * (i % per + 1))) & mask
                s.append(v if ctype == 3 else v * 255 // mask)
        for x in range(w):
            o = (y * w + x) * 4
            if ctype == 6:
                out[o:o + 4] = s[4 * x:4 * x + 4]
            elif ctype == 2:
                out[o:o + 3] = s[3 * x:3 * x + 3]
                out[o + 3] = 255
            elif ctype == 3:
                i = s[x]
                out[o:o + 3] = plte[3 * i:3 * i + 3]
                out[o + 3] = trns[i] if trns and i < len(trns) else 255
            elif ctype == 4:
                out[o] = out[o + 1] = out[o + 2] = s[2 * x]
                out[o + 3] = s[2 * x + 1]
            else:
                out[o] = out[o + 1] = out[o + 2] = s[x]
                out[o + 3] = 0 if grey_key is not None and s[x] == grey_key else 255
            if ctype == 2 and rgb_key is not None and tuple(out[o:o + 3]) == rgb_key:
                out[o + 3] = 0
    return w, h, bytes(out)


def tile_of(img):
    """A texture, or its first animation frame (first_frame), scaled whole to 16x16 (nearest).
    Whatever its shape: a still non-square texture is not cut to its top-left square (Horizon #19742)."""
    w, h, px = img
    out = bytearray(TILE * TILE * 4)
    for y in range(TILE):
        for x in range(TILE):
            sx, sy = x * w // TILE, y * h // TILE
            o = (sy * w + sx) * 4
            out[(y * TILE + x) * 4:(y * TILE + x) * 4 + 4] = px[o:o + 4]
    return bytes(out)


# ------------------------------------------------------------------ blocks

PLANT_CLASSES = {
    "BushBlock", "VegetationBlock", "FlowerBlock", "TallGrassBlock", "SaplingBlock", "CropBlock",
    "MushroomBlock", "DoublePlantBlock", "TorchBlock", "WallTorchBlock", "RedstoneWireBlock",
    "VineBlock", "SugarCaneBlock", "FireBlock", "SoulFireBlock", "SeagrassBlock",
    "TallSeagrassBlock", "KelpBlock", "KelpPlantBlock", "RailBlock", "BaseRailBlock", "ButtonBlock",
    "LeverBlock", "SignBlock", "WallSignBlock", "StandingSignBlock", "CeilingHangingSignBlock",
    "WallHangingSignBlock", "PressurePlateBlock", "WeightedPressurePlateBlock", "TripWireBlock",
    "TripWireHookBlock", "CobwebBlock", "WebBlock", "BannerBlock", "WallBannerBlock",
    "SweetBerryBushBlock", "NetherSproutsBlock", "RootsBlock", "DeadBushBlock",
    "DryVegetationBlock", "ShortDryGrassBlock", "TallDryGrassBlock", "FireflyBushBlock",
    "LeafLitterBlock", "FlowerBedBlock", "PinkPetalsBlock", "GlowLichenBlock", "MultifaceBlock",
    "SculkVeinBlock", "HangingRootsBlock", "SporeBlossomBlock", "CaveVinesBlock",
    "CaveVinesPlantBlock", "SmallDripleafBlock", "CoralPlantBlock", "CoralFanBlock",
    "CoralWallFanBlock", "BaseCoralPlantBlock", "BaseCoralFanBlock", "BaseCoralWallFanBlock",
    "WaterlilyBlock", "LilyPadBlock", "NetherWartBlock", "StemBlock", "AttachedStemBlock",
    "TwistingVinesBlock", "TwistingVinesPlantBlock", "WeepingVinesBlock", "WeepingVinesPlantBlock",
    "PitcherCropBlock", "TorchflowerCropBlock", "BeetrootBlock", "CarrotBlock", "PotatoBlock",
    "CocoaBlock", "BambooSaplingBlock", "EyeblossomBlock", "CactusFlowerBlock", "BushBlock",
    "HangingMossBlock", "MossyCarpetBlock_none",
}


def read_nbt(raw):
    """A whole NBT file (gzip or not) as Python values."""
    import gzip
    if raw[:2] == b"\x1f\x8b":
        raw = gzip.decompress(raw)
    pos = [0]

    def take(fmt):
        v = struct.unpack_from(">" + fmt, raw, pos[0])
        pos[0] += struct.calcsize(">" + fmt)
        return v[0]

    def name():
        n = take("H")
        v = raw[pos[0]:pos[0] + n].decode("utf-8", "replace")
        pos[0] += n
        return v

    def payload(t):
        if t in (1, 2, 3, 4, 5, 6):
            return take("bhiqfd"[t - 1])
        if t == 7:
            n = take("i")
            v = raw[pos[0]:pos[0] + n]
            pos[0] += n
            return v
        if t == 8:
            return name()
        if t == 9:
            et, n = take("b"), take("i")
            return [payload(et) for _ in range(n)]
        if t == 10:
            d = {}
            while True:
                tt = take("b")
                if tt == 0:
                    return d
                k = name()
                d[k] = payload(tt)
        if t in (11, 12):
            n = take("i")
            f = ">%d%s" % (n, "i" if t == 11 else "q")
            v = list(struct.unpack_from(f, raw, pos[0]))
            pos[0] += struct.calcsize(f)
            return v
        raise ValueError("bad NBT tag %d" % t)

    t = take("b")
    name()
    return payload(t)


def shape_boxes(sh):
    """A cached VoxelShape (coordinates + occupied cells) as merged boxes in blocks."""
    bits = lambda v: struct.unpack(">d", struct.pack(">q", v))[0]
    xs, ys, zs = ([bits(v) for v in sh[k]] for k in ("x", "y", "z"))
    nx, ny, nz = len(xs) - 1, len(ys) - 1, len(zs) - 1
    if nx <= 0 or ny <= 0 or nz <= 0:
        return []
    # "cube" only says the grid is a power-of-two one: the cells are always in "occupied".
    occ = sh.get("occupied", b"")
    assert len(occ) == nx * ny * nz, "cached shape without a full occupancy grid"
    full = lambda x, y, z: (x * ny + y) * nz + z < len(occ) and occ[(x * ny + y) * nz + z] != 0
    used = set()
    out = []
    for x in range(nx):
        for y in range(ny):
            for z in range(nz):
                if not full(x, y, z) or (x, y, z) in used:
                    continue
                z1 = z
                while z1 + 1 < nz and full(x, y, z1 + 1) and (x, y, z1 + 1) not in used:
                    z1 += 1
                y1 = y
                while y1 + 1 < ny and all(full(x, y1 + 1, k) and (x, y1 + 1, k) not in used for k in range(z, z1 + 1)):
                    y1 += 1
                x1 = x
                while x1 + 1 < nx and all(full(x1 + 1, j, k) and (x1 + 1, j, k) not in used
                                          for j in range(y, y1 + 1) for k in range(z, z1 + 1)):
                    x1 += 1
                for i in range(x, x1 + 1):
                    for j in range(y, y1 + 1):
                        for k in range(z, z1 + 1):
                            used.add((i, j, k))
                out.append((xs[x], ys[y], zs[z], xs[x1 + 1], ys[y1 + 1], zs[z1 + 1]))
    return out


# Blocks whose collision depends on the entity or the world (dynamic_shape): their own rules
# in physics.rs. 1 bamboo stalk, 2 scaffolding, 3 powder snow, 4 the outline moved by the
# block's offset (pointed dripstone, sulfur spike), 5 the outline (shulker boxes, closed).
DYNAMIC_COLLISION = {"bamboo": 1, "scaffolding": 2, "powder_snow": 3, "pointed_dripstone": 4,
                     "sulfur_spike": 4}


def kind_of(short, cls, facts):
    if short in ("air", "cave_air", "void_air", "structure_void", "light"):
        return 0
    if short == "barrier":
        return 6
    if short in ("water", "bubble_column"):
        return 4
    if short == "lava":
        return 5
    # Plants by class that vanilla still gives a collision shape equal to their outline.
    if short in ("lily_pad", "azalea", "flowering_azalea", "cocoa", "sea_pickle"):
        return 2
    if any(c in PLANT_CLASSES for c in cls) or (not facts.get("full_collision") and not facts.get("outline")):
        if not any(c in ("LadderBlock", "SnowLayerBlock") for c in cls):
            return 3
    if facts.get("solid_render") and facts.get("can_occlude"):
        return 1
    return 2 if not facts.get("can_occlude") else 1


def tint_of(short):
    if short in ("grass_block", "short_grass", "tall_grass", "fern", "large_fern", "potted_fern",
                 "sugar_cane", "bush"):
        return 1
    if short in ("birch_leaves",):
        return 3
    if short in ("spruce_leaves",):
        return 4
    if short.endswith("_leaves") and short not in ("cherry_leaves", "azalea_leaves",
                                                    "flowering_azalea_leaves", "pale_oak_leaves"):
        return 2
    if short in ("vine", "leaf_litter"):
        return 2
    if short in ("water", "bubble_column", "water_cauldron"):
        return 5
    if short == "redstone_wire":
        return 6
    if short in ("pumpkin_stem", "melon_stem", "attached_pumpkin_stem", "attached_melon_stem"):
        return 7
    return 0


# ------------------------------------------------------------------ models

DIRS = ["down", "up", "north", "south", "west", "east"]
NORMAL = {"down": (0, -1, 0), "up": (0, 1, 0), "north": (0, 0, -1), "south": (0, 0, 1),
          "west": (-1, 0, 0), "east": (1, 0, 0)}
SHADE = {"down": 0.5, "up": 1.0, "north": 0.8, "south": 0.8, "west": 0.6, "east": 0.6}


def face_corners(d, f, t):
    """The face's corners counter-clockwise from outside, each with its (s, t) on the
    texture rectangle (0..1, s along u, t along v)."""
    x0, y0, z0 = f
    x1, y1, z1 = t
    if d == "up":
        return [((x0, y1, z0), (0, 0)), ((x0, y1, z1), (0, 1)), ((x1, y1, z1), (1, 1)), ((x1, y1, z0), (1, 0))]
    if d == "down":
        return [((x0, y0, z1), (0, 0)), ((x0, y0, z0), (0, 1)), ((x1, y0, z0), (1, 1)), ((x1, y0, z1), (1, 0))]
    if d == "north":
        return [((x1, y1, z0), (0, 0)), ((x1, y0, z0), (0, 1)), ((x0, y0, z0), (1, 1)), ((x0, y1, z0), (1, 0))]
    if d == "south":
        return [((x0, y1, z1), (0, 0)), ((x0, y0, z1), (0, 1)), ((x1, y0, z1), (1, 1)), ((x1, y1, z1), (1, 0))]
    if d == "west":
        return [((x0, y1, z0), (0, 0)), ((x0, y0, z0), (0, 1)), ((x0, y0, z1), (1, 1)), ((x0, y1, z1), (1, 0))]
    # east
    return [((x1, y1, z1), (0, 0)), ((x1, y0, z1), (0, 1)), ((x1, y0, z0), (1, 1)), ((x1, y1, z0), (1, 0))]


def default_uv(d, f, t):
    x0, y0, z0 = f
    x1, y1, z1 = t
    return {
        "down": [x0, 16 - z1, x1, 16 - z0],
        "up": [x0, z0, x1, z1],
        "north": [16 - x1, 16 - y1, 16 - x0, 16 - y0],
        "south": [x0, 16 - y1, x1, 16 - y0],
        "west": [z0, 16 - y1, z1, 16 - y0],
        "east": [16 - z1, 16 - y1, 16 - z0, 16 - y0],
    }[d]


def rot_point(p, axis, deg, origin, rescale):
    a = math.radians(deg)
    c, s = math.cos(a), math.sin(a)
    x, y, z = p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]
    if axis == "x":
        y, z = y * c - z * s, y * s + z * c
        k = (1 / c) if rescale else 1
        y, z = y * k, z * k
    elif axis == "y":
        x, z = x * c + z * s, -x * s + z * c
        k = (1 / c) if rescale else 1
        x, z = x * k, z * k
    else:
        x, y = x * c - y * s, x * s + y * c
        k = (1 / c) if rescale else 1
        x, y = x * k, y * k
    return (x + origin[0], y + origin[1], z + origin[2])


def variant_rotate(p, xr, yr):
    """Blockstate x then y rotation (multiples of 90) about the block's centre, in sixteenths."""
    x, y, z = p
    for _ in range((xr // 90) % 4):  # x=90: up -> north, north -> down
        y, z = 16 - z, y
    for _ in range((yr // 90) % 4):  # y=90: north -> east
        x, z = 16 - z, x
    return (x, y, z)


def dir_rotate(d, xr, yr):
    n = NORMAL[d]
    x, y, z = n
    for _ in range((xr // 90) % 4):
        y, z = -z, y
    for _ in range((yr // 90) % 4):
        x, z = -z, x
    for k, v in NORMAL.items():
        if v == (x, y, z):
            return k
    return d


class Models:
    def __init__(self, assets):
        self.a = assets
        self.cache = {}

    def load(self, ref, depth=0):
        """(textures, elements) with parents resolved."""
        ref = nsref(ref)
        if ref in self.cache:
            return self.cache[ref]
        m = self.a.json("models/" + ref + ".json") or {}
        tex, elements = {}, None
        if "parent" in m and depth < 16:
            pt, pe = self.load(m["parent"], depth + 1)
            tex.update(pt)
            elements = pe
        tex.update(m.get("textures", {}))
        if "elements" in m:
            elements = m["elements"]
        self.cache[ref] = (tex, elements)
        return tex, elements


def resolve_tex(tex, v):
    for _ in range(12):
        if isinstance(v, dict):  # 26.x: {"sprite": ..., "force_translucent": ...}
            v = v.get("sprite")
        if not isinstance(v, str):
            return None
        # Some models (heavy_core) name a texture key without its "#".
        if not v.startswith("#") and v in tex and ":" not in v and "/" not in v:
            v = "#" + v
        if not v.startswith("#"):
            return nsref(v)
        v = tex.get(v[1:])
    return None


def matches(cond, props):
    if "OR" in cond:
        return any(matches(c, props) for c in cond["OR"])
    if "AND" in cond:
        return all(matches(c, props) for c in cond["AND"])
    for k, v in cond.items():
        if str(props.get(k)) not in str(v).split("|"):
            return False
    return True


def applied_models(bs, props):
    out = []
    if not bs:
        return out
    if "variants" in bs:
        for key, v in bs["variants"].items():
            want = dict(kv.split("=", 1) for kv in key.split(",") if "=" in kv)
            if all(str(props.get(k)) == val for k, val in want.items()):
                out.append(v[0] if isinstance(v, list) else v)
                break
    for part in bs.get("multipart", []):
        if "when" not in part or matches(part["when"], props):
            a = part["apply"]
            out.append(a[0] if isinstance(a, list) else a)
    return out


# ------------------------------------------------------------------ particles

PARTICLES_MAGIC = b"PNP1"
ITEMS_MAGIC = b"PNI1"


def block_particle_tiles(names, states, assets, models, texture):
    """Per block (registry order): the atlas tile of its model's particle texture (what
    vanilla's TerrainParticle draws), from the first state's block-state model; 0xFFFF none.
    texture() may add a tile to the atlas, so this runs before the atlas is laid out."""
    out = []
    for name in names:
        short = name.split(":", 1)[1]
        props = (states.get(name, {"states": [{}]})["states"] or [{}])[0]
        tile = 0xFFFF
        for ap in applied_models(assets.json("blockstates/" + short + ".json"), props):
            tex, _ = models.load(ap["model"])
            path = resolve_tex(tex, "#particle")
            if path:
                tile = texture(path)[0]
                break
        out.append(tile)
    return out


def make_particles(assets, res, out_dir, block_tiles, atlas_w, atlas_h):
    """particles.bin: vanilla's particle sprites (textures/particle) packed in one sheet, each
    particle type's sprite set (particles/*.json) in registry order, and each block's particle
    tile in atlas.bin (written in the same run, so they agree).

    "PNP1", u16 sheet width, u16 sheet height, RGBA; u16 sprites, per sprite u16 x y w h;
    u16 types, per type u8 name length, name, u8 sprite count, u16 sprites;
    u16 atlas width, u16 atlas height, u32 blocks, per block u16 tile (0xFFFF none)."""
    reg = json.load(open(os.path.join(res, "registries.json")))["minecraft:particle_type"]["entries"]
    types = [k for k, _ in sorted(reg.items(), key=lambda kv: kv[1]["protocol_id"])]
    sprite_index, sprites, sets = {}, [], []
    for t in types:
        d = assets.json("particles/" + t.split(":", 1)[1] + ".json") or {}
        ids = []
        for ref in d.get("textures", []):
            ref = nsref(ref)
            if ref not in sprite_index:
                img = assets.image(("textures/particle/" + ref + ".png") or b"")
                if img is None:
                    continue
                sprite_index[ref] = len(sprites)
                sprites.append(img)
            ids.append(sprite_index[ref])
        sets.append((t.split(":", 1)[1], ids[:255]))
    # Shelves, tallest first, 256 wide; the height a power of two.
    W = 256
    order = sorted(range(len(sprites)), key=lambda i: (-sprites[i][1], -sprites[i][0]))
    place = [None] * len(sprites)
    x = y = shelf = 0
    for i in order:
        w, h, _ = sprites[i]
        if x + w > W:
            x, y, shelf = 0, y + shelf, 0
        place[i] = (x, y)
        x += w
        shelf = max(shelf, h)
    H = 1
    while H < y + shelf:
        H *= 2
    sheet = bytearray(W * H * 4)
    for i, (w, h, px) in enumerate(sprites):
        sx, sy = place[i]
        for r in range(h):
            o = ((sy + r) * W + sx) * 4
            sheet[o:o + w * 4] = px[r * w * 4:(r + 1) * w * 4]
    with open(os.path.join(out_dir, "particles.bin"), "wb") as f:
        f.write(PARTICLES_MAGIC + struct.pack("<HH", W, H) + bytes(sheet))
        f.write(struct.pack("<H", len(sprites)))
        for i, (w, h, _) in enumerate(sprites):
            f.write(struct.pack("<4H", place[i][0], place[i][1], w, h))
        f.write(struct.pack("<H", len(sets)))
        for n, ids in sets:
            nb = n.encode()
            f.write(bytes([len(nb)]) + nb + bytes([len(ids)]) + struct.pack("<%dH" % len(ids), *ids))
        f.write(struct.pack("<HHI", atlas_w, atlas_h, len(block_tiles)))
        f.write(struct.pack("<%dH" % len(block_tiles), *block_tiles))
    print("particles.bin: %d sprites in %dx%d, %d types" % (len(sprites), W, H, len(sets)))
# ------------------------------------------------------------------ the creative inventory

def snbt_parse(s):
    """SNBT as creative-inventory.json writes stacks: compounds, lists, strings, numbers."""
    i = 0

    def ws():
        nonlocal i
        while i < len(s) and s[i] in " \t\n":
            i += 1

    def value():
        nonlocal i
        ws()
        c = s[i]
        if c == "{":
            i += 1
            out = {}
            ws()
            if s[i] == "}":
                i += 1
                return out
            while True:
                ws()
                k = word()
                ws()
                assert s[i] == ":", s[i:i + 20]
                i += 1
                out[k] = value()
                ws()
                if s[i] == ",":
                    i += 1
                    continue
                assert s[i] == "}"
                i += 1
                return out
        if c == "[":
            i += 1
            out = []
            ws()
            if s[i] == "]":
                i += 1
                return out
            while True:
                out.append(value())
                ws()
                if s[i] == ",":
                    i += 1
                    continue
                assert s[i] == "]"
                i += 1
                return out
        return word()

    def word():
        nonlocal i
        if s[i] in "\"'":
            q = s[i]
            i += 1
            out = []
            while s[i] != q:
                if s[i] == "\\":
                    i += 1
                out.append(s[i])
                i += 1
            i += 1
            return "".join(out)
        j = i
        while i < len(s) and s[i] not in ",:}] \t\n":
            i += 1
        t = s[j:i]
        try:
            if t[-1:] in "bBsSlL" and t[:-1].lstrip("-").isdigit():
                return int(t[:-1])
            if t.lstrip("-").isdigit():
                return int(t)
            if t[-1:] in "fFdD":
                return float(t[:-1])
            if t == "true":
                return 1
            if t == "false":
                return 0
        except ValueError:
            pass
        return t

    return value()


def varint(v):
    v &= 0xFFFFFFFF
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def creative_section(res, reg, items):
    """The creative inventory (CreativeModeTabs as horizon-prepare exported them) for gui.bin:

        "CRT1"
        u16 items; per item (registry order): u8 max stack size, u8 rarity (0 common 1 uncommon
            2 rare 3 epic), u8 equipment slot (0 none 1 head 2 chest 3 legs 4 feet 5 offhand)
        u16 tags; per tag: u8 length, name ("minecraft:logs")
        u32 entries; per entry (a distinct stack): u16 item, u8 flags (1 operator only),
            u16 tabs it is in (bit per tab), u8 lines (its tooltip, the name first); per line
            u16 length, text; u8 tags, u16 each; u16 length, component spec
        u8 tabs; per tab: u8 type (0 category 1 hotbar 2 search 3 inventory), u8 row (0 top
            1 bottom), u8 column, u8 flags (1 aligned right, 2 no title, 4 no scroll bar, 8
            operators only), u8 length + title key, u8 length + English title, u16 icon item,
            u8 length + background sprite, u32 entries, u32 index each
    The component spec is the stack's data components for ServerboundSetCreativeModeSlot:
    per component u16 type, then ops up to a 0: 1 u16 length + bytes (literal), 2 u8 registry
    (0 enchantment 1 painting_variant 2 instrument 3 banner_pattern, numbered by the server)
    u8 length + name, i8 added (a holder id of the synchronised registry, + 1 for a holder)."""
    path = os.path.join(res, "creative-inventory.json")
    if not os.path.isfile(path):
        return None
    tabs = json.load(open(path))["tabs"]
    ids = {n: i for i, n in enumerate(items)}

    def rid(r, name):
        return reg[r]["entries"][name]["protocol_id"]

    comp_id = {k: v["protocol_id"] for k, v in reg["minecraft:data_component_type"]["entries"].items()}
    idata = {}
    ip = os.path.join(res, "items.json")
    if os.path.isfile(ip):
        idata = json.load(open(ip))
    rar = {"common": 0, "uncommon": 1, "rare": 2, "epic": 3}
    slots = {"head": 1, "chest": 2, "legs": 3, "feet": 4, "offhand": 5}
    out = bytearray(b"CRT1") + struct.pack("<H", len(items))
    for n in items:
        d = idata.get(n, {})
        eq = d.get("minecraft:equippable") or {}
        out += struct.pack("<BBB", min(255, d.get("minecraft:max_stack_size", 64)), rar.get(d.get("minecraft:rarity"), 0),
                           slots.get(eq.get("slot"), 0) if isinstance(eq, dict) else 0)

    def string(t):
        b = t.encode()
        return varint(len(b)) + b

    def nbt_text(v):
        """A text component as network NBT (an unnamed root tag)."""
        if isinstance(v, str):
            b = v.encode()
            return b"\x08" + struct.pack(">H", len(b)) + b
        body = bytearray(b"\x0a")
        for k, x in v.items():
            if not isinstance(x, str):
                raise ValueError("text")
            kb, xb = k.encode(), x.encode()
            body += b"\x08" + struct.pack(">H", len(kb)) + kb + struct.pack(">H", len(xb)) + xb
        return bytes(body + b"\x00")

    def lit(b):
        return b"\x01" + struct.pack("<H", len(b)) + b

    def dyn(r, name, add):
        nb = name.encode()
        return b"\x02" + bytes([r, len(nb)]) + nb + struct.pack("<b", add)

    dyes = ["white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan",
            "purple", "blue", "brown", "green", "red", "black"]

    def component(name, v):
        """The ops of one component's network value (its STREAM_CODEC), or ValueError."""
        if name == "minecraft:potion_contents":
            if set(v) != {"potion"}:
                raise ValueError(name)
            return lit(b"\x01" + varint(rid("minecraft:potion", v["potion"])) + b"\x00\x00\x00")
        if name in ("minecraft:stored_enchantments", "minecraft:enchantments"):
            ops = lit(varint(len(v)))
            for e, lvl in v.items():
                ops += dyn(0, e, 0) + lit(varint(lvl))
            return ops
        if name == "minecraft:painting/variant":
            return dyn(1, v, 1)
        if name == "minecraft:instrument":
            return dyn(2, v, 1)
        if name == "minecraft:suspicious_stew_effects":
            b = varint(len(v))
            for e in v:
                b += varint(rid("minecraft:mob_effect", e["id"])) + varint(e.get("duration", 160))
            return lit(b)
        if name == "minecraft:ominous_bottle_amplifier":
            return lit(varint(v))
        if name == "minecraft:fireworks":
            if v.get("explosions"):
                raise ValueError(name)
            return lit(varint(v.get("flight_duration", 1)) + b"\x00")
        if name == "minecraft:block_state":
            b = varint(len(v))
            for k, x in v.items():
                b += string(k) + string(str(x))
            return lit(b)
        if name == "minecraft:banner_patterns":
            ops = lit(varint(len(v)))
            for layer in v:
                ops += dyn(3, layer["pattern"], 1) + lit(varint(dyes.index(layer["color"])))
            return ops
        if name == "minecraft:item_name":
            return lit(nbt_text(v))
        if name == "minecraft:rarity":
            return lit(varint(rar[v]))
        if name == "minecraft:tooltip_display":
            hidden = v.get("hidden_components", [])
            return lit(bytes([1 if v.get("hide_tooltip") else 0]) + varint(len(hidden)) +
                       b"".join(varint(comp_id[h]) for h in hidden))
        raise ValueError(name)

    search = next((t for t in tabs if t["type"] == "SEARCH"), None)
    lines = {}
    tags_of = {}
    op_only = set()
    if search:
        for k, it in enumerate(search.get("items", [])):
            lines[it] = search.get("lines", [])[k] if k < len(search.get("lines", [])) else []
            tags_of[it] = search.get("tags", [])[k] if k < len(search.get("tags", [])) else []
        for k in search.get("permissions_only", []):
            if k < len(search["items"]):
                op_only.add(search["items"][k])
    for t in tabs:
        if t.get("permissions_only") is True or (t["type"] == "CATEGORY" and not t.get("visible", True)):
            op_only.update(t.get("items", []))
    entries = {}
    order = []
    mask = {}
    for ti, t in enumerate(tabs):
        for it in t.get("items", []):
            if it not in entries:
                entries[it] = len(order)
                order.append(it)
            if t["type"] != "SEARCH":
                mask[it] = mask.get(it, 0) | (1 << ti)
    tag_names = sorted({tg for v in tags_of.values() for tg in v})
    tag_idx = {n: i for i, n in enumerate(tag_names)}
    out += struct.pack("<H", len(tag_names))
    for n in tag_names:
        nb = n.encode()
        out += bytes([len(nb)]) + nb
    out += struct.pack("<I", len(order))
    plain = 0
    for it in order:
        st = snbt_parse(it)
        item = ids.get(st.get("id"), 0)
        spec = bytearray()
        try:
            for cn, cv in (st.get("components") or {}).items():
                spec += struct.pack("<H", comp_id[cn]) + component(cn, cv) + b"\x00"
        except (ValueError, KeyError):
            spec = bytearray()
            plain += 1
        ls = lines.get(it, [])[:255]
        out += struct.pack("<HBHB", item, 1 if it in op_only else 0, mask.get(it, 0) & 0xFFFF, len(ls))
        for l in ls:
            lb = l.encode()[:65535]
            out += struct.pack("<H", len(lb)) + lb
        tg = tags_of.get(it, [])[:255]
        out += bytes([len(tg)]) + b"".join(struct.pack("<H", tag_idx[x]) for x in tg)
        out += struct.pack("<H", len(spec)) + spec
    keys = {"minecraft:building_blocks": "itemGroup.buildingBlocks", "minecraft:colored_blocks": "itemGroup.coloredBlocks",
            "minecraft:natural_blocks": "itemGroup.natural", "minecraft:functional_blocks": "itemGroup.functional",
            "minecraft:redstone_blocks": "itemGroup.redstone", "minecraft:hotbar": "itemGroup.hotbar",
            "minecraft:search": "itemGroup.search", "minecraft:tools_and_utilities": "itemGroup.tools",
            "minecraft:combat": "itemGroup.combat", "minecraft:food_and_drinks": "itemGroup.foodAndDrink",
            "minecraft:ingredients": "itemGroup.ingredients", "minecraft:spawn_eggs": "itemGroup.spawnEggs",
            "minecraft:op_blocks": "itemGroup.op", "minecraft:inventory": "itemGroup.inventory"}
    kinds = {"CATEGORY": 0, "HOTBAR": 1, "SEARCH": 2, "INVENTORY": 3}
    out += bytes([len(tabs)])
    for t in tabs:
        kind = kinds.get(t["type"], 0)
        col = t["column"]
        # CreativeModeTabs: the hotbar, search, operator and inventory tabs sit on the right.
        right = kind != 0 or t["id"] == "minecraft:op_blocks"
        flags = (1 if right else 0) | (2 if kind == 3 else 0) | (4 if kind == 3 else 0)
        flags |= 8 if t["id"] == "minecraft:op_blocks" else 0
        bg = {2: "container/creative_inventory/tab_item_search", 3: "container/creative_inventory/tab_inventory"}.get(
            kind, "container/creative_inventory/tab_items")
        icon = ids.get(snbt_parse(t["icon"]).get("id"), 0)
        kb, tb, bb = keys.get(t["id"], t["id"]).encode(), t.get("title", "").encode(), bg.encode()
        out += bytes([kind, 0 if t["row"] == "TOP" else 1, col, flags, len(kb)]) + kb + bytes([len(tb)]) + tb
        out += struct.pack("<H", icon) + bytes([len(bb)]) + bb
        its = t.get("items", [])
        out += struct.pack("<I", len(its)) + b"".join(struct.pack("<I", entries[x]) for x in its)
    print("creative: %d tabs, %d stacks (%d sent without their components), %d tags" % (len(tabs), len(order), plain, len(tag_names)))
    return bytes(out)


# ------------------------------------------------------------------ main

def norm_type(t):
    """A 26.x item model's type: packs leave out "minecraft:" ("model", "select")."""
    return t if isinstance(t, str) and ":" in t else "minecraft:" + str(t)


def uv_locked(verts, n):
    """uvlock (a blockstate part's "uvlock": true): the face's texture stays as the unrotated
    model has it on the world, not turned with the model. Its up direction is world up on the
    sides and north on the top and bottom: the quarter turn of the texture that brings the
    rotated face's up there is found by testing the four turns against the face's geometry.
    verts: [(point, u, v)] with u, v in texture pixels; n: the rotated face normal."""
    horizontal = abs(n[1]) < 0.5
    want = (0.0, 1.0, 0.0) if horizontal else (0.0, 0.0, -1.0)
    pts = [v[0] for v in verts]
    pm = [sum(p[a] for p in pts) / len(pts) for a in range(3)]
    best = None
    for k in range(4):
        uv = [(v[1], v[2]) for v in verts]
        for _ in range(k):
            uv = [(16 - vv, uu) for uu, vv in uv]
        vm = sum(t for _, t in uv) / len(uv)
        den = sum((t - vm) ** 2 for _, t in uv)
        if den < 1e-9:
            return verts
        g = [0.0, 0.0, 0.0]
        for p, (_, t) in zip(pts, uv):
            for a in range(3):
                g[a] += (t - vm) * (p[a] - pm[a])
        g = [c / den for c in g]
        score = -sum(g[a] * want[a] for a in range(3))  # texture up is the way v decreases
        if best is None or score > best[0] + 1e-9:
            best = (score, uv)
    return [(verts[i][0], best[1][i][0], best[1][i][1]) for i in range(len(verts))]


def main():
    args = sys.argv[1:]
    jar = None
    if "--jar" in args:
        i = args.index("--jar")
        jar = args[i + 1]
        del args[i:i + 2]
    # --packs A,B,...: resource packs (zips or folders) over the game, bottom to top. Only the
    # files they change are worth serving; sounds.bin is then a small overlay (packsounds.bin).
    packs = []
    if "--packs" in args:
        i = args.index("--packs")
        packs = [p for p in args[i + 1].split(",") if p]
        del args[i:i + 2]
    res, out_dir = args[0], args[1]
    os.makedirs(out_dir, exist_ok=True)
    assets = Assets(jar, res, packs)
    models = Models(assets)

    beh = json.load(open(os.path.join(res, "block-behaviours.json")))["blocks"]
    states = json.load(open(os.path.join(res, "model-states.json")))
    reg = json.load(open(os.path.join(res, "registries.json")))
    names = [k for k, _ in sorted(reg["minecraft:block"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])]
    items = [k for k, _ in sorted(reg["minecraft:item"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])]
    by_name = {b["name"]: b for b in beh}
    assert names == [b["name"] for b in beh], "registry order differs from block-behaviours.json"
    facts = json.load(open(os.path.join(res, "gameplay-facts.json")))["states"]
    # Vanilla's collision shape of every state (getCollisionShape at the origin), as
    # horizon-prepare caches it.
    coll_nbt = read_nbt(open(os.path.join(res, "cached-collision.nbt"), "rb").read())
    coll_of_state = coll_nbt["cached_shapes"]
    coll_shapes = coll_nbt["shapes"]
    assert len(coll_of_state) == len(facts), "cached-collision.nbt is for another version"
    coll_boxes = []       # (x0, y0, z0, x1, y1, z1) in 1/32 block, signed bytes
    coll_index = {}       # shape id -> (first, count)
    coll_inexact = set()
    raw_tags = json.load(open(os.path.join(res, "block-tags.json")))
    sound_names = [k for k, _ in sorted(reg["minecraft:sound_event"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])]
    sound_ids = {n: i for i, n in enumerate(sound_names)}
    emodels = json.load(open(os.path.join(res, "entity-models.json")))

    def tag(name, seen=()):
        v = raw_tags.get(name)
        if v is None or name in seen:
            return set()
        if isinstance(v, str):
            v = json.loads(v)
        out = set()
        for e in v.get("values", []):
            e = e["id"] if isinstance(e, dict) else e
            out |= tag(e[1:], seen + (name,)) if e.startswith("#") else {e}
        return out

    mine_tags = [tag("minecraft:mineable/" + t) for t in ("pickaxe", "axe", "shovel", "hoe")]
    sword = tag("minecraft:sword_efficient") | {"minecraft:cobweb"}
    bad_tags = [tag("minecraft:incorrect_for_%s_tool" % t)
                for t in ("wooden", "stone", "copper", "iron", "gold", "diamond", "netherite")]

    # ---- textures into the atlas, as they are first used
    tiles = []          # RGBA 16x16
    tile_index = {}     # texture path -> (index, alpha class: 0 opaque, 1 cutout, 2 translucent)
    missing = set()

    def texture(path):
        if path in tile_index:
            return tile_index[path]
        img = assets.image(("textures/" + path + ".png"))
        if img is None:
            missing.add(path)
            path = "__missing__"
            if path in tile_index:
                return tile_index[path]
            t = bytearray()
            for y in range(TILE):
                for x in range(TILE):
                    t += bytes([248, 0, 248, 255] if (x // 8 + y // 8) % 2 else [0, 0, 0, 255])
            t = bytes(t)
        else:
            t = tile_of(img)
        alphas = t[3::4]
        cls = 0
        if any(0 < a < 255 for a in alphas):
            cls = 2
        elif any(a == 0 for a in alphas):
            cls = 1
        tile_index[path] = (len(tiles) + RESERVED, cls)
        tiles.append(t)
        return tile_index[path]

    # The first rows of the atlas hold 64x64 block-entity textures (chests), 16 to a row.
    BIG_ROWS = 8
    RESERVED = BIG_ROWS * 64
    big = {}  # path -> (x, y, w, h, rgba)

    def big_texture(path):
        if path in big:
            return big[path]
        img = assets.image(("textures/" + path + ".png"))
        if img is None:
            return None
        w, h, px = img
        n = len(big)
        x, y = (n % 16) * 64, (n // 16) * 64
        if y + 64 > BIG_ROWS * TILE or w > 64 or h > 64:
            return None
        big[path] = (x, y, w, h, px)
        return big[path]

    CHEST_TEX = {"chest": "normal", "trapped_chest": "trapped", "ender_chest": "ender"}
    for stage in ("", "exposed_", "weathered_", "oxidized_"):
        for wax in ("", "waxed_"):
            CHEST_TEX[wax + stage + "copper_chest"] = "copper" + ("_" + stage[:-1] if stage else "")

    def chest_quads(short, facing, kind):
        """ChestRenderer's model as block quads: the chest (or one half of a double chest),
        turned by -facing.toYRot() about the block's centre."""
        base = "entity/chest/" + CHEST_TEX[short]
        key = "minecraft:chest#main"
        if kind in ("left", "right") and short != "ender_chest":
            key = "minecraft:double_chest_%s#main" % kind
            base += "_" + kind
        tex = big_texture(base)
        model = emodels.get(key)
        if tex is None or model is None:
            return []
        bx, by, tw, th, _ = tex
        rot = {"south": 0, "west": 90, "north": 180, "east": 270}.get(facing, 0)
        a = math.radians(-rot)
        ca, sa = math.cos(a), math.sin(a)
        out = []

        def walk(part, off):
            pose = part.get("pose", [0] * 9)
            o = (off[0] + pose[0], off[1] + pose[1], off[2] + pose[2])
            for poly, nrm in zip(part.get("polygons", []), part.get("normals", []) or [[0, 1, 0]] * 99):
                q = bytearray()
                for vtx in poly[:4]:
                    x, y, z = vtx[0] + o[0] - 8, vtx[1] + o[1], vtx[2] + o[2] - 8
                    x, z = x * ca + z * sa, -x * sa + z * ca
                    au = (bx + vtx[3] * tw) / (64 * TILE)
                    av16 = int(round((by + vtx[4] * th) * 16))
                    q += struct.pack("<hhhHH", int(round((x + 8) * 64)), int(round(y * 64)), int(round((z + 8) * 64)),
                                     int(round(au * 65535)), min(65535, av16))
                nx, nz = nrm[0] * ca + nrm[2] * sa, -nrm[0] * sa + nrm[2] * ca
                ny = nrm[1]
                d = "up" if ny > 0.5 else "down" if ny < -0.5 else ("east" if nx > 0.5 else "west" if nx < -0.5 else ("south" if nz > 0 else "north"))
                q += struct.pack("<BBBBi", 6, 2, int(round(SHADE[d] * 255)), 0, 0)
                out.append(bytes(q))
            for child in part.get("children", {}).values():
                walk(child, o)

        walk(model, (0, 0, 0))
        return out

    # Water and lava are drawn by the program itself; their textures go in first.
    texture("block/water_still")
    texture("block/lava_still")
    for i in range(10):
        texture("block/destroy_stage_%d" % i)

    quads = []        # bytes
    quad_lists = {}   # tuple of quad bytes -> first index

    blockstate_cache = {}
    baked_cache = {}

    def bake_state(short, props, kind):
        bs = blockstate_cache.get(short)
        if bs is None:
            bs = blockstate_cache[short] = assets.json("blockstates/" + short + ".json")
        out = []
        for ap in applied_models(bs, props):
            # The same model at the same rotation bakes to the same quads (many states share).
            key = json.dumps(ap, sort_keys=True)
            q = baked_cache.get(key)
            if q is None:
                q = baked_cache[key] = bake_model(ap)
            out.extend(q)
        return out

    def bake_model(ap):
        out = []
        if True:
            xr, yr = int(ap.get("x", 0)), int(ap.get("y", 0))
            tex, elements = models.load(ap["model"])
            for el in elements or []:
                f, t = el["from"], el["to"]
                rot = el.get("rotation")
                shade_el = el.get("shade", True)
                for d, face in el.get("faces", {}).items():
                    path = resolve_tex(tex, face.get("texture"))
                    if not path:
                        continue
                    ti, alpha = texture(path)
                    uv = face.get("uv") or default_uv(d, f, t)
                    corners = face_corners(d, f, t)
                    frot = (int(face.get("rotation", 0)) // 90) % 4
                    verts = []
                    for p, (s_, t_) in corners:
                        for _ in range(frot):
                            s_, t_ = t_, 1 - s_
                        u = uv[0] + (uv[2] - uv[0]) * s_
                        v = uv[1] + (uv[3] - uv[1]) * t_
                        if rot:
                            p = rot_point(p, rot.get("axis", "y"), float(rot.get("angle", 0)),
                                          rot.get("origin", [8, 8, 8]), rot.get("rescale", False))
                        p = variant_rotate(p, xr, yr)
                        verts.append((p, u, v))
                    cull = face.get("cullface")
                    cull = DIRS.index(dir_rotate(cull, xr, yr)) if cull in NORMAL else 6
                    nd = dir_rotate(d, xr, yr)
                    # A face lying flat on the block boundary in its own direction.
                    n = NORMAL[nd]
                    if ap.get("uvlock"):
                        verts = uv_locked(verts, n)
                    axis = 0 if n[0] else (1 if n[1] else 2)
                    plane = 16 if sum(n) > 0 else 0
                    on_edge = all(abs(vv[0][axis] - plane) < 1e-4 for vv in verts) and not rot
                    flags = (1 if face.get("tintindex", -1) >= 0 else 0) | (2 if alpha == 1 else 0) \
                        | (4 if alpha == 2 else 0) | (0 if shade_el else 8) | (16 if on_edge else 0)
                    light = int(round(SHADE[nd] * 255)) if shade_el else 255
                    col, row = ti % 64, ti // 64
                    q = bytearray()
                    for p, u, v in verts:
                        au = (col * TILE + max(0.0, min(16.0, u))) / (64 * TILE)
                        # v in atlas pixels x16 for now: made a fraction once the height is known
                        av16 = int(round((row * TILE + max(0.0, min(16.0, v))) * 16))
                        q += struct.pack("<hhhHH", int(round(p[0] * 64)), int(round(p[1] * 64)),
                                         int(round(p[2] * 64)), int(round(au * 65535)), min(65535, av16))
                    q += struct.pack("<BBBBi", cull, flags, light, 0, 0)
                    out.append(bytes(q))
        return out

    blocks_out = []
    state_rows = []
    boxes = []
    box_index = {}
    sid = 0
    for name in names:
        short = name.split(":", 1)[1]
        b = by_name[name]
        sl = states.get(name, {"states": [{}]})["states"]
        first = facts[sid]
        kind = kind_of(short, b.get("class", []), first)
        mine = sum(1 << i for i, t in enumerate(mine_tags) if name in t) | (16 if name in sword else 0)
        bad = sum(1 << i for i, t in enumerate(bad_tags) if name in t)
        mc = first.get("map_color", 0)
        snd = first.get("sound") or {}
        sids = [sound_ids.get((snd.get(k) or {}).get("id", ""), 0xFFFF) for k in ("break", "step", "place", "hit", "fall")]
        sv = (int(round(float(snd.get("volume", 1.0)) * 100)), int(round(float(snd.get("pitch", 1.0)) * 100)))
        # Movement: Block.friction, speedFactor, jumpFactor, and the random offset of plants,
        # bamboo and dripstone (BlockBehaviour.OffsetType, maxHorizontalOffset).
        off = (1 if first.get("offset") else 0) | (2 if first.get("outline_offset") else 0) \
            | (4 if short in ("short_grass", "fern", "short_dry_grass", "tall_dry_grass", "small_dripleaf") else 0)
        max_h = 0.125 if short in ("pointed_dripstone", "sulfur_spike") else 0.25
        max_v = 0.1 if short == "small_dripleaf" else 0.2
        move = (float(first.get("friction", 0.6)), float(first.get("speed_factor", 1.0)),
                float(first.get("jump_factor", 1.0)), off, max_h, max_v)
        blocks_out.append((kind, tint_of(short), mine, bad, ((mc >> 16) & 255, (mc >> 8) & 255, mc & 255), short, sids, sv, move))
        bidx = len(blocks_out) - 1
        for props in sl:
            fct = facts[sid]
            assert fct["id"] == sid
            sid += 1
            h = fct.get("hardness", 0.0)
            hard = 65535 if h < 0 else min(65534, int(round(h * 100)))
            bx = []
            for o in fct.get("outline", []):
                q = tuple(max(0, min(16, int(round(v * 16)))) for v in o)
                if q[0] < q[3] and q[1] < q[4] and q[2] < q[5]:
                    bx.append(q)
            if kind in (0, 4, 5):
                bx = [] if kind == 0 else [(0, 0, 0, 16, 14 if kind == 4 else 16, 16)]
            full = kind == 1 and bx == [(0, 0, 0, 16, 16, 16)] and fct.get("can_occlude") and fct.get("solid_render")
            flags = (1 if fct.get("requires_tool") else 0) | (2 if fct.get("full_collision") else 0) \
                | (4 if fct.get("replaceable") else 0) | (8 if full else 0) \
                | (16 if short == "bubble_column" and props.get("drag") == "true" else 0) \
                | (32 if props.get("waterlogged") == "true" or short in ("seagrass", "tall_seagrass", "kelp", "kelp_plant",
                                                                        "bubble_column") else 0) \
                | (64 if props.get("open") == "true" else 0)
            facing = {"north": 0, "south": 1, "west": 2, "east": 3, "down": 4, "up": 5}.get(props.get("facing"), 255)
            # The collision shape: vanilla's, or a rule of its own for dynamic shapes.
            special = DYNAMIC_COLLISION.get(short, 0)
            if special == 0 and short.endswith("shulker_box"):
                special = 5
            sh_id = coll_of_state[sid - 1]
            if sh_id not in coll_index:
                first_c = len(coll_boxes)
                for c in shape_boxes(coll_shapes[sh_id]):
                    q = tuple(int(round(v * 32)) for v in c)
                    if any(abs(v * 32 - round(v * 32)) > 1e-6 for v in c):
                        coll_inexact.add(short)
                    q = tuple(max(-128, min(127, v)) for v in q)
                    if q[0] < q[3] and q[1] < q[4] and q[2] < q[5]:
                        coll_boxes.append(q)
                coll_index[sh_id] = (first_c, len(coll_boxes) - first_c)
            fc, nc = coll_index[sh_id]
            if special:
                fc, nc = 0, 0
            key = tuple(bx)
            if key not in box_index:
                box_index[key] = len(boxes)
                boxes.extend(bx)
            if short in CHEST_TEX:
                ql = chest_quads(short, props.get("facing", "south"), props.get("type", "single"))
            else:
                ql = [] if kind in (0, 4, 5, 6) else bake_state(short, props, kind)
            qk = tuple(ql)
            if qk not in quad_lists:
                quad_lists[qk] = len(quads)
                quads.extend(ql)
            state_rows.append((bidx, hard, flags, len(bx), box_index[key], quad_lists[qk], len(ql), fc, nc, special, facing))
    assert sid == len(facts)
    if coll_inexact:
        print("collision shapes rounded to 1/32 for:", ", ".join(sorted(coll_inexact)))


    # ---- items: a flat icon (layer textures into the block atlas) or a 3D model
    def find_model(m, depth=0):
        """The first plain model in a 26.x item definition (conditions, selects, ranges and
        special renderers fall back to a model they name)."""
        if not isinstance(m, dict) or depth > 12:
            return None, []
        t = norm_type(m.get("type", "minecraft:model"))
        if t == "minecraft:model":
            return m.get("model"), m.get("tints", [])
        if t == "minecraft:special":
            return m.get("base"), []
        if t == "minecraft:composite":
            parts = m.get("models", [])
            if parts:
                return find_model(parts[0], depth + 1)
        for key in ("fallback", "on_false", "on_true", "model"):
            if isinstance(m.get(key), dict):
                r = find_model(m[key], depth + 1)
                if r[0]:
                    return r
        for key in ("cases", "entries", "models"):
            for c in m.get(key, []) or []:
                r = find_model(c.get("model", c) if isinstance(c, dict) else c, depth + 1)
                if r[0]:
                    return r
        return None, []

    def parents(ref):
        out = []
        ref = nsref(ref)
        for _ in range(16):
            out.append(ref)
            m = assets.json("models/" + ref + ".json") or {}
            if "parent" not in m:
                break
            ref = nsref(m["parent"])
        return out

    def display(ref, ctx, default):
        for r in parents(ref):
            m = assets.json("models/" + r + ".json") or {}
            g = m.get("display", {}).get(ctx)
            if g:
                return (g.get("rotation", [0, 0, 0]), g.get("translation", [0, 0, 0]), g.get("scale", [1, 1, 1]))
        return default

    def gui_display(ref):
        return display(ref, "gui", ([30, 225, 0], [0, 0, 0], [0.625, 0.625, 0.625]))

    def hand_display(ref):
        return display(ref, "firstperson_righthand", ([0, 45, 0], [0, 0, 0], [0.4, 0.4, 0.4]))

    def tint_value(t):
        if not isinstance(t, dict):
            return 0
        if t.get("type") == "minecraft:grass":
            return 0xFF91BD59
        if t.get("type") == "minecraft:foliage":
            return 0xFF77AB2F
        v = t.get("value", t.get("default"))
        if isinstance(v, int):
            return v & 0xFFFFFFFF | 0xFF000000
        if isinstance(v, list) and len(v) >= 3:
            return 0xFF000000 | int(v[0] * 255) << 16 | int(v[1] * 255) << 8 | int(v[2] * 255)
        return 0

    item_icons = []
    item_hands = []
    for name in items:
        short = name.split(":", 1)[1]
        d = assets.json("items/" + short + ".json")
        ref, tints = find_model(d.get("model") if d else None)
        icon = (0,)
        if ref:
            chain = parents(ref)
            tex, elements = models.load(ref)
            if "builtin/generated" in chain:
                layers = []
                for i in range(5):
                    path = resolve_tex(tex, tex.get("layer%d" % i))
                    if not path:
                        break
                    ti, _ = texture(path)
                    layers.append((ti, tint_value(tints[i]) if i < len(tints) else 0))
                if layers:
                    icon = (1, layers)
            elif elements:
                ql = bake_model({"model": ref})
                # A composite (a bed: head and foot) adds its other plain parts, moved.
                if d and d.get("model", {}).get("type") == "minecraft:composite":
                    for part in d["model"].get("models", [])[1:]:
                        if part.get("type", "minecraft:model") != "minecraft:model":
                            continue
                        tr = (part.get("transformation") or {}).get("translation", [0, 0, 0])
                        for q in bake_model({"model": part["model"]}):
                            q = bytearray(q)
                            for k in range(4):
                                o = k * 10
                                for a in range(3):
                                    v = struct.unpack("<h", q[o + 2 * a:o + 2 * a + 2])[0] + int(round(tr[a] * 1024))
                                    q[o + 2 * a:o + 2 * a + 2] = struct.pack("<h", max(-32768, min(32767, v)))
                            ql.append(bytes(q))
                qk = tuple(ql)
                if qk not in quad_lists:
                    quad_lists[qk] = len(quads)
                    quads.extend(ql)
                tint = tint_value(tints[0]) if tints else 0
                icon = (2, quad_lists[qk], len(ql), gui_display(ref), tint)
            else:
                path = resolve_tex(tex, tex.get("particle"))
                if path:
                    ti, _ = texture(path)
                    icon = (1, [(ti, 0)])
        if short in CHEST_TEX:
            ql = chest_quads(short, "south", "single")
            if ql:
                qk = tuple(ql)
                if qk not in quad_lists:
                    quad_lists[qk] = len(quads)
                    quads.extend(ql)
                icon = (2, quad_lists[qk], len(ql), ([30, 45, 0], [0, 0, 0], [0.625, 0.625, 0.625]), 0)
        item_icons.append(icon)
        item_hands.append(hand_display(ref) if ref else ([0, 45, 0], [0, 0, 0], [0.4, 0.4, 0.4]))

    # ---- the item definitions of the packs (items.bin, "PNI1"). An item_model component names
    # one: items/<path>.json of its namespace (ns:path). A definition that selects on
    # custom_model_data (minecraft:select, property minecraft:custom_model_data, the stack's
    # first string) gives one icon per case too, keyed "ns:path|string". The icons are gui.bin's
    # (flat layers, or a block model's quads, appended to blocks.bin's quads).
    overlay_icons = []
    if packs:
        import re
        found = set()
        for n in assets.names():
            m = re.match(r"^assets/([a-z0-9_.-]+)/items/(.+)\.json$", n)
            if m:
                found.add((m.group(1), m.group(2)))

        def icon_of_model(model):
            ref, tints = find_model(model)
            if not ref:
                return (0,)
            chain = parents(ref)
            tex, elements = models.load(ref)
            if "builtin/generated" in chain:
                layers = []
                for i in range(5):
                    path = resolve_tex(tex, tex.get("layer%d" % i))
                    if not path:
                        break
                    ti, _ = texture(path)
                    layers.append((ti, tint_value(tints[i]) if i < len(tints) else 0))
                return (1, layers) if layers else (0,)
            if elements:
                ql = bake_model({"model": ref})
                if not ql:
                    return (0,)
                qk = tuple(ql)
                if qk not in quad_lists:
                    quad_lists[qk] = len(quads)
                    quads.extend(ql)
                tint = tint_value(tints[0]) if tints else 0
                return (2, quad_lists[qk], len(ql), gui_display(ref), tint)
            path = resolve_tex(tex, tex.get("particle"))
            if path:
                ti, _ = texture(path)
                return (1, [(ti, 0)])
            return (0,)

        for ns, path in sorted(found):
            key = "%s:%s" % (ns, path)
            d = assets.json("%s:items/%s.json" % (ns, path))
            if not isinstance(d, dict):
                continue
            model = d.get("model")
            if ns != "minecraft" and isinstance(model, dict) and norm_type(model.get("type")) != "minecraft:select":
                icon = icon_of_model(model)
                if icon[0]:
                    overlay_icons.append((key, icon))
            if isinstance(model, dict) and norm_type(model.get("type")) == "minecraft:select" \
                    and model.get("property") == "minecraft:custom_model_data":
                for case in model.get("cases", []) or []:
                    when = case.get("when")
                    for w in (when if isinstance(when, list) else [when]):
                        if isinstance(w, str) and isinstance(case.get("model"), dict):
                            icon = icon_of_model(case["model"])
                            if icon[0]:
                                overlay_icons.append(("%s|%s" % (key, w), icon))
                if ns != "minecraft" and isinstance(model.get("fallback"), dict):
                    icon = icon_of_model(model["fallback"])
                    if icon[0]:
                        overlay_icons.append((key, icon))
        print("items.bin: %d definitions of the packs" % len(overlay_icons))

    # ---- biome colours
    def colormap(path):
        img = assets.image(("textures/colormap/%s.png" % path))
        return img

    grass_map, foliage_map = colormap("grass"), colormap("foliage")

    def from_map(img, temp, down, default):
        if not img:
            return default
        w, h, px = img
        t = max(0.0, min(1.0, temp))
        d = max(0.0, min(1.0, down)) * t
        i = int((1.0 - t) * 255)
        j = int((1.0 - d) * 255)
        o = (j * w + i) * 4
        if o + 3 >= len(px):
            return default
        return px[o] << 16 | px[o + 1] << 8 | px[o + 2]

    def hexcol(v, default):
        if isinstance(v, str) and v.startswith("#"):
            return int(v[1:], 16)
        if isinstance(v, int):
            return v & 0xFFFFFF
        return default

    biome_rows = []
    bdir = os.path.join(res, "data", "minecraft", "worldgen", "biome")
    for fn in sorted(os.listdir(bdir)) if os.path.isdir(bdir) else []:
        if not fn.endswith(".json"):
            continue
        b = json.load(open(os.path.join(bdir, fn)))
        eff = b.get("effects", {})
        temp, down = float(b.get("temperature", 0.8)), float(b.get("downfall", 0.4))
        grass = hexcol(eff.get("grass_color"), None)
        if grass is None:
            grass = from_map(grass_map, temp, down, 0x91BD59)
        mod = eff.get("grass_color_modifier")
        if mod == "swamp":
            grass = 0x6A7039
        elif mod == "dark_forest":
            grass = ((grass & 0xFEFEFE) + 0x28340A) >> 1
        foliage = hexcol(eff.get("foliage_color"), None)
        if foliage is None:
            foliage = from_map(foliage_map, temp, down, 0x77AB2F)
        water = hexcol(eff.get("water_color"), 0x3F76E4)
        sky = hexcol(b.get("attributes", {}).get("minecraft:visual/sky_color"), 0x78A7FF)
        biome_rows.append(("minecraft:" + fn[:-5], (grass, foliage, water, sky)))

    particle_tiles = block_particle_tiles(names, states, assets, models, texture)
    # ---- atlas: 64 tiles a row, height a power of two
    rows = (len(tiles) + RESERVED + 63) // 64
    height = 1
    while height < rows * TILE:
        height *= 2
    width = 64 * TILE
    atlas = bytearray(width * height * 4)
    for i, t in enumerate(tiles):
        slot = i + RESERVED
        cx, cy = (slot % 64) * TILE, (slot // 64) * TILE
        for y in range(TILE):
            o = ((cy + y) * width + cx) * 4
            atlas[o:o + TILE * 4] = t[y * TILE * 4:(y + 1) * TILE * 4]
    for bx_, by_, bw_, bh_, bpx in big.values():
        for y in range(bh_):
            o = ((by_ + y) * width + bx_) * 4
            atlas[o:o + bw_ * 4] = bpx[y * bw_ * 4:(y + 1) * bw_ * 4]
    # v was stored in atlas pixels x16; make it a fraction of the height.
    fixed = []
    for q in quads:
        q = bytearray(q)
        for k in range(4):
            o = k * 10 + 8
            px16 = struct.unpack("<H", q[o:o + 2])[0]
            q[o:o + 2] = struct.pack("<H", min(65535, int(round(px16 / 16 / height * 65535))))
        fixed.append(bytes(q))
    quads = fixed

    def rect(path):
        i = tile_index[path][0]
        cx, cy = (i % 64) * TILE, (i // 64) * TILE
        return struct.pack("<ffff", cx / width, cy / height, (cx + TILE) / width, (cy + TILE) / height)

    with open(os.path.join(out_dir, "blocks.bin"), "wb") as f:
        f.write(b"PNBC" + struct.pack("<IIIIII", len(state_rows), len(blocks_out), len(boxes), len(quads), len(items),
                                      len(coll_boxes)))
        for blk, hard, flags, nbx, fbx, fq, nq, fc, nc, special, facing in state_rows:
            f.write(struct.pack("<HHBBHIHHIBBH", blk, hard, flags, nbx, fbx, fq, nq, facing, fc, nc, special, 0))
        for bx in boxes:
            f.write(bytes(bx))
        for c in coll_boxes:
            f.write(struct.pack("<6b", *c))
        for q in quads:
            f.write(q)
        for kind, tint, mine, bad, mc, short, sids, sv, move in blocks_out:
            nb = short.encode()[:255]
            f.write(bytes([kind, tint, mine, bad]) + bytes(mc) + bytes([len(nb)]) + nb)
            f.write(struct.pack("<5HBB", *sids, min(255, sv[0]), min(255, sv[1])))
            f.write(struct.pack("<fffBff", *move))
        for n in items:
            nb = n.split(":", 1)[1].encode()[:255]
            f.write(bytes([len(nb)]) + nb)
        f.write(rect("block/water_still") + rect("block/lava_still"))
        for i in range(10):
            f.write(rect("block/destroy_stage_%d" % i))
        # Biomes by name: grass, foliage, water and sky colours (BiomeSpecialEffects, the
        # colour maps for temperature and downfall, the swamp and dark forest modifiers).
        f.write(struct.pack("<I", len(biome_rows)))
        for name, cols in biome_rows:
            nb = name.encode()
            f.write(bytes([len(nb)]) + nb + struct.pack("<4I", *cols))
    with open(os.path.join(out_dir, "atlas.bin"), "wb") as f:
        f.write(b"PNA2" + struct.pack("<HH", width, height) + bytes(atlas))
    make_particles(assets, res, out_dir, particle_tiles, width, height)

    # ---- the GUI: vanilla's sprites, the font, the item icons
    sprites = {}  # name -> (w, h, rgba, nine-slice or None)

    def add_sprite(name, rel, crop=None):
        img = assets.image(("textures/" + rel + ".png"))
        if img is None:
            return
        w, h, px = img
        if crop:
            cw, ch = crop
            out = bytearray()
            for y in range(ch):
                out += px[(y * w) * 4:(y * w + cw) * 4]
            w, h, px = cw, ch, bytes(out)
        nine = None
        meta = assets.meta("textures/" + rel + ".png")
        if meta:
            try:
                sc = json.loads(meta).get("gui", {}).get("scaling", {})
                if sc.get("type") == "nine_slice":
                    b = sc.get("border", 0)
                    b = b if isinstance(b, int) else b.get("left", 0)
                    nine = (b, sc.get("width", w), sc.get("height", h))
            except ValueError:
                pass
        sprites[name] = (w, h, px, nine)

    listing = assets.names()
    for full in listing:
        for folder in ("hud/", "widget/", "tooltip/", "container/slot/", "container/inventory/", "container/furnace/",
                       "container/blast_furnace/", "container/smoker/", "container/creative_inventory/"):
            pre = "assets/minecraft/textures/gui/sprites/" + folder
            if full.startswith(pre) and full.endswith(".png"):
                rel = full[len("assets/minecraft/textures/"):-4]
                add_sprite(rel[len("gui/sprites/"):], rel)
    add_sprite("container/inventory", "gui/container/inventory", crop=(176, 166))
    # The chat's "Modified" message tag icon (GuiMessageTag.Icon.CHAT_MODIFIED).
    add_sprite("icon/chat_modified", "gui/sprites/icon/chat_modified")
    for name in ("tab_items", "tab_item_search", "tab_inventory"):
        add_sprite("container/creative_inventory/" + name, "gui/container/creative_inventory/" + name, crop=(195, 136))
    # The sky: sun, the eight moon phases (MoonPhase order), clouds.
    add_sprite("sky/sun", "environment/celestial/sun")
    for phase in ("full_moon", "waning_gibbous", "third_quarter", "waning_crescent", "new_moon", "waxing_crescent",
                  "first_quarter", "waxing_gibbous"):
        add_sprite("sky/moon/" + phase, "environment/celestial/moon/" + phase)
    add_sprite("sky/clouds", "environment/clouds")
    for name, crop in (("generic_54", (176, 222)), ("dispenser", (176, 166)), ("crafting_table", (176, 166)),
                       ("furnace", (176, 166)), ("smoker", (176, 166)), ("blast_furnace", (176, 166)),
                       ("hopper", (176, 133)), ("shulker_box", (176, 166))):
        add_sprite("container/" + name, "gui/container/" + name, crop=crop)

    # The default skins (DefaultPlayerSkin's order: slim then wide), for the player model.
    SKINS = ["alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri"]
    for kind in ("slim", "wide"):
        for n in SKINS:
            add_sprite("skin/%s/%s" % (kind, n), "entity/player/%s/%s" % (kind, n))

    # Entity models (vanilla's ModelPart trees, exported by horizon-prepare): loaded above.
    model_out = []
    for key in ("minecraft:player#main", "minecraft:player_slim#main"):
        parts = []

        def walk(name, part, parent):
            idx = len(parts)
            parts.append((name, parent, part.get("pose", [0] * 6 + [1] * 3), part.get("polygons", []), part.get("normals", [])))
            for cn, child in sorted(part.get("children", {}).items()):
                walk(cn, child, idx)

        if key in emodels:
            walk("root", emodels[key], -1)
        model_out.append((key, parts))

    # The font: bitmap providers in order (the first that has a character wins), widths as
    # vanilla's BitmapProvider finds them, and the space provider's advances.
    glyphs = {}
    font_pages = []

    def font_refs(ref, out, depth=0):
        # Every pack's font file, the top pack's providers first (a glyph comes from the first
        # provider that has it), as FontManager's resource stack.
        stack = assets.json_stack("font/" + nsref(ref) + ".json")
        for pr in [p for d in reversed(stack) for p in (d or {}).get("providers", [])]:
            if pr.get("type") == "reference" and depth < 4:
                if pr.get("filter", {}).get("uniform") is True:
                    continue
                font_refs(pr["id"], out, depth + 1)
            else:
                out.append(pr)
        return out

    # Which code points of the unihex font (GNU Unifont) are drawn: the symbols servers use
    # (punctuation, arrows, maths, boxes, shapes, dingbats) and the Latin, Greek and Cyrillic the
    # bitmap font lacks. All 65 thousand would cost too much. They live in a sheet of their own
    # (one byte a pixel, alpha only) beside the GUI atlas, not in it: the atlas is full at 2048 rows.
    UNIHEX_RANGES = [(0x00A0, 0x024F), (0x0370, 0x04FF), (0x2000, 0x2BFF)]
    hexcache = {}

    def unihex_glyphs(pr, glyphs):
        """UnihexProvider: Unifont's .hex lines (code point, 16 rows of 8 or 16 bits) as 16 x 16
        cells of one sheet, drawn at half size (an 8-high font). Every font with the provider
        shares the cells."""
        if pr.get("filter", {}).get("jp"):
            return
        if "cells" not in hexcache:
            hexcache["cells"] = {}
            data = assets.read(nsref(pr["hex_file"]))
            if data:
                import io
                zf = zipfile.ZipFile(io.BytesIO(data))
                want = {}
                for name in zf.namelist():
                    if not name.endswith(".hex"):
                        continue
                    for line in zf.read(name).decode("utf-8", "replace").splitlines():
                        cp_s, _, bits = line.partition(":")
                        try:
                            cp = int(cp_s, 16)
                        except ValueError:
                            continue
                        if cp in glyphs or len(bits) not in (32, 64) or not any(a <= cp <= b for a, b in UNIHEX_RANGES):
                            continue
                        want[cp] = bits
                cols = 64
                rows = (len(want) + cols - 1) // cols
                w, h = cols * 16, rows * 16
                alpha = bytearray(w * h)
                for i, (cp, bits) in enumerate(sorted(want.items())):
                    gx, gy = (i % cols) * 16, (i // cols) * 16
                    digits = len(bits) // 16          # hex digits a row: 2 (8 bits) or 4 (16 bits)
                    width = digits * 4
                    wid = 0
                    for y in range(16):
                        row = int(bits[y * digits:(y + 1) * digits], 16)
                        for x in range(width):
                            if row >> (width - 1 - x) & 1:
                                alpha[(gy + y) * w + gx + x] = 255
                                wid = max(wid, x + 1)
                    # Half size: the advance is the drawn width halved, rounded, plus the gap.
                    hexcache["cells"][cp] = ("HEX", gx, gy, width, 16, int(0.5 + wid * 0.5) + 1, 8, 0)
                if want:
                    hexcache["sheet"] = (w, h, bytes(alpha))
        for cp, rec in hexcache["cells"].items():
            glyphs.setdefault(cp, rec)

    def build_font(ref):
        """One font's glyphs (a table of code point -> record) from its providers; the pages
        they use are added to the atlas."""
        glyphs = {}
        for pr in font_refs(ref, []):
            if pr.get("type") == "space":
                for ch, adv in pr.get("advances", {}).items():
                    # A negative advance moves the text back (CraftEngine's GUI titles do).
                    glyphs.setdefault(ord(ch), (None, 0, 0, 0, 0, max(-32768, min(32767, int(adv))), 0, 0))
                continue
            if pr.get("type") == "unihex":
                unihex_glyphs(pr, glyphs)
                continue
            if pr.get("type") != "bitmap":
                continue
            rel = "textures/" + nsref(pr["file"])
            img = assets.image((rel))
            if img is None:
                continue
            w, h, px = img
            rows = pr["chars"]
            cw, chh = w // max(len(r) for r in rows), h // len(rows)
            fh = pr.get("height", 8)
            ascent = pr["ascent"]
            scale = fh / chh
            page = len(font_pages)
            font_pages.append(("font/page%d" % page, rel))
            sprites["font/page%d" % page] = (w, h, px, None)
            for ry, row in enumerate(rows):
                for rx, ch in enumerate(row):
                    cp = ord(ch)
                    if cp == 0 or cp in glyphs:
                        continue
                    gwid = 0
                    for gx_ in range(cw - 1, -1, -1):
                        if any(px[((ry * chh + gy_) * w + rx * cw + gx_) * 4 + 3] for gy_ in range(chh)):
                            gwid = gx_ + 1
                            break
                    adv = int(0.5 + gwid * scale) + 1
                    if cw > 255 or chh > 255 or fh > 255:
                        print("%s: glyph U+%04X is too large for gui.bin (cell %dx%d, height %d): left out"
                              % (rel, cp, cw, chh, fh))
                        continue
                    glyphs[cp] = (page, rx * cw, ry * chh, cw, chh, adv, fh, 7 - ascent)
        return glyphs

    glyphs = build_font("minecraft:default")
    # The packs' other fonts (CraftEngine's and the plugins' GUI pictures are glyphs of fonts like
    # minecraft:gui): a text's "font" picks one; the default font's glyphs stand in for a missing one.
    extra_fonts = []
    if packs:
        import re as _re
        names = set()
        for n in assets.names():
            m = _re.match(r"^assets/([a-z0-9_.-]+)/font/([a-z0-9_.-]+)\.json$", n)
            if m:
                names.add("%s:%s" % (m.group(1), m.group(2)))
        for name in sorted(names - {"minecraft:default", "minecraft:alt", "minecraft:illageralt"}):
            tbl = build_font(name)
            if tbl:
                extra_fonts.append((name, tbl))
        print("fonts: %d extra: %s" % (len(extra_fonts), ", ".join("%s (%d glyphs)" % (n, len(t)) for n, t in extra_fonts)))


    SKINS = ["alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri"]

    # ---- entities: each type's models, textures and equipment (make_entities)
    make_entities(res, assets, reg, out_dir)

    # Shelf-pack the sprites into a power-of-two atlas.
    order = sorted(sprites, key=lambda k: -sprites[k][1])
    gw = 1024
    placed = {}
    x = y = shelf = 0
    for k in order:
        w, h, _, _ = sprites[k]
        if x + w > gw:
            x, y, shelf = 0, y + shelf, 0
        placed[k] = (x, y)
        x += w + 1
        shelf = max(shelf, h + 1)
    gh = 1
    while gh < y + shelf:
        gh *= 2
    print("gui atlas: %d rows used of %d" % (y + shelf, gh))
    gatlas = bytearray(gw * gh * 4)
    for k, (px0, py0) in placed.items():
        w, h, px, _ = sprites[k]
        for yy in range(h):
            o = ((py0 + yy) * gw + px0) * 4
            gatlas[o:o + w * 4] = px[yy * w * 4:(yy + 1) * w * 4]

    with open(os.path.join(out_dir, "gui.bin"), "wb") as f:
        f.write(b"PNG6" + struct.pack("<HH", gw, gh) + bytes(gatlas))
        names_ = [k for k in sprites if not k.startswith("font/")]
        f.write(struct.pack("<I", len(names_)))
        for k in names_:
            w, h, _, nine = sprites[k]
            nb = k.encode()
            px0, py0 = placed[k]
            f.write(bytes([len(nb)]) + nb + struct.pack("<HHHH", px0, py0, w, h))
            f.write(struct.pack("<BBHH", 1 if nine else 0, nine[0] if nine else 0, nine[1] if nine else w, nine[2] if nine else h))
        def write_glyphs(table):
            f.write(struct.pack("<I", len(table)))
            # A glyph record (14 bytes, gui.rs's Glyph::at): cp, atlas x, y, cell w, h, rendered
            # height, yoff, advance (signed, a space provider's may be negative).
            for cp, (page, gx, gy, cw, chh, adv, fh, yoff) in sorted(table.items()):
                if page is None:
                    f.write(struct.pack("<IHHBBBbh", cp, 0, 0, 0, 0, 0, 0, adv))
                else:
                    if page == "HEX":
                        px0, py0, cp = 0, 0, cp | 0x80000000
                    else:
                        px0, py0 = placed["font/page%d" % page]
                    if not (0 <= cw <= 255 and 0 <= chh <= 255 and 0 <= fh <= 255) or not -128 <= yoff <= 127:
                        # A pack's glyph a byte cannot hold (a negative or huge size): drawn clamped.
                        print("font: glyph %x is %dx%d (height %d): clamped to 0..255" % (cp, cw, chh, fh))
                        cw, chh, fh = [max(0, min(255, v)) for v in (cw, chh, fh)]
                        yoff = max(-128, min(127, yoff))
                    f.write(struct.pack("<IHHBBBbh", cp, px0 + gx, py0 + gy, cw, chh, fh, yoff, adv))

        write_glyphs(glyphs)
        # The packs' other fonts: name, then their glyphs (the order is a font's id, from 1).
        f.write(struct.pack("<I", len(extra_fonts)))
        for fname, tbl in extra_fonts:
            nb = fname.encode()
            f.write(bytes([len(nb)]) + nb)
            write_glyphs(tbl)
        # The unihex sheet: width, height, then one alpha byte a pixel (0 0: none).
        sheet = hexcache.get("sheet")
        if sheet:
            f.write(struct.pack("<HH", sheet[0], sheet[1]) + sheet[2])
        else:
            f.write(struct.pack("<HH", 0, 0))
        f.write(struct.pack("<I", len(model_out)))
        for key, parts in model_out:
            kb = key.encode()
            f.write(bytes([len(kb)]) + kb + struct.pack("<I", len(parts)))
            for name, parent, pose, polys, normals in parts:
                nb = name.encode()
                f.write(bytes([len(nb)]) + nb + struct.pack("<h9f", parent, *[float(v) for v in pose[:9]]))
                f.write(struct.pack("<H", len(polys)))
                for i, poly in enumerate(polys):
                    for vtx in poly[:4]:
                        f.write(struct.pack("<5f", *[float(v) for v in vtx[:5]]))
                    nrm = normals[i] if i < len(normals) else [0, 0, 1]
                    f.write(struct.pack("<3f", *[float(v) for v in nrm]))
        # Each item's first-person transform (display.firstperson_righthand).
        f.write(struct.pack("<I", len(item_hands)))
        for rot, tr, sc in item_hands:
            f.write(struct.pack("<9f", *(list(rot) + list(tr) + list(sc))))
        # Every pack's en_us.json over the game's, later keys winning (ClientLanguage).
        lang = {}
        for d in assets.json_stack("lang/en_us.json"):
            if isinstance(d, dict):
                lang.update({k: v for k, v in d.items() if isinstance(v, str)})
        f.write(struct.pack("<I", len(lang)))
        for k, v in sorted(lang.items()):
            kb, vb = k.encode()[:65535], v.encode()[:65535]
            f.write(struct.pack("<H", len(kb)) + kb + struct.pack("<H", len(vb)) + vb)
        f.write(struct.pack("<I", len(item_icons)))
        for icon in item_icons:
            if icon[0] == 1:
                layers = icon[1]
                f.write(bytes([1, len(layers)]))
                for ti, tint in layers:
                    f.write(struct.pack("<HI", ti, tint))
            elif icon[0] == 2:
                _, fq, nq, (rot, tr, sc), tint = icon
                f.write(bytes([2, 0]) + struct.pack("<IHI", fq, nq, tint))
                f.write(struct.pack("<9f", *(list(rot) + list(tr) + list(sc))))
            else:
                f.write(bytes([0, 0]))
        # The creative inventory, found from the offset in the last 4 bytes (0: none).
        crt = creative_section(res, reg, items)
        at = f.tell()
        if crt:
            f.write(crt)
        f.write(struct.pack("<I", at if crt else 0))
    print("gui.bin: %d sprites, %d glyphs, %d translations, atlas %dx%d; %d items (%d flat, %d 3D)"
          % (len(sprites) - len(font_pages), len(glyphs), len(lang), gw, gh, len(item_icons),
             sum(1 for i in item_icons if i[0] == 1), sum(1 for i in item_icons if i[0] == 2)))
    # items.bin, "PNI1": u32 count, then per icon a u8 key length and key ("ns:path" or
    # "ns:path|string"), then u8 kind and u8 n: kind 1 is flat (n layers, each u16 atlas tile and
    # u32 tint); kind 2 is a model (n is 0, then u32 first quad, u16 quads, u32 tint and nine
    # f32 display transforms: rotation, translation, scale). Quad indices refer to blocks.bin.
    with open(os.path.join(out_dir, "items.bin"), "wb") as f:
        f.write(ITEMS_MAGIC + struct.pack("<I", len(overlay_icons)))
        for key, icon in overlay_icons:
            kb = key.encode()[:255]
            f.write(bytes([len(kb)]) + kb)
            if icon[0] == 1:
                f.write(bytes([1, len(icon[1])]))
                for ti, tint in icon[1]:
                    f.write(struct.pack("<HI", ti, tint))
            else:
                _, fq, nq, (rot, tr, sc), tint = icon
                f.write(bytes([2, 0]) + struct.pack("<IHI", fq, nq, tint))
                f.write(struct.pack("<9f", *(list(rot) + list(tr) + list(sc))))
    print("items.bin: %d icons" % len(overlay_icons))
    make_signs(assets, names, out_dir)
    make_effects(assets, out_dir, reg, facts)
    if packs:
        make_pack_sounds(res, assets, out_dir, sound_names)
    else:
        make_sounds(res, out_dir, sound_names)
    make_weather(res, assets, out_dir, reg)
    make_menu(res, assets, out_dir)
    make_serverui(assets, out_dir, res, reg)
    make_containers(assets, out_dir, reg)
    print("blocks.bin: %d states, %d blocks, %d quads; atlas %dx%d with %d textures%s"
          % (len(state_rows), len(blocks_out), len(quads), width, height, len(tiles),
             (", missing: " + ", ".join(sorted(missing))) if missing else ""))



def make_menu(res, assets, out_dir):
    """menu.bin, "PNM1": the title screen and the multiplayer screens (TitleScreen, Panorama,
    LogoRenderer, SplashRenderer, JoinMultiplayerScreen). Optional for Penryn: without it the
    title screen is drawn on a plain background, without the logo and splashes.

        "PNM1", u32 size of the rest unpacked, then the rest as one zlib stream:
        u32 length, texts/splashes.txt (UTF-8)
        u16 images; per image: u8 name length, name, u16 width, u16 height, u8 channels (3 RGB
            or 4 RGBA), the pixels (rows top to bottom)
        u16 sprites; per sprite: u8 name length, name, u8 image, u16 x, y, width, height

    The panorama's six faces (gui/title/background, which the game's jar only holds as
    placeholders: they come from the prepared data or the installed assets) at 512 x 512."""
    def read_any(rel):
        # The prepared data and the installed asset objects have the real panorama.
        p = os.path.join(res, "assets", "minecraft", rel)
        if os.path.isfile(p):
            with open(p, "rb") as f:
                return f.read()
        index_path, objects = find_objects()
        if index_path and objects:
            try:
                objs = json.load(open(index_path)).get("objects", {})
                o = objs.get("minecraft/" + rel)
                if o:
                    with open(os.path.join(objects, o["hash"][:2], o["hash"]), "rb") as f:
                        return f.read()
            except (OSError, ValueError):
                pass
        return assets.read(rel)

    images, sprites = [], []

    def add(name, w, h, ch, px):
        images.append((name, w, h, ch, bytes(px)))
        return len(images) - 1

    def rgb(px):
        out = bytearray(len(px) // 4 * 3)
        out[0::3], out[1::3], out[2::3] = px[0::4], px[1::4], px[2::4]
        return out

    for i in range(6):
        img = png_decode(read_any("textures/gui/title/background/panorama_%d.png" % i))
        if img is None:
            print("menu.bin: panorama_%d.png missing: no panorama" % i)
            images = [im for im in images if not im[0].startswith("panorama")]
            break
        w, h, px = img
        step = max(1, w // 512)
        n = w // step
        full = rgb(px)
        face = bytearray()
        for y in range(0, h, step):
            row = full[y * w * 3:(y + 1) * w * 3]
            r = bytearray(n * 3)
            r[0::3], r[1::3], r[2::3] = row[0::3 * step], row[1::3 * step], row[2::3 * step]
            face += r
        add("panorama_%d" % i, n, h // step, 3, face)
    for name, rel in (("overlay", "gui/title/background/panorama_overlay"), ("minecraft", "gui/title/minecraft"),
                      ("minceraft", "gui/title/minceraft"), ("edition", "gui/title/edition"),
                      ("menu_background", "gui/menu_background"), ("menu_list_background", "gui/menu_list_background"),
                      ("header_separator", "gui/header_separator"), ("footer_separator", "gui/footer_separator"),
                      ("unknown_server", "misc/unknown_server")):
        img = png_decode(read_any("textures/%s.png" % rel))
        if img is None:
            print("menu.bin: %s.png missing" % rel)
            continue
        add(name, img[0], img[1], 4, img[2])
    # The small sprites, side by side in one 256-wide atlas (shelf packing).
    names = (["server_list/" + n for n in ("incompatible", "unreachable", "join", "join_highlighted", "move_up",
                                           "move_up_highlighted", "move_down", "move_down_highlighted")]
             + ["server_list/ping_%d" % i for i in range(1, 6)] + ["server_list/pinging_%d" % i for i in range(1, 6)]
             + ["icon/language", "icon/accessibility", "friends/friends"])
    found = []
    for n in names:
        img = png_decode(read_any("textures/gui/sprites/%s.png" % n))
        if img is not None:
            found.append((n, img))
    aw, x, y, shelf = 256, 0, 0, 0
    placed = []
    for n, (w, h, px) in found:
        if x + w > aw:
            x, y, shelf = 0, y + shelf, 0
        placed.append((n, x, y, w, h, px))
        x += w
        shelf = max(shelf, h)
    ah = max(1, y + shelf)
    atlas = bytearray(aw * ah * 4)
    for n, sx, sy, w, h, px in placed:
        for j in range(h):
            atlas[((sy + j) * aw + sx) * 4:((sy + j) * aw + sx + w) * 4] = px[j * w * 4:(j + 1) * w * 4]
    ai = add("sprites", aw, ah, 4, atlas)
    for n, sx, sy, w, h, _ in placed:
        sprites.append((n, ai, sx, sy, w, h))
    splashes = (read_any("texts/splashes.txt") or b"")
    raw = bytearray(struct.pack("<I", len(splashes)) + splashes)
    raw += struct.pack("<H", len(images))
    for name, w, h, ch, px in images:
        nb = name.encode()
        raw += bytes([len(nb)]) + nb + struct.pack("<HHB", w, h, ch) + px
    raw += struct.pack("<H", len(sprites))
    for name, i, sx, sy, w, h in sprites:
        nb = name.encode()
        raw += bytes([len(nb)]) + nb + struct.pack("<BHHHH", i, sx, sy, w, h)
    with open(os.path.join(out_dir, "menu.bin"), "wb") as f:
        f.write(b"PNM1" + struct.pack("<I", len(raw)) + zlib.compress(bytes(raw), 6))
    print("menu.bin: %d images (%s panorama), %d sprites, %d splashes"
          % (len(images), "with" if any(im[0] == "panorama_0" for im in images) else "no", len(sprites),
             splashes.count(b"\n")))
def class_ops(data, method):
    """The bytecode of one method of a class file as (opcode name, operand) pairs: ldc and
    push operands resolved to their values, the rest left as None (enough to read constant
    tables out of vanilla's classes)."""
    p = 8
    n = struct.unpack(">H", data[p:p + 2])[0]
    p += 2
    cp = [None] * n
    i = 1
    while i < n:
        t = data[p]
        p += 1
        if t == 1:
            l = struct.unpack(">H", data[p:p + 2])[0]
            cp[i] = data[p + 2:p + 2 + l].decode("utf8", "replace")
            p += 2 + l
        elif t == 3:
            cp[i] = struct.unpack(">i", data[p:p + 4])[0]
            p += 4
        elif t == 4:
            p += 4
        elif t in (5, 6):
            p += 8
            i += 1
        elif t == 8:
            cp[i] = ("str", struct.unpack(">H", data[p:p + 2])[0])
            p += 2
        elif t in (7, 16, 19, 20):
            p += 2
        elif t == 15:
            p += 3
        else:
            p += 4
        i += 1
    val = lambda k: cp[cp[k][1]] if isinstance(cp[k], tuple) else cp[k]
    p += 6
    p += 2 + 2 * struct.unpack(">H", data[p:p + 2])[0]

    def attrs(p):
        out = []
        cnt = struct.unpack(">H", data[p:p + 2])[0]
        p += 2
        for _ in range(cnt):
            nm, l = struct.unpack(">HI", data[p:p + 6])
            out.append((cp[nm], data[p + 6:p + 6 + l]))
            p += 6 + l
        return p, out

    nf = struct.unpack(">H", data[p:p + 2])[0]
    p += 2
    for _ in range(nf):
        p, _a = attrs(p + 6)
    ops = []
    nm_ = struct.unpack(">H", data[p:p + 2])[0]
    p += 2
    for _ in range(nm_):
        nm = cp[struct.unpack(">H", data[p + 2:p + 4])[0]]
        p, at = attrs(p + 6)
        if nm != method:
            continue
        for an, ab in at:
            if an != "Code":
                continue
            code = ab[8:8 + struct.unpack(">I", ab[4:8])[0]]
            q = 0
            # Operand lengths of the opcodes (tableswitch and lookupswitch handled below).
            two = set(range(0x13, 0x15)) | {0x11} | set(range(0x99, 0xA9)) | set(range(0xB2, 0xB9)) | {0xBB, 0xBD, 0xC0, 0xC1, 0xC6, 0xC7, 0x84}
            one = {0x10, 0x12, 0xBC, 0xA9} | set(range(0x15, 0x1A)) | set(range(0x36, 0x3B))
            while q < len(code):
                op = code[q]
                if op == 0x10:
                    ops.append(("push", struct.unpack(">b", code[q + 1:q + 2])[0]))
                elif op == 0x11:
                    ops.append(("push", struct.unpack(">h", code[q + 1:q + 3])[0]))
                elif 0x02 <= op <= 0x08:
                    ops.append(("push", op - 3))
                elif op == 0x12:
                    ops.append(("push", val(code[q + 1])))
                elif op in (0x13, 0x14):
                    ops.append(("push", val(struct.unpack(">H", code[q + 1:q + 3])[0])))
                else:
                    ops.append((op, None))
                if op == 0xAA:
                    q2 = (q + 4) & ~3
                    lo, hi = struct.unpack(">ii", code[q2 + 4:q2 + 12])
                    q = q2 + 12 + 4 * (hi - lo + 1)
                elif op == 0xAB:
                    q2 = (q + 4) & ~3
                    q = q2 + 8 + 8 * struct.unpack(">i", code[q2 + 4:q2 + 8])[0]
                elif op == 0xC4:
                    q += 6 if code[q + 1] == 0x84 else 4
                elif op in (0xB9, 0xBA, 0xC8, 0xC9):
                    q += 5
                elif op == 0xC5:
                    q += 4
                elif op in two:
                    q += 3 if op != 0x84 else 3
                elif op in one:
                    q += 2
                else:
                    q += 1
    return ops


def make_containers(assets, out_dir, reg):
    """containers.bin, "PNC1" (little-endian): what the container screens Penryn draws itself
    need beyond gui.bin (anvil, beacon, brewing stand, enchanting table, grindstone, loom,
    merchant, smithing, stonecutter, cartography, crafter, horse, books), maps and item frames.
    Optional: without it those menus show their slots as chest rows and maps stay blank.

        u16 width, u16 height, width x height RGBA (the sprites)
        u16 sprites; per sprite: u8 name length, name, u16 x, y, w, h
        u16 glyphs of the enchanting table's alt font (ascii_sga); per glyph: u16 code point,
            u16 x, y, u8 cell width, cell height, advance
        u16 length, the EnchantmentNames words (space separated)
        u8 map colours (MapColor ids); per colour: u32 RGB
        u8 banner patterns without an item (the no_item_required tag, in order); per one:
            u8 length, name. u8 pattern items; per item: u8 length, item name, u8 patterns,
            each u8 length, name
        u8 item frame models (item_frame, item_frame_map, glow_item_frame,
            glow_item_frame_map); per model: u16 quads; per quad: 4 x (f32 x, y, z in block
            units, f32 u, v in the atlas), f32 shade
        u8 map decoration types (registry order); per type: u8 length, sprite name
            (map/decorations/<name>), u8 shown in item frames
    """
    jar = assets.zip
    sprites = {}

    def add(name, rel, crop=None):
        img = png_decode(assets.read("textures/" + rel + ".png"))
        if img is None:
            return False
        w, h, px = img
        if crop:
            cw, ch = min(crop[0], w), min(crop[1], h)
            out = bytearray()
            for y in range(ch):
                out += px[(y * w) * 4:(y * w + cw) * 4]
            w, h, px = cw, ch, bytes(out)
        sprites[name] = (w, h, px)
        return True

    for name, crop in (("anvil", (176, 166)), ("beacon", (230, 219)), ("brewing_stand", (176, 166)),
                       ("cartography_table", (176, 166)), ("crafter", (176, 166)), ("enchanting_table", (176, 166)),
                       ("grindstone", (176, 166)), ("horse", (176, 166)), ("loom", (176, 166)), ("smithing", (176, 166)),
                       ("stonecutter", (176, 166)), ("villager", (276, 166))):
        add("bg/" + name, "gui/container/" + name, crop)
    add("bg/book", "gui/book", (192, 192))
    add("container/slot", "gui/sprites/container/slot")
    def raw(path):
        if jar:
            try:
                return jar.read(path)
            except KeyError:
                return None
        try:
            with open(os.path.join(assets.folder, path), "rb") as f:
                return f.read()
        except OSError:
            return None

    if jar:
        listing = jar.namelist()
    else:
        listing = []
        for top in ("assets", "data"):
            for root, _, files in os.walk(os.path.join(assets.folder, top)):
                for fn in files:
                    listing.append(os.path.relpath(os.path.join(root, fn), assets.folder).replace(os.sep, "/"))
    for full in listing:
        for folder in ("anvil", "beacon", "brewing_stand", "cartography_table", "crafter", "enchanting_table", "grindstone",
                       "horse", "loom", "smithing", "stonecutter", "villager"):
            pre = "assets/minecraft/textures/gui/sprites/container/%s/" % folder
            if full.startswith(pre) and full.endswith(".png"):
                rel = full[len("assets/minecraft/textures/"):-4]
                add(rel[len("gui/sprites/"):], rel)
        pre = "assets/minecraft/textures/map/decorations/"
        if full.startswith(pre) and full.endswith(".png"):
            add("map/decorations/" + full[len(pre):-4], "map/decorations/" + full[len(pre):-4])
    add("map/background", "map/map_background")
    add("map/checkerboard", "map/map_background_checkerboard")
    for t in ("item_frame", "glow_item_frame", "birch_planks"):
        add("block/" + t, "block/" + t)
    # The banner patterns' flags (u 1..21, v 1..41 of their 64 x 64 textures), for the loom.
    patterns = sorted(reg.get("minecraft:banner_pattern", {}).get("entries", {}))
    names = set()
    for full in listing:
        pre = "assets/minecraft/textures/entity/banner/"
        if full.startswith(pre) and full.endswith(".png") and "/" not in full[len(pre):]:
            names.add(full[len(pre):-4])
    for n in sorted(names):
        img = png_decode(assets.read("textures/entity/banner/%s.png" % n))
        if img is None or img[0] != 64:
            continue
        w, h, px = img
        out = bytearray()
        for y in range(1, 41):
            out += px[(y * w + 1) * 4:(y * w + 21) * 4]
        sprites["banner/" + n] = (20, 40, bytes(out))
    # The alt font's glyphs (BitmapProvider's widths).
    glyph_rows = []
    font = assets.json("font/alt.json") or {}
    sga = None
    for pr in font.get("providers", []):
        if pr.get("type") == "bitmap":
            sga = pr
    if sga:
        img = png_decode(assets.read("textures/" + sga["file"].split(":", 1)[-1]))
        if img:
            w, h, px = img
            rows = sga["chars"]
            cw, chh = w // max(len(r) for r in rows), h // len(rows)
            sprites["font/sga"] = (w, h, px)
            for ry, row in enumerate(rows):
                for rx, ch in enumerate(row):
                    if ord(ch) == 0:
                        continue
                    gwid = 0
                    for gx in range(cw - 1, -1, -1):
                        if any(px[((ry * chh + gy) * w + rx * cw + gx) * 4 + 3] for gy in range(chh)):
                            gwid = gx + 1
                            break
                    glyph_rows.append((ord(ch), rx * cw, ry * chh, cw, chh, int(0.5 + gwid * 8 / chh) + 1))

    def cls(path):
        if not jar:
            return None
        try:
            return jar.read(path)
        except KeyError:
            return None

    words = []
    d = cls("net/minecraft/client/gui/screens/inventory/EnchantmentNames.class")
    if d:
        words = [v for k, v in class_ops(d, "<init>") if k == "push" and isinstance(v, str)]
    colours = {}
    d = cls("net/minecraft/world/level/material/MapColor.class")
    if d:
        ops = class_ops(d, "<clinit>")
        for i in range(len(ops) - 3):
            # new MapColor, dup, id, colour, invokespecial
            if ops[i][0] == 0xBB and ops[i + 1][0] == 0x59 and ops[i + 2][0] == "push" and ops[i + 3][0] == "push" \
                    and ops[i + 4][0] == 0xB7 and isinstance(ops[i + 2][1], int) and isinstance(ops[i + 3][1], int):
                colours[ops[i + 2][1]] = ops[i + 3][1] & 0xFFFFFF
    # MapDecorationTypes: each type's sprite and whether item frames show it, by registry id.
    decorations = []
    d = cls("net/minecraft/world/level/saveddata/maps/MapDecorationTypes.class")
    if d:
        ops = class_ops(d, "<clinit>")
        ids = reg.get("minecraft:map_decoration_type", {}).get("entries", {})
        found = {}
        for i in range(len(ops) - 3):
            a, b, c_, e = ops[i:i + 4]
            if a[0] == b[0] == c_[0] == e[0] == "push" and isinstance(a[1], str) and isinstance(b[1], str) \
                    and isinstance(c_[1], int) and isinstance(e[1], int) and ("minecraft:" + a[1]) in ids:
                found[ids["minecraft:" + a[1]]["protocol_id"]] = (b[1], c_[1])
        decorations = [found.get(i, ("", 0)) for i in range(max(found) + 1)] if found else []
    loom_free = []
    t = raw("data/minecraft/tags/banner_pattern/no_item_required.json")
    t = json.loads(t) if t else None
    if t:
        loom_free = [v.split(":", 1)[-1] for v in t.get("values", []) if not v.startswith("#")]
    loom_items = []
    for full in listing:
        pre = "data/minecraft/tags/banner_pattern/pattern_item/"
        if full.startswith(pre) and full.endswith(".json"):
            key = full[len(pre):-5]
            vals = [v.split(":", 1)[-1] for v in json.loads(raw(full)).get("values", []) if not v.startswith("#")]
            loom_items.append((key + "_banner_pattern", vals))

    # Item frame models: template_item_frame(_map) with their textures.
    frame_models = []
    for name in ("item_frame", "item_frame_map", "glow_item_frame", "glow_item_frame_map"):
        m = assets.json("models/block/%s.json" % name) or {}
        texs = dict(m.get("textures", {}))
        parent = assets.json("models/block/%s.json" % m.get("parent", "").split("/")[-1]) or {}
        frame_models.append((texs, parent.get("elements", [])))

    # Shelf-pack.
    order = sorted(sprites, key=lambda k: -sprites[k][1])
    aw = 1024
    placed = {}
    x = y = shelf = 0
    for k in order:
        w, h, _ = sprites[k]
        if x + w > aw:
            x, y, shelf = 0, y + shelf, 0
        placed[k] = (x, y)
        x += w + 1
        shelf = max(shelf, h + 1)
    ah = 1
    while ah < y + shelf:
        ah *= 2
    atlas = bytearray(aw * ah * 4)
    for k, (sx, sy) in placed.items():
        w, h, px = sprites[k]
        for j in range(h):
            atlas[((sy + j) * aw + sx) * 4:((sy + j) * aw + sx + w) * 4] = px[j * w * 4:(j + 1) * w * 4]

    out = bytearray(b"PNC1" + struct.pack("<HH", aw, ah)) + atlas
    out += struct.pack("<H", len(placed))
    for k in sorted(placed):
        nb = k.encode()
        w, h, _ = sprites[k]
        out += bytes([len(nb)]) + nb + struct.pack("<4H", placed[k][0], placed[k][1], w, h)
    fx, fy = placed.get("font/sga", (0, 0))
    out += struct.pack("<H", len(glyph_rows))
    for cp, gx, gy, cw, chh, adv in glyph_rows:
        out += struct.pack("<HHHBBB", cp, fx + gx, fy + gy, cw, chh, adv)
    wb = " ".join(words).encode()
    out += struct.pack("<H", len(wb)) + wb
    n = max(colours) + 1 if colours else 0
    out += bytes([n])
    for i in range(n):
        out += struct.pack("<I", colours.get(i, 0))

    def strs(lst):
        b = bytes([len(lst)])
        for s_ in lst:
            sb = s_.encode()
            b += bytes([len(sb)]) + sb
        return b

    out += strs(loom_free)
    out += bytes([len(loom_items)])
    for item, vals in sorted(loom_items):
        ib = item.encode()
        out += bytes([len(ib)]) + ib + strs(vals)
    out += bytes([len(frame_models)])
    for texs, elements in frame_models:
        quads = []
        for el in elements:
            f, t = el["from"], el["to"]
            for dname, face in el.get("faces", {}).items():
                tex = texs.get(face.get("texture", "").lstrip("#"), "")
                key = "block/" + tex.split("/")[-1]
                if key not in placed:
                    continue
                sx, sy = placed[key]
                uv = face.get("uv") or default_uv(dname, f, t)
                vs = []
                for p_, (s_, t_) in face_corners(dname, f, t):
                    u = (sx + (uv[0] + (uv[2] - uv[0]) * s_)) / aw
                    v = (sy + (uv[1] + (uv[3] - uv[1]) * t_)) / ah
                    vs.append((p_[0] / 16, p_[1] / 16, p_[2] / 16, u, v))
                quads.append((vs, SHADE[dname]))
        out += struct.pack("<H", len(quads))
        for vs, sh in quads:
            for v in vs:
                out += struct.pack("<5f", *v)
            out += struct.pack("<f", sh)
    out += bytes([len(decorations)])
    for asset, on_frame in decorations:
        ab = asset.encode()
        out += bytes([len(ab)]) + ab + bytes([on_frame])
    with open(os.path.join(out_dir, "containers.bin"), "wb") as f:
        f.write(out)
    print("containers.bin: %d sprites (atlas %dx%d), %d alt glyphs, %d words, %d map colours, %d+%d banner patterns, "
          "%d map decorations" % (len(placed), aw, ah, len(glyph_rows), len(words), n, len(loom_free), len(loom_items),
                                  len(decorations)))


def make_weather(res, assets, out_dir, reg):
    """weather.bin, "PNW1" (little-endian): u16 the lightning_bolt entity type id; rain.png and
    snow.png (environment/) each as u16 width, u16 height, RGBA; then u32 biome count and per
    biome: u8 name length, name, f32 temperature, u8 flags (1 has_precipitation, 2 the frozen
    temperature modifier). Optional for Penryn: without it there is no rain or snow."""
    bolt = reg["minecraft:entity_type"]["entries"]["minecraft:lightning_bolt"]["protocol_id"]
    out = bytearray(b"PNW1" + struct.pack("<H", bolt))
    for name in ("rain", "snow"):
        img = assets.image(("textures/environment/%s.png" % name))
        if img is None:
            print("weather.bin: environment/%s.png missing: no weather" % name)
            return
        w, h, px = img
        out += struct.pack("<HH", w, h) + bytes(px)
    rows = []
    bdir = os.path.join(res, "data", "minecraft", "worldgen", "biome")
    for fn in sorted(os.listdir(bdir)):
        if not fn.endswith(".json"):
            continue
        b = json.load(open(os.path.join(bdir, fn)))
        flags = (1 if b.get("has_precipitation", True) else 0) | (2 if b.get("temperature_modifier") == "frozen" else 0)
        rows.append(("minecraft:" + fn[:-5], float(b.get("temperature", 0.8)), flags))
    out += struct.pack("<I", len(rows))
    for name, t, flags in rows:
        nb = name.encode()
        out += struct.pack("<B", len(nb)) + nb + struct.pack("<fB", t, flags)
    with open(os.path.join(out_dir, "weather.bin"), "wb") as f:
        f.write(out)
    print("weather.bin: rain and snow textures, %d biomes" % len(rows))

def make_serverui(assets, out_dir, res=None, reg=None):
    """serverui.bin, "PNU1" (little-endian): the pictures of what servers show besides the
    world: the boss bars (gui/sprites/boss_bar/), the tab list's ping icons (icon/ping_*), the
    spectator hotbar's icons (spectator/), the world border's wall (misc/forcefield) and the
    vignette (misc/vignette). u16 count; per picture: u8 name length, name, u16 width,
    u16 height, RGBA. Then the items' default attribute modifiers (items.json), which tooltips
    list: u16 items; per item: u16 item id, u8 modifiers; per modifier: u8 attribute id, f64
    amount, u8 operation, u8 EquipmentSlotGroup id, u8 flags (1 the base attack damage, 2 the
    base attack speed, 4 hidden). Optional for Penryn: without it boss bars are plain bars, the
    border is not drawn and tooltips have no default attributes."""
    names = []
    for colour in ("pink", "blue", "red", "green", "yellow", "purple", "white"):
        names += ["boss_bar/%s_background" % colour, "boss_bar/%s_progress" % colour]
    for n in (6, 10, 12, 20):
        names += ["boss_bar/notched_%d_background" % n, "boss_bar/notched_%d_progress" % n]
    names += ["icon/ping_%s" % p for p in ("unknown", "1", "2", "3", "4", "5")]
    names += ["spectator/%s" % s for s in ("close", "scroll_left", "scroll_right", "teleport_to_player", "teleport_to_team")]
    pics = []
    for n in names:
        img = png_decode(assets.read("textures/gui/sprites/%s.png" % n))
        if img is not None:
            pics.append((n, img))
    for n in ("forcefield", "vignette"):
        img = png_decode(assets.read("textures/misc/%s.png" % n))
        if img is not None:
            pics.append(("misc/" + n, img))
    out = bytearray(b"PNU1" + struct.pack("<H", len(pics)))
    for n, (w, h, px) in pics:
        nb = n.encode()
        out += struct.pack("<B", len(nb)) + nb + struct.pack("<HH", w, h) + bytes(px)
    groups = ["any", "mainhand", "offhand", "hand", "feet", "legs", "chest", "head", "armor", "body", "saddle"]
    ops = ["add_value", "add_multiplied_base", "add_multiplied_total"]
    rows = []
    path = os.path.join(res, "items.json") if res else None
    if path and reg and os.path.isfile(path):
        attrs = reg["minecraft:attribute"]["entries"]
        item_ids = reg["minecraft:item"]["entries"]
        for name, comps in sorted(json.load(open(path)).items(), key=lambda kv: item_ids[kv[0]]["protocol_id"]):
            mods = comps.get("minecraft:attribute_modifiers") or []
            if not mods or name not in item_ids:
                continue
            ms = []
            for m in mods[:255]:
                flags = (1 if m["id"] == "minecraft:base_attack_damage" else 0) | (2 if m["id"] == "minecraft:base_attack_speed" else 0)
                flags |= 4 if (m.get("display") or {}).get("type") == "hidden" else 0
                ms.append(struct.pack("<BdBBB", attrs[m["type"]]["protocol_id"], float(m["amount"]),
                                      ops.index(m["operation"]), groups.index(m.get("slot", "any")), flags))
            rows.append(struct.pack("<HB", item_ids[name]["protocol_id"], len(ms)) + b"".join(ms))
    out += struct.pack("<H", len(rows)) + b"".join(rows)
    with open(os.path.join(out_dir, "serverui.bin"), "wb") as f:
        f.write(out)
    print("serverui.bin: %d pictures, %d items' attributes" % (len(pics), len(rows)))

def make_signs(assets, block_names, out_dir):
    """signs.bin, "PNT1": the sign editing screen's pictures, gui/signs/<wood>.png (24 x 26)
    and gui/hanging_signs/<wood>.png (16 x 16), for each wood that has a sign block.

        u16 width, u16 height, width x height RGBA
        u8 woods; per wood: u8 name length, name, u16 x, y of the sign picture,
        u16 x, y of the hanging sign picture (65535 when the game has none)
    """
    woods = [n[len("minecraft:"):-len("_sign")] for n in block_names
             if n.endswith("_sign") and not n.endswith("_hanging_sign") and not n.endswith("_wall_sign")]
    width, height = 512, 128
    atlas = bytearray(width * height * 4)
    rows = []

    def put(img, x, y):
        w, h, px = img
        for j in range(h):
            atlas[((y + j) * width + x) * 4:((y + j) * width + x + w) * 4] = px[j * w * 4:(j + 1) * w * 4]

    for i, wood in enumerate(woods):
        at = []
        for kind, (w, h), (x, y) in (("signs", (24, 26), ((i % 21) * 24, (i // 21) * 26)),
                                      ("hanging_signs", (16, 16), ((i % 32) * 16, 52 + (i // 32) * 16))):
            img = assets.image(("textures/gui/%s/%s.png" % (kind, wood)))
            if img is None or img[0] != w or img[1] != h or y + h > height:
                at.append((0xFFFF, 0xFFFF))
                continue
            put(img, x, y)
            at.append((x, y))
        rows.append((wood, at))
    with open(os.path.join(out_dir, "signs.bin"), "wb") as f:
        f.write(b"PNT1" + struct.pack("<HH", width, height) + bytes(atlas))
        f.write(bytes([len(rows)]))
        for wood, ((sx, sy), (hx, hy)) in rows:
            nb = wood.encode()
            f.write(bytes([len(nb)]) + nb + struct.pack("<HHHH", sx, sy, hx, hy))
    print("signs.bin: %d woods" % len(rows))
def make_effects(assets, out_dir, reg, facts):
    """effects.bin, "PNF1": the status effect icons (textures/mob_effect), for the HUD and the
    inventory, and how pistons treat each block state. Optional: without it the effects show
    their backgrounds only and pistons guess.

        u16 icons, u16 atlas width, u16 atlas height
        per icon (mob_effect registry order): u8 name length, name (without "minecraft:"),
            u16 x, u16 y, u8 width, u8 height (0 x 0: no texture)
        then width x height RGBA
        u32 states; per state: u8 piston push reaction (0 normal, 1 destroy, 2 block, 3 ignore,
            4 push only), + 128 when it has a block entity"""
    names = [k.split(":", 1)[-1] for k, _ in
             sorted(reg["minecraft:mob_effect"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])]
    imgs = []
    for n in names:
        img = assets.image(("textures/mob_effect/%s.png" % n) or b"")
        imgs.append(img if img and img[0] <= 255 and img[1] <= 255 else None)
    cols = 16
    cell = max([18] + [max(i[0], i[1]) for i in imgs if i])
    aw = 1
    while aw < cols * cell:
        aw *= 2
    ah = 1
    while ah < ((len(names) + cols - 1) // cols) * cell:
        ah *= 2
    atlas = bytearray(aw * ah * 4)
    rows = []
    for k, (n, img) in enumerate(zip(names, imgs)):
        x0, y0 = (k % cols) * cell, (k // cols) * cell
        if not img:
            rows.append((n, 0, 0, 0, 0))
            continue
        w, h, px = img
        for yy in range(h):
            o = ((y0 + yy) * aw + x0) * 4
            atlas[o:o + w * 4] = px[yy * w * 4:(yy + 1) * w * 4]
        rows.append((n, x0, y0, w, h))
    with open(os.path.join(out_dir, "effects.bin"), "wb") as f:
        f.write(b"PNF1" + struct.pack("<HHH", len(rows), aw, ah))
        for n, x0, y0, w, h in rows:
            nb = n.encode()
            f.write(bytes([len(nb)]) + nb + struct.pack("<HHBB", x0, y0, w, h))
        f.write(bytes(atlas))
        push = {"PUSH_PULL": 0, "NORMAL": 0, "POPPED": 1, "DESTROY": 1, "IMMOVEABLE": 2, "BLOCK": 2,
                "IGNORE": 3, "PUSH": 4, "PUSH_ONLY": 4}
        f.write(struct.pack("<I", len(facts)))
        f.write(bytes(push.get(st.get("push"), 0) | (128 if st.get("block_entity") else 0) for st in facts))
    print("effects.bin: %d effect icons, atlas %dx%d" % (sum(1 for r in rows if r[3]), aw, ah))
# The default textures of entity types whose texture is not at entity/<type>.
ENTITY_TEX_OF = {
    "elder_guardian": "guardian_elder", "skeleton_horse": "horse/horse_skeleton",
    "zombie_horse": "horse/horse_zombie", "ender_dragon": "enderdragon/dragon",
    "mooshroom": "cow/mooshroom_red", "ocelot": "cat/ocelot", "trader_llama": "llama/llama_creamy",
    "llama": "llama/llama_creamy", "giant": "zombie/zombie", "armor_stand": "armorstand/wood",
    "arrow": "projectiles/arrow", "spectral_arrow": "projectiles/spectral_arrow",
    "polar_bear": "bear/polarbear", "magma_cube": "slime/magmacube", "zombified_piglin": "piglin/zombified_piglin",
    "piglin_brute": "piglin/piglin_brute", "wither_skeleton": "skeleton/wither_skeleton",
    "glow_squid": "squid/glow_squid", "cave_spider": "spider/cave_spider", "iron_golem": "iron_golem/iron_golem",
    "snow_golem": "snow_golem", "wolf": "wolf/wolf", "cat": "cat/cat_tabby", "horse": "horse/horse_brown",
    "fox": "fox/fox", "rabbit": "rabbit/rabbit_brown", "parrot": "parrot/parrot_red_blue", "bee": "bee/bee",
    "panda": "panda/panda", "frog": "frog/frog_temperate", "axolotl": "axolotl/axolotl_lucy",
    "tropical_fish": "fish/tropical_a", "cod": "fish/cod", "salmon": "fish/salmon", "pufferfish": "fish/pufferfish",
    "boat": "boat/oak", "chest_boat": "chest_boat/oak", "minecart": "minecart",
    "pig": "pig/pig_temperate", "cow": "cow/cow_temperate", "chicken": "chicken/chicken_temperate",
}
# Entity texture folders Penryn draws no entity with (block entities, items, effects).
ENTITY_TEX_SKIP = ("banner/", "shield/", "end_portal", "conduit/", "decorated_pot/", "cushion/", "bell/", "beacon",
                   "enchantment/", "chest/", "experience/", "fishing/", "signs/", "shulker/shulker_")
# AgeableMob baby dimensions (the classes' BABY_DIMENSIONS, or the adult's scaled): (w, h) or scale.
BABY_DIMS = {
    "pig": (0.45, 0.45), "cow": (0.45, 0.7), "mooshroom": (0.45, 0.7), "sheep": (0.45, 0.65), "chicken": (0.3, 0.4),
    "rabbit": (0.24, 0.4), "zombie_horse": 0.7, "skeleton_horse": 0.7, "horse": 0.7, "donkey": 0.5, "mule": 0.5,
    "llama": 0.5, "trader_llama": 0.5, "goat": (0.45, 0.65), "panda": 0.5, "polar_bear": (0.7, 0.7), "fox": 0.6,
    "wolf": (0.3, 0.425), "cat": (0.3, 0.35), "ocelot": (0.3, 0.35), "bee": 0.5, "turtle": 0.3, "armadillo": 0.6,
    "axolotl": (0.375, 0.21), "squid": (0.5, 0.5), "glow_squid": (0.5, 0.5), "dolphin": 0.65,
    "villager": (0.49, 0.98), "wandering_trader": 0.5, "zombie": (0.49, 0.98), "husk": (0.49, 0.98),
    "drowned": (0.49, 0.98), "zombie_villager": (0.49, 0.98), "camel": (0.95, 1.4), "happy_ghast": 0.2375,
    "sniffer": 0.5, "piglin": (0.49, 0.98), "zombified_piglin": (0.49, 0.98), "hoglin": (0.75, 0.85),
    "zoglin": (0.75, 0.85), "strider": (0.45, 0.85), "nautilus": 0.5,
}
# ClientboundSetEquipmentPacket's slots (EquipmentSlot order).
EQUIP_SLOTS = {"mainhand": 0, "offhand": 1, "feet": 2, "legs": 3, "chest": 4, "head": 5, "body": 6, "saddle": 7}
PLAYER_SKINS = ["alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri"]


def make_entities(res, assets, reg, out_dir):
    """entities.bin, "PNE3": every entity texture in one atlas (by name), every entity model
    layer (vanilla's ModelPart trees, exported by horizon-prepare, by layer name), each entity
    type's default model, texture, size and baby size, the player models and default skins,
    the villager types and professions, the equipment assets' layers, and each item's
    equipment slot, asset and third-person transform."""
    emodels = json.load(open(os.path.join(res, "entity-models.json")))
    edims = json.load(open(os.path.join(res, "entity-types.json")))
    items_json = json.load(open(os.path.join(res, "items.json")))
    etypes = [k for k, _ in sorted(reg["minecraft:entity_type"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])]
    items = [k for k, _ in sorted(reg["minecraft:item"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])]

    def ordered(name):
        return [k.split(":", 1)[1] for k, _ in sorted(reg.get(name, {}).get("entries", {}).items(),
                                                      key=lambda kv: kv[1]["protocol_id"])]

    # Every entity texture.
    ent_files = [x[len("assets/minecraft/textures/"):-4] for x in assets.names()
                 if x.startswith("assets/minecraft/textures/entity/") and x.endswith(".png")]
    ent_set = set(ent_files)
    sprites = {}
    for tex in sorted(ent_files):
        if any(tex[len("entity/"):].startswith(s) for s in ENTITY_TEX_SKIP):
            continue
        img = assets.image(("textures/" + tex + ".png"))
        if img:
            sprites[tex] = img

    def entity_texture(short):
        if short in ENTITY_TEX_OF and "entity/" + ENTITY_TEX_OF[short] in sprites:
            return "entity/" + ENTITY_TEX_OF[short]
        if short.endswith("_minecart") and "entity/minecart" in sprites:
            return "entity/minecart"
        for c in ("entity/%s" % short, "entity/%s/%s" % (short, short), "entity/%s/%s_temperate" % (short, short),
                  "entity/%s/temperate_%s" % (short, short)):
            if c in sprites:
                return c
        flat = short.replace("_", "")
        cands = [f for f in sprites if f.rsplit("/", 1)[-1] in (short, flat)]
        if cands:
            return sorted(cands, key=len)[0]
        cands = sorted(f for f in sprites if f.startswith("entity/%s/" % short))
        warm = [f for f in cands if "temperate" in f]
        return (warm or cands or [None])[0]

    # Every model layer, by name.
    ent_models, model_index = [], {}
    for key in sorted(emodels):
        parts = []

        def walk(name, part, parent):
            idx = len(parts)
            parts.append((name, parent, part.get("pose", [0] * 6 + [1] * 3), part.get("polygons", []), part.get("normals", [])))
            for cn, child in sorted(part.get("children", {}).items()):
                walk(cn, child, idx)

        walk("root", emodels[key], -1)
        model_index[key] = len(ent_models)
        ent_models.append((key, parts))

    rows = []
    for et in etypes:
        short = et.split(":", 1)[1]
        key = et + "#main"
        mi = model_index.get(key, -1) if short != "player" else -1
        tex = entity_texture(short) if mi >= 0 else None
        dims = edims.get(et, {})
        w, h = float(dims.get("width", 0.6)), float(dims.get("height", 1.8))
        b = BABY_DIMS.get(short)
        bw, bh = (w * b, h * b) if isinstance(b, float) else (b if b else (w, h))
        baby_index = 17 if short == "piglin" else (16 if b else 0)
        rows.append((mi, tex, w, h, bw, bh, baby_index, short))
    player_models = [model_index.get(k, -1) for k in ("minecraft:player_slim#main", "minecraft:player#main")]
    skin_paths = [t for t in ("entity/player/%s/%s" % (kind, n) for kind in ("slim", "wide") for n in PLAYER_SKINS) if t in sprites]

    # Equipment assets: their layers by layer type, (texture, colour when undyed).
    assets_list, asset_index = [], {}
    for it in items:
        eq = (items_json.get(it) or {}).get("minecraft:equippable") or {}
        aid = eq.get("asset_id")
        if not aid or aid in asset_index:
            continue
        d = assets.json("equipment/" + aid.split(":", 1)[1] + ".json") or {}
        layers = []
        for ltype, lst in sorted((d.get("layers") or {}).items()):
            out = []
            for l in lst:
                tex = "entity/equipment/%s/%s" % (ltype, l.get("texture", "").split(":", 1)[-1])
                dye = l.get("dyeable")
                undyed = 0
                if isinstance(dye, dict):
                    undyed = 0xFF000000 | ((dye.get("color_when_undyed", -1) & 0xFFFFFF) if "color_when_undyed" in dye else 0xFFFFFF)
                if tex in sprites:
                    out.append((tex, undyed))
            if out:
                layers.append((ltype, out))
        asset_index[aid] = len(assets_list)
        assets_list.append((aid.split(":", 1)[1], layers))

    # Each item's model display.thirdperson_righthand (ItemInHandLayer), as gui.bin's hands.
    def find_model(m, depth=0):
        if not isinstance(m, dict) or depth > 12:
            return None
        t = norm_type(m.get("type", "minecraft:model"))
        if t == "minecraft:model":
            return m.get("model")
        if t == "minecraft:special":
            # Drawn by a special renderer (shields, banners, heads, chests...): no flat model.
            return "special"
        for key in ("fallback", "on_false", "on_true", "model"):
            if isinstance(m.get(key), dict):
                r = find_model(m[key], depth + 1)
                if r:
                    return r
        for key in ("cases", "entries", "models"):
            for c in m.get(key, []) or []:
                r = find_model(c.get("model", c) if isinstance(c, dict) else c, depth + 1)
                if r:
                    return r
        return None

    shown = {}

    def third(ref):
        ref = nsref(ref)
        start = ref
        if start in shown:
            return shown[start]
        out = ([0, 0, 0], [0, 0, 0], [1, 1, 1])
        for _ in range(16):
            m = assets.json("models/" + ref + ".json") or {}
            g = m.get("display", {}).get("thirdperson_righthand")
            if g:
                out = (g.get("rotation", [0, 0, 0]), g.get("translation", [0, 0, 0]), g.get("scale", [1, 1, 1]))
                break
            if "parent" not in m:
                break
            ref = nsref(m["parent"])
        shown[start] = out
        return out

    item_rows = []
    for it in items:
        eq = (items_json.get(it) or {}).get("minecraft:equippable") or {}
        slot = EQUIP_SLOTS.get(eq.get("slot"), 0xFF)
        asset = asset_index.get(eq.get("asset_id"), 0xFFFF)
        d = assets.json("items/" + it.split(":", 1)[1] + ".json")
        ref = find_model(d.get("model")) if d else None
        if ref == "special":
            rot, tr, sc = [0, 0, 0], [0, 0, 0], [0, 0, 0]  # not drawn in a hand
        else:
            rot, tr, sc = third(ref) if ref else ([0, 0, 0], [0, 3, 1], [0.55, 0.55, 0.55])
        item_rows.append((slot, asset, list(rot) + list(tr) + list(sc)))

    # Shelf-pack the textures, tallest first.
    eorder = sorted(sprites, key=lambda k: (-sprites[k][1], k))
    ew, eplaced = 2048, {}
    x = y = shelf = 0
    for k in eorder:
        w, h, _ = sprites[k]
        if x + w > ew:
            x, y, shelf = 0, y + shelf, 0
        eplaced[k] = (x, y)
        x += w
        shelf = max(shelf, h)
    eh = 1
    while eh < y + shelf:
        eh *= 2
    eatlas = bytearray(ew * eh * 4)
    for k, (px0, py0) in eplaced.items():
        w, h, px = sprites[k]
        for yy in range(h):
            o = ((py0 + yy) * ew + px0) * 4
            eatlas[o:o + w * 4] = px[yy * w * 4:(yy + 1) * w * 4]

    def rect(tex):
        px0, py0 = eplaced[tex]
        w, h, _ = sprites[tex]
        return struct.pack("<HHHH", px0, py0, w, h)

    def sb(s):
        b = s.encode()[:255]
        return bytes([len(b)]) + b

    with open(os.path.join(out_dir, "entities.bin"), "wb") as f:
        f.write(b"PNE3" + struct.pack("<HH", ew, eh) + bytes(eatlas))
        f.write(struct.pack("<I", len(ent_models)))
        for key, parts in ent_models:
            kb = key.encode()
            f.write(bytes([len(kb)]) + kb + struct.pack("<I", len(parts)))
            for name, parent, pose, polys, normals in parts:
                nb = name.encode()
                f.write(bytes([len(nb)]) + nb + struct.pack("<h9f", parent, *[float(v) for v in (list(pose) + [1, 1, 1])[:9]]))
                f.write(struct.pack("<H", len(polys)))
                for i, poly in enumerate(polys):
                    for vtx in poly[:4]:
                        f.write(struct.pack("<5f", *[float(v) for v in vtx[:5]]))
                    nrm = normals[i] if i < len(normals) else [0, 0, 1]
                    f.write(struct.pack("<3f", *[float(v) for v in nrm]))
        f.write(struct.pack("<I", len(rows)))
        for mi, tex, w, h, bw, bh, bi, short in rows:
            f.write(struct.pack("<h", mi if tex else -1) + (rect(tex) if tex else bytes(8)))
            f.write(struct.pack("<ffffB", w, h, bw, bh, bi) + sb(short) + sb(tex or ""))
        f.write(struct.pack("<hhI", player_models[0], player_models[1], len(skin_paths)))
        for tex in skin_paths:
            f.write(rect(tex))
        f.write(struct.pack("<i", etypes.index("minecraft:player") if "minecraft:player" in etypes else -1))
        f.write(struct.pack("<I", len(eorder)))
        for tex in sorted(eorder):
            f.write(sb(tex) + rect(tex))
        # The villager types and professions, each with its texture's hat (VillagerMetadataSection:
        # 0 none, 1 partial, 2 full).
        for lst, folder in ((ordered("minecraft:villager_type"), "type"), (ordered("minecraft:villager_profession"), "profession")):
            f.write(struct.pack("<H", len(lst)))
            for n in lst:
                meta = assets.json("textures/entity/villager/%s/%s.png.mcmeta" % (folder, n)) or {}
                hat = {"partial": 1, "full": 2}.get((meta.get("villager") or {}).get("hat"), 0)
                f.write(sb(n) + bytes([hat]))
        f.write(struct.pack("<H", len(assets_list)))
        for name, layers in assets_list:
            f.write(sb(name) + bytes([len(layers)]))
            for ltype, lst in layers:
                f.write(sb(ltype) + bytes([len(lst)]))
                for tex, undyed in lst:
                    f.write(sb(tex) + struct.pack("<I", undyed))
        f.write(struct.pack("<I", len(item_rows)))
        for slot, asset, d in item_rows:
            f.write(struct.pack("<BH9f", slot, asset, *[float(v) for v in d]))
    print("entities.bin: %d types, %d with a model and texture, %d models, %d textures, %d equipment assets, atlas %dx%d"
          % (len(rows), sum(1 for r in rows if r[1] and r[0] >= 0), len(ent_models), len(sprites), len(assets_list), ew, eh))


def find_objects():
    """The asset index and objects folder of an installed 26.3 (Horizon's launcher, the
    official launcher, Prism...)."""
    home = os.path.expanduser("~")
    for base in (os.path.join(home, ".local/share/horizon-launcher/minecraft/assets"),
                 os.path.join(home, ".minecraft/assets"),
                 os.path.join(home, "Library/Application Support/minecraft/assets"),
                 os.path.join(home, "AppData/Roaming/.minecraft/assets")):
        idx = os.path.join(base, "indexes")
        if not os.path.isdir(idx):
            continue
        for n in sorted(os.listdir(idx), reverse=True):
            if n.endswith(".json"):
                return os.path.join(idx, n), os.path.join(base, "objects")
    return None, None


def make_sounds(res, out_dir, sound_names):
    """sounds.bin, "PNS1": the game's sound files (Ogg Vorbis, as they are) and its sound
    events (sounds.json), indexed by the sound-event registry ids the server sends.

        u32 files; per file: u16 name length, name, u64 offset, u32 size
        u32 events (registry order); per event: u16 entries; per entry: u32 file, f32 volume,
            f32 pitch, u16 weight, u8 stream, u8 attenuation distance
        u32 extra events by name (sounds.json events outside the registry, e.g. music):
            u16 name length, name, then the same entry list
        u32 registry names (without "minecraft:"), in id order
        then the file bytes, at their offsets (from the start of the file)."""
    index_path, objects = find_objects()
    prepared_index = os.path.join(res, "asset-index.json")
    if os.path.isfile(prepared_index):
        index_path = prepared_index
    if not index_path or not objects or not os.path.isdir(objects):
        print("sounds.bin: no installed game assets found: no sound")
        return
    idx = json.load(open(index_path))
    objs = idx.get("objects", idx)

    def obj_path(key):
        o = objs.get(key)
        if not o:
            return None
        p = os.path.join(objects, o["hash"][:2], o["hash"])
        return p if os.path.isfile(p) else None

    sj = obj_path("minecraft/sounds.json")
    if not sj:
        print("sounds.bin: sounds.json missing: no sound")
        return
    events = json.load(open(sj))
    files, file_index = [], {}

    def file_of(name):
        name = name.split(":", 1)[-1]
        if name in file_index:
            return file_index[name]
        p = obj_path("minecraft/sounds/" + name + ".ogg")
        if not p:
            return None
        file_index[name] = len(files)
        files.append((name, p))
        return file_index[name]

    def entries(ev, depth=0):
        out = []
        for e in (events.get(ev.split(":", 1)[-1]) or {}).get("sounds", []):
            if isinstance(e, str):
                e = {"name": e}
            if e.get("type") == "event":
                if depth < 4:
                    for sub in entries(e["name"], depth + 1):
                        out.append(sub)
                continue
            fi = file_of(e["name"])
            if fi is None:
                continue
            out.append((fi, float(e.get("volume", 1.0)), float(e.get("pitch", 1.0)), int(e.get("weight", 1)),
                        1 if e.get("stream") else 0, int(e.get("attenuation_distance", 16))))
        return out

    reg_entries = [entries(n) for n in sound_names]
    named = set(n.split(":", 1)[-1] for n in sound_names)
    extra = [(k, entries(k)) for k in sorted(events) if k not in named]
    head = bytearray()
    head += struct.pack("<I", len(files))
    sizes = [os.path.getsize(p) for _, p in files]
    # Offsets are filled once the header's length is known.
    file_rows_at = []
    for (name, _), size in zip(files, sizes):
        nb = name.encode()
        head += struct.pack("<H", len(nb)) + nb
        file_rows_at.append(len(head))
        head += struct.pack("<QI", 0, size)

    def ent_bytes(lst):
        b = struct.pack("<H", len(lst))
        for fi, vol, pit, w, st, att in lst:
            b += struct.pack("<IffHBB", fi, vol, pit, min(65535, w), st, min(255, att))
        return b

    head += struct.pack("<I", len(reg_entries))
    for lst in reg_entries:
        head += ent_bytes(lst)
    head += struct.pack("<I", len(extra))
    for name, lst in extra:
        nb = name.encode()
        head += struct.pack("<H", len(nb)) + nb + ent_bytes(lst)
    # The registry's names, so the client can play its own sounds by name.
    head += struct.pack("<I", len(sound_names))
    for n in sound_names:
        nb = n.split(":", 1)[-1].encode()
        head += struct.pack("<H", len(nb)) + nb
    base = 4 + len(head)
    off = base
    for at, size in zip(file_rows_at, sizes):
        head[at:at + 8] = struct.pack("<Q", off)
        off += size
    with open(os.path.join(out_dir, "sounds.bin"), "wb") as f:
        f.write(b"PNS1")
        f.write(head)
        for _, p in files:
            with open(p, "rb") as src:
                f.write(src.read())
    print("sounds.bin: %d files, %d events, %.0f MB" % (len(files), len(reg_entries) + len(extra), off / 1e6))


def sound_entries(events, sound_names, file_of):
    """Per registry sound event and per other sounds.json event: (file, volume, pitch, weight,
    stream, attenuation) entries, nested "event" entries followed as SoundManager does."""
    def entries(ev, depth=0):
        out = []
        key = ev[len("minecraft:"):] if ev.startswith("minecraft:") else ev
        for e in (events.get(key) or {}).get("sounds", []):
            if isinstance(e, str):
                e = {"name": e}
            if not isinstance(e, dict) or "name" not in e:
                continue
            if e.get("type") == "event":
                if depth < 4:
                    out.extend(entries(e["name"], depth + 1))
                continue
            fi = file_of(e["name"])
            if fi is None:
                continue
            out.append((fi, float(e.get("volume", 1.0)), float(e.get("pitch", 1.0)), int(e.get("weight", 1)),
                        1 if e.get("stream") else 0, int(e.get("attenuation_distance", 16))))
        return out

    reg_entries = [entries(n) for n in sound_names]
    named = set(n.split(":", 1)[-1] for n in sound_names)
    extra = [(k, entries(k)) for k in sorted(events) if k not in named]
    return reg_entries, extra


SOUND_OVERLAY = 0x80000000


def make_pack_sounds(res, assets, out_dir, sound_names):
    """packsounds.bin, "PNSO": the packs' sounds over sounds.bin, laid out as sounds.bin
    (make_sounds) but holding only the packs' own .ogg files; an entry's file with the top bit
    set is one of them, else sounds.bin's file of that index. The event tables are complete:
    the game's sounds.json with every pack's sounds.json merged in (an event with "replace"
    replaces, else its sounds are added), as SoundManager's preparations. Not written when no
    pack has sounds."""
    path = os.path.join(out_dir, "packsounds.bin")
    if os.path.exists(path):
        os.remove(path)
    pack_names = [n for pk in assets.packs for n in pk.names()]
    has_json = [n for n in pack_names if n.endswith("/sounds.json") and n.count("/") == 2]
    has_ogg = any(n.endswith(".ogg") and "/sounds/" in n for n in pack_names)
    if not has_json and not has_ogg:
        return
    index_path, objects = find_objects()
    prepared_index = os.path.join(res, "asset-index.json")
    if os.path.isfile(prepared_index):
        index_path = prepared_index
    if not index_path or not objects:
        print("packsounds.bin: no installed game sounds: skipped")
        return
    objs = json.load(open(index_path)).get("objects", {})

    def obj_path(key):
        o = objs.get(key)
        if not o:
            return None
        q = os.path.join(objects, o["hash"][:2], o["hash"])
        return q if os.path.isfile(q) else None

    sj = obj_path("minecraft/sounds.json")
    base_events = json.load(open(sj)) if sj else {}
    # sounds.bin's file numbering, made again the way make_sounds made it.
    base_index = {}

    def base_file(name):
        name = name.split(":", 1)[-1]
        if name in base_index:
            return base_index[name]
        if not obj_path("minecraft/sounds/" + name + ".ogg"):
            return None
        base_index[name] = len(base_index)
        return base_index[name]

    sound_entries(base_events, sound_names, base_file)
    events = {k: dict(v) for k, v in base_events.items()}
    for full in sorted(set(has_json)):
        ns = full.split("/")[1]
        for d in assets.json_stack(ns + ":sounds.json" if ns != "minecraft" else "sounds.json", packs_only=True):
            if not isinstance(d, dict):
                continue
            for k, v in d.items():
                if not isinstance(v, dict):
                    continue
                key = k if ns == "minecraft" else ns + ":" + k
                sounds = [dict(e, name=(e["name"] if ":" in e["name"] or ns == "minecraft" else ns + ":" + e["name"]))
                          if isinstance(e, dict) and "name" in e else
                          (e if not isinstance(e, str) or ":" in e or ns == "minecraft" else ns + ":" + e)
                          for e in v.get("sounds", [])]
                if v.get("replace") or key not in events:
                    events[key] = dict(v, sounds=sounds)
                else:
                    events[key] = dict(events[key], sounds=list(events[key].get("sounds", [])) + sounds)
    files, file_index = [], {}

    def file_of(name):
        rel = "sounds/" + nsref(name) + ".ogg" if ":" not in nsref(name) else \
            nsref(name).split(":", 1)[0] + ":sounds/" + nsref(name).split(":", 1)[1] + ".ogg"
        if rel in file_index:
            return file_index[rel]
        own = assets.from_pack(rel)
        if own is not None:
            file_index[rel] = len(files) | SOUND_OVERLAY
            files.append((nsref(name), own))
            return file_index[rel]
        if ":" in nsref(name):
            return None
        return base_index.get(nsref(name))

    reg_entries, extra = sound_entries(events, sound_names, file_of)
    head = bytearray()
    head += struct.pack("<I", len(files))
    rows = []
    for name, data in files:
        nb = name.encode()
        head += struct.pack("<H", len(nb)) + nb
        rows.append(len(head))
        head += struct.pack("<QI", 0, len(data))

    def ent_bytes(lst):
        b = struct.pack("<H", len(lst))
        for fi, vol, pit, w, st, att in lst:
            b += struct.pack("<IffHBB", fi, vol, pit, min(65535, w), st, min(255, att))
        return b

    head += struct.pack("<I", len(reg_entries))
    for lst in reg_entries:
        head += ent_bytes(lst)
    head += struct.pack("<I", len(extra))
    for name, lst in extra:
        nb = name.encode()
        head += struct.pack("<H", len(nb)) + nb + ent_bytes(lst)
    head += struct.pack("<I", len(sound_names))
    for n in sound_names:
        nb = n.split(":", 1)[-1].encode()
        head += struct.pack("<H", len(nb)) + nb
    off = 4 + len(head)
    for at, (_, data) in zip(rows, files):
        head[at:at + 8] = struct.pack("<Q", off)
        off += len(data)
    with open(path, "wb") as f:
        f.write(b"PNSO")
        f.write(head)
        for _, data in files:
            f.write(data)
    print("packsounds.bin: %d sounds of the packs, %d events" % (len(files), len(reg_entries) + len(extra)))


if __name__ == "__main__":
    main()
