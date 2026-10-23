// Invariant #5 (non-negotiable): the shim must never block CC — it always exits 0
// silently on any error. A prod `unwrap()`/`expect()`/`panic!` violates that, so
// they are compiler-denied here (tests unwrap freely).
// Indexing joins them because `release` sets `panic = "abort"`: an out-of-bounds
// slice is a SIGABRT the agent CLI sees, and the parsers this crate feeds on
// untrusted process rows are exactly where one would land.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::ffi::OsString;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

mod cli_pid;
use cli_pid::cli_pid;

mod paths;
use paths::default_socket_path;

mod transport;

/// What [`enrich_payload`] stamps, plus the trailing newline.
const STAMP_HEADROOM: u64 = 256;

/// `STDIN_CAP + STAMP_HEADROOM` is the daemon's pipe `IN_BUFFER_SIZE` (pinned by
/// `tests/pipe_quota_parity.rs`), so a stamped payload can't stall the sync
/// write; a pathological body can still exceed it and degrade to
/// stall→watchdog→drop, never a block of CC.
const STDIN_CAP: u64 = (1 << 20) - STAMP_HEADROOM;

/// Saturating `u128 → u64` narrowing: a truncating `as` cast would wrap anything
/// above `u64::MAX` to a small number.
fn ms_u128_to_u64(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| ms_u128_to_u64(d.as_millis()))
}

fn main() {
    let socket = default_socket_path();

    // `args_os` + lossy, NOT `args()`: `std::env::args()` PANICS on any
    // non-Unicode argument (legal Unix argv), breaching invariant #5. Lossy
    // rather than filter_map: dropping a non-UTF-8 arg would shift
    // `--source <value>` pairing so the NEXT arg gets read as the value.
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let source = source_from_argv(&args)
        .or_else(|| std::env::var_os("PIXTUOID_SOURCE").and_then(|v| v.into_string().ok()));
    let jcode = source.as_deref() == Some("jcode");
    let mut payload: Value = match (jcode, event_from_argv(&args)) {
        (true, _) => {
            let Some(payload) = jcode_payload() else {
                return;
            };
            Value::Object(payload)
        }
        // CodeWhale env-mode: identity arrives as `DEEPSEEK_*` env vars with
        // `--event <name>` baked into the registered command. Critically,
        // CodeWhale does NOT pipe stdin for these events, so the hook child
        // inherits the TUI's terminal stdin and a blind `read_to_string` would
        // BLOCK (freezing the synchronous tool call until the hook timeout) —
        // when `--event` is present we never touch stdin.
        (false, Some(event)) => Value::Object(env_payload(&event)),
        (false, None) => {
            let mut buf = String::new();
            if std::io::stdin()
                .take(STDIN_CAP)
                .read_to_string(&mut buf)
                .is_err()
            {
                return;
            }
            match serde_json::from_str(&buf) {
                Ok(v) => v,
                Err(_) => return,
            }
        }
    };

    // Everything past here is OUR work on CC's clock — `cli_pid` walks a process
    // table, off-thread but not free — so the bound is armed first (see
    // `arm_watchdog`).
    let Some(bound) = transport::arm_watchdog() else {
        return;
    };

    if let Value::Object(map) = &mut payload {
        // Source precedence: the `--source <name>` argv flag (the Windows install
        // form) wins over the `PIXTUOID_SOURCE` env var (the Unix env-prefix
        // form; grok delivers the same var via its handler `env` map, so this arm
        // serves both). `--event` is orthogonal and never implies a source.
        // Jcode's shared server can own many sessions, so its PID cannot name one.
        enrich_payload(map, source, hook_event_from_argv(&args), now_ms(), || {
            (!jcode).then(cli_pid).flatten()
        });
    }

    // Best-effort send, hard-bounded so a stuck daemon can never block CC's
    // subprocess wait — see transport.rs.
    let mut line = serde_json::to_vec(&payload).unwrap_or_default();
    line.push(b'\n');
    transport::send_line(&bound, &socket, &line);
}

fn jcode_payload() -> Option<serde_json::Map<String, Value>> {
    let mut map = serde_json::Map::new();
    // Observers have null stdin; pre_tool stdin is arguments, not an envelope.
    // The full JCODE_HOOK_PAYLOAD also contains replies and tool input.
    for (env_key, field) in [
        ("JCODE_HOOK_EVENT", "event"),
        ("JCODE_HOOK_SESSION_ID", "session_id"),
        ("JCODE_HOOK_CWD", "cwd"),
        ("JCODE_HOOK_TOOL_NAME", "tool_name"),
        ("JCODE_HOOK_MODEL", "model"),
    ] {
        if let Some(value) = std::env::var_os(env_key).filter(|value| !value.is_empty()) {
            map.insert(
                field.into(),
                Value::from(cap_env_field(value.to_string_lossy().into_owned())),
            );
        }
    }
    (map.contains_key("event") && map.contains_key("session_id")).then_some(map)
}

