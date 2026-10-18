#!/usr/bin/env bash
# LIVE multi-source e2e — launches each supported agent CLI non-interactively and
# asserts ITS badge appears in one headless pixtuoid's scene. This is the only
# tier that proves a real CLI's real output reaches a real sprite; corpus_check
# proves decode over stored bytes, and the fixtures prove the wire contract.
#
# ⚠ BILLED and NOT hermetic: every CLI here makes one real model turn on YOUR
# account for that provider. Run deliberately. Zero repo side effects — each CLI
# runs in a throwaway workspace with an isolated socket and config.
#
# Build first:  just build --release
# Run:          just live-sources [source-id ...]   (default: every present CLI)
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$here/e2e-common.sh"

REPO="$(e2e_repo_root)"
PIX="$REPO/target/release/pixtuoid"
HOOK="$REPO/target/release/pixtuoid-hook"
ROSTER_BIN="$REPO/target/release/examples/corpus_check"
e2e_require_bin "$PIX" "$HOOK" "$ROSTER_BIN"
# Unguarded, an absent jq empties `present_ids` and every source reads as
# "its CLI is not installed here" — a full SKIP sweep that still exits 0.
command -v jq >/dev/null 2>&1 || {
    echo "missing jq — brew install jq" >&2
    exit 2
}

# The per-CLI knowledge that is genuinely ours: a headless invocation's
# SUBCOMMAND and flags are as source-specific as the decoder. The BINARY name is
# not — the registry owns it as `version_probe`, read below from `--roster`
# field 5, because a hand-copied binary table here shipped missing `agy` once.
# "<subcommand-or-flag>|<extra flags>". The extra field exists because a headless
# turn that a permission prompt auto-denies produces no tool call, and a tool
# call is half of what this tier measures — scope the grant to reading.
invocation_for() {
    case "$1" in
    # --allowedTools is VARIADIC, so it must follow the prompt or it eats it.
    claude-code) echo "-p|--allowedTools Read" ;;
    codex) echo "exec|" ;;
    antigravity) echo "-p|" ;;
    reasonix) echo "-p|" ;;
    codewhale) echo "exec|" ;;
    # A plain `run` attaches to the background service, whose env never carries
    # this tier's PIXTUOID_SOCKET; a private server inherits it
    # (`packages/cli/src/services/standalone.ts:24-25`).
    opencode) echo "run|--standalone" ;;
    copilot) echo "-p|" ;;
    cursor) echo "-p|" ;;
    hermes) echo "-z|" ;;
    grok) echo "-p|" ;;
    # Both invocations are the ones their recorded fixtures were captured with
    # (`fixtures/{kimi,omp}/tool-run*/provenance.json`) — this row is transcribed
    # from a run that happened, not composed.
    kimi) echo "-p|" ;;
    omp) echo "-p|" ;;
    *) return 1 ;;
    esac
}

# One turn, three things verified: the session registers, a TOOL call drives the
# per-source tool-detail decode (where the decoders differ most), and the slot
# returns. Asking for a file READ rather than a shell command keeps it inside
# every CLI's default permissions, so no turn is spent on an approval prompt.
PROMPT='Read the file NOTE.txt in the current directory and reply with only its contents.'
TURN_TIMEOUT=180

SB="$(e2e_sandbox)"
SOCK="$SB/pixtuoid.sock"
OUT="$SB/pixtuoid.log"
PROJ="$SB/projects"
CFG="$SB/config"
WS="$SB/workspace"
mkdir -p "$PROJ" "$CFG/pixtuoid" "$WS"
printf 'pong\n' >"$WS/NOTE.txt"
# A git repo, because several CLIs refuse to act outside one (codex: "Not inside
# a trusted directory"). Cheaper than a per-CLI trust flag, and it is what a real
# user's workspace looks like anyway.
e2e_init_repo "$WS"
PIXPID=""
CLIPID=""

cleanup() {
    [ -n "$CLIPID" ] && kill "$CLIPID" 2>/dev/null
    [ -n "$PIXPID" ] && kill "$PIXPID" 2>/dev/null
    rm -rf "$SB"
}
trap cleanup EXIT

# A flag connects a hookless source, and an `ONCE_FLAG_ONLY` one (sources.rs),
# which a headless run never adopts; any other hook source's key is ignored — its
# hooks in its real config connect it, which the loop below checks first.
{
    echo '[sources]'
    "$ROSTER_BIN" --roster | cut -f1 | sed 's/$/ = true/'
} >"$CFG/pixtuoid/config.toml"

