#!/usr/bin/env python3
"""Generate the one pixel face from a pinned open bitmap font, and write its
licenses beside it: crates/pixtuoid-scene/fonts/world.bin, every text the
cutaway and the floating window draw, from Fusion Pixel 8px; and the site's
copy of it, site/src/assets/fonts/, which the live office's captions draw in.

Usage: python3 scripts/gen-fonts.py   (just gen-fonts)

Fusion Pixel 8px is the one open pixel font whose CJK fits a terminal cell's
art (an 8x8 box); it has no Cyrillic or Greek, which draw as tofu. A glyph is
baseline-aligned into the face's line box, which `text.rs` and `grid.rs`
paint, and one whose ink leaves the box or the cells `unicode-width` gives it
is dropped, not cropped.

The format, which `cutaway::text` reads: one byte, the rows per glyph; a u16
LE glyph count n; n u16 LE code points, ascending; then n glyphs, a byte a row
from the top, the high bit the leftmost pixel. A symbol Fusion Pixel draws only
full-width fails the width check and is drawn by hand in `text.rs`.
"""

import hashlib
import io
import sys
import unicodedata
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FONTS = ROOT / "crates/pixtuoid-scene/fonts"
OUT = FONTS / "world.bin"
SITE_FONTS = ROOT / "site/src/assets/fonts"

FETCH_TIMEOUT_S = 30

FUSION_TAG = "2026.09.25"
FUSION_URL = (
    "https://github.com/TakWolf/fusion-pixel-font/releases/download/"
    f"{FUSION_TAG}/fusion-pixel-font-8px-monospaced-bdf-v{FUSION_TAG}.zip"
)
FUSION_SHA256 = "bd76834c43d6882184356394cfdd8c9c5e5623d9d1c9020564fe545b7ba769ce"
# Latin's half-width punctuation (`·`) first; then Simplified Chinese
# forms, since a language variant redraws only shared ideographs.
FUSION_MEMBERS = (
    "fusion-pixel-8px-monospaced-latin.bdf",
    "fusion-pixel-8px-monospaced-zh_hans.bdf",
)
# The font's license and its sources' licenses, as its release ships them.
FUSION_LICENSES = {"OFL.txt": "OFL-Fusion-Pixel.txt", "LICENSES/": "LICENSES-Fusion-Pixel/"}
WEB_URL = FUSION_URL.replace("bdf", "otf.woff2")
WEB_SHA256 = "5a2e0c345d83124a962772ecc7f04748bea14978f02ac1474d5fd38a00f3de56"
# Latin, for the half-width `·` a badge's `cc·<id4>` writes.
WEB_MEMBER = "fusion-pixel-8px-monospaced-latin.otf.woff2"
WEB_OUT = SITE_FONTS / "fusion-pixel-8px.woff2"
# `text.rs`'s LINE_H and the art pixels a cell is wide (its ADVANCE).
LINE_H = 8
CELL_W = 4
# Rows down to and including the baseline: Fusion Pixel's own, capitals
# five rows under two of accent room.
ASCENT = 7

RANGES = [
    (0x0020, 0x007E),  # ASCII
    (0x00A0, 0x024F),  # Latin-1, Latin Extended-A and -B
    (0x0370, 0x04FF),  # Greek, Cyrillic
    (0x2000, 0x206F),  # General Punctuation
    (0x3000, 0x30FF),  # CJK punctuation, Hiragana, Katakana
    (0x4E00, 0x9FFF),  # CJK Unified Ideographs
    (0xAC00, 0xD7A3),  # Hangul Syllables, narrowed to KS X 1001 below
    (0xFF00, 0xFFEF),  # Halfwidth and Fullwidth Forms
]


def fetch(url, sha256):
    with urllib.request.urlopen(url, timeout=FETCH_TIMEOUT_S) as r:
        data = r.read()
    got = hashlib.sha256(data).hexdigest()
    if got != sha256:
        sys.exit(f"{url}: sha256 {got}, pinned {sha256}")
    return data