/// CodeWhale env-mode: synthesize the hook envelope from `DEEPSEEK_*` env vars.
/// The `std::env` reads live here so `env_payload_from` stays testable without
/// mutating process-global env.
fn env_payload(event: &str) -> serde_json::Map<String, Value> {
    // CodeWhale runs the hook with current_dir = its working dir (= the
    // workspace), so the shim's own cwd is the reliable fallback.
    let cwd_fallback = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    env_payload_from(event, cwd_fallback, |k| std::env::var_os(k))
}

/// Per-field byte cap on env-mode values. The stdin arm enforces `STDIN_CAP`
/// before parsing; the env arm has no such gate and `DEEPSEEK_TOOL_ARGS` can be
/// large, so capping each folded field keeps the serialized line under the
/// daemon's pipe quota instead of building one the watchdog would drop.
const ENV_FIELD_CAP: usize = 128 * 1024;

fn cap_env_field(mut val: String) -> String {
    val.truncate(val.floor_char_boundary(ENV_FIELD_CAP));
    val
}

fn env_payload_from(
    event: &str,
    cwd_fallback: Option<String>,
    get: impl Fn(&str) -> Option<OsString>,
) -> serde_json::Map<String, Value> {
    let get = |k: &str| get(k).map(|v| v.to_string_lossy().into_owned());
    let mut map = serde_json::Map::new();
    map.insert("event".into(), Value::from(event));
    // cwd is the AgentId KEY (the decoder drops a cwd-less event), and
    // DEEPSEEK_WORKSPACE is UNSET for a fresh `codewhale` launched without `-C`
    // until the workspace resolves — so `session_start` would otherwise never
    // register a sprite. The fallback resolves to the same path the workspace
    // eventually does, so a session's events coalesce on one AgentId.
    if let Some(cwd) = get("DEEPSEEK_WORKSPACE")
        .filter(|v| !v.is_empty())
        .or_else(|| cwd_fallback.filter(|v| !v.is_empty()))
    {
        map.insert("cwd".into(), Value::from(cap_env_field(cwd)));
    }
    for (env_key, field) in [
        ("DEEPSEEK_TOOL_NAME", "tool"),
        ("DEEPSEEK_TOOL_ARGS", "tool_args"),
    ] {
        if let Some(val) = get(env_key).filter(|v| !v.is_empty()) {
            map.insert(field.into(), Value::from(cap_env_field(val)));
        }
    }
    map
}

/// The value of `--<flag> <val>` or `--<flag>=<val>` in argv (first match wins),
/// or `None` if absent or empty.
fn flag_from_argv(args: &[String], flag: &str) -> Option<String> {
    let eq_prefix = format!("{flag}=");
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some(val) = arg.strip_prefix(&eq_prefix) {
            return Some(val).filter(|s| !s.is_empty()).map(str::to_string);
        }
        if arg == flag {
            return it.next().filter(|s| !s.is_empty()).cloned();
        }
    }
    None
}

/// CodeWhale's env-mode trigger. Absent → the shim reads its payload from stdin
/// (the CC/Codex/Reasonix path).
fn event_from_argv(args: &[String]) -> Option<String> {
    flag_from_argv(args, "--event")
}

/// The event a stdin payload fires for, named in argv because the payload omits
/// it (agy's). Unlike `--event`, it leaves the payload on stdin.
fn hook_event_from_argv(args: &[String]) -> Option<String> {
    flag_from_argv(args, "--hook-event")
}

/// The trusted CLI source, in its Windows install form: the hook command runs
/// under `cmd.exe /C`, which has no inline `VAR=value cmd` env-prefix syntax (it
/// would try to exec a program literally named `PIXTUOID_SOURCE=codex`), so the
/// source rides as a flag. Absent → the caller falls back to `PIXTUOID_SOURCE`.
fn source_from_argv(args: &[String]) -> Option<String> {
    flag_from_argv(args, "--source")
}

/// Stamp the shim timestamp and, when resolved, the trusted CLI source and the
/// argv-named event under the PRIVATE `_pixtuoid_source`/`_pixtuoid_event` keys.
///
/// We deliberately do NOT write the public `source` field: CC's SessionStart
/// payload already uses `source` for the start *reason* (startup/resume/clear/
/// compact), and reading that as the CLI source namespaced the agent under
/// "startup" — an un-reapable ghost. The private keys are shim-OWNED, so an
/// inbound one (spoofed or replayed) is stripped unconditionally before stamping.
fn enrich_payload(
    map: &mut serde_json::Map<String, Value>,
    source: Option<String>,
    hook_event: Option<String>,
    ts_ms: u64,
    resolve_pid: impl FnOnce() -> Option<u32>,
) {
    map.remove("_pixtuoid_source");
    map.remove("_pixtuoid_event");
    map.insert("_shim_ts_ms".into(), Value::from(ts_ms));
    if let Some(src) = source
        && !src.is_empty()
    {
        map.insert("_pixtuoid_source".into(), Value::from(src));
    }
    if let Some(event) = hook_event {
        map.insert("_pixtuoid_event".into(), Value::from(event));
    }
    // The opencode/OpenClaw plugins stamp `process.pid` from inside the CLI —
    // keep theirs, and stay LAZY so they never pay for the Windows snapshot.
    if !map.contains_key("_pid")
        && let Some(pid) = resolve_pid()
    {
        map.insert("_pid".into(), Value::from(pid));
    }
}

#[cfg(test)]
mod tests;