# claude-code watches a projects ROOT; the others resolve their own homes, so the
# real host dirs stay in play for them and only CC's tree is isolated.
XDG_CONFIG_HOME="$CFG" PIXTUOID_SOCKET="$SOCK" \
    "$PIX" run --headless --projects-root "$PROJ" --log-level error >"$OUT" 2>&1 &
PIXPID=$!
for _ in $(seq 1 50); do
    [ -S "$SOCK" ] && break
    sleep 0.1
done
[ -S "$SOCK" ] || {
    echo "FAIL: HookRouter never bound $SOCK" >&2
    exit 1
}
echo "headless pixtuoid up (pid $PIXPID), socket $SOCK"

wanted=("$@")
if [ "${#wanted[@]}" -eq 0 ]; then
    while IFS=$'\t' read -r id _ _; do
        wanted+=("$id")
    done < <("$ROSTER_BIN" --roster)
fi

present_ids="$("$PIX" sources --json | jq -r '.[] | select(.cli_present) | .id' | paste -sd' ' -)"

# A hook command that is a BARE NAME resolves via the CLI's PATH, so this tier
# puts ITS OWN shim there for the CLIs it launches — the same reason it injects
# PIXTUOID_SOCKET. Without it the tier would measure whether a pixtuoid happens
# to be installed, not whether THIS build decodes. An ABSOLUTE hook path in a
# CLI's config is beyond reach: PATH cannot redirect it, only a reconnect can.
mkdir -p "$SB/bin"
ln -sf "$HOOK" "$SB/bin/pixtuoid-hook"
PATH="$SB/bin:$PATH"
export PATH
# Read back rather than assumed: asserting 1 here made the BLOCKED branch below
# dead code, so a shim that failed to land would have reported the generic
# "no sprite" instead of the specific reason.
command -v pixtuoid-hook >/dev/null 2>&1 && SHIM_ON_PATH=1 || SHIM_ON_PATH=0

