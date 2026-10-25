"""The README's pixel art kit: the office palette, a dot-matrix font and the sprite pack, drawn as SVG path runs.

Shared by `star-history.py` (the star chart), `bmc-button.py` (the Buy Me a
Coffee button), `gen-banner.py` (the README banner) and `gen-pix-icons.py` (the
README's pix icons); their selftests exercise it, and `run_selftest` runs the first three's.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys
import tomllib
import traceback
from typing import NamedTuple

PACK_DIR = pathlib.Path(__file__).resolve().parent.parent / "crates" / "pixtuoid-scene" / "sprites" / "default"


class Palette(NamedTuple):
    """The office roles the README's pixel art paints with; its `<picture>` picks light or dark."""

    wall: str
    trim: str
    carpet_light: str
    carpet_dark: str
    title: str
    text: str
    star: str
    window_frame: str
    sky_top: str
    sky_horizon: str
    building_dark: str
    building_light: str
    city_dark_window: str
    city_lit_a: str
    city_lit_b: str
    city_lit_c: str
    moon: str
    neon_panel: str
    neon_brand: str


# Copied from the themes, not derived — the renderer is Python. The copy's guard
# is `crates/pixtuoid-scene/tests/readme_chart_palette.rs`, which pins every
# value to its theme by struct access, so a theme edit fails `just test`.
PALETTE_PATH = pathlib.Path(__file__).with_name("readme-palette.json")
THEMES: dict[str, Palette] = {
    variant: Palette(**{f: row[f] for f in Palette._fields})
    for variant, row in json.loads(PALETTE_PATH.read_text(encoding="utf-8")).items()
}


# Forms are BASELINE rows tall on a GLYPH_H-row cell; the spare rows hold g j p q y's descenders.
GLYPH_W, GLYPH_H, BASELINE = 5, 9, 7
GLYPH_ADVANCE = GLYPH_W + 1

