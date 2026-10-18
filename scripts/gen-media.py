#!/usr/bin/env python3
"""Regenerate the office media from a release `snapshot` build: the README's
committed images and the site's CI-rendered demos.

Single source of truth for BOTH surfaces' render media. Every render job lives
in scripts/media.json; this driver builds the binary once and runs each job,
writing to docs/images/ and/or site/public/demos/ per the job's `targets`.
Theme/weather lists are read from site/src/{themes,weather}.json (`@themes.json`
/ `@weather.json` refs) so they are never duplicated.

  just gen-media           # regenerate everything
  just gen-media --only docs   # docs/images/ only
  just gen-media-check     # → gen-media.py --check --only docs (evidence on a PR)

--check renders to a temp dir and pixel-diffs every committed PNG of the
selected targets (scripts/compare-screenshots.py); video clips (.mp4/.webm) and the animated
demo.gif are presence-checked only, since ffmpeg/gifsicle output is not
byte-stable across versions. Exits non-zero on any drift.

Requires the .venv (Pillow) + ffmpeg + gifsicle for a full run (`--check` needs
neither). Run via `.venv/bin/python3`.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
SNAP = ROOT / "target/release/examples/snapshot"
SITE_SRC = ROOT / "site/src"
MANIFEST = ROOT / "scripts/media.json"
COMPARE = ROOT / "scripts/compare-screenshots.py"

TARGET_DIRS = {
    "docs": ROOT / "docs/images",
    "site": ROOT / "site/public/demos",
}
# Outside the temp dir so --check's diff overlays survive for REVIEW.md's
# Render and look.
DIFF_DIR = ROOT / "target/gen-check-diff"
# --check must REGENERATE the proof posters (pixel-gated stills) but skip the
# video encodes (presence-gated, non-byte-stable) — handlers read this flag.
CHECK_MODE = False
# A live-agent capture: committed but not generated here, so --check must never
# compare it.
NOT_GENERATED = {"screenshot-real.png"}
# The snapshot's `--frames-dir` naming (its `FrameSink::Pngs`).
FRAME_PATTERN = "f%04d.png"


def build_once():
    """cargo no-ops when fresh; a stale binary silently renders outdated art."""
    subprocess.run(
        ["cargo", "build", "--release", "--example", "snapshot"], cwd=ROOT, check=True
    )


def expand_ref(ref):
    return json.loads((SITE_SRC / ref[1:]).read_text(encoding="utf-8"))


def snap(out_path, *, cols, rows, hour, day=None, theme=None, weather=None,
         extra=(), gif=None, frames_dir=None):
    cmd = [str(SNAP), "--cols", str(cols), "--rows", str(rows), "--now-hour", str(hour)]
    if day is not None:
        cmd += ["--now-day", str(day)]
    if theme is not None:
        cmd += ["--theme", theme]
    if weather is not None:
        cmd += ["--weather", weather]
    if gif is not None:
        cmd += ["--gif", "--gif-duration", str(gif["duration"]), "--gif-fps", str(gif["fps"])]
    cmd += list(extra)
    cmd += ["--frames-dir", str(frames_dir)] if frames_dir is not None else [str(out_path)]
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", *args], check=True)


def run_render(job, out_dirs, work, intermediates):
    if "frames" in job:
        for f in job["frames"]:
            for d in out_dirs:
                snap(d / f"{f['name']}.png", cols=job["cols"], rows=job["rows"],
                     hour=f["hour"], day=job.get("day"), theme=f.get("theme"),
                     weather=f.get("weather"))
        return

    raw = work / f"{job['id']}_raw.png"
    snap(raw, cols=job["cols"], rows=job["rows"], hour=job["hour"], day=job.get("day"),
         theme=job.get("theme"), weather=job.get("weather"), extra=job.get("extra", ()))
    intermediates[job["id"]] = raw  # crops read the unscaled render

    scale = job.get("scale")
    for d in out_dirs:
        dst = d / f"{job['id']}.png"
        if scale:
            with Image.open(raw) as im:
                img = im.convert("RGB")
            img.resize((img.width * scale, img.height * scale), Image.NEAREST).save(dst)
        else:
            shutil.copyfile(raw, dst)


def crop_box(img, spec, job_id):
    """`img` cropped to `spec` ("W:H:X:Y"); a box past the image edge exits,
    where Pillow would pad it with black."""
    w, h, x, y = map(int, spec.split(":"))
    if x < 0 or y < 0 or x + w > img.width or y + h > img.height:
        sys.exit(f"gen-media: job '{job_id}' crop {spec} falls outside its "
                 f"{img.width}x{img.height} source")
    return img.crop((x, y, x + w, y + h))


def run_crop(job, out_dirs, work, intermediates):
    src = intermediates.get(job["from"])
    if src is None:
        sys.exit(
            f"gen-media: crop job '{job['id']}' needs its source render '{job['from']}' "
            f"— include it, e.g. --jobs {job['from']},{job['id']}"
        )
    with Image.open(src) as im:
        img = im.convert("RGB")
    if "quadrants" in job:
        w, h = img.size
        scale = job.get("scale", 1)
        for name, (x0, y0, x1, y1) in job["quadrants"].items():
            crop = img.crop((int(w * x0), int(h * y0), int(w * x1), int(h * y1)))
            out = crop.resize((crop.width * scale, crop.height * scale), Image.NEAREST)
            for d in out_dirs:
                out.save(d / f"{job['id']}-{name}.png")
    else:
        for key, spec in job["crops"].items():
            out = crop_box(img, spec, job["id"])
            for d in out_dirs:
                out.save(d / f"{job['id']}_{key}.png")


def run_composite(job, out_dirs, work, intermediates):
    themes = [t["id"] for t in expand_ref(job["over"])]
    slant = job["slant"]
    paths = []
    for i, theme in enumerate(themes):
        p = work / f"composite_{i}.png"
        snap(p, cols=job["cols"], rows=job["rows"], hour=job["hour"], day=job.get("day"),
             theme=theme)
        paths.append(p)

    with Image.open(paths[0]) as im:
        comp = im.convert("RGB")
    w, h = comp.size
    n = len(themes)
    half = h / 2
    far = w + abs(slant) * h + 10

    def boundary(k, y):  # x of the k-th band boundary at row y (centre-anchored)
        return k * w / n + slant * (y - half)

    for i in range(n):
        with Image.open(paths[i]) as src:
            im = src.convert("RGB")
        lt = -far if i == 0 else boundary(i, 0)
        lb = -far if i == 0 else boundary(i, h)
        rt = far if i == n - 1 else boundary(i + 1, 0)
        rb = far if i == n - 1 else boundary(i + 1, h)
        mask = Image.new("L", (w, h), 0)
        ImageDraw.Draw(mask).polygon([(lt, 0), (rt, 0), (rb, h), (lb, h)], fill=255)
        comp.paste(im, (0, 0), mask)
    for d in out_dirs:
        comp.save(d / "themes-composite.png")


def run_gif(job, out_dirs, work, intermediates):
    if not shutil.which("gifsicle"):
        sys.exit("gifsicle not found — brew install gifsicle")
    for d in out_dirs:
        dst = d / f"{job['id']}.gif"
        snap(dst, cols=job["cols"], rows=job["rows"], hour=job["hour"], day=job.get("day"),
             theme=job.get("theme"), gif={"duration": job["duration"], "fps": job["fps"]})
        # NOT --lossy: it breaks gifsicle's inter-frame diff and ships a bigger file.
        subprocess.run(
            ["gifsicle", "-b", "-O3", "--colors", str(job["colors"]), str(dst)], check=True
        )


def run_matrix(job, out_dirs, work, intermediates):
    items = [x["id"] for x in expand_ref(job["over"])]
    axis = job["axis"]  # "theme" | "weather"
    for item in items:
        for d in out_dirs:
            kwargs = {"theme": item} if axis == "theme" else {"weather": item}
            snap(d / f"{axis}_{item}.png", cols=job["cols"], rows=job["rows"],
                 hour=job["hour"], **kwargs)


# H.264 and VP9 both require even width/height. `neighbor` keeps the art's hard
# pixel edges, where swscale's default is bicubic. Set in `flags`, which
# ffmpeg-scaler deprecates for `scaler`: that option exists only from FFmpeg
# 9.0 (3503b19711), and the apt ffmpeg a CI runner has is older.
SCALE_EVEN = "scale=trunc(iw/2)*2:trunc(ih/2)*2:flags=neighbor"
# VP9 constant-quality knob (with `-b:v 0`, no target bitrate).
VP9_CRF = "36"
# Without it the muxers stamp every file with ffmpeg's version (MP4's `©too`
# atom, Matroska's MuxingApp/WritingApp) and Matroska gives each track a random
# TrackUID (libavformat movenc.c, matroskaenc.c), so a regen whose frames did not
# change still re-commits every clip.
BITEXACT = ("-fflags", "+bitexact")


def encode_mp4_webm(frames_glob, fps, vf, out_stem):
    """Encode a FRAME_PATTERN sequence at `fps`, through the `vf` filter, to
    BOTH `{out_stem}.mp4` and `{out_stem}.webm`."""
    ffmpeg("-framerate", str(fps), "-i", frames_glob,
           "-movflags", "+faststart", "-pix_fmt", "yuv420p", *BITEXACT, "-vf", vf,
           f"{out_stem}.mp4")
    ffmpeg("-framerate", str(fps), "-i", frames_glob,
           "-c:v", "libvpx-vp9", "-b:v", "0", "-crf", VP9_CRF, "-row-mt", "1",
           "-pix_fmt", "yuv420p", *BITEXACT, "-vf", vf,
           f"{out_stem}.webm")


def poster_frame(frames_dir, job):
    """The frame `job["poster"]` seconds in, else the first: a staged clip whose
    opening seconds are pre-action posters on the money shot."""
    return frames_dir / (FRAME_PATTERN % (int(round(job.get("poster", 0) * job["fps"], 6)) + 1))


def run_clip(job, out_dirs, work, intermediates):
    fps = job["fps"]
    cid = job["id"]
    frames = work / f"frames-{cid}"
    snap(None, cols=job["cols"], rows=job["rows"], hour=job["hour"],
         extra=job.get("extra", ()), gif={"duration": job["duration"], "fps": fps},
         frames_dir=frames)
    # Optional `crop` ("W:H:X:Y", in the unscaled render's px space) frames
    # a close-up on a fixed region. NB: this singular clip-level `crop` is
    # unrelated to the separate kind:"crop" job (run_crop), which reads a plural
    # `crops` dict off a `from` render — different mechanism, different key.
    crop = job.get("crop")
    vf = f"crop={crop},{SCALE_EVEN}" if crop else SCALE_EVEN
    # RGB, not a copy of the frame: the snapshot writes opaque RGBA, far larger.
    with Image.open(poster_frame(frames, job)) as im:
        poster = im.convert("RGB")
    if crop:
        poster = crop_box(poster, crop, cid)
    for d in out_dirs:
        encode_mp4_webm(str(frames / FRAME_PATTERN), fps, vf, str(d / cid))
        poster.save(d / f"{cid}-poster.png")


def run_wasm_still(job, out_dirs, work, intermediates):
    # The live-office backdrop's poster: a REAL frame of the pixtuoid-web Office,
    # in the same buffer geometry the live canvas computes, so the poster→canvas
    # crossfade dissolves in place instead of reframing. Deterministic per
    # (t0_ms, advance_ms) under the TZ=UTC pin, so a re-render reproduces it; it is
    # site-only, so no gate compares it.
    subprocess.run(
        ["cargo", "build", "--release", "-p", "pixtuoid-web", "--example", "hero_still"],
        check=True,
        cwd=ROOT,
    )
    # `t0_ms` (an exact epoch) and `hour` (a 0-23 convenience) are mutually
    # exclusive time pins; hero_still prefers `--t0-ms` if both are given.
    extra = []
    if "t0_ms" in job:
        extra += ["--t0-ms", str(job["t0_ms"])]
    if "hour" in job:
        extra += ["--hour", str(job["hour"])]
    if "weather" in job:
        extra += ["--weather", str(job["weather"])]
    # Layout seed: must match the LIVE canvas this poster falls back for, else
    # the poster's office layout pops when the live canvas paints over it.
    if "seed" in job:
        extra += ["--seed", str(job["seed"])]
    for d in out_dirs:
        subprocess.run(
            [str(ROOT / "target/release/examples/hero_still"),
             str(d / f"{job['id']}.png"),
             "--width", str(job["w"]), "--height", str(job["h"]),
             "--advance-ms", str(job["advance_ms"]),
             *extra],
            check=True,
        )


def run_proof(job, out_dirs, work, intermediates):
    # ONE snapshot --proof pass renders BOTH compositions (wide + tall) from the
    # same fixture-driven reducer replay; the poster is a designated frame COPY,
    # so it stays pixel-gated while the encodes stay presence-only.
    fps = job["fps"]
    # In --check only the poster frame matters: rendering a deterministic PREFIX
    # of the timeline (through the poster frame) yields the identical poster.
    secs = int(job["poster"]) + 2 if CHECK_MODE else job["duration"]
    frames = work / "proof-frames"
    subprocess.run(
        [str(SNAP), "--proof", str(ROOT / job["fixture"]),
         "--frames-dir", str(frames),
         "--proof-fps", str(fps), "--proof-secs", str(secs),
         "--cols", str(job["cols"]), "--rows", str(job["rows"]),
         "--now-hour", str(job["hour"]), "--theme", job["theme"], "--weather", "clear",
         *(["--now-day", str(job["day"])] if "day" in job else [])],
        check=True, stdout=subprocess.DEVNULL,
    )
    for layout, suffix in (("wide", ""), ("tall", "-tall")):
        ldir = frames / layout
        for d in out_dirs:
            shutil.copyfile(poster_frame(ldir, job),
                            d / f"{job['id']}{suffix}-poster.png")
            if CHECK_MODE:
                continue
            encode_mp4_webm(str(ldir / FRAME_PATTERN), fps, SCALE_EVEN,
                            str(d / f"{job['id']}{suffix}"))


HANDLERS = {
    "render": run_render,
    "crop": run_crop,
    "composite": run_composite,
    "gif": run_gif,
    "matrix": run_matrix,
    "clip": run_clip,
    "wasm-still": run_wasm_still,
    "proof": run_proof,
}


def _presence_only_names(manifest, target):
    """Filenames owned by clip/gif jobs for `target` — outputs --check skips
    rendering (their encodes aren't byte-stable cross-version), so they're
    presence-checked, never pixel-gated. Derived from the MANIFEST, not a name-shape rule: a
    `-poster.png` suffix rule silently exempted a deterministic render still that
    merely kept a poster name when its job flipped clip→render."""
    return {
        name
        for job in manifest
        if target in job["targets"]
        for name in _expected_presence_outputs(job)
    }


def _expected_presence_outputs(job):
    """Committed filenames a presence-only (clip/gif) job owns."""
    if job["kind"] == "gif":
        return [f"{job['id']}.gif"]
    if job["kind"] == "clip":
        return [f"{job['id']}.mp4", f"{job['id']}.webm", f"{job['id']}-poster.png"]
    if job["kind"] == "proof":
        return [f"{job['id']}{s}.{ext}" for s in ("", "-tall") for ext in ("mp4", "webm")]
    return []


def run_check(out_base, work, manifest, only=None):
    """Pixel-diff every committed STILL against a fresh render; presence-check the
    clip/gif jobs' outputs (clips, gif, clip posters) without regenerating them."""
    failures = []
    DIFF_DIR.mkdir(parents=True, exist_ok=True)
    for target, tdir in out_base.items():
        if only and target != only:
            continue
        committed_dir = TARGET_DIRS[target]
        produced = {p.name for p in tdir.iterdir() if p.is_file()}
        presence_only = _presence_only_names(manifest, target)

        for c in sorted((p for p in committed_dir.iterdir() if p.is_file()), key=lambda p: p.name):
            name = c.name
            if name in NOT_GENERATED:
                continue
            if name in presence_only:
                print(f"  present (not pixel-gated): {target}/{name}")
                continue
            if name not in produced:
                failures.append(f"NOT REGENERATED: {target}/{name}")
                continue
            diff = DIFF_DIR / f"diff-{target}-{name}"
            rc = subprocess.run(
                [sys.executable, str(COMPARE), str(c), str(tdir / name), str(diff)]
            ).returncode
            if rc != 0:
                failures.append(f"PIXEL DRIFT: {target}/{name} (compare rc={rc})")

        for name in sorted(produced):
            if not (committed_dir / name).exists():
                failures.append(f"NEW (uncommitted) output: {target}/{name}")

    # Presence-only outputs are skipped in --check, so the committed-dir walk
    # above can't catch one that's MISSING; assert them from the MANIFEST.
    for job in manifest:
        for t in job["targets"]:
            if only and t != only:
                continue
            for name in _expected_presence_outputs(job):
                if not (TARGET_DIRS[t] / name).exists():
                    failures.append(f"MISSING (per manifest): {t}/{name}")

    print()
    if failures:
        print(f"\033[31mgen-check FAILED — {len(failures)} issue(s):\033[0m")
        for x in failures:
            print(f"  ✗ {x}")
        return 1
    print("\033[32mgen-media --check OK.\033[0m")
    return 0


def main():
    ap = argparse.ArgumentParser(description="Regenerate office media from scripts/media.json")
    ap.add_argument("--check", action="store_true",
                    help="render to a temp dir and diff vs committed; write nothing")
    ap.add_argument("--only", choices=["docs", "site"], help="restrict to one surface")
    ap.add_argument("--jobs", help="comma-separated job ids to run (default: all)")
    args = ap.parse_args()
    global CHECK_MODE
    CHECK_MODE = args.check

    # run_check walks the FULL committed tree, so every still owned by a
    # filtered-out job would report "NOT REGENERATED" — spurious failures.
    if args.check and args.jobs:
        ap.error("--check renders everything and walks the full committed tree; "
                 "--jobs cannot be combined with it (run --check without --jobs)")

    # Pin TZ=UTC for EVERY render (inherited by every subprocess) so the office's
    # epoch-derived weather slot + lighting phase — snapshot reads --now-hour as a
    # chrono::Local wall time — are machine-independent. Without it a dev box and
    # the UTC CI runner render different frames and gen-check calls that drift.
    os.environ["TZ"] = "UTC"

    only_jobs = set(args.jobs.split(",")) if args.jobs else None

    # Validate --jobs BEFORE the release build: an unknown id would otherwise be
    # a silent no-op that still prints "wrote media → …".
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if only_jobs:
        known = {j["id"] for j in manifest}
        unknown = sorted(only_jobs - known)
        if unknown:
            sys.exit(
                f"gen-media: unknown job id(s): {', '.join(unknown)}\n"
                f"available: {', '.join(sorted(known))}"
            )

    build_once()
    work = Path(tempfile.mkdtemp(prefix="gen-media-"))

    if args.check:
        out_base = {t: work / f"out-{t}" for t in TARGET_DIRS}
    else:
        out_base = dict(TARGET_DIRS)
    for d in out_base.values():
        d.mkdir(parents=True, exist_ok=True)

    intermediates = {}
    try:
        for job in manifest:
            if only_jobs and job["id"] not in only_jobs:
                continue
            targets = [t for t in job["targets"] if not args.only or t == args.only]
            if not targets:
                continue
            # Clips/gif are presence-checked from the manifest, so don't waste
            # minutes rendering + vp9-encoding them under --check.
            if args.check and job["kind"] in ("gif", "clip"):
                print(f"· {job['id']} ({job['kind']}) → presence-only, skipped in --check")
                continue
            out_dirs = [out_base[t] for t in targets]
            print(f"· {job['id']} ({job['kind']}) → {', '.join(targets)}")
            HANDLERS[job["kind"]](job, out_dirs, work, intermediates)

        if args.check:
            sys.exit(run_check(out_base, work, manifest, only=args.only))

        surfaces = [args.only] if args.only else list(TARGET_DIRS)
        print(f"\nwrote media → {', '.join(str(TARGET_DIRS[t]) for t in surfaces)}")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
