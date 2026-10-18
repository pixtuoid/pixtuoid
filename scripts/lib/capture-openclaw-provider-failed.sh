#!/usr/bin/env bash
# One OpenClaw turn at a provider that refuses every connection, for
# `just capture-fixture openclaw <scenario>`: the run a failed provider ends
# `success: true`, which the plugin's `errored` marks. No model call is billed and
# no real config is read: the gateway, its config and the plugin `pixtuoid connect`
# installs all live in a throwaway home. At
# 2026.10.1 `openclaw agent` fires the session pair around the run as well.
#
# `openclaw` is whatever is first on PATH; put the release under test there. The
# plugin is the one THIS checkout's release build renders (`just build --release`).
set -uo pipefail

# The CLI the recorder asks its version (`CLI_ENV`, capture_fixture.rs).
[[ -z ${PIXTUOID_CAPTURE_CLI:-} ]] || echo openclaw >"$PIXTUOID_CAPTURE_CLI"

PROMPT="${1:?prompt}"
PORT="${OC_PORT:-19098}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SBX="${PIXTUOID_CAPTURE_DRIVER_DIR:-$(mktemp -d)}/openclaw-home"
mkdir -p "$SBX/.openclaw" "$SBX/xdg"
export HOME="$SBX" OPENCLAW_HOME="$SBX" OPENCLAW_STATE_DIR="$SBX/.openclaw" \
    OPENCLAW_CONFIG_PATH="$SBX/.openclaw/openclaw.json" XDG_CONFIG_HOME="$SBX/xdg" \
    PIXTUOID_HOOK="$REPO/target/release/pixtuoid-hook"

# A closed loopback port: the call fails at connect, before any request leaves.
cat >"$OPENCLAW_CONFIG_PATH" <<EOF
{
  "gateway": { "mode": "local", "bind": "loopback", "port": $PORT },
  "logging": { "file": "$SBX/openclaw.log" },
  "models": { "providers": { "deadend": {
    "baseUrl": "http://127.0.0.1:59999/v1", "apiKey": "not-a-real-key", "api": "openai-completions",
    "models": [{ "id": "unreachable", "name": "unreachable", "reasoning": false, "input": ["text"],
      "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
      "contextWindow": 32000, "maxTokens": 256 }] } } },
  "agents": { "defaults": { "model": { "primary": "deadend/unreachable", "fallbacks": [] } } }
}
EOF
"$REPO/target/release/pixtuoid" connect openclaw >"$SBX/connect.log" 2>&1 || {
    echo "pixtuoid connect openclaw failed: see $SBX/connect.log" >&2
    exit 1
}

openclaw gateway run --bind loopback --port "$PORT" >"$SBX/gateway.log" 2>&1 &
gw=$!
trap 'kill -TERM "$gw" 2>/dev/null' EXIT
up=0
for _ in $(seq 1 90); do
    openclaw gateway health --port "$PORT" >/dev/null 2>&1 && {
        up=1
        break
    }
    sleep 1
done
[[ $up -eq 1 ]] || {
    echo "the gateway never answered: see $SBX/gateway.log" >&2
    exit 1
}

openclaw agent --session-id "pixcap-$(date +%s)" -m "$PROMPT" --timeout 120 >"$SBX/agent.log" 2>&1
echo "agent rc=$? (non-zero: the provider refused, as intended)"

# SIGTERM, not kill: `gateway_stop` is a clean-shutdown hook.
kill -TERM "$gw" 2>/dev/null
wait "$gw" 2>/dev/null
sleep 2
