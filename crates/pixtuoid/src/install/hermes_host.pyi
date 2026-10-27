# The part of hermes's plugin API the plugin calls, as hermes-agent v0.22.5
# declares it (hermes_cli/plugins.py:229, :605, :943): hermes publishes no
# types, so the plugin's selftest and hermetic profile back this copy.
from collections.abc import Callable

class PluginContext:
    def register_hook(
        self, hook_name: str, callback: Callable[..., object]
    ) -> object: ...
    def session_cwd(self) -> str: ...
