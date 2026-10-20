#!/usr/bin/env python3
r"""Drive omp's interactive TUI under a pty for `just capture-fixture`.

The recorder can only run a command; omp's approval gates and /-commands need
keystrokes, so this driver types them on a resilient schedule. Modes:

    capture-omp-tui.py approve <prompt...>   one always-ask gate, approved
    capture-omp-tui.py deny <prompt...>      one always-ask gate, denied
    capture-omp-tui.py exit                  no prompt: /exit an empty session

The gate response repeats every few seconds rather than guessing model
latency — an extra Enter (or Down+Enter) on the empty composer is a no-op, so
only the raised gate consumes one. Composer submits are `\r` (a TUI reads a
bare `\n` as insert-newline).
"""
import os
import pty
import select
import sys
import time

# The CLI the recorder asks its version (`CLI_ENV`, capture_fixture.rs).
if os.environ.get("PIXTUOID_CAPTURE_CLI"):
    with open(os.environ["PIXTUOID_CAPTURE_CLI"], "w") as f:
        f.write("omp\n")

mode = sys.argv[1]
prompt = " ".join(sys.argv[2:])
RESPONSE = {"approve": b"\r", "deny": b"\x1b[B\r"}.get(mode)

TOTAL = 90.0 if RESPONSE else 30.0
pid, fd = pty.fork()
if pid == 0:
    argv = ["omp"]
    if RESPONSE:
        argv += ["--approval-mode", "always-ask"]
    os.execvp("omp", argv)

os.set_blocking(fd, False)
start = time.time()
sent: set[str] = set()
next_response = 16.0


def once(key: str, data: bytes) -> None:
    if key not in sent:
        os.write(fd, data)
        sent.add(key)


while time.time() - start < TOTAL:
    r, _, _ = select.select([fd], [], [], 0.25)
    if r:
        try:
            if not os.read(fd, 65536):
                break
        except OSError:
            break
    t = time.time() - start
    try:
        if RESPONSE:
            if t >= 4:
                once("prompt", prompt.encode())
            if t >= 6:
                once("submit", b"\r")
            if "submit" in sent and t >= next_response and t < TOTAL - 15:
                os.write(fd, RESPONSE)
                next_response = t + 8
        if t >= TOTAL - 12:
            once("exit", b"/exit")
        if t >= TOTAL - 10:
            once("exit-enter", b"\r")
    except OSError:
        break

deadline = time.time() + 6
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.25)
    if r:
        try:
            if not os.read(fd, 65536):
                break
        except OSError:
            break
try:
    os.kill(pid, 15)
except ProcessLookupError:
    pass
try:
    os.waitpid(pid, os.WNOHANG)
except ChildProcessError:
    pass
