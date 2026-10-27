#!/usr/bin/env python3
"""Render the README's star-history chart from GitHub's own stargazers data.

GitHub announced on 2026-06-30 that `stargazers` (the who-starred-when list)
would be limited to a repository's admins and collaborators —
https://github.blog/changelog/2026-06-30-upcoming-access-restrictions-to-public-api-endpoints-and-ui-views/
— and the cut-over blanked every hosted chart service (star-history.com
included) for every repo it does not own. The repo's own Actions token is
assumed to be a collaborator — the changelog names neither GraphQL nor Actions
tokens, so the first live run is the proof — and the chart is drawn here and
published to the `star-history` branch by `.github/workflows/star-history.yml`;
nothing leaves the repo.

The chart is the office's night window: one tower per month of the last
year, a floor per `stars_per_floor` stars, and this month's tower still going
up under a crane.

Usage: `star-history.py OWNER/REPO OUT_DIR` with the token in `GH_TOKEN`
(`GITHUB_TOKEN` accepted). Writes `star-history-{light,dark}.svg`.
`--selftest` exercises the pure halves (bucketing, axes, rendering, paging)
with no network; exit 0 = pass.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import pathlib
import re
import sys
import unittest
import xml.etree.ElementTree as ET
from collections.abc import Callable, Iterable
from typing import NamedTuple
from unittest import mock
from urllib import request

from readme_pixels import (
    BASELINE,
    GLYPH_ADVANCE,
    GLYPH_H,
    GLYPH_W,
    GLYPHS,
    PACK_DIR,
    THEMES,
    _run,
    animation,
    compact,
    load_pack,
    mask_path,
    text_path,
    text_width,
)

GRAPHQL_URL = "https://api.github.com/graphql"
# The API's hard page ceiling for `stargazers(first:)`.
PAGE_SIZE = 100
STARGAZERS_QUERY = """
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    stargazerCount
    stargazers(first: $first, after: $after, orderBy: {field: STARRED_AT, direction: ASC}) {
      edges { starredAt }
      pageInfo { hasNextPage endCursor }
    }
  }
}
"""

Point = tuple[dt.date, int]

WIDTH, HEIGHT = 640, 320
# One chart pixel — coarser than a label glyph needs.
PX = 4
COUNT_PX, LABEL_PX = 3, 2
# The window the city stands in; the wall around it carries the axes and the header.
MARGIN_LEFT, MARGIN_RIGHT, MARGIN_TOP, MARGIN_BOTTOM = 64, 20, 60, 64
PLOT_COLS = (WIDTH - MARGIN_LEFT - MARGIN_RIGHT) // PX
PLOT_ROWS = (HEIGHT - MARGIN_TOP - MARGIN_BOTTOM) // PX
Y_TICKS = 5
LABEL_GAP = 3 * PX
HEADER_Y = 4 * PX
MONTHS = 12
# A window row and a slab row.
FLOOR_ROWS = 2
# The newest tower's crane: its mast's rows (the beacon sits one above), the jib's reach left and right of the mast, the hook's drop.
CRANE_MAST_ROWS = 7
CRANE_JIB = (9, 2)
CRANE_HOOK_ROWS = 3
# Sky above the tallest tower (whose scaffold `stars_per_floor` already counts): the crane and its beacon, plus a row of air the moon and the meteor share.
HEADROOM = CRANE_MAST_ROWS + 2
MAX_FLOORS = (PLOT_ROWS - HEADROOM) // FLOOR_ROWS
LOT_GAP = 2
LIT_PERCENT = 14
SKY_BANDS = 6
STAR_SEEDS, STAR_CLEARANCE, BIG_STAR_EVERY = 80, 2, 9
MOON = ("..###..", ".#####.", "#######", "#######", "#######", ".#####.", "..###..")
# Chart pixels from the window's top-left; inside HEADROOM, so no tower reaches it.
MOON_AT = (4, 1)
# Glyph pixels of air between the header's star icon and the count.
HEADER_ICON_GAP = 3
STAR_ICON = (
    "....#....",
    "...###...",
    "...###...",
    "#########",
    ".#######.",
    "..#####..",
    "..#####..",
    ".###.###.",
    ".##...##.",
)
METEOR = ("#...", ".#..", "..#.", "...#")
# Start (column, row from the top) and travel, in chart pixels: its whole run stays in HEADROOM, right of the moon.
METEOR_AT, METEOR_TRAVEL = (46, 1), (42, 4)
CAT, CAT_PX = "cat_sit", 4


class Bucket(NamedTuple):
    label: str
    end: dt.date
    stars: int


def cumulative_by_day(dates: Iterable[dt.date]) -> list[Point]:
    """Running star total, one point per distinct day, ascending."""
    series: list[Point] = []
    for total, day in enumerate(sorted(dates), start=1):
        if series and series[-1][0] == day:
            series[-1] = (day, total)
        else:
            series.append((day, total))
    return series


def nice_step(top: int) -> int:
    """Smallest 1/2/5×10ⁿ step that fits `top` into the Y tick budget."""
    step = 1
    while top // step + 1 > Y_TICKS + 1:
        mant = step / 10 ** (len(str(step)) - 1)
        step = step * 2 if mant == 1 else int(step * 2.5) if mant == 2 else step * 2
    return step


def _hash(*key: object) -> int:
    """A stable pseudo-random int per key, so a window only changes where the data does."""
    return int.from_bytes(
        hashlib.blake2b(repr(key).encode(), digest_size=4).digest(), "big"
    )


def _count_on(series: list[Point], day: dt.date) -> int:
    count = 0
    for d, n in series:
        if d > day:
            break
        count = n
    return count


def gain_since(series: list[Point], day: dt.date) -> int:
    """Stars added after `day`."""
    return (series[-1][1] if series else 0) - _count_on(series, day)


def month_buckets(series: list[Point], today: dt.date) -> list[Bucket]:
    """One bucket per month from the first star's (at most MONTHS back) to today's, each with its closing cumulative count."""
    first = series[0][0] if series else today
    months = (today.year - first.year) * 12 + today.month - first.month + 1
    y, m = today.year, today.month - min(months, MONTHS) + 1
    while m < 1:
        y, m = y - 1, m + 12
    out = []
    while (y, m) <= (today.year, today.month):
        ny, nm = (y + 1, 1) if m == 12 else (y, m + 1)
        end = min(dt.date(ny, nm, 1) - dt.timedelta(days=1), today)
        label = str(y) if m == 1 else dt.date(y, m, 1).strftime("%b")
        out.append(Bucket(label, end, _count_on(series, end)))
        y, m = ny, nm
    return out


def floor_ladder(limit: int) -> list[int]:
    """Whole 1/2/2.5/3/4/5/6/8 × 10ⁿ values up to `limit`, ascending."""
    out, k = [], 1
    while k <= max(limit, 1):
        out += [
            int(m * k)
            for m in (1, 2, 2.5, 3, 4, 5, 6, 8)
            if m * k == int(m * k) and m * k <= limit
        ]
        k *= 10
    return sorted(set(out))


def stars_per_floor(top: int) -> int:
    """The smallest ladder value that keeps `top` stars within MAX_FLOORS."""
    return next(
        n
        for n in floor_ladder(max(top, 1)) + [max(top, 1)]
        if -(-top // n) <= MAX_FLOORS
    )


def scaffold_cols(count: int, per_floor: int, width: int) -> int:
    """Columns of the floor going up: its earned share of `width`, at least a post once any star is in."""
    partial = count % per_floor
    return max(1, round(width * partial / per_floor)) if partial else 0


def lots(n: int) -> tuple[int, int]:
    """(first column, columns per month) of n month lots centred in the window."""
    slot = PLOT_COLS // max(n, 1)
    return (PLOT_COLS - slot * n) // 2, slot


def tower_cols(i: int, n: int) -> tuple[int, int]:
    """(first column, width) of the tower on lot i of n."""
    lot0, slot = lots(n)
    return lot0 + i * slot + LOT_GAP // 2, slot - LOT_GAP


def month_label_xs(labels: list[str]) -> list[int]:
    lot0, slot = lots(len(labels))
    return [
        MARGIN_LEFT
        + (lot0 + i * slot) * PX
        + (slot * PX - text_width(label, LABEL_PX)) // 2
        for i, label in enumerate(labels)
    ]


def sky_stars(roofs: list[int]) -> list[tuple[int, int, bool]]:
    """(column, row up from the ground, big?) for every star seed that lands in open sky."""
    mc, mr = MOON_AT
    out = []
    for s in range(STAR_SEEDS):
        c, r = (
            _hash("sx", s) % PLOT_COLS,
            PLOT_ROWS - 1 - _hash("sy", s) % (PLOT_ROWS - 2),
        )
        big = s % BIG_STAR_EVERY == 0
        # A big star is a plus: its arms reach the columns either side.
        if r <= max(roofs[max(c - big, 0) : c + big + 1]) + STAR_CLEARANCE:
            continue
        if mc - 2 <= c <= mc + len(MOON[0]) + 1 and PLOT_ROWS - 1 - r <= mr + len(MOON):
            continue
        out.append((c, r, big))
    return out


class Header(NamedTuple):
    icon_x: int
    count_x: int
    count_end: int
    right_x: int
    legend_left: int
    week: str
    legend: str


def header_layout(top: int, gain: int, per_floor: int) -> Header:
    """The header as drawn: the star icon and count on the left, the week's gain and the floor legend right-aligned."""
    icon_x = MARGIN_LEFT - PX
    count_x = icon_x + (len(STAR_ICON[0]) + HEADER_ICON_GAP) * COUNT_PX
    count_end = (
        count_x + text_width(compact(top), COUNT_PX) + text_width(" stars", LABEL_PX)
    )
    right_x = WIDTH - MARGIN_RIGHT + PX
    week, legend = f"+{compact(gain)} this week", f"{compact(per_floor)} stars / floor"
    return Header(
        icon_x,
        count_x,
        count_end,
        right_x,
        right_x - max(text_width(week, LABEL_PX), text_width(legend, LABEL_PX)),
        week,
        legend,
    )