FAILED=0
covered=0
declare_uncovered=""
for id in "${wanted[@]}"; do
    prefix="$("$ROSTER_BIN" --roster | awk -F'\t' -v i="$id" '$1==i{print $2}')"
    if [ -z "$prefix" ]; then
        echo "  SKIP $id — not a registered source"
        continue
    fi
    if ! spec="$(invocation_for "$id")"; then
        declare_uncovered="$declare_uncovered $id"
        continue
    fi
    case " $present_ids " in
    *" $id "*) ;;
    *)
        echo "  SKIP $id — its CLI is not installed here"
        declare_uncovered="$declare_uncovered $id"
        continue
        ;;
    esac

    # A hook source without installed hooks is gated off, so its turn would read
    # as "no sprite" for the wrong reason.
    if [ "$(XDG_CONFIG_HOME="$CFG" "$PIX" sources --json | jq -r --arg i "$id" '.[] | select(.id==$i) | .connected')" != "true" ]; then
        echo "  BLOCKED $id — not connected here (pixtuoid connect $id)"
        declare_uncovered="$declare_uncovered $id"
        continue
    fi

    # Relocation is a REPAIR, not a default: a throwaway home also throws away the
    # CLI's credentials, so it is used only when the host's own install is broken
    # and the turn would otherwise be certain to report nothing. A healthy host
    # config keeps its auth and is left alone.
    health="$("$PIX" sources --json | jq -r --arg i "$id" '.[] | select(.id==$i) | .health // empty')"
    env_pair=""
    if [ -n "$health" ]; then
        home_env="$("$ROSTER_BIN" --roster | awk -F'\t' -v i="$id" '$1==i{print $4}')"
        if [ "$home_env" != "-" ] && [ -n "$home_env" ] &&
            mkdir -p "$SB/home-$id" &&
            env "$home_env=$SB/home-$id" "$PIX" connect "$id" --json >/dev/null 2>&1; then
            env_pair="$home_env=$SB/home-$id"
            echo "  (host install broken — retrying in an isolated $home_env)"
        else
            echo "  BLOCKED $id — $health"
            declare_uncovered="$declare_uncovered $id"
            continue
        fi
    fi

    bin="$("$ROSTER_BIN" --roster | awk -F'\t' -v i="$id" '$1==i{split($5,a," "); print a[1]}')"
    IFS='|' read -r sub extra <<<"$spec"
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "  BLOCKED $id — '$bin' is not on PATH here"
        declare_uncovered="$declare_uncovered $id"
        continue
    fi
    echo "[$id] $bin $sub — one real model turn"
    # stdin from /dev/null: a CLI that also accepts a piped prompt otherwise waits
    # on the inherited terminal forever (codex: "Reading additional input...").
    # shellcheck disable=SC2086  # $extra is a flag LIST and must word-split
    (cd "$WS" && env ${env_pair:+"$env_pair"} PIXTUOID_SOCKET="$SOCK" \
        "$bin" "$sub" "$PROMPT" $extra </dev/null >"$SB/$id.log" 2>&1) &
    CLIPID=$!

    # Registration alone proves little — junk content registers a sprite too. The
    # tool-driven transition is the assertion; registration is reported so a
    # failure says WHICH half broke.
    seen=0
    drove=0
    # `[^],]*`, not `[^]]*`: entries are comma-joined, so a class stopping only
    # at `]` walks into the next agent and passes this source on its state.
    state_re="agents=\[[^]]*${prefix}[·@][^],]*:(active|waiting)"
    timed_out=1
    for _ in $(seq 1 "$TURN_TIMEOUT"); do
        grep -q "agents=\[[^]]*${prefix}[·@]" "$OUT" 2>/dev/null && seen=1
        grep -qE "$state_re" "$OUT" 2>/dev/null && drove=1
        [ "$drove" = 1 ] && {
            timed_out=0
            break
        }
        kill -0 "$CLIPID" 2>/dev/null || {
            # The CLI finished; give the watcher a beat to observe its last write.
            sleep 3
            grep -q "agents=\[[^]]*${prefix}[·@]" "$OUT" 2>/dev/null && seen=1
            grep -qE "$state_re" "$OUT" 2>/dev/null && drove=1
            timed_out=0
            break
        }
        sleep 1
    done
    kill "$CLIPID" 2>/dev/null
    wait "$CLIPID" 2>/dev/null
    cli_rc=$?
    CLIPID=""

    # A CLI that failed on ITS OWN account (no credit, no API key, not logged in)
    # says nothing about pixtuoid. Reporting that as a decode failure would be the
    # same lie as calling an unrunnable check a pass. `timed_out` guards it: the
    # timeout path arrives here with `cli_rc=143` from OUR OWN kill.
    if [ "$timed_out" = 0 ] && [ "$drove" = 0 ] && [ "$seen" = 0 ] && [ "$cli_rc" -ne 0 ]; then
        echo "  BLOCKED $id — the CLI itself failed (exit $cli_rc), not a pixtuoid result"
        tail -2 "$SB/$id.log" | sed 's/^/      /'
        declare_uncovered="$declare_uncovered $id"
        continue
    fi
    if [ "$drove" = 0 ] && [ "$seen" = 0 ] && [ "$SHIM_ON_PATH" = 0 ]; then
        echo "  BLOCKED $id — its turn ran, but pixtuoid-hook is off PATH so no hook could fire"
        declare_uncovered="$declare_uncovered $id"
        continue
    fi

    if [ "$drove" = 1 ]; then
        echo "  PASS $id — ${prefix} registered AND reached a lifecycle state"
        covered=$((covered + 1))
    elif [ "$seen" = 1 ]; then
        echo "  FAIL $id — ${prefix} registered but never left idle (tool-detail decode?)" >&2
        FAILED=1
    elif [ "$timed_out" = 1 ]; then
        echo "  FAIL $id — no ${prefix} sprite after ${TURN_TIMEOUT}s (this tier killed the CLI)" >&2
        echo "  --- $bin output tail ---" >&2
        tail -5 "$SB/$id.log" >&2
        FAILED=1
    else
        echo "  FAIL $id — no ${prefix} sprite after its turn" >&2
        echo "  --- $bin output tail ---" >&2
        tail -5 "$SB/$id.log" >&2
        FAILED=1
    fi
done

echo "--- scene timeline ---"
grep 'agents=' "$OUT" | tail -5 | sed 's/^/    /'
echo "live-sources: $covered source(s) rendered from a real CLI turn"
[ -n "$declare_uncovered" ] && echo "live-sources: NOT COVERED —$declare_uncovered"
if [ "$FAILED" != 0 ]; then
    echo "live-sources: FAIL" >&2
elif [ "$covered" -eq 0 ]; then
    # `corpus-all`'s convention: an absent CLI is not a defect, but it is not
    # coverage either, and a green here reads as "every source works".
    echo "live-sources: NOTHING RAN — no source reached a real turn" >&2
    FAILED=2
else
    echo "live-sources: PASS"
fi
trap - EXIT
cleanup
exit "$FAILED"
