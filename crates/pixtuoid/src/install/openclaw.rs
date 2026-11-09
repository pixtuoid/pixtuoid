//! OpenClaw install target — the TWO-OWNERSHIP hybrid: a plugin DIR wholly owned
//! by pixtuoid (`<state-dir>/plugins/pixtuoid/`, the target's `beside` files,
//! with the shim path baked into `index.js`) PLUS a config merge into
//! `<state-dir>/openclaw.json`; neither alone loads the plugin. The install is a
//! pure `ConfigLock` write: a bare `plugins.load.paths` entry follows the normal
//! policy for its origin (openclaw `docs/cli/plugins/install.md:318-320` @
//! v2026.10.1), while `plugins install --link` creates a managed install record that
//! requires capability consent (`docs/plugins/manage-plugins.md:216-218`). The
//! `allowConversationAccess` grant un-gates `before_agent_run`/`agent_end` (the running
//! tell) and UNINSTALL REVOKES it; the plugin FILES are left in place (the config
//! un-merge stops the gateway loading them).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::install::io;
use crate::install::merge::{prune_empty, prune_empty_root};
use crate::install::target::MergeOutcome;

const PLUGIN_ID: &str = "pixtuoid";

const STATE_DIR: &str = ".openclaw";
const CONFIG_FILE: &str = "openclaw.json";
#[cfg(test)]
const SENTINEL: &str = "@pixtuoid-openclaw-plugin";
pub(crate) const PLUGIN_TEMPLATE: &str = include_str!("openclaw_plugin.js");

/// The OpenClaw gateway hook events the plugin FORWARDS, pinned to what it
/// registers by its selftest and to `decode_openclaw_hook_payload`'s arms by the
/// test below.
#[cfg(test)]
pub(crate) const OPENCLAW_EVENTS: &[&str] = &[
    "gateway_start",
    "gateway_stop",
    "session_start",
    "session_end",
    "before_agent_run",
    "agent_end",
];

/// The hooks the plugin registers only to stamp `agent_end`'s verdict.
#[cfg(test)]
pub(crate) const OPENCLAW_VERDICT_EVENTS: &[&str] = &["model_call_ended"];

const MANIFEST: &str = r#"{
  "id": "pixtuoid",
  "name": "Pixtuoid",
  "description": "Forwards OpenClaw gateway daemon-presence signals to pixtuoid (the terminal office visualizer).",
  "configSchema": { "type": "object", "additionalProperties": false, "properties": {} },
  "activation": { "onStartup": true }
}
"#;

const PACKAGE: &str = r#"{
  "name": "pixtuoid",
  "version": "0.0.0",
  "type": "module",
  "private": true,
  "openclaw": { "extensions": ["./index.js"], "runtimeExtensions": ["./index.js"] }
}
"#;

/// OpenClaw's state dir (holds `openclaw.json` + `plugins/`), mirroring its own
/// `resolveStateDir` (openclaw `src/config/state-dir.ts` @ v2026.11.2): the
/// `OPENCLAW_STATE_DIR` override wins; else `<effective-home>/.openclaw`, where
/// the effective home is `OPENCLAW_HOME`, then [`os_home`] — HOME-FIRST, NOT
/// pixtuoid's generic `USERPROFILE`-first `user_home_opt`. A Windows user who
/// exports `HOME` (Git Bash / MSYS2 / Cygwin) has the gateway read
/// `%HOME%\.openclaw\`, so writing to `%USERPROFILE%\.openclaw\` would leave our
/// plugin where the gateway never loads it.
fn openclaw_state_dir() -> Result<PathBuf> {
    // OpenClaw `~`-expands both env vars against its OS home, so mirror that before
    // the path logic.
    let home = os_home();
    resolve_openclaw_state_dir(
        pixtuoid_core::platform::path_env("OPENCLAW_STATE_DIR")
            .map(|v| io::expand_tilde(&v, home.as_deref())),
        pixtuoid_core::platform::path_env("OPENCLAW_HOME")
            .map(|v| io::expand_tilde(&v, home.as_deref())),
        home,
    )
}

/// Pure core for [`openclaw_state_dir`] — inputs injected so the precedence is
/// unit-testable without env mutation.
fn resolve_openclaw_state_dir(
    state_dir_env: Option<PathBuf>,
    openclaw_home_env: Option<PathBuf>,
    os_home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(d) = state_dir_env {
        return Ok(d);
    }
    let home = openclaw_home_env.or(os_home).ok_or_else(|| {
        io::home_unset(
            "OpenClaw's home",
            "OPENCLAW_STATE_DIR/OPENCLAW_HOME/HOME/USERPROFILE",
        )
    })?;
    Ok(home.join(STATE_DIR))
}

/// OpenClaw's OS home, its `resolveRawOsHomeDir`
/// (`packages/normalization-core/src/home-dir.ts:28-35` @ v2026.11.2): `HOME`,
/// then `USERPROFILE`, then Node's `os.homedir()`, each unset when it trims to
/// nothing, `undefined` or `null` (`:4-15`). Not mirrored: its Termux rung
/// (`:17-26`), which needs Android's `ANDROID_DATA`.
fn os_home() -> Option<PathBuf> {
    use pixtuoid_core::platform::{path_env_trimmed, user_home_opt};
    resolve_os_home(
        path_env_trimmed("HOME"),
        path_env_trimmed("USERPROFILE"),
        user_home_opt,
    )
}

/// Pure core of [`os_home`], the reads injected so every rung unit-tests.
fn resolve_os_home(
    home: Option<PathBuf>,
    userprofile: Option<PathBuf>,
    os_homedir: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let set = |p: &PathBuf| !matches!(p.to_str().map(str::trim), Some("" | "undefined" | "null"));
    home.filter(set)
        .or_else(|| userprofile.filter(set))
        .or_else(|| os_homedir().filter(set))
}

