#!/usr/bin/env python3
"""Render the README's banner: the office's neon sign, with the cat asleep on it and a coworker holding a coffee.

The figures are the cutaway's own art as the office draws it, read from the
golden `character.rs`'s `the_readme_banner_art_is_the_office_s_own` writes; the
sign's colours are the theme copies `readme_chart_palette.rs` pins. The image is
static, so it is committed under docs/images, and `--check` holds it to its
inputs.

Usage: `gen-banner.py OUT_DIR` writes `banner-{light,dark}.svg`; `--check OUT_DIR`
fails when either differs from a fresh render; `--selftest` runs the tests.
"""

from __future__ import annotations

import pathlib
import sys
import unittest

from readme_pixels import (
    BASELINE,
    THEMES,
    Frame,
    Pack,
    _run,
    animation,
    sprite_path,
    sprite_size,
    text_path,
    text_width,
)

ART_PATH = (
    pathlib.Path(__file__).resolve().parent.parent
    / "crates"
    / "pixtuoid-scene"
    / "src"
    / "character"
    / "banner.golden"
)
WORD, WORD_PX = "pixtuoid", 6
# One sign pixel: the sign's frame, its padding and the air round the picture.
PX = 4
# The @4x art's pixels, doubled so the figures stand level with the sign.
SPRITE_PX = 2
# Sign pixels of board round the word, across and down.
SIGN_PAD = (6, 4)
# The cat's own pixels sunk into the sign's top frame, and the sign pixels
# between its tail and the sign's right edge.
CAT_SINK, CAT_INSET = 2, 4
# Sign pixels between the sign and the coworker, and round the whole picture.
GAP, MARGIN = 6, 2
HALO_OPACITY = 0.22
# The neon's light round the coworker and the cat, one opacity per ring outward.
# Only a dark page swallows the art's dark outline, so only it gets the light.
GLOW = {"light": (), "dark": (0.40, 0.16)}
N4 = ((1, 0), (-1, 0), (0, 1), (0, -1))


def read_art(text: str) -> Pack:
    """The golden's `@palette`, then each `@frame <name> <frame_ms>` and its rows, one animation per name."""
    palette: dict[str, str] = {}
    animations: dict[str, tuple[list, int]] = {}
    rows: list[list[str]] = []
    for line in text.splitlines():
        if line.startswith("@palette "):
            palette = dict(kv.split("=") for kv in line.split()[1:])
        elif line.startswith("@frame "):
            _, name, ms = line.split()
            rows = []
            animations.setdefault(name, ([], int(ms)))[0].append(rows)
        elif line.strip():
            rows.append(line.split())
    return Pack(palette, animations)


def glowing(frame: Frame, rings: int, rows: int | None = None) -> Frame:
    """`frame` padded by `rings`, each empty cell `i + 1` N4 steps from the art keyed `glow<i>`, in the padded frame's first `rows` only."""
    w, h = len(frame[0]) + 2 * rings, len(frame) + 2 * rings
    out = [["."] * w for _ in range(h)]
    for y, row in enumerate(frame):
        for x, key in enumerate(row):
            out[y + rings][x + rings] = key
    front = [(x, y) for y in range(h) for x in range(w) if out[y][x] != "."]
    for i in range(rings):
        front = sorted(
            {
                (x + dx, y + dy)
                for x, y in front
                for dx, dy in N4
                if 0 <= x + dx < w
                and 0 <= y + dy < (h if rows is None else rows)
                and out[y + dy][x + dx] == "."
            }
        )
        for x, y in front:
            out[y][x] = f"glow{i}"
    return out


