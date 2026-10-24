#!/usr/bin/env python3
"""Render the README's Buy Me a Coffee button in the office's own pixels.

A coworker from the sprite pack walks with a mug beside the live supporter
count, the office cat sits on the carpet, and every colour is the pack's or
the star chart's palette. It is drawn here and published to the `bmc-button`
branch by `.github/workflows/bmc-button.yml`, the way the star chart is,
because no hosted badge can draw it.

Usage: `bmc-button.py FUNDING_JSON OUT_DIR` writes `bmc-button-{light,dark}.svg`
for the Buy Me a Coffee account in FUNDING_JSON, the repository's GraphQL
`fundingLinks` the workflow fetches.
`--selftest` exercises everything but the fetch; exit 0 = pass.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys
from typing import NamedTuple
from urllib import request

from readme_pixels import GLYPH_H, PACK_DIR, THEMES, Pack, _run, animation, check, compact, load_pack, mask_path, parse_sprite, run_selftest, sprite_size, text_path, text_width

REPO = pathlib.Path(__file__).resolve().parent.parent
README_PATH = REPO / "README.md"
BMC_LINK = re.compile(r"https://buymeacoffee\.com/([\w-]+)")
# BMC's own app API, unauthenticated; `public_supporters_count` is the "N
# supporters" the creator page shows.
CREATOR_URL = "https://app.buymeacoffee.com/api/creators/slug/{slug}"

AGENT, CAT = "walking_coffee", "cat_sit"
TITLE = "Buy me a coffee"
# One button pixel; the sprites are drawn a size up so the coworker reads at a glance.
PX, SPRITE_PX = 3, 4
TITLE_PX, LABEL_PX = 2, 2
WIDTH, HEIGHT = 318, 76
CARPET_H = 4 * PX
FLOOR_Y = HEIGHT - PX - CARPET_H
# How far a sprite's feet sink into the carpet.
FOOTING = 2 * PX
TITLE_Y = 5 * PX
LABEL_Y = TITLE_Y + GLYPH_H * TITLE_PX + 2 * PX
HEART = (".##.##.", "#######", "#######", ".#####.", "..###..", "...#...")
HEART_KEY = "o"
SHADOW_OPACITY = 0.35

class Layout(NamedTuple):
    agent_x: int
    cat_x: int
    text_x: int


def button_slug(funding: object, readme: str) -> str:
    # The Sponsor button's FUNDING.yml lives outside this tree, so this run is
    # what notices the README linking another account than the one it counts.
    data = funding.get("data") if isinstance(funding, dict) else None
    repo = data.get("repository") if isinstance(data, dict) else None
    links = repo.get("fundingLinks") if isinstance(repo, dict) else None
    if not isinstance(links, list):
        raise RuntimeError(f"no fundingLinks in {funding!r}")
    funded = {
        m.group(1)
        for link in links
        if isinstance(link, dict) and link.get("platform") == "BUY_ME_A_COFFEE"
        for m in [BMC_LINK.fullmatch(str(link.get("url")))]
        if m
    }
    linked = set(BMC_LINK.findall(readme))
    if len(funded) != 1 or linked != funded:
        raise RuntimeError(f"the Sponsor button funds {sorted(funded)}, but the README links {sorted(linked)}")
    return next(iter(funded))


def supporter_count(payload: object) -> int:
    data = payload.get("data") if isinstance(payload, dict) else None
    count = data.get("public_supporters_count") if isinstance(data, dict) else None
    # `type(...) is`, not isinstance: bool is an int subclass.
    if type(count) is not int or count < 0:
        raise RuntimeError(f"public_supporters_count is {count!r}, not a count")
    return count


def count_label(count: int) -> tuple[str, str]:
    return compact(count), " supporter" if count == 1 else " supporters"


def layout(pack: Pack) -> Layout:
    agent_x = 5 * PX
    cat_x = WIDTH - 3 * PX - sprite_size(pack, CAT, SPRITE_PX)[0]
    return Layout(agent_x, cat_x, agent_x + sprite_size(pack, AGENT, SPRITE_PX)[0] + 5 * PX)


def count_line_width(count: int) -> int:
    num, word = count_label(count)
    return max(text_width(TITLE, TITLE_PX), len(HEART[0]) * LABEL_PX + 2 * PX + text_width(num + word, LABEL_PX))


def render_svg(count: int, theme: str, pack: Pack) -> str:
    pal = THEMES[theme]
    lay = layout(pack)
    agent, agent_css = animation(pack, AGENT, lay.agent_x, FLOOR_Y + FOOTING, SPRITE_PX)
    cat, cat_css = animation(pack, CAT, lay.cat_x, FLOOR_Y + FOOTING, SPRITE_PX)
    num, word = count_label(count)
    heart = mask_path(HEART, lay.text_x, LABEL_Y + LABEL_PX, LABEL_PX)
    num_x = lay.text_x + len(HEART[0]) * LABEL_PX + 2 * PX
    frame = _run(0, 0, WIDTH, PX) + _run(0, HEIGHT - PX, WIDTH, PX) + _run(0, 0, PX, HEIGHT) + _run(WIDTH - PX, 0, PX, HEIGHT)
    tile = 2 * PX
    return "\n".join(
        [
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH + PX} {HEIGHT + PX}" width="{WIDTH + PX}" height="{HEIGHT + PX}" shape-rendering="crispEdges" role="img" aria-labelledby="t">',
            f'<title id="t">{TITLE}: {num}{word}</title>',
            f'<style>{"".join(agent_css + cat_css)}</style>',
            f'<pattern id="carpet" width="{2 * tile}" height="{2 * tile}" patternUnits="userSpaceOnUse">'
            f'<rect width="{2 * tile}" height="{2 * tile}" fill="{pal.carpet_dark}"/>'
            f'<rect width="{tile}" height="{tile}" fill="{pal.carpet_light}"/>'
            f'<rect x="{tile}" y="{tile}" width="{tile}" height="{tile}" fill="{pal.carpet_light}"/></pattern>',
            # A hard pixel drop shadow, so the panel reads as a button to press.
            f'<rect x="{PX}" y="{PX}" width="{WIDTH}" height="{HEIGHT}" fill="#000" fill-opacity="{SHADOW_OPACITY}"/>',
            f'<rect width="{WIDTH}" height="{HEIGHT}" fill="{pal.wall}"/>',
            f'<rect x="{PX}" y="{FLOOR_Y}" width="{WIDTH - 2 * PX}" height="{CARPET_H}" fill="url(#carpet)"/>',
            f'<path fill="{pal.trim}" d="{frame}{_run(PX, FLOOR_Y - PX, WIDTH - 2 * PX, PX)}"/>',
            agent,
            cat,
            f'<path fill="{pal.title}" d="{text_path(TITLE, lay.text_x, TITLE_Y, TITLE_PX)}"/>',
            f'<path fill="{pack.palette[HEART_KEY]}" d="{heart}"/>',
            f'<path fill="{pal.star}" d="{text_path(num, num_x, LABEL_Y, LABEL_PX)}"/>',
            f'<path fill="{pal.text}" d="{text_path(word, num_x + text_width(num, LABEL_PX), LABEL_Y, LABEL_PX)}"/>',
            "</svg>",
        ]
    ) + "\n"


def write_buttons(count: int, out_dir: pathlib.Path, pack: Pack) -> list[pathlib.Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    paths = []
    for theme in THEMES:
        p = out_dir / f"bmc-button-{theme}.svg"
        p.write_text(render_svg(count, theme, pack), encoding="utf-8")
        paths.append(p)
    return paths


def fetch_creator(slug: str) -> object:
    req = request.Request(CREATOR_URL.format(slug=slug), headers={"User-Agent": "pixtuoid-bmc-button", "Accept": "application/json"})
    with request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def _raises(fn, *args) -> bool:
    try:
        fn(*args)
    except (RuntimeError, ValueError):
        return True
    return False


def _funding(*links: tuple[str, str]) -> dict:
    return {"data": {"repository": {"fundingLinks": [{"platform": p, "url": u} for p, u in links]}}}


def test_button_slug_is_the_funded_account_the_readme_links() -> None:
    bmc = ("BUY_ME_A_COFFEE", "https://buymeacoffee.com/SomeOne42")
    readme = '<a href="https://buymeacoffee.com/SomeOne42">'
    check(button_slug(_funding(("GITHUB", "https://github.com/someone"), bmc), readme) == "SomeOne42", "the BMC link's account is the slug")
    for other in ("no link", '<a href="https://buymeacoffee.com/Other">', readme + " https://buymeacoffee.com/Other"):
        check(_raises(button_slug, _funding(bmc), other), f"README {other!r} must raise, not count another account")
    for funding in ({}, {"data": None}, _funding(), _funding(("GITHUB", "https://github.com/someone")), _funding(("BUY_ME_A_COFFEE", "https://example.com/SomeOne42"))):
        check(_raises(button_slug, funding, readme), f"fundingLinks {funding!r} must raise, not guess an account")
    # Funding and README agree on these, so only the one-account rule refuses them.
    other = ("BUY_ME_A_COFFEE", "https://buymeacoffee.com/Other")
    for funding, agreeing in ((_funding(), "no link"), (_funding(bmc, other), readme + " https://buymeacoffee.com/Other")):
        check(_raises(button_slug, funding, agreeing), f"fundingLinks {funding!r} must raise, not pick an account")


def test_count_is_the_public_supporters_count() -> None:
    check(supporter_count({"data": {"public_supporters_count": 19, "supporter_count": 0}}) == 19, "reads data.public_supporters_count")
    for bad in (None, True, -1, "19"):
        check(_raises(supporter_count, {"data": {"public_supporters_count": bad}}), f"public_supporters_count {bad!r} must raise")
    for bad in ({}, {"data": None}, [], "x"):
        check(_raises(supporter_count, bad), f"payload {bad!r} must raise, not publish a count")


def test_count_label_pluralizes_and_compacts() -> None:
    for count, want in ((0, ("0", " supporters")), (1, ("1", " supporter")), (3, ("3", " supporters")), (1500, ("1.5k", " supporters"))):
        check(count_label(count) == want, f"count_label({count}) = {count_label(count)}, want {want}")


def test_sprite_parse_skips_comments_and_directives() -> None:
    text = "# a comment\n@frame 0\n@mark head.front 1 1\n. H\nB .\n"
    check(parse_sprite(text) == [[".", "H"], ["B", "."]], f"got {parse_sprite(text)}")
    check(_raises(parse_sprite, "@frame 0\n. H\n@frame 1\nH .\n"), "a two-frame file must raise, not merge its frames")


def test_pack_animations_have_uniform_frames() -> None:
    pack = load_pack(PACK_DIR, (AGENT, CAT))
    for name, (frames, frame_ms) in pack.animations.items():
        check(frame_ms > 0 and len(frames) > 1, f"{name}: {len(frames)} frames at {frame_ms}ms")
        dims = {(len(f[0]), len(f)) for f in frames}
        check(len(dims) == 1, f"{name}: frames differ in size {dims}")
        keys = {k for f in frames for row in f for k in row}
        check(keys <= pack.palette.keys(), f"{name}: keys {keys - pack.palette.keys()} missing from the palette")


def test_render_draws_every_frame_and_names_the_count() -> None:
    import xml.etree.ElementTree as ET

    pack = load_pack(PACK_DIR, (AGENT, CAT))
    ns = "{http://www.w3.org/2000/svg}"
    for theme in ("light", "dark"):
        root = ET.fromstring(render_svg(3, theme, pack))
        title = root.find(f"{ns}title")
        check(title is not None and title.text == "Buy me a coffee: 3 supporters", f"{theme}: title {title is not None and title.text}")
        for name, (frames, _) in pack.animations.items():
            group = root.find(f".//{ns}g[@class='{name}']")
            check(group is not None and len(group) == len(frames), f"{theme}: {name} must draw all {len(frames)} frames")


def test_text_never_reaches_the_cat() -> None:
    pack = load_pack(PACK_DIR, (AGENT, CAT))
    lay = layout(pack)
    for count in (0, 1, 999, 1500, 123_456, 999_999, 10**6):
        check(lay.text_x + count_line_width(count) < lay.cat_x, f"count {count}: the label runs into the cat")


def test_label_clears_the_carpet() -> None:
    check(LABEL_Y + GLYPH_H * LABEL_PX < FLOOR_Y - PX, "the count line's descenders must clear the carpet's trim")


def test_write_buttons_emits_both_themes() -> None:
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        paths = write_buttons(3, pathlib.Path(tmp) / "nested", load_pack(PACK_DIR, (AGENT, CAT)))
        check(sorted(p.name for p in paths) == ["bmc-button-dark.svg", "bmc-button-light.svg"], f"got {paths}")
        check(all(p.stat().st_size > 0 for p in paths), "every button must be written non-empty")


def selftest() -> int:
    return run_selftest("bmc-button", globals())


def main(argv: list[str]) -> int:
    if "--selftest" in argv:
        return selftest()
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    funding = json.loads(pathlib.Path(argv[0]).read_text(encoding="utf-8"))
    slug = button_slug(funding, README_PATH.read_text(encoding="utf-8"))
    count = supporter_count(fetch_creator(slug))
    for p in write_buttons(count, pathlib.Path(argv[1]), load_pack(PACK_DIR, (AGENT, CAT))):
        print(p)
    print(f"{slug}: {count} supporters", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
