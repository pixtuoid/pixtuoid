#!/usr/bin/env python3
"""The fluency gate: run the release binary through a weather transition into
a storm, or across dusk, and fail when its hitch rate, how late frames
showed past their interval per second run, passes HITCH_RATE_MS_PER_S.

The binary measures itself: its `frame pacing` summaries (`jank.rs`), a
minute each and one at exit, are what this reads, from the log it writes.
`--live` runs it in this terminal, as a user would, which is the only run
whose writes meet a real terminal's parser; without it, a pseudo-terminal
sized for `--scale` stands in, which catches our side alone.

The summary also reports p99, the worst frame, the frames past their
interval and the 1-minute load average the run started under: other work's
load slows a frame woken from the TUI's sleep, and the load says how much
of a rate is the machine's.

`--hover` sweeps the pointer to and fro along the terminal's middle row, across the scene, so a
tooltip opens over each figure and fixture it crosses and moves with it; the
pty alone takes it, a live run's pointer being the user's.

`--floating` runs `pixtuoid floating` instead: a real window, at the size and
zoom its config saved, whose scale is whatever that size fits.

Usage:
    just pace-check [--live] [--scale classic|4|16] [--protocol kitty|sixel|iterm2]
                    [--run storm|dusk] [--secs N] [--hover]
    just pace-check --floating [--run storm|dusk] [--secs N]
"""

import argparse
import fcntl
import os
import pty
import re
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target/release/pixtuoid"
# The bar: Apple's "good" hitch rate, at most 10 ms of pause a second
# (https://developer.apple.com/documentation/xcode/understanding-hitches-in-your-app).
HITCH_RATE_MS_PER_S = 10.0
# The pty runs on the owner's office: the bench names its terminal per scale.
SCALES = ["classic", "4", "16"]
# The lead into the storm transition: a boot and some steady frames first.
LEAD_S = 10
# How long `--hover`'s pointer rests on a cell: a hand's slow sweep.
HOVER_STEP_S = 0.1