def parse_bdf(text):
    """{code point: (advance, ink as (row, col)s, row 0 the first under the baseline)}"""
    glyphs = {}
    ascent = None
    cp = advance = bbx = bitmap = None
    for line in text.splitlines():
        p = line.split()
        if not p:
            continue
        if p[0] == "FONT_ASCENT":
            ascent = int(p[1])
        elif p[0] == "ENCODING":
            cp = int(p[1])
        elif p[0] == "DWIDTH":
            advance = int(p[1])
        elif p[0] == "BBX":
            bbx = [int(v) for v in p[1:5]]
        elif p[0] == "BITMAP":
            bitmap = []
        elif p[0] == "ENDCHAR":
            w, h, xoff, yoff = bbx
            top = ascent - (h + yoff)
            ink = set()
            for r, row in enumerate(bitmap):
                bits, nbits = int(row, 16), len(row) * 4
                for c in range(w):
                    if bits >> (nbits - 1 - c) & 1:
                        ink.add((top + r - ascent, xoff + c))
            glyphs[cp] = (advance, ink)
            bitmap = None
        elif bitmap is not None:
            bitmap.append(p[0])
    return glyphs


def cells(cp):
    return 2 if unicodedata.east_asian_width(chr(cp)) in ("W", "F") else 1


def wanted(cp):
    if unicodedata.category(chr(cp)) in ("Cc", "Cf", "Cn", "Co", "Cs", "Mn", "Me", "Zl", "Zp"):
        return False
    # KS X 1001's 2,350 syllables: EUC-KR's two-byte set, the repertoire a
    # Korean bitmap font ships; the other 8,822 would quadruple the Hangul.
    if 0xAC00 <= cp <= 0xD7A3:
        # CPython's euc_kr spells any other syllable as an 8-byte KS X 1001
        # Annex 3 make-up sequence instead of raising.
        return len(chr(cp).encode("euc_kr")) == 2
    return True


def rows_of(advance, ink, n_cells, cell_w=CELL_W, line_h=LINE_H, ascent=ASCENT, bits=8):
    """The glyph as `line_h` rows of `bits`-bit ink, the high bit leftmost,
    or None when it leaves its box."""
    if advance != n_cells * cell_w:
        return None
    rows = [0] * line_h
    for dy, x in ink:
        y = dy + ascent
        if not (0 <= y < line_h and 0 <= x < n_cells * cell_w):
            return None
        rows[y] |= (1 << (bits - 1)) >> x
    return rows


def write(path, rows_per_glyph, picked, row_bytes):
    order = sorted(picked)
    out = bytearray([rows_per_glyph])
    out += len(order).to_bytes(2, "little")
    for cp in order:
        out += cp.to_bytes(2, "little")
    for cp in order:
        for row in picked[cp]:
            out += row.to_bytes(row_bytes, "little")
    path.write_bytes(out)
    print(f"{path.relative_to(ROOT)}: {len(order)} glyphs, {len(out)} bytes")


def unzip(url, sha256, into):
    """The pinned zip at `url`, its licenses written under `into`."""
    z = zipfile.ZipFile(io.BytesIO(fetch(url, sha256)))
    for name in z.namelist():
        for prefix, dest in FUSION_LICENSES.items():
            if name.startswith(prefix) and not name.endswith("/"):
                path = into / (dest + name[len(prefix):])
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(z.read(name))
    return z


def main():
    FONTS.mkdir(parents=True, exist_ok=True)
    bdfs = unzip(FUSION_URL, FUSION_SHA256, FONTS)
    fusion = [parse_bdf(bdfs.read(m).decode("utf-8")) for m in FUSION_MEMBERS]

    picked = {}
    for lo, hi in RANGES:
        for cp in range(lo, hi + 1):
            if not wanted(cp):
                continue
            for source in fusion:
                if cp in source:
                    rows = rows_of(*source[cp], cells(cp))
                    if rows is not None:
                        picked[cp] = rows
                        break

    write(OUT, LINE_H, picked, 1)

    WEB_OUT.write_bytes(unzip(WEB_URL, WEB_SHA256, SITE_FONTS).read(WEB_MEMBER))
    print(f"{WEB_OUT.relative_to(ROOT)}: {WEB_OUT.stat().st_size} bytes")


if __name__ == "__main__":
    main()