/// The config file we merge into, mirroring OpenClaw's `resolveConfigPath`
/// (`src/config/paths.ts` @ v2026.11.2): the `OPENCLAW_CONFIG_PATH` override (a
/// FULL config-file path), else `<state-dir>/openclaw.json`.
pub(crate) fn default_config_path() -> Result<PathBuf> {
    if let Some(p) = pixtuoid_core::platform::path_env("OPENCLAW_CONFIG_PATH") {
        return Ok(io::expand_tilde(&p, os_home().as_deref()));
    }
    Ok(openclaw_state_dir()?.join(CONFIG_FILE))
}

fn plugin_dir() -> Result<PathBuf> {
    Ok(openclaw_state_dir()?.join("plugins").join(PLUGIN_ID))
}

/// Auto-detect probe: is OpenClaw present (its state dir exists), so the
/// Sources panel OFFERS it? Probes OpenClaw's OWN dir, NOT our plugin/config —
/// keying on our artifact would chicken-and-egg.
pub(crate) fn detect_installed() -> bool {
    openclaw_state_dir().is_ok_and(|d| d.exists())
}

/// The shim's absolute path, baked into the plugin — the gateway runs it under
/// Node, so there is no PATH to rely on.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    crate::install::merge::hook_path_str(resolved).map(str::to_string)
}

pub(crate) fn plugin_artifacts(hook_path: &str) -> Result<Vec<(PathBuf, String)>> {
    let dir = plugin_dir()?;
    Ok(vec![
        (dir.join("index.js"), render_plugin(hook_path)?),
        (dir.join("openclaw.plugin.json"), MANIFEST.to_string()),
        (dir.join("package.json"), PACKAGE.to_string()),
    ])
}

fn render_plugin(hook_path: &str) -> Result<String> {
    crate::install::merge::bake_hook_path(PLUGIN_TEMPLATE, hook_path, "openclaw")
}

/// The advice the JSON5 refusal and the schema notes point at.
const OWNER_CLI_ADVICE: &str = "register it with OpenClaw's own CLI instead: \
     `openclaw plugins install --link <dir>` + `openclaw plugins enable pixtuoid`, \
     then set plugins.entries.pixtuoid.hooks.allowConversationAccess = true";

/// OpenClaw's config-include key — its presence means the EFFECTIVE `plugins`
/// block may not be the one in this document.
const INCLUDE_KEY: &str = "$include";

/// Does this JSON value name OUR plugin id? TRIM-only and CASE-SENSITIVE, because
/// that is what upstream does: `normalizePluginIdWithLookup` lowercases only to
/// build an ALIAS LOOKUP KEY and returns the `trimmed` original when no alias
/// matches (`pixtuoid` is not a built-in alias), and the allow/deny callers then
/// compare exactly. Do NOT "restore" a case-insensitive form: an allowlist
/// containing `"Pixtuoid"` would make install skip the join (plugin disabled,
/// `doctor` green), and a `deny: ["Pixtuoid"]` would make `doctor` claim a break
/// over a plugin OpenClaw had actually loaded.
fn is_plugin_id(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.trim() == PLUGIN_ID)
}

/// Does `v` carry an [`INCLUDE_KEY`] ANYWHERE, not just at the document root? A
/// root-only check would hand a clean bill to a config whose real `plugins` block
/// we never saw. Recursion depth is bounded by serde_json's own parse limit.
fn contains_include(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.contains_key(INCLUDE_KEY) || m.values().any(contains_include),
        Value::Array(a) => a.iter().any(contains_include),
        _ => false,
    }
}

/// Parse `openclaw.json` for a MERGE — strict JSON only, with the honest reason.
///
/// OpenClaw reads its own config with **JSON5**, so comments, trailing commas,
/// single quotes and unquoted keys are all LEGAL on disk even though OpenClaw's own
/// writer emits strict JSON. Our read→merge→write round-trip re-serializes through
/// `serde_json`, which cannot represent any of that: parsing it would mean silently
/// DELETING the user's comments on their next `connect`. So a non-strict document
/// is refused with the owner-CLI path instead of being rewritten. Upstream's own
/// `openclaw config patch` re-serializes strict and loses them too — measured, so
/// don't re-propose it as the fix without re-measuring. Pinned by
/// `merge_refuses_a_json5_document_instead_of_dropping_its_comments`.
fn parse_for_merge(content: &str) -> Result<Value> {
    if content.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(content).map_err(|e| {
        anyhow!(
            "openclaw.json is not strict JSON ({e}). OpenClaw parses it as JSON5, so \
             comments/trailing commas/single quotes are legal there and pixtuoid will not \
             rewrite the file rather than drop them — {OWNER_CLI_ADVICE}"
        )
    })
}

fn obj_mut<'a>(v: &'a mut Value, key: &str) -> Result<&'a mut serde_json::Map<String, Value>> {
    let map = v
        .as_object_mut()
        .ok_or_else(|| anyhow!("openclaw.json: `{key}` is not a JSON object"))?;
    Ok(map)
}

/// Merge our plugin registration into openclaw.json. `changed` is a semantic
/// (parsed) diff, so a same-state re-install is a no-op; `_hook_cmd` is unused
/// because the shim path lives in the plugin file, not the config.
pub(crate) fn merge_install(content: &str, _hook_cmd: &str) -> Result<MergeOutcome> {
    let dir = plugin_dir()?;
    let dir_str = dir
        .to_str()
        .ok_or_else(|| anyhow!("plugin dir path is non-UTF-8: {}", dir.display()))?
        .to_string();
    let mut root = parse_for_merge(content)?;
    let before = root.clone();
    {
        let root_obj = obj_mut(&mut root, "root")?;
        let plugins = root_obj.entry("plugins").or_insert_with(|| json!({}));
        let plugins = obj_mut(plugins, "plugins")?;

        let load = plugins.entry("load").or_insert_with(|| json!({}));
        let load = obj_mut(load, "plugins.load")?;
        let paths = load.entry("paths").or_insert_with(|| json!([]));
        let paths = paths
            .as_array_mut()
            .ok_or_else(|| anyhow!("openclaw.json: `plugins.load.paths` is not an array"))?;
        if !paths.iter().any(|p| p.as_str() == Some(dir_str.as_str())) {
            paths.push(json!(dir_str));
        }

        let entries = plugins.entry("entries").or_insert_with(|| json!({}));
        let entries = obj_mut(entries, "plugins.entries")?;
        entries.insert(
            PLUGIN_ID.to_string(),
            json!({ "enabled": true, "hooks": { "allowConversationAccess": true } }),
        );

        // Upstream gates on `allow.length === 0 || allow.includes(id)`, so a curated
        // list omitting us never loads the plugin however enabled its entry — while
        // an EMPTY list is already NO restriction, with nothing to join.
        if let Some(allow) = plugins.get_mut("allow").and_then(Value::as_array_mut)
            && !allow.is_empty()
            && !allow.iter().any(is_plugin_id)
        {
            allow.push(json!(PLUGIN_ID));
        }
    }
    let changed = root != before;
    Ok(MergeOutcome {
        changed,
        content: serde_json::to_string_pretty(&root)? + "\n",
    })
}

