# @pixtuoid-hermes-plugin — managed by pixtuoid (connect/disconnect Hermes Agent in pixtuoid's Sources panel: press s).
#
# Hermes runs every turn in its gateway, whose own cwd is its install dir, so
# pixtuoid integrates as a plugin there that reads each session's workspace from
# `ctx.session_cwd()` (website/docs/user-guide/features/plugins.md:1109 at
# v0.22.5) and pipes an allowlisted copy of each event into the `pixtuoid-hook`
# shim on stdin. A raise or a hang in `pre_tool_call` blocks the user's tool
# (hermes_cli/plugins_dispatch.py:60, :277-293), so every error is swallowed and
# the one wait is the shim's exit, which its write-timeout watchdog bounds
# (`transport::WRITE_TIMEOUT`, invariant #5).
#
# HOOK_PATH is baked in at install time (a JSON-encoded absolute path). The
# host types are a stub only the type check imports (`hermes_host.pyi`).
from __future__ import annotations

import contextlib
import json
import os
import subprocess
from collections.abc import Callable
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from hermes_host import PluginContext

HOOK_PATH = "{{HOOK_PATH_JSON}}"

# Only what pixtuoid maps; the decoder (`source/hermes.rs`) bails on anything else.
FORWARD = (
    "on_session_start",
    "pre_tool_call",
    "pre_approval_request",
    "post_tool_call",
    "on_session_end",
    "on_session_finalize",
)
# `ctx.session_cwd()` is defined only within a session's turn (plugins.md:1109),
# and the gateway finalizes a session outside any.
OUTSIDE_A_TURN = ("on_session_finalize",)
# Copied by name, so no prompt, tool output or other argument leaves hermes.
FIELDS = ("session_id", "tool_name", "tool_call_id", "description", "model")
ARGS = ("command", "file_path", "path", "pattern", "url")


def _payload(
    event: str, cwd: str | None, kwargs: dict[str, object]
) -> dict[str, object]:
    payload: dict[str, object] = {"hook_event_name": event, "_pid": os.getpid()}
    if cwd is not None:
        payload["cwd"] = cwd
    payload.update((name, kwargs[name]) for name in FIELDS if name in kwargs)
    args = kwargs.get("args")
    if isinstance(args, dict):
        given = cast("dict[object, object]", args)
        payload["args"] = {name: given[name] for name in ARGS if name in given}
    return payload


# Returns once the shim exits, so hermes fires an event's successor only after
# the shim delivered it: a call's start always reaches pixtuoid before its end.
def _send(payload: dict[str, object]) -> None:
    subprocess.run(
        [HOOK_PATH, "--source", "hermes"],
        input=json.dumps(payload, default=str).encode(),
        # Not pipes: a process the shim left behind could hold one open, and
        # the run would wait on it past the shim's exit.
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        # The gateway has no console, so a console child would open a window.
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        check=False,
    )


def _forwarder(ctx: PluginContext, event: str) -> Callable[..., None]:
    def forward(**kwargs: object) -> None:
        with contextlib.suppress(Exception):
            cwd = None if event in OUTSIDE_A_TURN else ctx.session_cwd()
            _send(_payload(event, cwd, kwargs))

    return forward


def register(ctx: PluginContext) -> None:
    for event in FORWARD:
        ctx.register_hook(event, _forwarder(ctx, event))
