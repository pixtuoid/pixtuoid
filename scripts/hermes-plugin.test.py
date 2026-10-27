"""Contract test for `crates/pixtuoid/src/install/hermes_plugin.py`. The Rust side
can only grep the template as a string; this loads the RENDERED plugin the way
hermes's loader does (`register(ctx)`) and drives its callbacks against fake shims,
so what keeps hermes's `pre_tool_call` from failing closed is executed, not read."""

from __future__ import annotations

import importlib.util
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
PLUGIN_SOURCE = REPO_ROOT / "crates" / "pixtuoid" / "src" / "install" / "hermes_plugin.py"
# Must match `HOOK_PLACEHOLDER` in `crates/pixtuoid/src/install/hermes.rs`.
HOOK_PLACEHOLDER = '"{{HOOK_PATH_JSON}}"'
POSIX = os.name == "posix"
JITTER_S = 1.5


def _shim_bound_s():
    """The real shim's own bound, read from its authority."""
    transport = REPO_ROOT / "crates" / "pixtuoid-hook" / "src" / "transport.rs"
    found = re.search(r"WRITE_TIMEOUT_MS: u64 = (\d+);", transport.read_text(encoding="utf-8"))
    if found is None:
        raise AssertionError(f"{transport} declares no WRITE_TIMEOUT_MS")
    return int(found.group(1)) / 1000


# A fake stands in for a shim that spends all of it.
SHIM_BOUND_S = _shim_bound_s()


class FakeCtx:
    """The plugin context surface the plugin touches."""

    def __init__(self, cwd: str | BaseException = "/w"):
        self.hooks = {}
        self.cwd = cwd

    def register_hook(self, name, callback):
        self.hooks[name] = callback

    def session_cwd(self):
        if isinstance(self.cwd, BaseException):
            raise self.cwd
        return self.cwd


