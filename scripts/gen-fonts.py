#!/usr/bin/env python3
"""Generate the one pixel face from a pinned open bitmap font, and write its
licenses beside it: crates/pixtuoid-scene/fonts/fusion-pixel-8px.bin, every text the
cutaway and the floating window draw, from Fusion Pixel 8px; and the site's
copy of its Latin, site/src/assets/fonts/, the live office's captions' face.

Usage: .venv/bin/python3 scripts/gen-fonts.py   (just gen-fonts)

Fusion Pixel 8px is the one open pixel font whose CJK fits a terminal cell's
art (an 8x8 box); it has no Cyrillic or Greek, which draw as tofu bar the
`Σ` `cutaway/text.rs` draws by hand. A glyph is
baseline-aligned into the face's line box, which `cutaway/text.rs` and `grid.rs`
paint, and one whose ink leaves the box or the cells `unicode-width` gives it
is dropped, not cropped.

The format, which `cutaway::text` reads: one byte, the rows per glyph; a u16
LE glyph count n; n u16 LE code points, ascending; then n glyphs, a byte a row
from the top, the high bit the leftmost pixel. A symbol Fusion Pixel draws only
full-width fails the width check and is drawn by hand in `cutaway/text.rs`.
"""

import hashlib
import io
import sys
import unicodedata
import urllib.request
import zipfile
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
FONTS = ROOT / "crates/pixtuoid-scene/fonts"
OUT = FONTS / "fusion-pixel-8px.bin"
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
FUSION_LICENSES = {
    "OFL.txt": "OFL-Fusion-Pixel.txt",
    "LICENSES/": "LICENSES-Fusion-Pixel/",
}
SITE_URL = FUSION_URL.replace("bdf", "otf.woff2")
SITE_SHA256 = "5a2e0c345d83124a962772ecc7f04748bea14978f02ac1474d5fd38a00f3de56"
# Latin, for the half-width `·` a badge's `cc·<id4>` writes.
SITE_MEMBER = "fusion-pixel-8px-monospaced-latin.otf.woff2"
SITE_OUT = SITE_FONTS / "fusion-pixel-8px.woff2"
# What the captions write: Latin, as the site's other faces load, and the
# board's symbols; the whole face is past the site's Lighthouse budgets.
SITE_RANGES = [(0x0020, 0x007E), (0x00A0, 0x017F), (0x2000, 0x206F), (0x2190, 0x26FF)]
# `display/text.rs`'s LINE_H and the art pixels a cell is wide (its ADVANCE).
LINE_H = 8
CELL_W = 4
# Rows down to and including the baseline: Fusion Pixel's own, `display/text.rs`'s
# ACCENT_ROWS + CAP_H.
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
    """{code point: ink as (row, col)s, row 0 the first under the baseline}"""
    glyphs = {}
    ascent = None
    cp = bbx = bitmap = None
    for line in text.splitlines():
        p = line.split()
        if not p:
            continue
        if p[0] == "FONT_ASCENT":
            ascent = int(p[1])
        elif p[0] == "ENCODING":
            cp = int(p[1])
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
            glyphs[cp] = ink
            bitmap = None
        elif bitmap is not None:
            bitmap.append(p[0])
    return glyphs


def cells(cp):
    return 2 if unicodedata.east_asian_width(chr(cp)) in ("W", "F") else 1


def wanted(cp):
    if unicodedata.category(chr(cp)) in (
        "Cc",
        "Cf",
        "Cn",
        "Co",
        "Cs",
        "Mn",
        "Me",
        "Zl",
        "Zp",
    ):
        return False
    # KS X 1001's 2,350 syllables: EUC-KR's two-byte set, the repertoire a
    # Korean bitmap font ships; the other 8,822 would quadruple the Hangul.
    if 0xAC00 <= cp <= 0xD7A3:
        # CPython's euc_kr spells any other syllable as an 8-byte KS X 1001
        # Annex 3 make-up sequence instead of raising.
        return len(chr(cp).encode("euc_kr")) == 2
    return True


def rows_of(ink, n_cells):
    """The glyph as LINE_H rows of 8-bit ink, the high bit leftmost, or None
    when it leaves its box. Its advance is not read: the font sets a quote or
    a `«` wider than its ink, which still fits the cell."""
    rows = [0] * LINE_H
    for dy, x in ink:
        y = dy + ASCENT
        if not (0 <= y < LINE_H and 0 <= x < n_cells * CELL_W):
            return None
        rows[y] |= 0x80 >> x
    return rows


def write(path, picked):
    order = sorted(picked)
    out = bytearray([LINE_H])
    out += len(order).to_bytes(2, "little")
    for cp in order:
        out += cp.to_bytes(2, "little")
    for cp in order:
        out += bytes(picked[cp])
    path.write_bytes(out)
    print(f"{path.relative_to(ROOT)}: {len(order)} glyphs, {len(out)} bytes")


def unzip(url, sha256, into):
    """The pinned zip at `url`, its licenses written under `into`."""
    z = zipfile.ZipFile(io.BytesIO(fetch(url, sha256)))
    for name in z.namelist():
        for prefix, dest in FUSION_LICENSES.items():
            if name.startswith(prefix) and not name.endswith("/"):
                path = into / (dest + name[len(prefix) :])
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
                    rows = rows_of(source[cp], cells(cp))
                    if rows is not None:
                        picked[cp] = rows
                        break

    write(OUT, picked)

    # The release's own timestamp, so a rerun writes the same bytes.
    face = TTFont(
        io.BytesIO(unzip(SITE_URL, SITE_SHA256, SITE_FONTS).read(SITE_MEMBER)),
        recalcTimestamp=False,
    )
    # Every name kept: the copyright and license notice the OFL requires.
    latin = subset.Subsetter(
        subset.Options(flavor="woff2", name_IDs=["*"], name_languages=["*"])
    )
    latin.populate(unicodes=[cp for lo, hi in SITE_RANGES for cp in range(lo, hi + 1)])
    latin.subset(face)
    face.save(SITE_OUT)
    print(f"{SITE_OUT.relative_to(ROOT)}: {SITE_OUT.stat().st_size} bytes")


if __name__ == "__main__":
    main()
