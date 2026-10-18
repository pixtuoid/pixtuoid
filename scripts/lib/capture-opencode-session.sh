#!/usr/bin/env bash
# Drive one opencode session for `just capture-fixture opencode <scenario>`:
# a private `serve` with the bundled plugin, one `run --auto` turn on "$1",
# then the session's deletion through that server. BILLED: one model turn.
#
# `session.deleted` has exactly one door, a mutation THROUGH the running
# server, so the deletion rides the same server the turn did.
set -uo pipefail

# The release the capture records, the registry's verified_version as the
# recorder names it, rather than whatever opencode is on PATH.
OC_VERSION="${PIXTUOID_CAPTURE_VERSION:?run through just capture-fixture, which names the version}"
PORT="${OC_PORT:-4103}"
BASE="http://127.0.0.1:$PORT"
LOGS="${PIXTUOID_CAPTURE_DRIVER_DIR:-$(mktemp -d)}"

repo="$(cd "$(dirname "$0")/../.." && pwd)"
# A missing shim would otherwise yield a silently EMPTY (still billed) capture:
# the plugin swallows a failed spawn.
shim="$repo/target/release/pixtuoid-hook"
if [[ ! -x $shim ]]; then
    echo "capture-opencode-session: missing $shim — run 'just build --release' first" >&2
    exit 1
fi

# The platform package's binary, unpacked: `@opencode/cli` itself only links it
# from a postinstall script, which `npx` does not run.
bin_dir="/tmp/pixtuoid-capture/opencode-$OC_VERSION"
oc_bin="$bin_dir/package/bin/opencode"
if [[ ! -x $oc_bin ]]; then
    platform="$(node -p 'process.platform + "-" + process.arch')"
    rm -rf "$bin_dir"
    mkdir -p "$bin_dir"
    if ! tgz="$(cd "$bin_dir" && npm pack -q "@opencode/cli-$platform@$OC_VERSION")" ||
        ! tar -xzf "$bin_dir/$tgz" -C "$bin_dir"; then
        echo "capture-opencode-session: could not unpack @opencode/cli-$platform@$OC_VERSION" >&2
        exit 1
    fi
fi
oc() { "$oc_bin" "$@"; }

# The CLI the recorder asks its version (`CLI_ENV`, capture_fixture.rs).
[[ -z ${PIXTUOID_CAPTURE_CLI:-} ]] || echo "$oc_bin" >"$PIXTUOID_CAPTURE_CLI"

# The plugin loads from a private config dir, so the operator's own opencode
# config and plugin stay untouched.
export OPENCODE_CONFIG_DIR="$LOGS/opencode-config"
mkdir -p "$OPENCODE_CONFIG_DIR/plugins"
python3 - "$repo" "$OPENCODE_CONFIG_DIR/plugins/pixtuoid.ts" "$shim" <<'PY'
import json, pathlib, sys
repo = pathlib.Path(sys.argv[1])
template = (repo / "crates/pixtuoid/src/install/opencode_plugin.ts").read_text()
marker = '"{{HOOK_PATH_JSON}}"'
if marker not in template:
    sys.exit("opencode_plugin.ts lost the HOOK_PATH placeholder")
pathlib.Path(sys.argv[2]).write_text(template.replace(marker, json.dumps(sys.argv[3])))
PY
render=$?
if [[ $render -ne 0 ]]; then
    echo "capture-opencode-session: plugin render failed" >&2
    exit 1
fi

# opencode allows every action by default
# (`services/www/src/docs/content/permissions.mdx:203`), so the gate needs an
# explicit ask rule, written into the recorder's sandbox (the cwd) rather than
# the user's config.
cat >opencode.jsonc <<'EOF'
{ "permissions": [{ "action": "shell", "resource": "*", "effect": "ask" }] }
EOF

# A run-scoped password for the private server; the clients read it from the env.
OPENCODE_PASSWORD="$(python3 -c 'import secrets; print(secrets.token_urlsafe(24))')"
export OPENCODE_PASSWORD

# The binary itself, not `oc`: `$!` must be the server, not a function's subshell.
"$oc_bin" serve --port "$PORT" >"$LOGS/opencode-serve.log" 2>&1 &
srv=$!
trap 'kill -TERM "$srv" 2>/dev/null' EXIT

# Listing from the sandbox also loads its location, and with it the plugin,
# before the turn's first event.
ready=
for _ in $(seq 1 60); do
    if oc session list --server "$BASE" --format json >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 1
done
if [[ -z $ready ]]; then
    echo "capture-opencode-session: the server never answered; see $LOGS/opencode-serve.log" >&2
    exit 1
fi
sleep 3

# OC_MODEL picks the turn's model; opencode's default can be a retired free one.
oc run --server "$BASE" --auto ${OC_MODEL:+--model "$OC_MODEL"} "$1"
rc=$?

sid="$(oc session list --server "$BASE" --format json --max-count 1 |
    python3 -c 'import json, sys; s = json.load(sys.stdin); print(s[0]["id"] if s else "")')"
echo "session ${sid:-NONE}"
if [[ -n $sid ]]; then
    oc session delete --server "$BASE" "$sid"
fi
sleep 3
exit "$rc"
