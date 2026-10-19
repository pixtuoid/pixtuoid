#!/usr/bin/env bash
# OpenClaw MULTI-GATEWAY live-e2e — N REAL `openclaw gateway run` processes, each in
# its own isolated state dir on its own port, all feeding ONE headless pixtuoid; kill
# one and only ITS `daemons=[…]` row goes down.
#
# This runs a REAL gateway with ZERO model calls, the only way to check that the config keys `pixtuoid connect
# openclaw` writes are the keys OpenClaw actually reads, and that N mascots coexist on
# the ONE ground floor they share — the crowding where a 4-gateway render was found
# drawing only 3 lobsters.
#
# `just hermetic` runs it as openclaw's live tier; by hand it needs a real `openclaw`
# on PATH. Side-effect-free: every gateway runs from a throwaway `OPENCLAW_HOME`.
#
# Build first:  just build --release
# Run:          just openclaw-multi-e2e [port ...]   (default: 4 ports)
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PIX="${PIXTUOID_BIN:-$REPO/target/release/pixtuoid}"
# Four by default: two would prove the keying, four also exercises the one-floor
# crowding the visit-spot offset logic exists for.
PORTS=("$@")
[ "${#PORTS[@]}" -gt 0 ] || PORTS=(18901 18902 18903 18904)

[ -x "$PIX" ] || {
    echo "missing $PIX — run: just build --release" >&2
    exit 2
}
command -v openclaw >/dev/null 2>&1 || {
    echo "missing 'openclaw' on PATH — this live test drives the REAL gateway" >&2
    exit 2
}
for port in "${PORTS[@]}"; do
    if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
        echo "port $port is already in use — pass free ports, or stop that listener" >&2
        exit 2
    fi
done

# 0700 by construction, with the SOCKET inside it: a fixed or `mktemp -u`
# socket name in shared temp lets concurrent runs clobber each other and is
# pre-plantable by another user, which nothing downstream polices —
# `ensure_owned_socket_dir` leaves an explicit PIXTUOID_SOCKET path alone.
MG="$(mktemp -d)"
SOCK="$MG/pixtuoid.sock"
OUT="$MG/pixtuoid.log"
GW_PIDS=()
PIXPID=""

cleanup() {
    for p in "${GW_PIDS[@]:-}"; do
        [ -n "$p" ] && kill "$p" 2>/dev/null
    done
    sleep 1
    # Belt-and-braces port reap: `$!` normally holds the port, so this only catches a
    # version that daemonises, or a gateway that outlived a failed kill.
    for port in "${PORTS[@]}"; do
        left="$(lsof -ti tcp:"$port" -sTCP:LISTEN 2>/dev/null)"
        # shellcheck disable=SC2086  # word-split intended — one kill per listener pid
        [ -n "$left" ] && kill -9 $left 2>/dev/null
    done
    [ -n "$PIXPID" ] && kill "$PIXPID" 2>/dev/null
    rm -rf "$MG"
}
trap cleanup EXIT

# An isolated pixtuoid config; each gateway's own `connect` below installs the
# plugin, which is what connects OpenClaw. Both unset vars outrank OPENCLAW_HOME
# (install/openclaw.rs), so one exported in the dev's shell would send those
# connects to their real config.
unset OPENCLAW_STATE_DIR OPENCLAW_CONFIG_PATH
mkdir -p "$MG/claude" "$MG/cfg/pixtuoid"

# One throwaway state dir per gateway. `gateway.mode` is REQUIRED — `gateway run`
# refuses a config without it as possibly-clobbered.
for port in "${PORTS[@]}"; do
    home="$MG/home$port"
    mkdir -p "$home/.openclaw"
    printf '{ "gateway": { "mode": "local", "bind": "loopback", "port": %d } }\n' "$port" \
        >"$home/.openclaw/openclaw.json"
    OPENCLAW_HOME="$home" XDG_CONFIG_HOME="$MG/cfg" "$PIX" connect openclaw --json >/dev/null || {
        echo "FAIL: connect openclaw failed for port $port" >&2
        exit 1
    }
    # THE install cross-check: a config merge that writes keys upstream does not read
    # passes every unit test and still renders no lobster.
    OPENCLAW_HOME="$home" openclaw plugins list 2>/dev/null | grep -q 'pixtuoid.*enabled' || {
        echo "FAIL: openclaw does not report the pixtuoid plugin enabled (port $port)" >&2
        exit 1
    }
    echo "  port $port: plugin installed, and OpenClaw itself reports it enabled"