def render_svg(theme: str) -> str:
    pal = THEMES[theme]
    art = read_art(ART_PATH.read_text(encoding="utf-8"))
    glow = GLOW[theme]
    n, pad = len(glow), len(glow) * SPRITE_PX
    lit = {
        **art.palette,
        **{
            f"glow{i}": f"{pal.neon_brand}{round(a * 255):02x}"
            for i, a in enumerate(glow)
        },
    }
    gw, gh = sprite_size(art, "coworker", SPRITE_PX)
    cw, ch = sprite_size(art, "cat", SPRITE_PX)
    board_w = text_width(WORD, WORD_PX) + 2 * SIGN_PAD[0] * PX
    board_h = BASELINE * WORD_PX + 2 * SIGN_PAD[1] * PX
    cat_above = ch - CAT_SINK * SPRITE_PX
    content_h = max(board_h + 2 * PX + cat_above, gh)
    width = 2 * MARGIN * PX + board_w + 2 * PX + GAP * PX + gw
    height = 2 * MARGIN * PX + content_h
    floor = height - MARGIN * PX
    bx, by = MARGIN * PX + PX, floor - PX - board_h
    tx, ty = bx + SIGN_PAD[0] * PX, by + SIGN_PAD[1] * PX
    halo = "".join(
        text_path(WORD, tx + dx * PX, ty + dy * PX, WORD_PX) for dx, dy in N4
    )
    # The cat's light stops at the sign's frame, which it lies on.
    cat_rows = n + ch // SPRITE_PX - CAT_SINK
    cats = Pack(
        lit,
        {
            "cat": (
                [glowing(f, n, cat_rows) for f in art.animations["cat"][0]],
                art.animations["cat"][1],
            )
        },
    )
    cat, cat_css = animation(
        cats,
        "cat",
        bx + board_w - cw - CAT_INSET * PX - pad,
        by - PX + CAT_SINK * SPRITE_PX + pad,
        SPRITE_PX,
    )
    css = "".join(
        [
            ".neon{animation:fl 6s steps(1,end) infinite}@keyframes fl{0%,89%,93%,100%{opacity:1}90%,92%{opacity:.35}}",
            "@media (prefers-reduced-motion:reduce){.neon{animation:none}}",
            *cat_css,
        ]
    )
    return (
        "\n".join(
            [
                f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" shape-rendering="crispEdges" role="img" aria-labelledby="t">',
                f'<title id="t">{WORD}: a neon office sign, a cat asleep on it and a coworker holding a coffee</title>',
                f"<style>{css}</style>",
                f'<path fill="{pal.trim}" d="{_run(bx - PX, by - PX, board_w + 2 * PX, board_h + 2 * PX)}"/>',
                f'<path fill="{pal.neon_panel}" d="{_run(bx, by, board_w, board_h)}"/>',
                f'<g class="neon"><path fill="{pal.neon_brand}" fill-opacity="{HALO_OPACITY}" d="{halo}"/><path fill="{pal.neon_brand}" d="{text_path(WORD, tx, ty, WORD_PX)}"/></g>',
                cat,
                sprite_path(
                    glowing(art.animations["coworker"][0][0], n),
                    bx + board_w + PX + GAP * PX - pad,
                    floor - gh - pad,
                    SPRITE_PX,
                    lit,
                ),
                "</svg>",
            ]
        )
        + "\n"
    )


