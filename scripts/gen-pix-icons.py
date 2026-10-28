#!/usr/bin/env python3
"""Generate the site's pixel-icon PNGs (site/src/assets/pix-icons/) plus the
root README's SVG variants (docs/images/pix-icons/).

Single color source: the bundled sprite pack's palette
(crates/pixtuoid-scene/sprites/default/pack.toml) — an icon grid may only use
keys defined there, so the icons can never drift off the office's own colors.
An icon is either extracted verbatim from a pack sprite ("sprite") or authored
here as a pixel grid ("grid"). The site's 1x RGBA PNGs are integer-upscaled by
PixIcon.astro with image-rendering: pixelated; GitHub strips that CSS, so the
README instead embeds an SVG per icon, crisp at any pixel density.

Usage:
  .venv/bin/python3 scripts/gen-pix-icons.py             # (re)generate (just gen-icons)
  .venv/bin/python3 scripts/gen-pix-icons.py --check     # exit 1 on drift
  .venv/bin/python3 scripts/gen-pix-icons.py --selftest  # the drift gate fails when it should

--check decode-compares the PNGs' pixels rather than their raw bytes: a raw-byte
compare is Pillow-version-fragile (re-encoding the identical pixels can change
the compressed bytes), which would make the gate flaky across machines/CI. The
SVGs are text and compare as such. It also diffs each output directory's listing
against ICONS.keys() so an orphaned file fails loudly.
"""

import io
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

from PIL import Image
from readme_pixels import PACK_DIR, load_pack, sprite_path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "site/src/assets/pix-icons"
README_OUT = ROOT / "docs/images/pix-icons"
# CSS pixels per icon pixel in the README: each SVG's width/height. gen-readme.mjs
# pins each <img> to them and derives the icon-column padding from the widest
# one; bump this to resize README icons.
README_SCALE = 2
COMPARE = ROOT / "scripts/compare-screenshots.py"
DIFF_DIR = ROOT / "target/gen-check-diff"

