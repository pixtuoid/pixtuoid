"""Contract test for `crates/pixtuoid/src/install/hermes_plugin.py`: loads the
RENDERED plugin the way hermes's loader does (`register(ctx)`) and drives its
callbacks against fake shims, holding it to `plugin-contract.json`, so what keeps
hermes's `pre_tool_call` from failing closed is executed, not read."""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
INSTALL_DIR = REPO_ROOT / "crates" / "pixtuoid" / "src" / "install"
CONTRACT = json.loads(
    (INSTALL_DIR / "plugin-contract.json").read_text(encoding="utf-8")
)
ROW = next(p for p in CONTRACT["plugins"] if p["source"] == "hermes")
PLUGIN_SOURCE = INSTALL_DIR / ROW["file"]
HOOK_PLACEHOLDER = CONTRACT["hook_placeholder"]
POSIX = os.name == "posix"
JITTER_S = 1.5


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
        rendered.write_text(
            source.replace(HOOK_PLACEHOLDER, json.dumps(str(hook_path)))
        )
        spec = importlib.util.spec_from_file_location(
            f"pixtuoid_hermes_{id(self)}", rendered
        )
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

    def test_it_registers_exactly_the_contracts_events(self):
        _, ctx = self.plugin(self.dir / "absent")
        self.assertEqual(set(ctx.hooks), set(ROW["registers"]))

    def test_it_copies_the_contracts_fields_and_nothing_more(self):
        # The plugin's own allowlists, so a name the fixtures never pass is held
        # too: `cwd` comes from the session, not the kwargs.
        module, _ = self.plugin(self.dir / "absent")
        self.assertEqual(set(module.FIELDS) | {"cwd"}, set(ROW["fields"]))
        self.assertEqual(set(module.ARGS), set(ROW["detail_keys"]))

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_each_event_sends_exactly_the_contracts_fields(self):
        _, ctx = self.plugin(self.recorder())
        fields = {name: name for name in ROW["fields"] if name != "cwd"}
        args = {name: name for name in ROW["detail_keys"]}
        for event in ROW["registers"]:
            ctx.hooks[event](
                **fields, args={**args, "content": "SECRET"}, user_message="SECRET"
            )
        for event, (argv, payload) in zip(ROW["registers"], self.sent(), strict=True):
            with self.subTest(event=event):
                self.assertEqual(argv, f"{CONTRACT['source_flag']} {ROW['source']}")
                self.assertEqual(payload.pop(CONTRACT["pid_key"]), os.getpid())
                self.assertEqual(payload.pop("hook_event_name"), event)
                self.assertEqual(set(payload.pop("args")), set(ROW["detail_keys"]))
                outside = event in ROW["outside_a_turn"]
                self.assertEqual(
                    set(payload), set(ROW["fields"]) - ({"cwd"} if outside else set())
                )

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_it_forwards_the_allowlist_and_no_prompt_tool_output_or_other_argument(
        self,
    ):
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
        self.assertEqual(argv, f"{CONTRACT['source_flag']} {ROW['source']}")
        self.assertEqual(
            payload,
            {
                "hook_event_name": "pre_tool_call",
                "cwd": "/w",
                CONTRACT["pid_key"]: os.getpid(),
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
        _, ctx = self.plugin(
            self.recorder(), FakeCtx(cwd=RuntimeError("outside a turn"))
        )
        ctx.hooks["on_session_finalize"](session_id="s1")
        [(_, payload)] = self.sent()
        self.assertEqual(payload["hook_event_name"], "on_session_finalize")
        self.assertNotIn("cwd", payload)

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_each_event_reaches_the_shim_before_hermes_can_fire_the_next(self):
        _, ctx = self.plugin(self.recorder(hold_s=0.1))
        ctx.hooks["pre_tool_call"](
            session_id="s1", tool_name="terminal", tool_call_id="c1"
        )
        ctx.hooks["post_tool_call"](
            session_id="s1", tool_name="terminal", tool_call_id="c1"
        )
        self.assertEqual(
            self.trace.read_text().split(), ["begin", "end", "begin", "end"]
        )
        self.assertEqual(
            [p["hook_event_name"] for _, p in self.sent()],
            ["pre_tool_call", "post_tool_call"],
        )

    @unittest.skipUnless(POSIX, "the fake shims are sh scripts")
    def test_a_process_the_shim_leaves_behind_holds_the_hook_no_longer_than_the_shim(
        self,
    ):
        # The leftover holds whatever stdio it inherited: a pipe there would make
        # the plugin wait on it, long past the shim's own exit.
        hook = self.hook("cat >/dev/null\n(sleep 5 &)\nexit 0\n")
        _, ctx = self.plugin(hook)
        start = time.monotonic()
        self.assertIsNone(
            ctx.hooks["pre_tool_call"](session_id="s1", tool_name="terminal")
        )
        self.assertLess(time.monotonic() - start, JITTER_S)

    def test_an_absent_shim_neither_raises_nor_waits(self):
        _, ctx = self.plugin(self.dir / "absent")
        start = time.monotonic()
        self.assertIsNone(
            ctx.hooks["pre_tool_call"](session_id="s1", tool_name="terminal")
        )
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
        with (
            mock.patch.object(subprocess, "CREATE_NO_WINDOW", sentinel, create=True),
            mock.patch.object(subprocess, "run") as run,
        ):
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