/// Remove our registration, revoking the conversation-access grant with it. A
/// foreign plugin's entries/paths survive.
pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    let dir = plugin_dir()?;
    let dir_str = dir.to_str().map(str::to_string);
    let mut root = parse_for_merge(content)?;
    let before = root.clone();
    // `has_hooks` asks whether a dry-run uninstall CHANGES the parsed doc, so
    // pruning a pre-existing empty `plugins: {}` — a shape we never wrote — reports
    // hooks-installed to a user who never connected.
    let mut removed = false;
    if let Some(plugins) = root.get_mut("plugins").and_then(Value::as_object_mut) {
        if let Some(paths) = plugins
            .get_mut("load")
            .and_then(Value::as_object_mut)
            .and_then(|l| l.get_mut("paths"))
            .and_then(Value::as_array_mut)
        {
            let was = paths.len();
            paths.retain(|p| p.as_str().map(str::to_string) != dir_str);
            removed |= paths.len() != was;
        }
        if let Some(entries) = plugins.get_mut("entries").and_then(Value::as_object_mut) {
            removed |= entries.remove(PLUGIN_ID).is_some();
        }
        if removed {
            // NEVER by emptying the list: dropping the last member turns
            // `["pixtuoid"]` into `[]`, which upstream reads as NO restriction — an
            // uninstall flipping a fail-closed key fail-OPEN. A single-member list is
            // one we never wrote (install only joins a list that already had
            // members), so leaving the dangling id is the fail-CLOSED residue.
            if let Some(allow) = plugins.get_mut("allow").and_then(Value::as_array_mut) {
                // POST-condition, not a pre-count: `["pixtuoid", " pixtuoid "]` is two
                // entries BOTH ours, so a `len() > 1` pre-check let the retain empty it.
                let kept: Vec<Value> = allow.iter().filter(|v| !is_plugin_id(v)).cloned().collect();
                if !kept.is_empty() {
                    *allow = kept;
                }
            }
            // Prune containers now OURS-ONLY-AND-EMPTY so a disconnect leaves the
            // config as it found it; one holding anything else is never touched.
            prune_empty(plugins, "entries");
            if let Some(load) = plugins.get_mut("load").and_then(Value::as_object_mut) {
                prune_empty(load, "paths");
            }
            prune_empty(plugins, "load");
        }
    }
    if removed {
        prune_empty_root(&mut root, "plugins");
    }
    let changed = root != before;
    Ok(MergeOutcome {
        changed,
        content: serde_json::to_string_pretty(&root)? + "\n",
    })
}