# Icon manifest. "sprite": extract a whole pack sprite frame verbatim.
# "grid": rows of space-separated pack-palette keys ('.' = transparent).
ICONS = {
    # the office's own walker, straight from the pack
    "walk": {"sprite": "walking_0.sprite"},
    "coffee": {
        "grid": [
            ". . K . . K . . . .",
            ". K . . K . . . . .",
            ". . K . . K . . . .",
            ". . . . . . . . . .",
            ". V V V V V V . . .",
            ". V d d d d V V V .",
            ". V d d d d V . V .",
            ". V V V V V V V V .",
            ". . V V V V V V . .",
            ". K K K K K K K K .",
        ]
    },
    "chat": {
        "grid": [
            ". . n n n n n n . .",
            ". n w w w w w w n .",
            "n w w w w w w w w n",
            "n w q w q w q w w n",
            "n w w w w w w w w n",
            ". n w w w w w w n .",
            ". . n n w w n n . .",
            ". . . n w n . . . .",
            ". . . n n . . . . .",
            ". . . . . . . . . .",
        ]
    },
    "palette": {
        "grid": [
            ". . D D D D D D . .",
            ". D D D D D D D D .",
            "D D r r D D b b D D",
            "D D r r D D b b D D",
            "D D D D D D D D D D",
            "D D y y D D . . D D",
            "D D y y D . . . D D",
            ". D D D D . . D D .",
            ". . D D D D D D . .",
            ". . . . . . . . . .",
        ]
    },
    # light (K) outline so the dark bezel doesn't dissolve into the DARK theme
    "glow": {
        "grid": [
            ". K K K K K K K K .",
            ". K M M M M M M K .",
            ". K M c c c c M K .",
            ". K M c c c c M K .",
            ". K M c c c c M K .",
            ". K M M M M M M K .",
            ". K K K K K K K K .",
            ". . . K K K K . . .",
            ". . K K K K K K . .",
            ". . . . . . . . . .",
        ]
    },
    # hover tooltips → an info "i" badge; a magnifier reads as "search / zoom"
    "magnify": {
        "grid": [
            ". . K K K K K K . .",
            ". K w w w w w w K .",
            ". K w w b w w w K .",
            ". K w w w w w w K .",
            ". K w b b w w w K .",
            ". K w w b w w w K .",
            ". K w w b w w w K .",
            ". K w b b b w w K .",
            ". K w w w w w w K .",
            ". . K K K K K K . .",
        ]
    },
    # token meter → a stack of sheets; dark (n) side-edges so the near-white
    # sheets don't vanish into the LIGHT theme's cream
    "tokens": {
        "grid": [
            ". . . n n n . . . .",
            ". . n w w w n . . .",
            ". n w w w w w n . .",
            ". n V V V V V n . .",
            ". n w w w w w n . .",
            ". n V V V V V n . .",
            ". n w w w w w n . .",
            ". n V V V V V n . .",
            ". D D D D D D D D .",
            ". D D D D D D D D .",
        ]
    },
    "note": {
        "grid": [
            ". . y y y y y y y .",
            ". . y y y y y y y .",
            ". . y . . . . . y .",
            ". . y . . . . . y .",
            ". . y . . . . . y .",
            ". . y . . . . . y .",
            "y y y . . . y y y .",
            "y y y . . . y y y .",
            ". y y . . . . y y .",
            ". . . . . . . . . .",
        ]
    },
    "shield": {
        "grid": [
            ". n n n n n n n n .",
            "n B B B B B B B B n",
            "n B B B B B B w B n",
            "n B B B B B w w B n",
            "n B w B B w w B B n",
            "n B w w w w B B B n",
            ". n B w w B B B n .",
            ". n B B B B B B n .",
            ". . n B B B B n . .",
            ". . . n n n n . . .",
        ]
    },
    # stacked floors with orange (both-theme-visible) up/down arrows — this reads
    # as floor navigation, where a shaft reads as a filing cabinet/building
    "multifloor": {
        "grid": [
            ". . . . o . . . . .",
            ". . . o o o . . . .",
            ". K K K K K K K K .",
            ". K V V V V V V K .",
            ". K K K K K K K K .",
            ". K V V V V V V K .",
            ". K K K K K K K K .",
            ". . . o o o . . . .",
            ". . . . o . . . . .",
            ". . . . . . . . . .",
        ]
    },
    # an office facade, one differently-hued lit window per agent
    "multiagent": {
        "grid": [
            ". K K K K K K K K .",
            ". K c c M y y M K .",
            ". K M M M M M M K .",
            ". K r r M c c M K .",
            ". K M M M M M M K .",
            ". K y y M r r M K .",
            ". K M M M M M M K .",
            ". K M w w w w M K .",
            ". K M w w w w M K .",
            ". K K K K K K K K .",
        ]
    },
    # a top-down floor plan: rooms with desks, so it reads as rooms and not a
    # bare "+" grid. Light (K) walls — a brown frame merges into the night bg.
    "spaces": {
        "grid": [
            "K K K K K K K K K .",
            "K V V V K V V V K .",
            "K V c V K V c V K .",
            "K V V V K V V V K .",
            "K K . K K K . K K .",
            "K V V V K V V V K .",
            "K V c V K V c V K .",
            "K V V V K V V V K .",
            "K K K K K K K K K .",
            ". . . . . . . . . .",
        ]
    },
    # the agent-tree dashboard as a file-tree view: the sidebar-tree idiom reads
    # far clearer at ~20px than an org-chart's thin diagonal lines
    "tree": {
        "grid": [
            ". . . . . . . . . .",
            ". . c c c . . . . .",
            ". . . K . . . . . .",
            ". . . K K l l l . .",
            ". . . K . . . . . .",
            ". . . K K y y y . .",
            ". . . K . . . . . .",
            ". . . K K r r r . .",
            ". . . . . . . . . .",
            ". . . . . . . . . .",
        ]
    },
    # a paw print, centred and in warm brown (D) so it isn't a heavy dark lump
    "pets": {
        "grid": [
            ". . D D . . D D . .",
            ". . D D . . D D . .",
            "D D . . . . . . D D",
            "D D . . . . . . D D",
            ". . . D D D D . . .",
            ". . D D D D D D . .",
            ". . D D D D D D . .",
            ". . D D D D D D . .",
            ". . . D D D D . . .",
            ". . . . . . . . . .",
        ]
    },
    # the OpenClaw gateway mascot, front-on: the pack's 1x top-down read is one
    # solid mass at icon size
    "lobster": {
        "grid": [
            ". . . . . a . . a . . . . .",
            ". . . . . a . . a . . . . .",
            ". A A . . . a a . . . A A .",
            ". A A A . N o o N . A A A .",
            ". A A A o e o o e o A A A .",
            ". . N A . o Q Q o . A N . .",
            ". . . N N o Q Q o N N . . .",
            ". . . . . o o o o . . . . .",
            ". . . N . N o o N . N . . .",
            ". . . . . . o o . . . . . .",
            ". . . . . o N N o . . . . .",
            ". . . . o N o o N o . . . .",
        ]
    },
    # ambient atmosphere: day/night + weather + themes (NOT audio — that's the
    # lofi row). The cloud is fully grey-OUTLINED, not base-edged, or its white
    # body washes out on the light theme's cream.
    "vibes": {
        "grid": [
            ". t . . . . . . . .",
            ". . t t t . . . . .",
            ". t t t t t . . . .",
            "t . t t t t . . . .",
            ". . t t t K K K . .",
            ". . . K w w w w K .",
            ". . K w w w w w w K",
            ". . K w w w w w w K",
            ". . . K K K K K K .",
            ". . . . . . . . . .",
        ]
    },
    # a floating desktop window: a light content pane with text lines, NOT a
    # solid cyan screen, which twins the monitor-glow icon
    "window": {
        "grid": [
            "K K K K K K K K K K",
            "K M M M M M M M M K",
            "K r . y . l . M M K",
            "K M M M M M M M M K",
            "K w w w w w w w w K",
            "K w M M M M w w w K",
            "K w w w w w w w w K",
            "K w M M M w w w w K",
            "K w w w w w w w w K",
            "K K K K K K K K K K",
        ]
    },
    # the cutaway office: a terminal framing sky, sun, skyline and floor, so
    # it reads as a picture where `window` reads as text
    "cutaway": {
        "grid": [
            "K K K K K K K K K K",
            "K M M M M M M M M K",
            "K c c c c c c y y K",
            "K c c c c c c y y K",
            "K c n c c c n c c K",
            "K n n c n c n n c K",
            "K n n n n n n n n K",
            "K D D D D D D D D K",
            "K d D D d D D d D K",
            "K K K K K K K K K K",
        ]
    },
}