# Classic dot-matrix forms. Drawn as rectangles so the art
# needs no font: an SVG inside a GitHub `<img>` can't fetch a webfont, and a
# system font wouldn't be pixel art.
_FORMS: dict[str, tuple[str, ...]] = {
    "0": (".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."),
    "1": ("..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."),
    "2": (".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"),
    "3": ("#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###."),
    "4": ("...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."),
    "5": ("#####", "#....", "####.", "....#", "....#", "#...#", ".###."),
    "6": ("..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."),
    "7": ("#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."),
    "8": (".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."),
    "9": (".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."),
    "A": (".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"),
    "B": ("####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."),
    "C": (".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."),
    "D": ("###..", "#..#.", "#...#", "#...#", "#...#", "#..#.", "###.."),
    "E": ("#####", "#....", "#....", "####.", "#....", "#....", "#####"),
    "F": ("#####", "#....", "#....", "####.", "#....", "#....", "#...."),
    "G": (".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".####"),
    "H": ("#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"),
    "I": (".###.", "..#..", "..#..", "..#..", "..#..", "..#..", ".###."),
    "J": ("..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##.."),
    "K": ("#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"),
    "L": ("#....", "#....", "#....", "#....", "#....", "#....", "#####"),
    "M": ("#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#"),
    "N": ("#...#", "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#"),
    "O": (".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."),
    "P": ("####.", "#...#", "#...#", "####.", "#....", "#....", "#...."),
    "Q": (".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#"),
    "R": ("####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"),
    "S": (".####", "#....", "#....", ".###.", "....#", "....#", "####."),
    "T": ("#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."),
    "U": ("#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."),
    "V": ("#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.."),
    "W": ("#...#", "#...#", "#...#", "#.#.#", "#.#.#", "#.#.#", ".#.#."),
    "X": ("#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"),
    "Y": ("#...#", "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#.."),
    "Z": ("#####", "....#", "...#.", "..#..", ".#...", "#....", "#####"),
    "a": (".....", ".....", ".###.", "....#", ".####", "#...#", ".####"),
    "b": ("#....", "#....", "#.##.", "##..#", "#...#", "#...#", "####."),
    "c": (".....", ".....", ".###.", "#....", "#....", "#...#", ".###."),
    "d": ("....#", "....#", ".##.#", "#..##", "#...#", "#...#", ".####"),
    "e": (".....", ".....", ".###.", "#...#", "#####", "#....", ".###."),
    "f": ("..##.", ".#..#", ".#...", "###..", ".#...", ".#...", ".#..."),
    "g": (".....", ".....", ".####", "#...#", "#...#", "#...#", ".####", "....#", ".###."),
    "h": ("#....", "#....", "#.##.", "##..#", "#...#", "#...#", "#...#"),
    "i": ("..#..", ".....", ".##..", "..#..", "..#..", "..#..", ".###."),
    "j": ("...#.", ".....", "..##.", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##.."),
    "k": ("#....", "#....", "#..#.", "#.#..", "##...", "#.#..", "#..#."),
    "l": (".##..", "..#..", "..#..", "..#..", "..#..", "..#..", ".###."),
    "m": (".....", ".....", "##.#.", "#.#.#", "#.#.#", "#...#", "#...#"),
    "n": (".....", ".....", "#.##.", "##..#", "#...#", "#...#", "#...#"),
    "o": (".....", ".....", ".###.", "#...#", "#...#", "#...#", ".###."),
    "p": (".....", ".....", "####.", "#...#", "#...#", "#...#", "####.", "#....", "#...."),
    "q": (".....", ".....", ".####", "#...#", "#...#", "#...#", ".####", "....#", "....#"),
    "r": (".....", ".....", "#.##.", "##..#", "#....", "#....", "#...."),
    "s": (".....", ".....", ".####", "#....", ".###.", "....#", "####."),
    "t": (".#...", ".#...", "###..", ".#...", ".#...", ".#..#", "..##."),
    "u": (".....", ".....", "#...#", "#...#", "#...#", "#..##", ".##.#"),
    "v": (".....", ".....", "#...#", "#...#", "#...#", ".#.#.", "..#.."),
    "w": (".....", ".....", "#...#", "#...#", "#.#.#", "#.#.#", ".#.#."),
    "x": (".....", ".....", "#...#", ".#.#.", "..#..", ".#.#.", "#...#"),
    "y": (".....", ".....", "#...#", "#...#", "#...#", "#...#", ".####", "....#", ".###."),
    "z": (".....", ".....", "#####", "...#.", "..#..", ".#...", "#####"),
    " ": (".....", ".....", ".....", ".....", ".....", ".....", "....."),
    "/": ("....#", "....#", "...#.", "..#..", ".#...", "#....", "#...."),
    "-": (".....", ".....", ".....", "#####", ".....", ".....", "....."),
    "_": (".....", ".....", ".....", ".....", ".....", ".....", "#####"),
    ".": (".....", ".....", ".....", ".....", ".....", ".##..", ".##.."),
    "+": (".....", "..#..", "..#..", "#####", "..#..", "..#..", "....."),
}
GLYPHS: dict[str, tuple[str, ...]] = {ch: rows + ("." * GLYPH_W,) * (GLYPH_H - len(rows)) for ch, rows in _FORMS.items()}


def compact(n: int) -> str:
    """`1.5k`/`20k`/`1M` — a label slot's width, not a raw 5-digit count."""
    for unit, size in (("M", 1_000_000), ("k", 1_000)):
        if n >= size:
            return f"{n / size:.1f}".removesuffix(".0") + unit
    return str(n)


def _run(x: int, y: int, w: int, h: int) -> str:
    return f"M{x} {y}h{w}v{h}h-{w}z"


FAILS: list[str] = []


def check(cond: bool, msg: str) -> None:
    if not cond:
        FAILS.append(msg)


def run_selftest(label: str, namespace: dict[str, object]) -> int:
    """Run every `test_*` callable of `namespace` (a script's `globals()`); a crashing test is a failing test."""
    for name, fn in sorted(namespace.items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
            except Exception:
                FAILS.append(f"{name} crashed:\n{traceback.format_exc()}")
    for f in FAILS:
        print(f"FAIL: {f}", file=sys.stderr)
    print(f"{label} selftest: {'FAIL' if FAILS else 'ok'}")
    return 1 if FAILS else 0


def text_width(s: str, px: int) -> int:
    return (len(s) * GLYPH_ADVANCE - 1) * px


def mask_path(rows: tuple[str, ...], x: int, y: int, px: int) -> str:
    """Path data lighting every `#` of `rows` top-left at (x, y), `px` real pixels per mask pixel; one run per horizontal stroke."""
    return "".join(_run(x + m.start() * px, y + r * px, (m.end() - m.start()) * px, px) for r, row in enumerate(rows) for m in re.finditer(r"#+", row))


def text_path(s: str, x: int, y: int, px: int) -> str:
    """Path data lighting `s` top-left at (x, y), `px` real pixels per glyph pixel."""
    runs = []
    for i, ch in enumerate(s):
        rows = GLYPHS[ch]
        runs.append(mask_path(rows, x + i * GLYPH_ADVANCE * px, y, px))
    return "".join(runs)


Frame = list[list[str]]


class Pack(NamedTuple):
    """The sprite pack's palette and the named animations' frames with their `frame_ms`."""

    palette: dict[str, str]
    animations: dict[str, tuple[list[Frame], int]]


def parse_sprite(text: str) -> Frame:
    lines = [ln.strip() for ln in text.splitlines()]
    if sum(ln.startswith("@frame") for ln in lines) > 1:
        raise ValueError("a multi-frame sprite file; the README art reads one frame per file")
    return [ln.split() for ln in lines if ln and not ln.startswith(("#", "@"))]


def load_pack(pack_dir: pathlib.Path, names: tuple[str, ...]) -> Pack:
    manifest = tomllib.loads((pack_dir / "pack.toml").read_text(encoding="utf-8"))
    animations = {}
    for name in names:
        a = manifest["animations"][name]
        frames = [parse_sprite((pack_dir / f).read_text(encoding="utf-8")) for f in a["frames"]]
        animations[name] = (frames, a["frame_ms"])
    return Pack(manifest["palette"], animations)


def sprite_size(pack: Pack, name: str, px: int) -> tuple[int, int]:
    frame = pack.animations[name][0][0]
    return len(frame[0]) * px, len(frame) * px


def sprite_path(frame: Frame, x: int, y: int, px: int, palette: dict[str, str]) -> str:
    """One path per palette key, one run per horizontal stretch of it."""
    runs: dict[str, list[str]] = {}
    for r, row in enumerate(frame):
        c = 0
        while c < len(row):
            key, end = row[c], c
            while end + 1 < len(row) and row[end + 1] == key:
                end += 1
            if key != ".":
                runs.setdefault(key, []).append(_run(x + c * px, y + r * px, (end - c + 1) * px, px))
            c = end + 1
    return "".join(f'<path fill="{palette[k]}" d="{"".join(v)}"/>' for k, v in runs.items())


def animation(pack: Pack, name: str, x: int, bottom: int, px: int) -> tuple[str, list[str]]:
    """Every frame drawn standing on `bottom`, each shown for its `frame_ms` turn by a stepped CSS opacity loop."""
    frames, frame_ms = pack.animations[name]
    n, period = len(frames), frame_ms * len(frames)
    y = bottom - sprite_size(pack, name, px)[1]
    css = [
        f".{name} g{{opacity:0;animation:{name} {period}ms steps(1,end) infinite}}",
        f"@keyframes {name}{{0%{{opacity:1}}{100 / n:g}%{{opacity:0}}100%{{opacity:0}}}}",
        *(f".{name} g:nth-child({i + 1}){{animation-delay:{(i - n) * frame_ms}ms}}" for i in range(1, n)),
        f"@media (prefers-reduced-motion:reduce){{.{name} g{{animation:none}}.{name} g:first-child{{opacity:1}}}}",
    ]
    body = "".join(f"<g>{sprite_path(f, x, y, px, pack.palette)}</g>" for f in frames)
    return f'<g class="{name}">{body}</g>', css
