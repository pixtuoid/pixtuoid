#!/usr/bin/env python3
"""Crop a snapshot PNG into per-quadrant images for visual inspection.

Usage:
    .venv/bin/python3 scripts/crop-snapshot.py /tmp/snap.png          # 2× scale
    .venv/bin/python3 scripts/crop-snapshot.py /tmp/snap.png --scale 3

Outputs four files next to the input:
    <name>_meeting.png   — top-left (meeting room + sofas)
    <name>_pantry.png    — bottom-left (pantry + floor seats)
    <name>_cubicle.png   — top-right (cubicle pods)
    <name>_corridor.png  — bottom-right (walkway + baseboard)

Setup (once):
    python3 -m venv .venv && .venv/bin/pip install -r requirements-dev.txt
"""

import argparse
from pathlib import Path

from PIL import Image

QUADRANTS = {
    "meeting": (0.00, 0.00, 0.30, 0.55),
    "pantry": (0.00, 0.49, 0.30, 1.00),
    "cubicle": (0.30, 0.00, 1.00, 0.55),
    "corridor": (0.30, 0.70, 1.00, 1.00),
}


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("input", type=Path, help="Snapshot PNG to crop")
    parser.add_argument(
        "--scale",
        type=int,
        default=2,
        help="Nearest-neighbor upscale factor (default 2)",
    )
    parser.add_argument(
        "--quadrant",
        "-q",
        choices=list(QUADRANTS) + ["all"],
        default="all",
        help="Which quadrant to crop (default: all)",
    )
    args = parser.parse_args()

    with Image.open(args.input) as img:
        img.load()
    w, h = img.size
    stem = args.input.stem
    out_dir = args.input.parent

    quads = (
        QUADRANTS
        if args.quadrant == "all"
        else {args.quadrant: QUADRANTS[args.quadrant]}
    )
    for name, (x0, y0, x1, y1) in quads.items():
        crop = img.crop((int(w * x0), int(h * y0), int(w * x1), int(h * y1)))
        crop = crop.resize(
            (crop.width * args.scale, crop.height * args.scale),
            Image.Resampling.NEAREST,
        )
        out = out_dir / f"{stem}_{name}.png"
        crop.save(out)
        print(f"  {out}  ({crop.width}×{crop.height})")


if __name__ == "__main__":
    main()