def load_palette():
    with open(PACK_DIR / "pack.toml", "rb") as f:
        pack = tomllib.load(f)
    pal = {}
    for key, hexval in pack["palette"].items():
        if hexval == "transparent":
            pal[key] = None
        else:
            pal[key] = tuple(int(hexval[i : i + 2], 16) for i in (1, 3, 5))
    return pal


def sprite_rows(name, frame=0):
    rows, in_frame = [], False
    for line in (PACK_DIR / name).read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("@frame"):
            in_frame = int(line.split()[1]) == frame
            continue
        if in_frame:
            rows.append(line.split())
    return rows


def render(icon_name, rows, pal):
    h, w = len(rows), len(rows[0])
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    px = img.load()
    assert px is not None
    for y, row in enumerate(rows):
        if len(row) != w:
            sys.exit(
                f"gen-pix-icons: {icon_name} row {y} is ragged ({len(row)} != {w})"
            )
        for x, key in enumerate(row):
            rgb = pal[key]
            if rgb is not None:
                px[x, y] = (*rgb, 255)
    return img


def svg(rows, hexes):
    """`rows` as an SVG of README_SCALE CSS pixels per icon pixel, one path per colour."""
    h, w = len(rows), len(rows[0])
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w * README_SCALE}" height="{h * README_SCALE}" '
        f'viewBox="0 0 {w} {h}" shape-rendering="crispEdges">{sprite_path(rows, 0, 0, 1, hexes)}</svg>\n'
    )


def shown(path):
    return path.relative_to(ROOT) if path.is_relative_to(ROOT) else path


def emit_svg(name, text, out_dir, check, stale):
    out = out_dir / f"{name}.svg"
    if check:
        if not out.exists() or out.read_text(encoding="utf-8") != text:
            stale.append(f"readme/{name} ({'differs' if out.exists() else 'missing'})")
    else:
        out.write_text(text, encoding="utf-8")
        print(f"wrote {shown(out)}")


def emit(name, img, label, out_dir, check, work, stale):
    out = out_dir / f"{name}.png"
    tag = f"{label}/{name}"
    if check:
        if not out.exists():
            stale.append(f"{tag} (missing)")
            return
        cand = work / f"{label}-{name}.png"
        img.save(cand)
        DIFF_DIR.mkdir(parents=True, exist_ok=True)
        rc = subprocess.run(
            [
                sys.executable,
                str(COMPARE),
                str(out),
                str(cand),
                str(DIFF_DIR / f"diff-{label}-{name}.png"),
            ],
            check=False,
        ).returncode
        if rc != 0:
            stale.append(f"{tag} (differs)")
    else:
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        out.write_bytes(buf.getvalue())
        print(f"wrote {shown(out)} ({img.width}x{img.height})")