def write_banners(out_dir: pathlib.Path) -> list[pathlib.Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    paths = []
    for theme in THEMES:
        p = out_dir / f"banner-{theme}.svg"
        p.write_text(render_svg(theme), encoding="utf-8")
        paths.append(p)
    return paths


def stale(out_dir: pathlib.Path) -> list[str]:
    """The committed banners that differ from a fresh render."""
    return [
        f"banner-{t}.svg"
        for t in THEMES
        if not (out_dir / f"banner-{t}.svg").is_file()
        or (out_dir / f"banner-{t}.svg").read_text(encoding="utf-8") != render_svg(t)
    ]


class SelfTest(unittest.TestCase):
    def test_read_art_keeps_each_animation_s_frames_in_order(self) -> None:
        art = read_art(
            "@palette a=#010203 b=#040506\n@frame cat 750\n. a\n@frame guy 600\nb b\n@frame cat 750\na .\n"
        )
        self.assertTrue(
            art.palette == {"a": "#010203", "b": "#040506"}, f"palette: {art.palette}"
        )
        self.assertTrue(
            art.animations
            == {
                "cat": ([[[".", "a"]], [["a", "."]]], 750),
                "guy": ([[["b", "b"]]], 600),
            },
            f"animations: {art.animations}",
        )

    def test_glowing_rings_the_art_ring_by_ring_above_the_cut(self) -> None:
        got = glowing([["a"]], 2)
        self.assertTrue(
            len(got) == 5 and len(got[0]) == 5 and got[2][2] == "a",
            f"padded by two rings round the art: {got}",
        )
        self.assertTrue(
            got[1][2] == got[2][3] == "glow0"
            and got[1][1] == got[0][2] == "glow1"
            and got[0][0] == ".",
            f"rings by N4 steps: {got}",
        )
        cut = glowing([["a"]], 2, rows=3)
        self.assertTrue(
            cut[3][2] == cut[4][2] == "." and cut[2][1] == "glow0",
            f"no light past `rows`: {cut}",
        )
        self.assertTrue(
            glowing([["a", "b"]], 0) == [["a", "b"]],
            "no rings leave the frame as it is",
        )

    def test_only_the_dark_banner_glows(self) -> None:
        import re

        for theme in THEMES:
            lit = re.findall(
                rf'fill="{THEMES[theme].neon_brand}([0-9a-f]{{2}})"', render_svg(theme)
            )
            want = [f"{round(a * 255):02x}" for a in GLOW[theme]]
            self.assertTrue(
                sorted(set(lit)) == sorted(want),
                f"{theme}: the figures glow only where GLOW says: {sorted(set(lit))}",
            )

    def test_stale_flags_an_edited_or_missing_banner_and_passes_fresh_ones(
        self,
    ) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = pathlib.Path(tmp)
            edited, missing = write_banners(out)
            self.assertTrue(
                stale(out) == [], f"fresh banners are not stale: {stale(out)}"
            )
            edited.write_text(
                edited.read_text(encoding="utf-8") + " ", encoding="utf-8"
            )
            missing.unlink()
            self.assertTrue(
                stale(out) == [edited.name, missing.name],
                f"an edited and a missing banner are both stale: {stale(out)}",
            )

    def test_every_banner_pixel_has_a_colour(self) -> None:
        for theme in THEMES:
            svg = render_svg(theme)
            self.assertTrue(
                'fill="None"' not in svg and 'fill=""' not in svg,
                f"{theme}: a pixel without a colour",
            )

    def test_render_is_well_formed_animated_and_deterministic(self) -> None:
        import xml.etree.ElementTree as ET

        for theme in THEMES:
            svg = render_svg(theme)
            root = ET.fromstring(svg)
            title = root.find("{http://www.w3.org/2000/svg}title")
            self.assertTrue(
                title is not None and "pixtuoid" in (title.text or ""),
                f"{theme}: the title must name pixtuoid",
            )
            self.assertTrue(
                'class="neon"' in svg
                and 'class="cat"' in svg
                and "prefers-reduced-motion" in svg,
                f"{theme}: the sign flickers, the cat sleeps and motion can be reduced",
            )
            self.assertTrue(
                svg == render_svg(theme), f"{theme}: render must be deterministic"
            )

    def test_render_paints_inside_the_canvas(self) -> None:
        import re

        for theme in THEMES:
            svg = render_svg(theme)
            w, h = (
                int(v) for v in re.search(r'viewBox="0 0 (\d+) (\d+)"', svg).groups()
            )
            runs = [
                tuple(map(int, r))
                for r in re.findall(r"M(-?\d+) (-?\d+)h(\d+)v(\d+)h-\d+z", svg)
            ]
            outside = [
                r
                for r in runs
                if r[0] < 0 or r[1] < 0 or r[0] + r[2] > w or r[1] + r[3] > h
            ]
            self.assertTrue(
                bool(runs) and not outside,
                f"{theme}: runs leave the {w}x{h} canvas: {outside[:3]}",
            )


def main(argv: list[str]) -> int:
    if "--selftest" in argv:
        unittest.main(argv=sys.argv[:1])
    check_mode = "--check" in argv
    args = [a for a in argv if a != "--check"]
    if len(args) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    out_dir = pathlib.Path(args[0])
    if check_mode:
        bad = stale(out_dir)
        for name in bad:
            print(
                f"error: {out_dir / name} is stale; run `just gen-banner`",
                file=sys.stderr,
            )
        return 1 if bad else 0
    for p in write_banners(out_dir):
        print(p)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