/// Install-schema check — the "silent-dead source" detector. The shim path lives
/// in the SEPARATE plugin `index.js`, NOT this config, so the shim ref is
/// `Unknown` and `verify_target` reads it from that file instead. The HARD checks
/// are the two config-level facts only WE write: the enabled `entries.pixtuoid`
/// entry + its `load.paths` dir registration.
pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    use crate::install::verify::{SchemaParse, ShimRef, reconnect};
    let Ok(root) = serde_json::from_str::<Value>(content) else {
        // NOT "broken": OpenClaw reads this as JSON5, so a document our strict parser
        // rejects may be loading fine. Reporting a break told the user to "reconnect
        // openclaw" — advice that cannot succeed, the merge refusing the same doc.
        return SchemaParse {
            issues: vec![],
            notes: vec![format!(
                "openclaw.json is not strict JSON (OpenClaw reads it as JSON5), so pixtuoid \
                 cannot verify the plugin registration — {OWNER_CLI_ADVICE}"
            )],
            shim: ShimRef::Unknown,
        };
    };
    let entry = &root["plugins"]["entries"][PLUGIN_ID];
    if entry.is_null() {
        return SchemaParse::broken(format!(
            "the pixtuoid plugin entry is missing from openclaw.json — {}",
            reconnect(pixtuoid_core::source::openclaw::SOURCE_NAME)
        ));
    }
    let mut issues = Vec::new();
    let mut notes = Vec::new();
    if entry["enabled"] != json!(true) {
        issues.push("the pixtuoid openclaw plugin is installed but disabled".into());
    }
    if entry["hooks"]["allowConversationAccess"] != json!(true) {
        issues.push(
            "openclaw.json `plugins.entries.pixtuoid.hooks.allowConversationAccess` is not true \
             — the gateway fires no run hooks, so the lobster never shows running"
                .into(),
        );
    }
    // Gates the WHOLE plugin system upstream, and an ABSENT key means enabled
    // (`plugins enable` writes only the entry, never this), so only an EXPLICIT
    // `false` is reportable — and as the user's own switch it is a NOTE.
    if root["plugins"]["enabled"] == json!(false) {
        notes.push(
            "openclaw.json `plugins.enabled = false` — OpenClaw loads NO plugin at all (your \
             own switch; pixtuoid leaves it untouched)"
                .into(),
        );
    }
    // Upstream marks `plugins.load` — the key we write — `kind: "restart"`, so `off`
    // and `hot` leave a RUNNING gateway serving without the plugin while
    // `restart`/`hybrid` pick it up. A NOTE: the install is sound, just not applied.
    if let Some(mode) = root["gateway"]["reload"]["mode"].as_str()
        && (mode == "off" || mode == "hot")
    {
        notes.push(format!(
            "openclaw.json `gateway.reload.mode = \"{mode}\"` never applies a restart-kind \
                 change, and the plugin registration is one — a RUNNING gateway keeps serving \
                 without pixtuoid until it is restarted (`openclaw gateway restart`)"
        ));
    }
    // Both FAIL-CLOSED for us and invisible to the checks above. An EMPTY allow list
    // is NO restriction upstream (`allow.length === 0 || …`), so it stays silent.
    if root["plugins"]["allow"]
        .as_array()
        .is_some_and(|allow| !allow.is_empty() && !allow.iter().any(is_plugin_id))
    {
        issues.push(
            "openclaw.json `plugins.allow` does not list pixtuoid — the allowlist is \
             fail-closed, so the plugin never loads"
                .into(),
        );
    }
    if root["plugins"]["deny"]
        .as_array()
        .is_some_and(|deny| deny.iter().any(is_plugin_id))
    {
        issues.push(
            "openclaw.json `plugins.deny` lists pixtuoid — the plugin is denied, so it never \
             loads"
                .into(),
        );
    }
    if contains_include(&root) {
        notes.push(format!(
            "openclaw.json uses `{INCLUDE_KEY}` — the effective plugins config may come from \
             an included file, which pixtuoid does not read"
        ));
    }
    // THIS state dir's plugin dir, compared as a `Path`: `verify_beside` stats only
    // that dir, so a path a relocated state dir left behind would verify green
    // while the gateway loads nothing (dsh's mount check, for the same reason).
    let ours = plugin_dir();
    let registered = root["plugins"]["load"]["paths"]
        .as_array()
        .is_some_and(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .any(|s| ours.as_deref().is_ok_and(|d| Path::new(s) == d))
        });
    if !registered {
        issues.push(format!(
            "openclaw.json `load.paths` does not register this state dir's pixtuoid plugin \
             dir — {}",
            reconnect(pixtuoid_core::source::openclaw::SOURCE_NAME)
        ));
    }
    SchemaParse {
        issues,
        notes,
        shim: ShimRef::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "/state/openclaw";

    /// Pins the state dir every config op derives `plugin_dir()` from, so no test
    /// reads the developer's own OpenClaw env or home.
    fn in_state_dir<R>(f: impl FnOnce() -> R) -> R {
        temp_env::with_vars(
            [
                ("OPENCLAW_STATE_DIR", Some(STATE)),
                ("OPENCLAW_HOME", None),
                ("OPENCLAW_CONFIG_PATH", None),
            ],
            f,
        )
    }

    #[test]
    fn verify_wants_this_state_dirs_plugin_path_not_any_pixtuoid_path() {
        in_state_dir(|| {
            let installed = merge_install("{}", "").unwrap();
            assert!(verify_schema(&installed.content).issues.is_empty());
            let mut moved: Value = serde_json::from_str(&installed.content).unwrap();
            moved["plugins"]["load"]["paths"] = json!(["/old/state/plugins/pixtuoid"]);
            let verdict = verify_schema(&moved.to_string());
            assert!(
                verdict
                    .issues
                    .iter()
                    .any(|i| i.contains("`load.paths` does not register")),
                "a path left by another state dir must not verify green: {:?}",
                verdict.issues
            );
        });
    }

    #[test]
    fn openclaw_state_dir_override_wins_outright() {
        let p = resolve_openclaw_state_dir(
            Some("/custom/state".into()),
            Some("/ignored/home".into()),
            Some(PathBuf::from("/ignored/oshome")),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/custom/state"));
    }

    #[test]
    fn openclaw_state_dir_honors_openclaw_home_then_os_home() {
        let p = resolve_openclaw_state_dir(
            None,
            Some(r"D:\claw".into()),
            Some(PathBuf::from(r"C:\Users\me")),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from(r"D:\claw").join(".openclaw"));
        let p =
            resolve_openclaw_state_dir(None, None, Some(PathBuf::from(r"C:\Users\me"))).unwrap();
        assert_eq!(p, PathBuf::from(r"C:\Users\me").join(".openclaw"));
    }

    #[test]
    fn os_home_is_home_then_userprofile_then_os_homedir_skipping_js_nullish() {
        let s = |v: &str| Some(PathBuf::from(v));
        let unreached = || -> Option<PathBuf> { panic!("a set HOME or USERPROFILE decides") };
        assert_eq!(resolve_os_home(s("/h"), s("/up"), unreached), s("/h"));
        assert_eq!(resolve_os_home(None, s("/up"), unreached), s("/up"));
        assert_eq!(
            resolve_os_home(s("undefined"), s("/up"), unreached),
            s("/up")
        );
        assert_eq!(resolve_os_home(s("null"), None, || s("/os")), s("/os"));
        assert_eq!(
            resolve_os_home(None, s("undefined"), || s("undefined")),
            None
        );
        assert_eq!(resolve_os_home(None, None, || s("   ")), None);
    }

    #[test]
    fn os_home_trims_as_upstream_does() {
        temp_env::with_vars([("HOME", Some(" /srv/h ")), ("USERPROFILE", None)], || {
            assert_eq!(os_home(), Some(PathBuf::from("/srv/h")));
        });
    }

    #[test]
    fn openclaw_state_dir_is_the_modern_dir_whatever_else_exists_beside_it() {
        let home = tempfile::tempdir().expect("temp home");
        std::fs::create_dir(home.path().join(".clawdbot")).expect("a leftover pre-rename dir");
        assert_eq!(
            resolve_openclaw_state_dir(None, None, Some(home.path().to_path_buf())).unwrap(),
            home.path().join(".openclaw"),
            "upstream's resolveStateDir never falls back to .clawdbot"
        );
    }

    #[test]
    fn the_config_is_the_override_else_openclaw_json_in_the_state_dir() {
        let home = tempfile::tempdir().expect("temp home");
        let state = home.path().join(".openclaw");
        std::fs::create_dir(&state).expect("state dir");
        std::fs::write(state.join("clawdbot.json"), "{}").expect("a pre-rename config file");
        let custom = home.path().join("custom").join("oc.json");
        let custom = custom.to_str().expect("utf-8 temp path");
        let home_str = home.path().to_str().expect("utf-8 temp path");
        temp_env::with_vars(
            [
                ("HOME", Some(home_str)),
                ("USERPROFILE", None),
                ("OPENCLAW_HOME", None),
                ("OPENCLAW_STATE_DIR", None),
                ("OPENCLAW_CONFIG_PATH", Some(custom)),
            ],
            || assert_eq!(default_config_path().unwrap(), PathBuf::from(custom)),
        );
        temp_env::with_vars(
            [
                ("HOME", Some(home_str)),
                ("USERPROFILE", None),
                ("OPENCLAW_HOME", None),
                ("OPENCLAW_STATE_DIR", None),
                ("OPENCLAW_CONFIG_PATH", None),
            ],
            || assert_eq!(default_config_path().unwrap(), state.join("openclaw.json")),
        );
    }

    #[test]
    fn detect_probes_the_dir_install_writes_to() {
        let home = tempfile::tempdir().expect("temp home");
        let state = home.path().join("claw");
        let state_str = state.to_str().expect("utf-8 temp path");
        let probe = || {
            temp_env::with_vars(
                [
                    ("OPENCLAW_STATE_DIR", Some(state_str)),
                    ("OPENCLAW_HOME", None),
                ],
                detect_installed,
            )
        };
        assert!(!probe(), "no state dir yet");
        std::fs::create_dir(&state).expect("state dir");
        assert!(probe());
    }

    #[test]
    fn openclaw_state_dir_errors_when_nothing_resolves() {
        let err = resolve_openclaw_state_dir(None, None, None).unwrap_err();
        assert!(
            err.to_string().ends_with("unset); set one and reconnect"),
            "unresolvable home must surface the actionable error: {err}"
        );
    }

    /// The plugin's selftest holds what it registers to these consts
    /// (`plugin-contract.json`); each must decode.
    #[test]
    fn every_registered_openclaw_event_decodes_to_its_gateway() {
        use pixtuoid_core::source::openclaw::decode_openclaw_hook_payload;
        for ev in OPENCLAW_EVENTS {
            let payload = json!({ "type": ev, "gatewayPort": 18789 });
            let decoded = decode_openclaw_hook_payload(payload.as_object().unwrap()).unwrap();
            assert!(
                !decoded.updates.is_empty(),
                "decode_openclaw_hook_payload has no arm for registered event `{ev}`"
            );
            assert_eq!(
                decoded.instance.as_str(),
                "18789",
                "`{ev}` must resolve its sending gateway, not fall back"
            );
        }
    }

    #[test]
    fn install_renders_plugin_with_baked_shim_path_and_sentinel() {
        in_state_dir(|| {
            let arts = plugin_artifacts("/opt/bin/pixtuoid-hook").unwrap();
            assert_eq!(arts.len(), 3, "manifest + package.json + index.js");
            let index = &arts
                .iter()
                .find(|(p, _)| p.ends_with("index.js"))
                .unwrap()
                .1;
            assert!(
                index.contains(SENTINEL),
                "entry module carries the sentinel"
            );
            assert!(
                index.contains("\"/opt/bin/pixtuoid-hook\""),
                "shim path baked JSON-escaped"
            );
            assert!(
                !index.contains(crate::install::merge::HOOK_PLACEHOLDER),
                "placeholder replaced"
            );
            assert!(
                index.contains("--source"),
                "spawns the shim with --source openclaw"
            );
        });
    }

    #[test]
    fn rendered_hook_binding_round_trips_escaped_paths() {
        for path in [
            r"C:\Program Files\Pixtuoid\pixtuoid-hook.exe",
            r#"/tmp/"quoted"/pixtuoid-hook"#,
        ] {
            let rendered = render_plugin(path).unwrap();
            let binding = rendered
                .lines()
                .find(|line| line.starts_with("const HOOK_PATH = "))
                .unwrap();
            let encoded = binding
                .strip_prefix("const HOOK_PATH = ")
                .unwrap()
                .strip_suffix(';')
                .unwrap();
            let expected_json = serde_json::to_string(path).unwrap();
            assert_eq!(binding, format!("const HOOK_PATH = {expected_json};"));
            assert_eq!(serde_json::from_str::<String>(encoded).unwrap(), path);
        }
    }

    #[test]
    fn merge_install_adds_load_path_enabled_and_the_grant() {
        in_state_dir(|| {
            let out = merge_install("{}", "/opt/bin/pixtuoid-hook").unwrap();
            assert!(out.changed);
            let v: Value = serde_json::from_str(&out.content).unwrap();
            let entry = &v["plugins"]["entries"]["pixtuoid"];
            assert_eq!(entry["enabled"], json!(true));
            assert_eq!(
                entry["hooks"]["allowConversationAccess"],
                json!(true),
                "the running-tell grant"
            );
            let ours = Path::new(STATE).join("plugins").join(PLUGIN_ID);
            assert_eq!(
                v["plugins"]["load"]["paths"],
                json!([ours.to_str().unwrap()]),
                "load.paths points at the state dir's plugin dir"
            );
        });
    }

    #[test]
    fn merge_install_is_idempotent() {
        in_state_dir(|| {
            let a = merge_install("{}", "/x").unwrap();
            let b = merge_install(&a.content, "/x").unwrap();
            assert!(!b.changed, "re-install of the same state is a no-op");
        });
    }

    #[test]
    fn merge_install_preserves_foreign_config() {
        in_state_dir(|| {
            let foreign = r#"{"gateway":{"mode":"local"},"plugins":{"entries":{"anthropic":{"enabled":true}},"load":{"paths":["/some/other/plugin"]}}}"#;
            let out = merge_install(foreign, "/x").unwrap();
            let v: Value = serde_json::from_str(&out.content).unwrap();
            assert_eq!(v["gateway"]["mode"], json!("local"), "foreign keys survive");
            assert_eq!(v["plugins"]["entries"]["anthropic"]["enabled"], json!(true));
            let paths = v["plugins"]["load"]["paths"].as_array().unwrap();
            assert!(
                paths
                    .iter()
                    .any(|p| p.as_str() == Some("/some/other/plugin")),
                "foreign path kept"
            );
            assert_eq!(paths.len(), 2, "ours appended, foreign kept");
        });
    }

    #[test]
    fn include_is_detected_at_any_depth_and_only_when_present() {
        in_state_dir(|| {
            for clean in [
                json!({}),
                json!({"gateway": {"port": 18789}}),
                json!({"plugins": {"entries": {"pixtuoid": {"enabled": true}},
                               "load": {"paths": ["/o/plugins/pixtuoid"]}}}),
                json!({"notes": "$included by hand", "x": {"include": "y"}}),
            ] {
                assert!(
                    !contains_include(&clean),
                    "no {INCLUDE_KEY} here — must not be flagged: {clean}"
                );
            }

            for dirty in [
                json!({INCLUDE_KEY: "./base.json"}),
                json!({"plugins": {INCLUDE_KEY: "./plugins.json"}}),
                json!({"plugins": {"load": {"paths": [{INCLUDE_KEY: "./p.json"}]}}}),
                json!({"a": [[{"deep": {INCLUDE_KEY: "./d.json"}}]]}),
            ] {
                assert!(
                    contains_include(&dirty),
                    "an {INCLUDE_KEY} anywhere means our verdict may not describe what the \
                 gateway loads: {dirty}"
                );
            }

            let sound = merge_install("{}", "").unwrap().content;
            assert!(
                !verify_schema(&sound)
                    .notes
                    .iter()
                    .any(|n| n.contains(INCLUDE_KEY)),
                "a config we just wrote has no {INCLUDE_KEY} to report"
            );
            let mut with_include: Value = serde_json::from_str(&sound).unwrap();
            with_include["plugins"][INCLUDE_KEY] = json!("./more.json");
            let v = verify_schema(&with_include.to_string());
            assert!(
                v.notes.iter().any(|n| n.contains(INCLUDE_KEY)),
                "the note must surface — got {:?}",
                v.notes
            );
            assert!(
                v.issues.is_empty(),
                "…but an include is not itself broken, so never a HARD issue — got {:?}",
                v.issues
            );
        });
    }

    #[test]
    fn removing_only_our_load_path_still_counts_as_ours() {
        in_state_dir(|| {
            // A full install, then hand-drop the entry so ONLY the path identifies us.
            let installed = merge_install(r#"{"plugins":{"allow":["anthropic"]}}"#, "").unwrap();
            let mut v: Value = serde_json::from_str(&installed.content).unwrap();
            v["plugins"]["entries"]
                .as_object_mut()
                .expect("install wrote an entries map")
                .remove(PLUGIN_ID);
            assert!(
                v["plugins"]["load"]["paths"]
                    .as_array()
                    .expect("install wrote a paths array")
                    .iter()
                    .any(|p| p.as_str() == plugin_dir().unwrap().to_str()),
                "precondition: our path is what is left identifying us"
            );

            let out = merge_uninstall(&v.to_string()).unwrap();
            assert!(out.changed, "our path was removed — that IS a change");
            let after: Value = serde_json::from_str(&out.content).unwrap();
            assert!(
                !after["plugins"]["load"]["paths"]
                    .as_array()
                    .map(|a| a
                        .iter()
                        .any(|p| p.as_str() == plugin_dir().unwrap().to_str()))
                    .unwrap_or(false),
                "our path is gone: {after}"
            );
            assert_eq!(
                after["plugins"]["allow"],
                json!(["anthropic"]),
                "and the allow JOIN is undone symmetrically, leaving the user's own id"
            );
        });
    }

    #[test]
    fn install_joins_a_curated_allowlist_but_never_an_empty_one() {
        in_state_dir(|| {
            let curated = merge_install(r#"{"plugins":{"allow":["anthropic"]}}"#, "").unwrap();
            let v: Value = serde_json::from_str(&curated.content).unwrap();
            let allow = v["plugins"]["allow"].as_array().unwrap();
            assert!(
                allow.iter().any(|x| x == "pixtuoid") && allow.iter().any(|x| x == "anthropic"),
                "join the allowlist without evicting the user's own ids: {allow:?}"
            );
            let again = merge_install(&curated.content, "").unwrap();
            assert!(!again.changed, "a re-install is a semantic no-op");

            let cased = merge_install(r#"{"plugins":{"allow":["Pixtuoid"]}}"#, "").unwrap();
            let v: Value = serde_json::from_str(&cased.content).unwrap();
            assert_eq!(
                v["plugins"]["allow"],
                json!(["Pixtuoid", "pixtuoid"]),
                "a case-variant does not admit us — our exact id must be joined"
            );
            assert!(
                verify_schema(&cased.content).issues.is_empty(),
                "…and the joined list is then sound"
            );
            // Whitespace IS trimmed upstream, so a padded entry is already us.
            let padded = merge_install(r#"{"plugins":{"allow":[" pixtuoid "]}}"#, "").unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&padded.content).unwrap()["plugins"]["allow"],
                json!([" pixtuoid "]),
                "a padded form of our id is already us — no duplicate"
            );

            let empty = merge_install(r#"{"plugins":{"allow":[]}}"#, "").unwrap();
            let v: Value = serde_json::from_str(&empty.content).unwrap();
            assert_eq!(
                v["plugins"]["allow"].as_array().map(Vec::len),
                Some(0),
                "an empty allowlist must not be written into"
            );
            let verdict = verify_schema(&empty.content);
            assert!(
                verdict.issues.is_empty(),
                "an empty allowlist permits every plugin — not a break: {:?}",
                verdict.issues
            );
            assert!(
                !verdict.notes.iter().any(|n| n.contains("`plugins.allow`")),
                "…and not worth a note either: {:?}",
                verdict.notes
            );
        });
    }

    #[test]
    fn verify_flags_every_fail_closed_switch_that_silently_stops_the_plugin() {
        in_state_dir(|| {
            let installed = merge_install("{}", "").unwrap();
            let with = |mutate: &dyn Fn(&mut Value)| {
                let mut v: Value = serde_json::from_str(&installed.content).unwrap();
                mutate(&mut v);
                verify_schema(&v.to_string())
            };

            let verdict = with(&|v| v["plugins"]["deny"] = json!(["pixtuoid"]));
            assert!(
                verdict
                    .issues
                    .iter()
                    .any(|i| i.contains("`plugins.deny` lists pixtuoid")),
                "a denylist naming us is a HARD break: {:?}",
                verdict.issues
            );
            let verdict = with(&|v| v["plugins"]["deny"] = json!(["Pixtuoid"]));
            assert!(
                verdict.issues.is_empty(),
                "a case-variant in deny is NOT us — must not report a break: {:?}",
                verdict.issues
            );

            let verdict = with(&|v| v["plugins"]["entries"] = json!({}));
            assert!(
                verdict
                    .issues
                    .iter()
                    .any(|i| i.ends_with("reconnect OpenClaw")),
                "a missing entry names the Sources row to reconnect: {:?}",
                verdict.issues
            );

            let verdict = with(&|v| v["plugins"]["entries"]["pixtuoid"]["hooks"] = json!({}));
            assert!(
                verdict
                    .issues
                    .iter()
                    .any(|i| i.contains("allowConversationAccess")),
                "the grant we write must be verified: {:?}",
                verdict.issues
            );

            let verdict = with(&|v| v["plugins"]["enabled"] = json!(false));
            assert!(
                verdict.issues.is_empty(),
                "the user's global switch is not OUR break: {:?}",
                verdict.issues
            );
            assert!(
                verdict
                    .notes
                    .iter()
                    .any(|n| n.contains("`plugins.enabled = false`")),
                "…but it IS why nothing loads: {:?}",
                verdict.notes
            );
            assert!(
                !verify_schema(&installed.content)
                    .notes
                    .iter()
                    .any(|n| n.contains("plugins.enabled")),
                "an absent key is the enabled default — never reported"
            );

            let verdict = with(&|v| v["gateway"] = json!({ "$include": "./gw.json" }));
            assert!(
                verdict.notes.iter().any(|n| n.contains("$include")),
                "an include under any key must be surfaced: {:?}",
                verdict.notes
            );
        });
    }

    #[test]
    fn verify_flags_an_allowlist_that_omits_us_and_notes_json5_and_include() {
        in_state_dir(|| {
            let installed = merge_install("{}", "").unwrap();
            assert!(verify_schema(&installed.content).issues.is_empty());

            let mut v: Value = serde_json::from_str(&installed.content).unwrap();
            v["plugins"]["allow"] = json!(["anthropic"]);
            let verdict = verify_schema(&v.to_string());
            assert!(
                verdict
                    .issues
                    .iter()
                    .any(|i| i.contains("`plugins.allow` does not list pixtuoid")),
                "a fail-closed allowlist must be a HARD issue: {:?}",
                verdict.issues
            );

            let json5 = "{\n  // the user's note\n  \"plugins\": {},\n}\n";
            let verdict = verify_schema(json5);
            assert!(
                verdict.issues.is_empty(),
                "a JSON5 config is legal upstream — never a hard break: {:?}",
                verdict.issues
            );
            assert!(
                verdict.notes.iter().any(|n| n.contains("JSON5")),
                "…but it must say why it could not be verified: {:?}",
                verdict.notes
            );

            let mut v: Value = serde_json::from_str(&installed.content).unwrap();
            v["$include"] = json!("./extra.json");
            let verdict = verify_schema(&v.to_string());
            assert!(
                verdict.notes.iter().any(|n| n.contains("$include")),
                "an include must be surfaced: {:?}",
                verdict.notes
            );
        });
    }

    #[test]
    fn a_reload_mode_that_never_applies_our_write_is_noted_but_not_a_break() {
        in_state_dir(|| {
            let installed = merge_install("{}", "").unwrap();
            let with_mode = |mode: &str| {
                let mut v: Value = serde_json::from_str(&installed.content).unwrap();
                v["gateway"] = json!({ "reload": { "mode": mode } });
                verify_schema(&v.to_string())
            };
            for mode in ["off", "hot"] {
                let verdict = with_mode(mode);
                assert!(
                    verdict.issues.is_empty(),
                    "{mode}: the user's own reload switch is not OUR break: {:?}",
                    verdict.issues
                );
                assert!(
                    verdict
                        .notes
                        .iter()
                        .any(|n| n.contains("gateway.reload.mode") && n.contains(mode)),
                    "{mode}: must be surfaced as the reason nothing loaded yet: {:?}",
                    verdict.notes
                );
            }
            for mode in ["restart", "hybrid"] {
                assert!(
                    !with_mode(mode)
                        .notes
                        .iter()
                        .any(|n| n.contains("gateway.reload.mode")),
                    "{mode} applies our write on its own — it must stay silent"
                );
            }
            assert!(
                !verify_schema(&installed.content)
                    .notes
                    .iter()
                    .any(|n| n.contains("gateway.reload.mode")),
                "an absent reload mode is the hybrid default — never reported"
            );
        });
    }

    #[test]
    fn uninstall_prunes_its_own_husk_but_keeps_anything_foreign() {
        in_state_dir(|| {
            let installed = merge_install("{}", "").unwrap();
            let removed = merge_uninstall(&installed.content).unwrap();
            assert!(removed.changed);
            let v: Value = serde_json::from_str(&removed.content).unwrap();
            assert_eq!(v, json!({}), "no husk left behind, got {v}");

            let shared = merge_install(
                r#"{"plugins":{"entries":{"anthropic":{"enabled":true}}}}"#,
                "",
            )
            .unwrap();
            let removed = merge_uninstall(&shared.content).unwrap();
            let v: Value = serde_json::from_str(&removed.content).unwrap();
            assert_eq!(v["plugins"]["entries"]["anthropic"]["enabled"], json!(true));
            assert!(v["plugins"]["entries"].get("pixtuoid").is_none());
        });
    }

    #[test]
    fn uninstall_revokes_the_grant_but_keeps_foreign_entries() {
        in_state_dir(|| {
            let installed = merge_install(
                r#"{"plugins":{"entries":{"anthropic":{"enabled":true}}}}"#,
                "/x",
            )
            .unwrap();
            let removed = merge_uninstall(&installed.content).unwrap();
            assert!(removed.changed);
            let v: Value = serde_json::from_str(&removed.content).unwrap();
            assert!(
                v["plugins"]["entries"].get("pixtuoid").is_none(),
                "our entry (incl. the conversation-access grant) is revoked"
            );
            assert_eq!(
                v["plugins"]["entries"]["anthropic"]["enabled"],
                json!(true),
                "a foreign plugin's grant survives"
            );
            // `load.paths` held ONLY ours, so the uninstall prunes the emptied array
            // (and its `load` container) — either shape satisfies "our path removed".
            let paths = v["plugins"]["load"]["paths"].as_array();
            assert!(
                paths.is_none_or(|ps| !ps
                    .iter()
                    .any(|p| p.as_str().is_some_and(|s| s.ends_with("plugins/pixtuoid")))),
                "our load.path removed, got {paths:?}"
            );
        });
    }

    #[test]
    fn uninstall_of_unmanaged_config_is_a_no_op() {
        in_state_dir(|| {
            // `changed` IS the `has_hooks` signal, so any shape we did not write must
            // report false — including the empty containers our own prune produces.
            for unmanaged in [
                "{}",
                "",
                r#"{"gateway":{"mode":"local"}}"#,
                r#"{"plugins":{}}"#,
                r#"{"plugins":{"entries":{}}}"#,
                r#"{"plugins":{"load":{"paths":[]}}}"#,
                r#"{"plugins":{"allow":[]}}"#,
                r#"{"plugins":{"entries":{"anthropic":{"enabled":true}}}}"#,
            ] {
                assert!(
                    !merge_uninstall(unmanaged).unwrap().changed,
                    "uninstall must be a no-op on a config we never wrote: {unmanaged}"
                );
            }
        });
    }

    #[test]
    fn uninstall_undoes_the_allowlist_join_without_ever_emptying_it() {
        in_state_dir(|| {
            let joined = merge_install(r#"{"plugins":{"allow":["anthropic"]}}"#, "").unwrap();
            let v: Value = serde_json::from_str(&merge_uninstall(&joined.content).unwrap().content)
                .expect("valid json");
            assert_eq!(
                v["plugins"]["allow"],
                json!(["anthropic"]),
                "the user's own allowlist must come back exactly as we found it, got {v}"
            );
            assert!(v["plugins"]["entries"].get("pixtuoid").is_none());

            let lone = merge_install(r#"{"plugins":{"allow":["pixtuoid"]}}"#, "").unwrap();
            let v: Value = serde_json::from_str(&merge_uninstall(&lone.content).unwrap().content)
                .expect("valid json");
            assert_eq!(
                v["plugins"]["allow"],
                json!(["pixtuoid"]),
                "the last member must survive — emptying it widens the allowlist, got {v}"
            );

            let cased =
                merge_install(r#"{"plugins":{"allow":["anthropic","Pixtuoid"]}}"#, "").unwrap();
            let v: Value = serde_json::from_str(&merge_uninstall(&cased.content).unwrap().content)
                .expect("valid json");
            assert_eq!(
                v["plugins"]["allow"],
                json!(["anthropic", "Pixtuoid"]),
                "only OUR exact id is un-joined; the user's own entries survive, got {v}"
            );
        });
    }

    #[test]
    fn uninstall_never_empties_the_allowlist_even_when_every_entry_is_ours() {
        in_state_dir(|| {
            // Upstream trims, so a padded copy is the SAME id — the first two inputs are
            // lists whose every entry is ours.
            for input in [
                r#"{"plugins":{"allow":["pixtuoid"," pixtuoid "]}}"#,
                r#"{"plugins":{"allow":["pixtuoid","pixtuoid"]}}"#,
                r#"{"plugins":{"allow":["pixtuoid"]}}"#,
            ] {
                let installed = merge_install(input, "").unwrap();
                let out = merge_uninstall(&installed.content).unwrap();
                let v: Value = serde_json::from_str(&out.content).unwrap();
                let allow = v["plugins"]["allow"].as_array();
                assert!(
                    allow.is_none_or(|a| !a.is_empty()),
                    "uninstall emptied a curated allowlist (fail-OPEN) for {input}: {v}"
                );
            }
        });
    }

    #[test]
    fn merge_refuses_a_json5_document_instead_of_dropping_its_comments() {
        in_state_dir(|| {
            // VALID JSON5 that serde_json cannot represent: a comment plus a trailing
            // comma.
            let json5 = "{\n  // my gateway notes — DO NOT LOSE\n  \"gateway\": { \"port\": 19789 },\n  \"plugins\": {},\n}\n";
            for err in [
                merge_install(json5, "/x").unwrap_err(),
                merge_uninstall(json5).unwrap_err(),
            ] {
                let msg = err.to_string();
                assert!(
                    msg.contains("not strict JSON") && msg.contains("openclaw plugins install"),
                    "the refusal must name the reason AND the owner CLI: {msg}"
                );
            }
        });
    }

    #[test]
    fn install_then_uninstall_round_trips() {
        in_state_dir(|| {
            let installed = merge_install("{}", "/x").unwrap();
            let removed = merge_uninstall(&installed.content).unwrap();
            let v: Value = serde_json::from_str(&removed.content).unwrap();
            assert!(v["plugins"]["entries"].get("pixtuoid").is_none());
        });
    }

    #[test]
    fn empty_content_is_treated_as_empty_document() {
        in_state_dir(|| {
            let out = merge_install("", "/x").unwrap();
            assert!(out.changed);
            assert!(serde_json::from_str::<Value>(&out.content).is_ok());
        });
    }

    #[test]
    fn hook_command_returns_absolute_path() {
        assert_eq!(
            hook_command(Path::new("/opt/bin/pixtuoid-hook"), false).unwrap(),
            "/opt/bin/pixtuoid-hook"
        );
    }
}