def generate(site_dir, readme_dir, check, icons=ICONS):
    """Write `icons` into both dirs, or with `check` list what differs from a fresh render."""
    pal = load_palette()
    hexes = load_pack(PACK_DIR, ()).palette
    # The label disambiguates the two outputs: both dirs are basenamed "pix-icons".
    outputs = [("site", site_dir, "png"), ("readme", readme_dir, "svg")]
    for _, out_dir, _ in outputs:
        out_dir.mkdir(parents=True, exist_ok=True)
    stale = []
    work = Path(tempfile.mkdtemp(prefix="gen-pix-icons-"))
    try:
        for name, spec in icons.items():
            rows = (
                sprite_rows(spec["sprite"])
                if "sprite" in spec
                else [r.split() for r in spec["grid"]]
            )
            emit(name, render(name, rows, pal), "site", site_dir, check, work, stale)
            emit_svg(name, svg(rows, hexes), readme_dir, check, stale)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    # An orphaned committed file (its manifest entry removed, or another format)
    # is invisible to the loop above, which only ever iterates `icons`; both
    # output dirs hold icons alone, so a write deletes it and a check fails on
    # it. A dotfile is the OS's (Finder's .DS_Store), never committed.
    for label, out_dir, ext in outputs:
        orphans = sorted(
            p
            for p in out_dir.iterdir()
            if p.is_file()
            and not p.name.startswith(".")
            and (p.stem not in icons or p.suffix != f".{ext}")
        )
        if check and orphans:
            stale.append(f"{label}: orphaned {', '.join(p.name for p in orphans)}")
        elif not check:
            for p in orphans:
                p.unlink()
                print(f"deleted {shown(p)}")
    return stale


class SelfTest(unittest.TestCase):
    def test_each_kind_of_drift_fails_and_a_dotfile_does_not(self):
        """A drift gate that cannot fail is no gate."""
        name = next(iter(ICONS))
        with tempfile.TemporaryDirectory() as tmp:
            site, readme = Path(tmp, "site"), Path(tmp, "readme")
            one = {name: ICONS[name]}

            def stale():
                return generate(site, readme, check=True, icons=one)

            generate(site, readme, check=False, icons=one)
            self.assertEqual(stale(), [], "a fresh write is not stale")
            for d in (site, readme):
                (d / ".DS_Store").write_bytes(b"")
            self.assertEqual(stale(), [], "a dotfile is no orphan")
            (readme / f"{name}.png").write_bytes(b"")
            self.assertEqual(
                stale(),
                [f"readme: orphaned {name}.png"],
                "an icon in the other format is an orphan",
            )
            (site / "gone.png").write_bytes(b"")
            (readme / "gone.svg").write_bytes(b"")
            readme_orphans = ", ".join(sorted(["gone.svg", f"{name}.png"]))
            self.assertEqual(
                stale(),
                ["site: orphaned gone.png", f"readme: orphaned {readme_orphans}"],
                "a removed icon's files are orphans",
            )
            generate(site, readme, check=False, icons=one)
            self.assertEqual(
                {p.name for d in (site, readme) for p in d.iterdir()},
                {f"{name}.png", f"{name}.svg", ".DS_Store"},
                "a write deletes the orphans and keeps the dotfiles",
            )
            out = readme / f"{name}.svg"
            out.write_text(out.read_text(encoding="utf-8") + " ", encoding="utf-8")
            self.assertEqual(
                stale(), [f"readme/{name} (differs)"], "an edited SVG differs"
            )
            out.unlink()
            self.assertEqual(
                stale(), [f"readme/{name} (missing)"], "a deleted SVG is missing"
            )


def main():
    if "--selftest" in sys.argv[1:]:
        unittest.main(argv=sys.argv[:1])
    check = "--check" in sys.argv[1:]
    stale = generate(OUT, README_OUT, check)
    if stale:
        sys.exit(f"gen-pix-icons --check: {', '.join(stale)} — run just gen-icons")
    if check:
        print(
            f"gen-pix-icons --check: OK ({len(ICONS)} icons match in both output dirs)"
        )


if __name__ == "__main__":
    main()