class PluginTest(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="pixtuoid-hermes-plugin-"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.out = self.dir / "payloads.ndjson"
        self.trace = self.dir / "trace.txt"

    def hook(self, body):
        path = self.dir / "fake-hook"
        path.write_text("#!/bin/sh\n" + body)
        path.chmod(0o755)
        return path

    def recorder(self, hold_s: float = 0):
        """A shim that records its argv, its stdin and its own span."""
        return self.hook(
            f'echo begin >> "{self.trace}"\n'
            f"sleep {hold_s}\n"
            f'{{ printf "%s\\n" "$*"; cat; echo; }} >> "{self.out}"\n'
            f'echo end >> "{self.trace}"\n'
        )

    def plugin(self, hook_path, ctx=None):
        source = PLUGIN_SOURCE.read_text(encoding="utf-8")
        self.assertEqual(source.count(HOOK_PLACEHOLDER), 1, "one hook placeholder")
        rendered = self.dir / "pixtuoid_hermes.py"
        rendered.write_text(source.replace(HOOK_PLACEHOLDER, json.dumps(str(hook_path))))
        spec = importlib.util.spec_from_file_location(f"pixtuoid_hermes_{id(self)}", rendered)
        if spec is None or spec.loader is None:
            self.fail(f"{rendered} does not load as a module")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        ctx = ctx or FakeCtx()
        module.register(ctx)
        return module, ctx

    def sent(self):
        lines = self.out.read_text().splitlines()
        return [(lines[i], json.loads(lines[i + 1])) for i in range(0, len(lines), 2)]

    def test_it_registers_exactly_its_forward_set(self):
        module, ctx = self.plugin(self.dir / "absent")
        self.assertEqual(set(ctx.hooks), set(module.FORWARD))

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_it_forwards_the_allowlist_and_no_prompt_tool_output_or_other_argument(self):
        _, ctx = self.plugin(self.recorder())
        returned = ctx.hooks["pre_tool_call"](
            session_id="s1",
            tool_name="write_file",
            tool_call_id="c1",
            turn_id="t1",
            args={"path": "NOTE.txt", "content": "SECRET-ARGUMENT"},
            user_message="SECRET-PROMPT",
            conversation_history=[{"content": "SECRET-HISTORY"}],
            result="SECRET-OUTPUT",
        )
        self.assertIsNone(returned)
        [(argv, payload)] = self.sent()
        self.assertEqual(argv, "--source hermes")
        self.assertEqual(
            payload,
            {
                "hook_event_name": "pre_tool_call",
                "cwd": "/w",
                "_pid": os.getpid(),
                "session_id": "s1",
                "tool_name": "write_file",
                "tool_call_id": "c1",
                "args": {"path": "NOTE.txt"},
            },
        )
        self.assertNotIn("SECRET", self.out.read_text())

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_a_finalize_outside_any_turn_neither_asks_for_nor_sends_a_cwd(self):
        # Asking raises here, as a context outside any session's turn has no cwd.
        _, ctx = self.plugin(self.recorder(), FakeCtx(cwd=RuntimeError("outside a turn")))
        ctx.hooks["on_session_finalize"](session_id="s1")
        [(_, payload)] = self.sent()
        self.assertEqual(payload["hook_event_name"], "on_session_finalize")
        self.assertNotIn("cwd", payload)

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_each_event_reaches_the_shim_before_hermes_can_fire_the_next(self):
        _, ctx = self.plugin(self.recorder(hold_s=0.1))
        ctx.hooks["pre_tool_call"](session_id="s1", tool_name="terminal", tool_call_id="c1")
        ctx.hooks["post_tool_call"](session_id="s1", tool_name="terminal", tool_call_id="c1")
        self.assertEqual(self.trace.read_text().split(), ["begin", "end", "begin", "end"])
        self.assertEqual(
            [p["hook_event_name"] for _, p in self.sent()], ["pre_tool_call", "post_tool_call"]
        )

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_a_shim_that_spends_its_bound_and_leaves_a_process_behind_blocks_no_longer(self):
        # The leftover holds whatever stdio it inherited: a pipe there would make
        # the plugin wait on it, long past the shim's own exit.
        hook = self.hook(f"cat >/dev/null\n(sleep 5 &)\nsleep {SHIM_BOUND_S}\nexit 0\n")
        _, ctx = self.plugin(hook)
        start = time.monotonic()
        self.assertIsNone(ctx.hooks["pre_tool_call"](session_id="s1", tool_name="terminal"))
        self.assertLess(time.monotonic() - start, SHIM_BOUND_S + JITTER_S)

    def test_an_absent_shim_neither_raises_nor_waits(self):
        _, ctx = self.plugin(self.dir / "absent")
        start = time.monotonic()
        self.assertIsNone(ctx.hooks["pre_tool_call"](session_id="s1", tool_name="terminal"))
        self.assertLess(time.monotonic() - start, JITTER_S)

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_nothing_raises_out_of_a_callback(self):
        failing = self.hook("exit 1\n")
        for hook_path, ctx in [
            (failing, FakeCtx()),
            (self.dir, FakeCtx()),
            (self.recorder(), FakeCtx(cwd=RuntimeError("no session"))),
        ]:
            _, ctx = self.plugin(hook_path, ctx)
            for event, callback in ctx.hooks.items():
                with self.subTest(hook=str(hook_path), event=event):
                    self.assertIsNone(callback(session_id="s1", args=object()))

    def test_a_platform_with_a_console_flag_spawns_the_shim_without_a_window(self):
        sentinel = 0x5A5A
        _, ctx = self.plugin(self.dir / "absent")
        with mock.patch.object(subprocess, "CREATE_NO_WINDOW", sentinel, create=True), mock.patch.object(
            subprocess, "run"
        ) as run:
            ctx.hooks["on_session_start"](session_id="s1")
        self.assertEqual(run.call_args.kwargs["creationflags"], sentinel)

    @unittest.skipUnless(POSIX, "Windows defines the console flag")
    def test_a_platform_without_a_console_flag_passes_no_flag(self):
        _, ctx = self.plugin(self.dir / "absent")
        with mock.patch.object(subprocess, "run") as run:
            ctx.hooks["on_session_start"](session_id="s1")
        self.assertEqual(run.call_args.kwargs["creationflags"], 0)


if __name__ == "__main__":
    unittest.main()