def pacing(*args):
    """What `examples/pacing.rs` prints for `args`: the authority on where a
    run starts and which terminal gives the owner's office."""
    out = subprocess.run(
        [
            "cargo",
            "run",
            "-q",
            "--release",
            "-p",
            "pixtuoid",
            "--example",
            "pacing",
            "--",
            *args,
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return out.stdout.split()


def start_at(run):
    """The Unix second the run's clock starts at: today's nightfall, or the
    lead into the clock's next transition into a storm."""
    if run == "dusk":
        return int(pacing("dusk")[0])
    return int(pacing("next-storm")[0]) - LEAD_S


def sweep(fd, cols, rows, stop):
    """Move the pointer a cell at a time along the terminal's middle row, to and
    fro, until `stop`: SGR reports of motion, 32, with no button, 3
    (https://invisible-island.net/xterm/ctlseqs/ctlseqs.html, "Button-event
    tracking" and "Extended coordinates")."""
    y, x, step = rows // 2, 1, 1
    while not stop.is_set():
        os.write(fd, f"\x1b[<35;{x};{y}M".encode())
        if not 1 <= x + step <= cols:
            step = -step
        x += step
        stop.wait(HOVER_STEP_S)


def run_pty(argv, env, geometry, secs, hover):
    cols, rows, cell_w, cell_h = geometry
    pid, fd = pty.fork()
    if pid == 0:
        try:
            os.execve(argv[0], argv, env)
        finally:
            # An exec that fails must not go on running this script as a child.
            os._exit(127)
    fcntl.ioctl(
        fd,
        termios.TIOCSWINSZ,
        struct.pack("HHHH", rows, cols, cols * cell_w, rows * cell_h),
    )
    os.kill(pid, signal.SIGWINCH)
    # `cat` drains the pty: a reader slower than the binary's writes would
    # block them, and charge this script's speed to its frames.
    drain = subprocess.Popen(["cat"], stdin=fd, stdout=subprocess.DEVNULL)
    stop = threading.Event()
    if hover:
        threading.Thread(target=sweep, args=(fd, cols, rows, stop), daemon=True).start()
    time.sleep(secs)
    stop.set()
    os.kill(pid, signal.SIGTERM)
    os.waitpid(pid, 0)
    drain.terminate()
    drain.wait()
    os.close(fd)


def run_for(child, secs):
    """Let `child` run `secs`, then end it as a user's kill would: SIGTERM, which
    the binary quits on, writing its last summary."""
    try:
        child.wait(timeout=secs)
    except subprocess.TimeoutExpired:
        child.send_signal(signal.SIGTERM)
        child.wait()


def run_live(argv, env, secs):
    # The binary draws to the terminal, by its device's name, whatever this
    # script's own output is redirected to.
    with open(os.ttyname(0), "wb", buffering=0) as tty:
        run_for(subprocess.Popen(argv, env=env, stdout=tty, stderr=tty), secs)


FIELD = re.compile(r'(\w+)=("[^"]*"|\S+)')


def events(log, message):
    for line in log.read_text(errors="replace").splitlines():
        if message in line:
            yield {k: v.strip('"') for k, v in FIELD.findall(line)}


def causes(log):
    """The over-interval frames by what they were: Dirty kind, why the
    cutaway repainted whole, a view the terminal had not shown, and a
    strike."""
    counts = {}
    for e in [*events(log, "frame slow"), *events(log, "frame jank")]:
        repaint = e.get("repaint", "None")
        key = (
            e.get("dirty"),
            "repaint" if repaint != "None" else "-",
            "fresh" if e.get("fresh") == "true" else "-",
            "strike" if e.get("strike") == "true" else "-",
        )
        counts[key] = counts.get(key, 0) + 1
    return sorted(counts.items(), key=lambda kv: -kv[1])


def main():
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("--live", action="store_true")
    ap.add_argument("--floating", action="store_true")
    # `None` until resolved below, so `--floating` can refuse an explicit one.
    ap.add_argument("--scale", choices=SCALES)
    ap.add_argument("--protocol", choices=["kitty", "sixel", "iterm2"])
    ap.add_argument("--run", choices=["storm", "dusk"], default="storm")
    ap.add_argument("--secs", type=float, default=200.0)
    ap.add_argument("--hover", action="store_true")
    args = ap.parse_args()
    if args.floating:
        terminal_only = [
            f"--{k}" for k in ("live", "scale", "protocol", "hover") if getattr(args, k)
        ]
        if terminal_only:
            sys.exit(
                f"pace-check: {' '.join(terminal_only)} picks a terminal run, and --floating runs a window"
            )
    args.scale = args.scale or "16"
    args.protocol = args.protocol or "kitty"
    if args.hover and args.live:
        sys.exit("pace-check: --hover drives the pty's pointer; live, hover by hand")
    if not os.access(BIN, os.X_OK):
        sys.exit(f"pace-check: no {BIN}: run it as `just pace-check`, which builds it")
    load = os.getloadavg()[0]

    log = Path(tempfile.mkstemp(prefix="pace-check-", suffix=".log")[1])
    env = dict(
        os.environ,
        PIXTUOID_FAKE_NOW=str(start_at(args.run)),
        PIXTUOID_LOG=str(log),
        # `frame slow` is debug: every frame past its interval, with its state.
        RUST_LOG="info,pixtuoid::jank=debug",
    )
    look = (
        ["--look", "classic"]
        if args.scale == "classic"
        else ["--protocol", args.protocol]
    )
    argv = [str(BIN), "--log-level", "info", "run", *look]
    if args.floating:
        run_for(
            subprocess.Popen([str(BIN), "--log-level", "info", "floating"], env=env),
            args.secs,
        )
    elif args.live:
        run_live(argv, env, args.secs)
    else:
        # No pty answers the probe: a terminal the environment names plans
        # without one, and `--protocol` then picks it.
        env.update(TERM="xterm-256color", TERM_PROGRAM="WezTerm")
        env.pop("TMUX", None)
        geometry = tuple(
            int(n)
            for n in pacing("terminal", "16" if args.scale == "classic" else args.scale)
        )
        run_pty(argv, env, geometry, args.secs, args.hover)

    if not any(events(log, "the clock starts at PIXTUOID_FAKE_NOW")):
        sys.exit(f"pace-check: the binary ran on the real clock, not the run's: {log}")
    windows = list(events(log, "frame pacing"))
    if not windows:
        sys.exit(f"pace-check: no `frame pacing` summary in {log}")
    w0 = windows[0]
    if args.floating:
        if w0.get("look") != "floating":
            sys.exit(f"pace-check: ran {w0.get('look')}, not floating: {log}")
    else:
        wanted = (
            ("classic", "1") if args.scale == "classic" else (args.protocol, args.scale)
        )
        if (w0.get("look"), w0.get("scale")) != wanted:
            sys.exit(
                f"pace-check: ran {w0.get('look')} x{w0.get('scale')}, not {wanted[0]} x{wanted[1]}: {log}"
            )
    # scheduled, hitch_ms and span_ms are indexed, not .get: a binary that logs
    # none raises instead of passing, and one that schedules no frame must not PASS.
    if not sum(int(w["scheduled"]) for w in windows):
        sys.exit(f"pace-check: no frame was shown on a schedule: {log}")
    over = sum(int(w.get("over", 0)) for w in windows)
    p99 = max(float(w.get("p99", 0)) for w in windows)
    worst = max(float(w.get("max", 0)) for w in windows)
    frames = sum(int(w.get("frames", 0)) for w in windows)
    ran_s = sum(float(w["span_ms"]) for w in windows) / 1000
    hitch_ms = sum(float(w["hitch_ms"]) for w in windows)
    rate = hitch_ms / ran_s
    terminal = "window" if args.floating else w0.get("terminal") if args.live else "pty"
    print(
        f"{w0.get('look')} x{w0.get('scale')} tmux={w0.get('tmux')} terminal={terminal} sync={w0.get('sync')} "
        f"run={args.run} hover={args.hover} load={load:.2f}: {frames} frames, hitch rate {rate:.2f} ms/s "
        f"({hitch_ms:.0f} ms late over {ran_s:.0f} s), worst window p99 {p99:.1f} ms, "
        f"worst frame {worst:.1f} ms, over {over} | log {log}"
    )
    for (dirty, *why), n in causes(log):
        print(f"  over-interval: {n:4} dirty={dirty} {' '.join(why)}")
    failed = rate > HITCH_RATE_MS_PER_S
    print("FAIL" if failed else "PASS")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
