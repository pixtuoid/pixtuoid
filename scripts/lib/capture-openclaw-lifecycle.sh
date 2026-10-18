#!/usr/bin/env bash
# One whole OpenClaw gateway lifecycle for `just capture-fixture openclaw <scenario>`.
#
# The turn is a gateway-backed `openclaw agent` turn, which fires the session
# hooks as `openclaw tui` does (checked live at 2026.7.1 and 2026.10.1).
# `--deliver` stays off, so nothing reaches a real chat channel.
#
# `session_start` additionally needs a FRESH session: a reused one RESUMES and
# stays silent, which is why OC_SESSION defaults to a timestamp.
#
# Note the recorded order: `session_end` lands AFTER `gateway_stop`, because
# shutdown is what closes the session.
set -uo pipefail

# The CLI the recorder asks its version (`CLI_ENV`, capture_fixture.rs).
[[ -z ${PIXTUOID_CAPTURE_CLI:-} ]] || echo openclaw >"$PIXTUOID_CAPTURE_CLI"

PROMPT="${1:?prompt}"
PORT="${OC_PORT:-19099}"
SESSION="${OC_SESSION:-pixcap-$(date +%s)}"
LOGS="${PIXTUOID_CAPTURE_DRIVER_DIR:-$(mktemp -d)}"
# `agent` dials this port, and the variable outranks the operator's configured
# `gateway.port` (`resolveGatewayPort`, openclaw's dist `paths-*.mjs`, 2026.10.4).
export OPENCLAW_GATEWAY_PORT="$PORT"

openclaw gateway --port "$PORT" --force --allow-unconfigured >"$LOGS/openclaw-gw.log" 2>&1 &
gw=$!
trap 'kill -TERM "$gw" 2>/dev/null' EXIT

for _ in $(seq 1 60); do
    openclaw gateway health --port "$PORT" >/dev/null 2>&1 && break
    sleep 1
done

openclaw agent --session-id "$SESSION" -m "$PROMPT" >"$LOGS/openclaw-agent.log" 2>&1
rc=$?
echo "agent rc=$rc"

# SIGTERM, not kill: `gateway_stop` is a clean-shutdown hook.
kill -TERM "$gw" 2>/dev/null
wait "$gw" 2>/dev/null
sleep 2
exit "$rc"