done

# Connected is read from ONE state dir; any gateway's install marks it, and the
# presence connection-gate drops every delta for a source with none.
# CLAUDE_CONFIG_DIR keeps the dev's real CC sessions out of the headless roster.
OPENCLAW_HOME="$MG/home${PORTS[0]}" XDG_CONFIG_HOME="$MG/cfg" PIXTUOID_SOCKET="$SOCK" \
    CLAUDE_CONFIG_DIR="$MG/claude" "$PIX" run --headless >"$OUT" 2>&1 &
PIXPID=$!
for _ in $(seq 1 50); do
    [ -S "$SOCK" ] && break
    sleep 0.1
done
[ -S "$SOCK" ] || {
    echo "FAIL: HookRouter never bound $SOCK" >&2
    exit 1
}
echo "  headless pixtuoid up (pid $PIXPID), HookRouter owns $SOCK"

FAILED=0
# Wait until the LATEST `daemons=` line contains `want` — a substring match, so a
# per-instance assertion does not have to name every sibling.
expect_line() {
    local want="$1" label="$2" last
    for _ in $(seq 1 120); do
        last="$(grep 'daemons=' "$OUT" | tail -1)"
        case "$last" in
        *"$want"*)
            echo "  PASS $label  ($last)"
            return 0
            ;;
        esac
        sleep 0.3
    done
    echo "  FAIL $label — wanted '$want', last: $(grep 'daemons=' "$OUT" | tail -1)" >&2
    FAILED=1
}

echo "[1] start all ${#PORTS[@]} gateways"
for port in "${PORTS[@]}"; do
    OPENCLAW_HOME="$MG/home$port" PIXTUOID_SOCKET="$SOCK" \
        openclaw gateway run --bind loopback >"$MG/gw$port.log" 2>&1 &
    GW_PIDS+=("$!")
done
# The roster is a BTreeMap keyed by the instance STRING, so the summary lists gateways
# in LEXICOGRAPHIC port order, not the order they were passed — without this sort a
# reversed argument list fails on ordering alone while the render is correct.
want=""
while read -r port; do
    want="$want${want:+, }openclaw@$port:idle"
done < <(printf '%s\n' "${PORTS[@]}" | LC_ALL=C sort)
expect_line "daemons=[$want]" "${#PORTS[@]} gateways render ${#PORTS[@]} independent mascots"

# Step [1] is a hard PRECONDITION for step [2]: a gateway that has not announced yet
# would satisfy "sibling $port is untouched" on its FIRST-EVER announce, after the kill.
if [ "$FAILED" != 0 ]; then
    echo "  SKIP [2] — not every gateway announced; the instance-local assertions would be vacuous" >&2
else
    echo "[2] kill the FIRST gateway (${PORTS[0]}) — only its own mascot walks out"
    # `$!` holds the port and stamps `_pid` into every envelope, so this exercises the
    # INSTANT abrupt-down rung (ExitWatch on the gateway pid). `expect_line` gives up
    # far sooner than the silence path, so a lost pid rung cannot pass on the timeout.
    kill "${GW_PIDS[0]}" 2>/dev/null
    GW_PIDS[0]=""
    expect_line "openclaw@${PORTS[0]}:down" "the killed gateway goes down (instant pid rung)"
    for port in "${PORTS[@]:1}"; do
        expect_line "openclaw@$port:idle" "sibling $port is untouched"
    done
fi

echo "--- the lobster timeline (headless):"
grep 'daemons=' "$OUT" | sed 's/^/    /'
if [ "$FAILED" = 0 ]; then
    echo "openclaw-multi: PASS"
else
    echo "openclaw-multi: FAIL" >&2
fi
trap - EXIT
cleanup
exit "$FAILED"