def _mix(a: str, b: str, t: float) -> str:
    ca, cb = (
        (int(a[i : i + 2], 16) for i in (1, 3, 5)),
        (int(b[i : i + 2], 16) for i in (1, 3, 5)),
    )
    return "#" + "".join(f"{round(x + (y - x) * t):02x}" for x, y in zip(ca, cb))


def render_svg(repo: str, series: list[Point], theme: str, today: dt.date) -> str:
    pal = THEMES[theme]
    buckets = month_buckets(series, today)
    top = buckets[-1].stars
    per_floor = stars_per_floor(top)
    gain = gain_since(series, today - dt.timedelta(days=7))
    win_x, win_bottom = MARGIN_LEFT, MARGIN_TOP + PLOT_ROWS * PX

    def col_x(c: int) -> int:
        return win_x + c * PX

    def row_y(r: int) -> int:
        """Top of row `r`, counted up from the ground."""
        return win_bottom - (r + 1) * PX

    band_h = PLOT_ROWS * PX / SKY_BANDS
    bands = [
        _mix(pal.sky_top, pal.sky_horizon, b / (SKY_BANDS - 1))
        for b in range(SKY_BANDS)
    ]
    sky = [
        f'<rect x="{win_x}" y="{MARGIN_TOP + round(b * band_h)}" width="{PLOT_COLS * PX}" height="{round((b + 1) * band_h) - round(b * band_h)}" fill="{c}"/>'
        for b, c in enumerate(bands)
    ]

    # Faint star-count rules on nice values, placed where that count's floor would stand.
    step = nice_step(max(per_floor * MAX_FLOORS, 1))
    rules, ylabels = [], []
    for n in range(0, per_floor * MAX_FLOORS + 1, step):
        y = win_bottom - round(n / per_floor * FLOOR_ROWS) * PX
        if n:
            rules += [
                _run(col_x(c), y, PX // 2, PX // 2) for c in range(0, PLOT_COLS, 3)
            ]
        label = compact(n)
        ylabels.append(
            text_path(
                label,
                win_x - LABEL_GAP - text_width(label, LABEL_PX),
                y - BASELINE * LABEL_PX // 2,
                LABEL_PX,
            )
        )

    mc, mr = MOON_AT
    moon = mask_path(MOON, col_x(mc), MARGIN_TOP + mr * PX, PX)
    bite = mask_path(MOON, col_x(mc + 2), MARGIN_TOP + (mr - 1) * PX, PX)

    lits = (pal.city_lit_a, pal.city_lit_b, pal.city_lit_c)
    shades = (pal.building_dark, _mix(pal.building_dark, pal.building_light, 0.3))
    bodies: dict[str, list[str]] = {}
    roofline, windows, scaffold, crane, beacon = [], {}, [], [], []
    roofs = [0] * PLOT_COLS
    for i, bucket in enumerate(buckets):
        c0, width = tower_cols(i, len(buckets))
        floors = bucket.stars // per_floor
        rows = floors * FLOOR_ROWS
        if rows:
            bodies.setdefault(shades[i % 2], []).append(
                _run(col_x(c0), row_y(rows - 1), width * PX, rows * PX)
            )
            roofline.append(_run(col_x(c0), row_y(rows - 1), width * PX, PX // 2))
        month = (bucket.end.year, bucket.end.month)
        for f in range(floors):
            r = f * FLOOR_ROWS
            for c in range(c0 + 1, c0 + width - 1, 2):
                lit = _hash("lit", month, f, c - c0) % 100 < LIT_PERCENT
                key = (
                    lits[_hash("hue", month, f, c - c0) % len(lits)]
                    if lit
                    else pal.city_dark_window
                )
                windows.setdefault(key, []).append(_run(col_x(c), row_y(r), PX, PX))
        roofs[c0 : c0 + width] = [rows] * width
        if i == len(buckets) - 1:
            sw = scaffold_cols(bucket.stars, per_floor, width)
            if sw:
                scaffold.append(
                    _run(col_x(c0), row_y(rows + FLOOR_ROWS - 1), sw * PX, PX // 2)
                )
                scaffold += [
                    _run(
                        col_x(c), row_y(rows + FLOOR_ROWS - 1), PX // 2, FLOOR_ROWS * PX
                    )
                    for c in range(c0, c0 + sw, 3)
                ]
                rows += FLOOR_ROWS
                roofs[c0 : c0 + width] = [rows] * width
            mast = c0 + width - 1 - CRANE_JIB[1]
            jib, top_row = max(mast - CRANE_JIB[0], 0), rows + CRANE_MAST_ROWS - 1
            crane += [
                _run(col_x(mast), row_y(top_row), PX, CRANE_MAST_ROWS * PX),
                _run(
                    col_x(jib), row_y(top_row), (mast + CRANE_JIB[1] + 1 - jib) * PX, PX
                ),
                _run(col_x(jib + 1), row_y(top_row - 1), PX // 2, CRANE_HOOK_ROWS * PX),
            ]
            beacon.append(_run(col_x(mast), row_y(top_row + 1), PX, PX))
            for c in range(jib, min(mast + CRANE_JIB[1] + 1, PLOT_COLS)):
                roofs[c] = max(roofs[c], top_row + 1)

    small: dict[int, list[str]] = {0: [], 1: [], 2: []}
    big = []
    for k, (c, r, is_big) in enumerate(sky_stars(roofs)):
        if is_big:
            big.append(
                _run(col_x(c) - PX, row_y(r), 3 * PX, PX)
                + _run(col_x(c), row_y(r) - PX, PX, 3 * PX)
            )
        else:
            small[k % 3].append(_run(col_x(c), row_y(r), PX // 2, PX // 2))

    frame = "".join(
        [
            _run(win_x - PX, MARGIN_TOP - PX, PLOT_COLS * PX + 2 * PX, PX),
            _run(win_x - PX, win_bottom, PLOT_COLS * PX + 2 * PX, PX),
            _run(win_x - PX, MARGIN_TOP, PX, PLOT_ROWS * PX),
            _run(win_x + PLOT_COLS * PX, MARGIN_TOP, PX, PLOT_ROWS * PX),
        ]
    )
    sill_y = win_bottom + PX
    labels = [b.label for b in buckets]
    xlabels = "".join(
        text_path(label, x, sill_y + 4 * PX, LABEL_PX)
        for x, label in zip(month_label_xs(labels), labels)
    )
    cat, cat_css = animation(
        load_pack(PACK_DIR, (CAT,)),
        CAT,
        win_x + PLOT_COLS * PX - 8 * PX,
        sill_y,
        CAT_PX,
    )

    head = header_layout(top, gain, per_floor)
    count = compact(top)
    edge = (
        _run(0, 0, WIDTH, PX)
        + _run(0, HEIGHT - PX, WIDTH, PX)
        + _run(0, 0, PX, HEIGHT)
        + _run(WIDTH - PX, 0, PX, HEIGHT)
    )
    css = "".join(
        [
            ".tw{animation:tw 2.4s steps(2,jump-none) infinite}.tw1{animation-delay:-.8s}.tw2{animation-delay:-1.6s}",
            "@keyframes tw{0%,100%{opacity:1}50%{opacity:.25}}",
            ".beacon{animation:bk 1.2s steps(1,end) infinite}@keyframes bk{0%{opacity:1}50%{opacity:.15}}",
            f".meteor{{opacity:0;animation:mt 7s steps(14,end) infinite}}@keyframes mt{{0%{{opacity:0;transform:translate(0,0)}}3%{{opacity:1}}12%{{opacity:0;transform:translate({METEOR_TRAVEL[0] * PX}px,{METEOR_TRAVEL[1] * PX}px)}}100%{{opacity:0}}}}",
            "@media (prefers-reduced-motion:reduce){.tw,.beacon,.meteor{animation:none}}",
            *cat_css,
        ]
    )
    return (
        "\n".join(
            [
                f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" width="{WIDTH}" height="{HEIGHT}" shape-rendering="crispEdges" role="img" aria-labelledby="t">',
                f'<title id="t">{repo} star history: {top} stars as of {today.isoformat()}, {per_floor} per floor</title>',
                f"<style>{css}</style>",
                f'<clipPath id="moon"><path d="{moon}"/></clipPath>',
                f'<clipPath id="upper"><rect x="{win_x}" y="{MARGIN_TOP}" width="{PLOT_COLS * PX}" height="{HEADROOM * PX}"/></clipPath>',
                f'<rect width="{WIDTH}" height="{HEIGHT}" fill="{pal.wall}"/>',
                f'<path fill="{pal.trim}" d="{edge}"/>',
                *sky,
                f'<path fill="{pal.text}" fill-opacity="0.22" d="{"".join(rules)}"/>',
                f'<path fill="{pal.moon}" d="{moon}"/><path fill="{bands[0]}" clip-path="url(#moon)" d="{bite}"/>',
                *(f'<path fill="{c}" d="{"".join(d)}"/>' for c, d in bodies.items()),
                f'<path fill="{pal.building_light}" d="{"".join(roofline)}"/>',
                *(f'<path fill="{c}" d="{"".join(d)}"/>' for c, d in windows.items()),
                f'<path fill="{pal.star}" fill-opacity="0.8" d="{"".join(scaffold)}"/>',
                f'<path fill="{pal.building_light}" d="{"".join(crane)}"/>',
                f'<path class="beacon" fill="{pal.star}" d="{"".join(beacon)}"/>',
                *(
                    f'<path class="tw tw{g}" fill="{pal.title}" d="{"".join(d)}"/>'
                    for g, d in small.items()
                ),
                f'<path class="tw tw2" fill="{pal.star}" d="{"".join(big)}"/>',
                f'<g clip-path="url(#upper)"><path class="meteor" fill="{pal.title}" d="{mask_path(METEOR, col_x(METEOR_AT[0]), MARGIN_TOP + METEOR_AT[1] * PX, PX)}"/></g>',
                f'<path fill="{pal.window_frame}" d="{frame}"/>',
                f'<path fill="{pal.trim}" d="{_run(win_x - 3 * PX, sill_y, PLOT_COLS * PX + 6 * PX, 2 * PX)}"/>',
                f'<path fill="{pal.text}" d="{"".join(ylabels)}{xlabels}"/>',
                cat,
                f'<path fill="{pal.star}" d="{mask_path(STAR_ICON, head.icon_x, HEADER_Y - 2, COUNT_PX)}"/>',
                f'<path fill="{pal.title}" d="{text_path(count, head.count_x, HEADER_Y, COUNT_PX)}"/>',
                f'<path fill="{pal.text}" d="{text_path(" stars", head.count_x + text_width(count, COUNT_PX), HEADER_Y + (COUNT_PX - LABEL_PX) * BASELINE, LABEL_PX)}"/>',
                f'<path fill="{pal.star}" d="{text_path(head.week, head.right_x - text_width(head.week, LABEL_PX), HEADER_Y - 2, LABEL_PX)}"/>',
                f'<path fill="{pal.text}" fill-opacity="0.75" d="{text_path(head.legend, head.right_x - text_width(head.legend, LABEL_PX), HEADER_Y - 2 + GLYPH_H * LABEL_PX, LABEL_PX)}"/>',
                "</svg>",
            ]
        )
        + "\n"
    )


def write_charts(
    repo: str, series: list[Point], out_dir: pathlib.Path, today: dt.date
) -> list[pathlib.Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    paths = []
    for theme in THEMES:
        p = out_dir / f"star-history-{theme}.svg"
        p.write_text(render_svg(repo, series, theme, today), encoding="utf-8")
        paths.append(p)
    return paths


def _graphql_post(query: str, variables: dict, token: str) -> dict:
    body = json.dumps({"query": query, "variables": variables}).encode()
    req = request.Request(
        GRAPHQL_URL,
        data=body,
        headers={
            "Authorization": f"bearer {token}",
            "Content-Type": "application/json",
            "User-Agent": "pixtuoid-star-history",
        },
    )
    with request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def fetch_star_dates(
    repo: str, token: str, post: Callable[[str, dict, str], dict] = _graphql_post
) -> list[dt.date]:
    owner, name = repo.split("/", 1)
    dates: list[dt.date] = []
    after: str | None = None
    while True:
        reply = post(
            STARGAZERS_QUERY,
            {"owner": owner, "name": name, "first": PAGE_SIZE, "after": after},
            token,
        )
        if reply.get("errors"):
            raise RuntimeError(
                f"GraphQL errors for {repo}: {json.dumps(reply['errors'])}"
            )
        repository = reply["data"]["repository"]
        page = repository["stargazers"]
        dates.extend(
            dt.datetime.fromisoformat(e["starredAt"]).date() for e in page["edges"]
        )
        if page["pageInfo"]["hasNextPage"]:
            after = page["pageInfo"]["endCursor"]
            continue
        # The announced deprecation shape is an EMPTY list, not an error; a blank
        # chart would replace the README's, so fewer rows than the public count reds the run.
        if len(dates) < repository["stargazerCount"]:
            raise RuntimeError(
                f"{repo}: stargazers returned {len(dates)} rows for stargazerCount {repository['stargazerCount']}"
            )
        return dates


def _d(s: str) -> dt.date:
    return dt.date.fromisoformat(s)


class SelfTest(unittest.TestCase):
    def test_cumulative_by_day_counts_one_point_per_day_in_order(self) -> None:
        dates = [_d("2026-05-24"), _d("2026-05-26"), _d("2026-05-24"), _d("2026-05-25")]
        self.assertTrue(
            cumulative_by_day(dates)
            == [(_d("2026-05-24"), 2), (_d("2026-05-25"), 3), (_d("2026-05-26"), 4)],
            "cumulative_by_day must sort, bucket per day, and accumulate",
        )
        self.assertTrue(
            cumulative_by_day([]) == [], "cumulative_by_day of nothing is nothing"
        )

    def test_compact_keeps_axis_labels_to_four_glyphs(self) -> None:
        cases = {
            0: "0",
            500: "500",
            1000: "1k",
            1500: "1.5k",
            20000: "20k",
            200000: "200k",
            1000000: "1M",
            1500000: "1.5M",
        }
        for n, want in cases.items():
            self.assertTrue(
                compact(n) == want, f"compact({n}) = {compact(n)!r}, want {want!r}"
            )
            self.assertTrue(
                len(compact(n)) <= 4, f"compact({n}) must fit the left margin"
            )

    def test_nice_step_yields_at_most_the_tick_budget(self) -> None:
        for top in (1, 7, 452, 1000, 12345):
            step = nice_step(top)
            ticks = top // step + 1
            self.assertTrue(
                1 <= ticks <= Y_TICKS + 1,
                f"nice_step({top})={step} gives {ticks} ticks, budget {Y_TICKS}",
            )
            mant = step / 10 ** (len(str(step)) - 1)
            self.assertTrue(
                mant in (1, 2, 5), f"nice_step({top})={step} is not a 1/2/5 step"
            )

    def test_glyphs_fill_the_cell(self) -> None:
        for ch, rows in GLYPHS.items():
            self.assertTrue(
                len(rows) == GLYPH_H and all(len(r) == GLYPH_W for r in rows),
                f"glyph {ch!r} is not {GLYPH_W}x{GLYPH_H}",
            )
            self.assertTrue(
                all(set(r) <= {"#", "."} for r in rows),
                f"glyph {ch!r} has a stray character",
            )

    def test_descenders_drop_below_the_shared_baseline(self) -> None:
        def lowest_lit(ch: str) -> int:
            return max(r for r, row in enumerate(GLYPHS[ch]) if "#" in row)

        base = lowest_lit("x")
        self.assertTrue(
            all(lowest_lit(ch) == base for ch in "aAx0"),
            "x-height letters, capitals and digits share one baseline",
        )
        self.assertTrue(
            all(lowest_lit(ch) > base for ch in "gjpqy"),
            "g j p q y must descend below that baseline, not sit on it like ρ",
        )

    def test_text_path_paints_every_glyph_pixel_and_nothing_else(self) -> None:
        runs = RUN_RE.findall(text_path("1", 10, 20, 2))
        lit = {
            (rx + dx, ry + dy)
            for rx, ry, w, h in (map(int, r) for r in runs)
            for dx in range(0, w, 2)
            for dy in range(0, h, 2)
        }
        want = {
            (10 + c * 2, 20 + r * 2)
            for r, row in enumerate(GLYPHS["1"])
            for c, ch in enumerate(row)
            if ch == "#"
        }
        self.assertTrue(
            lit == want,
            f"text_path('1') must light exactly the glyph's pixels at scale 2; diff {lit ^ want}",
        )
        self.assertTrue(
            text_width("ab", 3) == (2 * GLYPH_ADVANCE - 1) * 3,
            "text_width is advance per glyph minus the trailing gap, times scale",
        )

    def test_text_path_rejects_a_character_without_a_glyph(self) -> None:
        with self.assertRaises(
            KeyError, msg="text_path must refuse a character it cannot draw"
        ) as raised:
            text_path("é", 0, 0, 1)
        self.assertIn(
            "é", str(raised.exception), "the offending character must be named"
        )

    def test_gain_since_counts_the_last_week(self) -> None:
        series = [
            (_d("2026-08-01"), 10),
            (_d("2026-08-17"), 15),
            (_d("2026-08-20"), 21),
        ]
        self.assertTrue(
            gain_since(series, _d("2026-08-16")) == 11,
            f"gain since 08-16 is 21 - 10, got {gain_since(series, _d('2026-08-16'))}",
        )
        self.assertTrue(
            gain_since(series, _d("2026-08-17")) == 6,
            "a star on the cut-off day itself is not new",
        )
        self.assertTrue(
            gain_since(series, _d("2026-07-01")) == 21,
            "a series younger than the window gains all of it",
        )
        self.assertTrue(gain_since([], _d("2026-08-16")) == 0, "no stars, no gain")

    def test_buckets_roll_the_last_twelve_months(self) -> None:
        series = cumulative_by_day(
            [_d("2025-03-10")] * 5 + [_d("2026-01-15")] * 3 + [_d("2026-08-20")]
        )
        got = month_buckets(series, _d("2026-08-23"))
        self.assertTrue(
            len(got) == MONTHS,
            f"a long history shows the last {MONTHS} months, got {len(got)}",
        )
        self.assertTrue(
            got[0].end == _d("2025-09-30") and got[-1].end == _d("2026-08-23"),
            f"window runs Sep 2025 to today: {got[0]}, {got[-1]}",
        )
        self.assertTrue(
            [b.stars for b in got] == [5] * 4 + [8] * 7 + [9],
            f"each month carries the cumulative count at its end: {[b.stars for b in got]}",
        )
        self.assertTrue(
            got[4].label == "2026" and got[3].label == "Dec",
            f"January is labelled with its year: {[b.label for b in got]}",
        )
        young = month_buckets(cumulative_by_day([_d("2026-06-02")]), _d("2026-08-23"))
        self.assertTrue(
            [b.label for b in young] == ["Jun", "Jul", "Aug"],
            f"a young repo starts at its first star's month: {young}",
        )
        empty = month_buckets([], _d("2026-08-23"))
        self.assertTrue(
            len(empty) == 1 and empty[0].stars == 0,
            f"no stars still draws this month's empty lot: {empty}",
        )

    def test_stars_per_floor_keeps_the_city_under_the_ceiling(self) -> None:
        for top in (0, 1, 19, 20, 21, 491, 6000, 123_456, 10**6):
            n = stars_per_floor(top)
            self.assertTrue(
                -(-top // n) <= MAX_FLOORS,
                f"{top} stars at {n}/floor overflows {MAX_FLOORS} floors",
            )
            smaller = [m for m in floor_ladder(n) if m < n]
            self.assertTrue(
                not smaller or -(-top // smaller[-1]) > MAX_FLOORS,
                f"{top} stars: {smaller[-1] if smaller else None}/floor would fit too, so {n} is not the smallest",
            )

    def test_scaffold_is_the_earned_share_of_the_next_floor(self) -> None:
        self.assertTrue(
            scaffold_cols(0, 25, 20) == 0 and scaffold_cols(50, 25, 20) == 0,
            "a whole number of floors needs no scaffold",
        )
        self.assertTrue(
            scaffold_cols(491, 25, 20) == 13,
            f"16 of 25 stars is 13 of 20 columns, got {scaffold_cols(491, 25, 20)}",
        )
        self.assertTrue(scaffold_cols(1, 25, 20) == 1, "one star still shows a post")

    def test_month_labels_never_touch_and_stay_on_the_canvas(self) -> None:
        for n in range(1, MONTHS + 1):
            labels = ["2027" if i % 5 == 0 else "Sep" for i in range(n)]
            boxes = [
                (x, x + text_width(label, LABEL_PX))
                for x, label in zip(month_label_xs(labels), labels)
            ]
            self.assertTrue(
                all(0 <= a and b <= WIDTH for a, b in boxes),
                f"{n} months: a label leaves the canvas: {boxes}",
            )
            self.assertTrue(
                all(boxes[i + 1][0] - boxes[i][1] >= PX for i in range(n - 1)),
                f"{n} months: labels touch: {boxes}",
            )
            for i, (a, b) in enumerate(boxes):
                c0, width = tower_cols(i, n)
                tower_mid = MARGIN_LEFT + c0 * PX + width * PX / 2
                self.assertTrue(
                    abs((a + b) / 2 - tower_mid) <= 1,
                    f"{n} months: label {i} centres at {(a + b) / 2}, its tower at {tower_mid}",
                )

    def test_stars_stay_in_open_sky(self) -> None:
        ramp = [min(c // 2, PLOT_ROWS - HEADROOM) for c in range(PLOT_COLS)]
        alleys = [
            0 if c % 9 < 2 else (PLOT_ROWS - HEADROOM) * (c // 9 % 3) // 2
            for c in range(PLOT_COLS)
        ]
        for roofs in (ramp, alleys):
            stars = sky_stars(roofs)
            self.assertTrue(len(stars) > 10, f"the sky needs stars, got {len(stars)}")
            for c, r, big in stars:
                under = roofs[max(c - 1, 0) : c + 2] if big else [roofs[c]]
                self.assertTrue(
                    r > max(under) + STAR_CLEARANCE,
                    f"a {'big' if big else 'small'} star at ({c}, {r}) paints over a roof",
                )
        stars = sky_stars(ramp)
        mc, mr = MOON_AT
        self.assertTrue(
            not any(
                mc - 2 <= c <= mc + len(MOON[0]) + 1
                and PLOT_ROWS - 1 - r <= mr + len(MOON)
                for c, r, _ in stars
            ),
            "no star on the moon",
        )
        self.assertTrue(
            PLOT_ROWS - mr - len(MOON) >= PLOT_ROWS - HEADROOM,
            "the moon hangs above the tallest tower",
        )

    def test_meteor_crosses_open_sky_only(self) -> None:
        (mc, mr), (dc, dr) = METEOR_AT, METEOR_TRAVEL
        self.assertTrue(
            mr + len(METEOR) + dr <= HEADROOM,
            "the meteor's run must stay above the tallest tower",
        )
        self.assertTrue(
            mc > MOON_AT[0] + len(MOON[0]) and mc + len(METEOR[0]) + dc <= PLOT_COLS,
            "the meteor starts right of the moon and ends inside the window",
        )

    def test_the_crane_tops_out_under_the_frame(self) -> None:
        self.assertTrue(
            MAX_FLOORS * FLOOR_ROWS + CRANE_MAST_ROWS < PLOT_ROWS,
            "the beacon on the tallest tower's crane must stay inside the window",
        )
        full = cumulative_by_day([_d("2026-08-01")] * (MAX_FLOORS * 25 - 3))
        svg = render_svg("o/r", full, "light", _d("2026-08-23"))
        beacon = re.search(r'class="beacon"[^>]* d="([^"]*)"', svg)
        ys = [int(r[1]) for r in RUN_RE.findall(beacon.group(1))] if beacon else []
        self.assertTrue(
            bool(ys) and min(ys) >= MARGIN_TOP,
            f"the beacon must sit below the window's top frame, at {ys}",
        )

    def test_live_tower_windows_hold_still_within_a_month(self) -> None:
        series = cumulative_by_day([_d("2026-09-10")] * 40 + [_d("2026-10-02")] * 30)

        def windows(today: dt.date) -> list[str]:
            svg = render_svg("o/r", series, "light", today)
            hues = {
                THEMES["light"].city_dark_window,
                THEMES["light"].city_lit_a,
                THEMES["light"].city_lit_b,
                THEMES["light"].city_lit_c,
            }
            return sorted(
                m.group(0)
                for m in re.finditer(r'<path fill="(#[0-9a-f]{6})" d="[^"]*"/>', svg)
                if m.group(1) in hues
            )

        self.assertTrue(
            windows(_d("2026-10-09")) == windows(_d("2026-10-23")),
            "a day with no new stars must not reshuffle this month's lit windows",
        )

    def test_header_halves_never_collide(self) -> None:
        for top, gain in ((0, 0), (491, 6), (123_456, 12_345), (10**6, 10**6)):
            h = header_layout(top, gain, stars_per_floor(top))
            self.assertTrue(
                h.count_end + LABEL_GAP <= h.legend_left,
                f"{top} stars, +{gain}: the count ends at {h.count_end}, the legend starts at {h.legend_left}",
            )

    def test_render_is_well_formed_and_carries_the_facts(self) -> None:
        series = [
            (_d("2026-05-24"), 2),
            (_d("2026-06-14"), 300),
            (_d("2026-08-02"), 452),
        ]
        for theme in THEMES:
            svg = render_svg("pixtuoid/pixtuoid", series, theme, _d("2026-08-23"))
            root = ET.fromstring(svg)
            title = root.findtext(f"{SVG_NS}title") or ""
            self.assertTrue(
                "pixtuoid/pixtuoid" in title
                and "452" in title
                and "2026-08-23" in title,
                f"{theme}: <title> must carry repo, count and date; got {title!r}",
            )
            self.assertTrue(
                THEMES[theme].star in svg and THEMES[theme].wall in svg,
                f"{theme}: the office palette must be painted",
            )
            self.assertTrue(
                root.get("shape-rendering") == "crispEdges",
                f"{theme}: pixels must not be anti-aliased",
            )
        light = render_svg("pixtuoid/pixtuoid", series, "light", _d("2026-08-23"))
        self.assertTrue(
            light == render_svg("pixtuoid/pixtuoid", series, "light", _d("2026-08-23")),
            "render must be deterministic",
        )

    def test_render_paints_only_inside_the_canvas(self) -> None:
        cases = (
            [(_d("2026-05-24"), 1)],
            [(_d("2020-01-01"), 1), (_d("2026-08-02"), 12345)],
            [(_d("2026-08-23"), 1234)],
            [(_d("2026-08-22"), 1), (_d("2026-08-23"), 2)],
            [],
        )
        for series in cases:
            svg = render_svg(
                "pixtuoid/pixtuoid_with-a.long-name", series, "light", _d("2026-08-23")
            )
            runs = [tuple(map(int, r)) for r in RUN_RE.findall(svg)]
            self.assertTrue(bool(runs), "the chart must be painted as rectangle runs")
            outside = [
                r
                for r in runs
                if r[0] < 0 or r[1] < 0 or r[0] + r[2] > WIDTH or r[1] + r[3] > HEIGHT
            ]
            self.assertTrue(
                not outside,
                f"{series[:1]}…: runs leave the {WIDTH}x{HEIGHT} canvas: {outside[:3]}",
            )

    def test_render_handles_a_repo_with_no_stars(self) -> None:
        svg = render_svg("o/r", [], "light", _d("2026-08-23"))
        root = ET.fromstring(svg)
        self.assertTrue(
            "0 stars" in (root.findtext(f"{SVG_NS}title") or ""),
            "empty series must still state a zero count",
        )

    def test_fetch_follows_cursors_until_the_last_page(self) -> None:
        pages = [
            {
                "data": {
                    "repository": {
                        "stargazerCount": 3,
                        "stargazers": {
                            "edges": [
                                {"starredAt": "2026-05-24T04:01:48Z"},
                                {"starredAt": "2026-05-24T07:07:52Z"},
                            ],
                            "pageInfo": {"hasNextPage": True, "endCursor": "c1"},
                        },
                    }
                }
            },
            {
                "data": {
                    "repository": {
                        "stargazerCount": 3,
                        "stargazers": {
                            "edges": [{"starredAt": "2026-05-25T00:00:00Z"}],
                            "pageInfo": {"hasNextPage": False, "endCursor": "c2"},
                        },
                    }
                }
            },
        ]
        seen: list[dict] = []

        def post(query: str, variables: dict, token: str) -> dict:
            seen.append(variables)
            return pages[len(seen) - 1]

        got = fetch_star_dates("o/r", "t", post=post)
        self.assertTrue(
            got == [_d("2026-05-24"), _d("2026-05-24"), _d("2026-05-25")],
            f"fetch must flatten pages to dates, got {got}",
        )
        self.assertTrue(
            [v.get("after") for v in seen] == [None, "c1"],
            f"fetch must thread the cursor, sent {seen}",
        )
        self.assertTrue(
            seen[0]["owner"] == "o" and seen[0]["name"] == "r",
            "fetch must split owner/name",
        )

    def test_fetch_refuses_a_withheld_list(self) -> None:
        def post(query: str, variables: dict, token: str) -> dict:
            return {
                "data": {
                    "repository": {
                        "stargazerCount": 452,
                        "stargazers": {
                            "edges": [],
                            "pageInfo": {"hasNextPage": False, "endCursor": None},
                        },
                    }
                }
            }

        with self.assertRaises(
            RuntimeError,
            msg="an empty list under a non-zero stargazerCount is the deprecation shape — it must raise, not publish a blank chart",
        ) as raised:
            fetch_star_dates("o/r", "t", post=post)
        self.assertTrue(
            "452" in str(raised.exception) and "0" in str(raised.exception),
            f"the message must show count vs rows: {raised.exception}",
        )

    def test_fetch_raises_on_graphql_errors(self) -> None:
        def post(query: str, variables: dict, token: str) -> dict:
            return {
                "errors": [
                    {
                        "type": "NOT_FOUND",
                        "message": "Could not resolve to a Repository",
                    }
                ]
            }

        with self.assertRaises(
            RuntimeError, msg="fetch must raise on a GraphQL errors array"
        ) as raised:
            fetch_star_dates("o/r", "t", post=post)
        self.assertIn(
            "NOT_FOUND",
            str(raised.exception),
            "the GraphQL error type must reach the message",
        )

    def test_write_charts_emits_one_file_per_theme(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = pathlib.Path(tmp) / "nested"
            paths = write_charts("o/r", [(_d("2026-05-24"), 1)], out, _d("2026-08-23"))
            self.assertTrue(
                sorted(p.name for p in paths)
                == sorted(f"star-history-{t}.svg" for t in THEMES),
                f"got {paths}",
            )
            self.assertTrue(
                all(p.exists() and p.stat().st_size > 0 for p in paths),
                "every chart must be written non-empty",
            )

    def test_main_needs_a_token(self) -> None:
        with mock.patch.dict(os.environ, clear=True):
            self.assertEqual(
                main(["o/r", "out"]), 2, "no token must exit 2, before any request"
            )


SVG_NS = "{http://www.w3.org/2000/svg}"
# One `_run` rectangle; every path here is built from them.
RUN_RE = re.compile(r"M(-?\d+) (-?\d+)h(\d+)v(\d+)h-\d+z")


def main(argv: list[str]) -> int:
    if "--selftest" in argv:
        unittest.main(argv=sys.argv[:1])
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    repo, out_dir = argv
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        print("star-history: GH_TOKEN (or GITHUB_TOKEN) is unset", file=sys.stderr)
        return 2
    dates = fetch_star_dates(repo, token)
    series = cumulative_by_day(dates)
    today = dt.datetime.now(dt.UTC).date()
    for p in write_charts(repo, series, pathlib.Path(out_dir), today):
        print(p)
    print(f"{repo}: {len(dates)} stars", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
