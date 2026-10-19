//! OpenClaw install target — the TWO-OWNERSHIP hybrid: a plugin DIR wholly owned
//! by pixtuoid (`<openclaw-home>/plugins/pixtuoid/`, the target's `beside` files,
//! with the shim path baked into `index.js`) PLUS a config merge into
//! `<openclaw-home>/openclaw.json`; neither alone loads the plugin. The install is a
//! pure `ConfigLock` write: a bare `plugins.load.paths` entry follows the normal
//! policy for its origin (openclaw `docs/cli/plugins/install.md:318-320` @
//! v2026.10.1), while `plugins install --link` creates a managed install record that
//! requires capability consent (`docs/plugins/manage-plugins.md:216-218`). The
//! `allowConversationAccess` grant un-gates `before_agent_run`/`agent_end` (the busy
//! tell) and UNINSTALL REVOKES it; the plugin FILES are left in place (the config
//! un-merge stops the gateway loading them).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::install::io;
use crate::install::merge::{prune_empty, prune_empty_root};
use crate::install::target::MergeOutcome;

const PLUGIN_ID: &str = "pixtuoid";

/// OpenClaw's state-dir names, MODERN first (the clawdbot→openclaw rebrand).
const STATE_DIRS: [&str; 2] = [".openclaw", ".clawdbot"];
/// Config filenames inside a state dir, MODERN first — `resolveConfigPath`'s own
/// search order.
const CONFIG_FILES: [&str; 2] = ["openclaw.json", "clawdbot.json"];
#[cfg(test)]
const SENTINEL: &str = "@pixtuoid-openclaw-plugin";
const HOOK_PLACEHOLDER: &str = "\"{{HOOK_PATH_JSON}}\"";
pub(crate) const PLUGIN_TEMPLATE: &str = include_str!("openclaw_plugin.js");

/// The OpenClaw gateway hook events the plugin FORWARDS, pinned to its `HOOKS`
/// array and `decode_openclaw_hook_payload`'s arms by the consistency test below.
/// A rename upstream makes that hook silently stop firing, since the plugin
/// registers by name.
#[cfg(test)]
pub(crate) const OPENCLAW_EVENTS: &[&str] = &[
    "gateway_start",
    "gateway_stop",
    "session_start",
    "session_end",
    "before_agent_run",
    "agent_end",
];

/// The hooks the plugin registers only to stamp `agent_end`'s verdict, pinned to its
/// `VERDICT_HOOKS`.
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
/// `resolveStateDir`: the `OPENCLAW_STATE_DIR` override wins; else
/// `<effective-home>/.openclaw`, where the effective home is `OPENCLAW_HOME`, then
/// **`$HOME`, then `%USERPROFILE%`** — HOME-FIRST, NOT pixtuoid's generic
/// `USERPROFILE`-first `io::home_relative`. A Windows user who exports `HOME` (Git
/// Bash / MSYS2 / Cygwin) has the gateway read `%HOME%\.openclaw\`, so writing to
/// `%USERPROFILE%\.openclaw\` would leave our plugin where the gateway never loads
/// it. The legacy `.clawdbot` dir is preferred when `.openclaw` is absent and it
/// exists — don't shadow the user's real config.
fn openclaw_state_dir() -> Result<PathBuf> {
    // OpenClaw `~`-expands both env vars against its OS home, so mirror that before
    // the path logic.
    let home = pixtuoid_core::platform::home_first_dir();
    resolve_openclaw_state_dir(
        pixtuoid_core::platform::path_env("OPENCLAW_STATE_DIR")
            .map(|v| io::expand_tilde(&v, home.as_deref())),
        pixtuoid_core::platform::path_env("OPENCLAW_HOME")
            .map(|v| io::expand_tilde(&v, home.as_deref())),
        home,
        |p| p.exists(),
    )
}

/// Pure core for [`openclaw_state_dir`] — inputs injected so the precedence is
/// unit-testable without env/FS mutation.
fn resolve_openclaw_state_dir(
    state_dir_env: Option<PathBuf>,
    openclaw_home_env: Option<PathBuf>,
    os_home_first: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf> {
    if let Some(d) = state_dir_env {
        return Ok(d);
    }
    let home = openclaw_home_env.or(os_home_first).ok_or_else(|| {
        io::home_unset(
            "OpenClaw's home",
            "OPENCLAW_STATE_DIR/OPENCLAW_HOME/HOME/USERPROFILE",
        )
    })?;
    let candidates = state_dir_candidates(&home);
    Ok(candidates
        .iter()
        .find(|c| exists(c))
        .cloned()
        // Nothing exists yet ⇒ a fresh install lands in the MODERN dir; never
        // shadow a real legacy one, which is why `find` runs first.
        .unwrap_or_else(|| candidates[0].clone()))
}

fn state_dir_candidates(home: &Path) -> Vec<PathBuf> {
    STATE_DIRS.iter().map(|d| home.join(d)).collect()
}

/// The config file we merge into, mirroring OpenClaw's `resolveConfigPath`: the
/// `OPENCLAW_CONFIG_PATH` override (a FULL config-file path, assumed absolute)
/// wins; else the first EXISTING of the four modern/legacy dir × file candidates;
/// else `<modern-dir>/openclaw.json` for a fresh install — never shadow a real
/// config the gateway still reads.
pub(crate) fn default_config_path() -> Result<PathBuf> {
    let home = pixtuoid_core::platform::home_first_dir();
    let state_env = pixtuoid_core::platform::path_env("OPENCLAW_STATE_DIR");
    // An explicit OPENCLAW_STATE_DIR points AT the dir and bypasses home resolution,
    // so it gets NO legacy sibling search — the operator named the scope.
    let legacy_home = match state_env {
        Some(_) => None,
        None => pixtuoid_core::platform::path_env("OPENCLAW_HOME")
            .map(|v| io::expand_tilde(&v, home.as_deref()))
            .or_else(|| home.clone()),
    };
    Ok(resolve_openclaw_config_path(
        pixtuoid_core::platform::path_env("OPENCLAW_CONFIG_PATH")
            .map(|v| io::expand_tilde(&v, home.as_deref())),
        openclaw_state_dir()?,
        legacy_home,
        |p| p.exists(),
    ))
}

/// Pure core for [`default_config_path`], inputs injected.
///
/// The candidate list is FLAT across both dirs, not "pick a dir, then pick a file
/// in it" (upstream searches the same way). The nested form had a real hole: with
/// `~/.openclaw` PRESENT (so it wins as the state dir) but the actual config at
/// `~/.clawdbot/openclaw.json`, we resolved a path the gateway never reads —
/// installing hooks into a file nobody loads, with `doctor` reporting green.
/// `home` only contributes the legacy SIBLING (`None` ⇒ just the state dir's two).
fn resolve_openclaw_config_path(
    config_path_env: Option<PathBuf>,
    state_dir: PathBuf,
    home: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(p) = config_path_env {
        return p;
    }
    let mut dirs = vec![state_dir.clone()];
    if let Some(h) = home {
        for legacy_dir in state_dir_candidates(&h) {
            if legacy_dir != state_dir {
                dirs.push(legacy_dir);
            }
        }
    }
    for dir in &dirs {
        for file in CONFIG_FILES {
            let cand = dir.join(file);
            if exists(&cand) {
                return cand;
            }
        }
    }
    state_dir.join(CONFIG_FILES[0])
}

fn plugin_dir() -> Result<PathBuf> {
    Ok(openclaw_state_dir()?.join("plugins").join(PLUGIN_ID))
}

/// Auto-detect probe: is OpenClaw present (its state dir exists), so the
/// Sources panel OFFERS it? Probes OpenClaw's OWN dir, NOT our plugin/config —
/// keying on our artifact would chicken-and-egg.
pub(crate) fn detect_installed() -> bool {
    // The SAME env vars the SAME way `openclaw_state_dir()` does: without the
    // `~`-expansion an override installs into the EXPANDED dir but probes the
    // literal `~/…`, so the panel never offers the OpenClaw it just wrote to.
    let home = pixtuoid_core::platform::home_first_dir();
    resolve_openclaw_detect(
        pixtuoid_core::platform::path_env("OPENCLAW_STATE_DIR")
            .map(|v| io::expand_tilde(&v, home.as_deref())),
        pixtuoid_core::platform::path_env("OPENCLAW_HOME")
            .map(|v| io::expand_tilde(&v, home.as_deref())),
        home,
        |p| p.exists(),
    )
}

/// Pure core for [`detect_installed`] — parallels [`resolve_openclaw_state_dir`]
/// but answers "does ANY OpenClaw state dir exist" instead of picking one.
fn resolve_openclaw_detect(
    state_dir_env: Option<PathBuf>,
    openclaw_home_env: Option<PathBuf>,
    os_home_first: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> bool {
    if let Some(d) = state_dir_env {
        return exists(&d);
    }
    let Some(home) = openclaw_home_env.or(os_home_first) else {
        return false;
    };
    state_dir_candidates(&home).iter().any(|c| exists(c))
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
    crate::install::merge::bake_hook_path(PLUGIN_TEMPLATE, HOOK_PLACEHOLDER, hook_path, "openclaw")
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
/// `Unknown` and `verify_target` downgrades that to a soft note. The HARD checks
/// are the two config-level facts only WE write: the enabled `entries.pixtuoid`
/// entry + its `load.paths` dir registration.
pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    use crate::install::verify::{SchemaParse, ShimRef};
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
        return SchemaParse::broken(
            "the pixtuoid plugin entry is missing from openclaw.json — reconnect openclaw",
        );
    }
    let mut issues = Vec::new();
    let mut notes = Vec::new();
    if entry["enabled"] != json!(true) {
        issues.push("the pixtuoid openclaw plugin is installed but disabled".into());
    }
    if entry["hooks"]["allowConversationAccess"] != json!(true) {
        issues.push(
            "openclaw.json `plugins.entries.pixtuoid.hooks.allowConversationAccess` is not true \
             — the gateway fires no run hooks, so the lobster never shows busy"
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
    // Separator-tolerant so a Windows backslash path still matches.
    let registered = root["plugins"]["load"]["paths"]
        .as_array()
        .is_some_and(|paths| {
            paths.iter().any(|p| {
                p.as_str().is_some_and(|s| {
                    s.replace('\\', "/")
                        .ends_with(&format!("plugins/{PLUGIN_ID}"))
                })
            })
        });
    if !registered {
        issues
            .push("openclaw.json `load.paths` no longer registers the pixtuoid plugin dir".into());
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

    #[test]
    fn openclaw_state_dir_override_wins_outright() {
        let p = resolve_openclaw_state_dir(
            Some("/custom/state".into()),
            Some("/ignored/home".into()),
            Some(PathBuf::from("/ignored/oshome")),
            |_| panic!("exists() must not be consulted when OPENCLAW_STATE_DIR is set"),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/custom/state"));
    }

    #[test]
    fn openclaw_state_dir_honors_openclaw_home_then_os_home_first() {
        let p = resolve_openclaw_state_dir(
            None,
            Some(r"D:\claw".into()),
            Some(PathBuf::from(r"C:\Users\me")),
            |_| false,
        )
        .unwrap();
        assert_eq!(p, PathBuf::from(r"D:\claw").join(".openclaw"));
        let p =
            resolve_openclaw_state_dir(None, None, Some(PathBuf::from(r"C:\Users\me")), |_| false)
                .unwrap();
        assert_eq!(p, PathBuf::from(r"C:\Users\me").join(".openclaw"));
    }

    #[test]
    fn openclaw_state_dir_prefers_legacy_clawdbot_only_when_modern_absent() {
        let home = PathBuf::from("/home/u");
        let modern = home.join(".openclaw");
        let legacy = home.join(".clawdbot");
        let p =
            resolve_openclaw_state_dir(None, None, Some(home.clone()), |q| q == modern).unwrap();
        assert_eq!(p, modern);
        let p =
            resolve_openclaw_state_dir(None, None, Some(home.clone()), |q| q == legacy).unwrap();
        assert_eq!(p, legacy);
        let p = resolve_openclaw_state_dir(None, None, Some(home), |_| false).unwrap();
        assert_eq!(p, modern);
    }

    #[test]
    fn openclaw_config_path_override_and_legacy_file_preference() {
        let home = PathBuf::from("/home/u");
        let state = home.join(".openclaw");
        let modern = state.join("openclaw.json");
        let legacy = state.join("clawdbot.json");
        let h = || Some(home.clone());
        let p = resolve_openclaw_config_path(
            Some("/custom/oc.json".into()),
            state.clone(),
            h(),
            |_| panic!("exists() must not be consulted when OPENCLAW_CONFIG_PATH is set"),
        );
        assert_eq!(p, PathBuf::from("/custom/oc.json"));
        assert_eq!(
            resolve_openclaw_config_path(None, state.clone(), h(), |q| q == modern),
            modern
        );
        assert_eq!(
            resolve_openclaw_config_path(None, state.clone(), h(), |q| q == legacy),
            legacy
        );
        assert_eq!(
            resolve_openclaw_config_path(None, state.clone(), h(), |_| false),
            modern
        );
    }

    #[test]
    fn openclaw_config_path_finds_a_config_in_the_legacy_dir_sibling() {
        let home = PathBuf::from("/home/u");
        let state = home.join(".openclaw"); // present, but holds no config
        let real = home.join(".clawdbot").join("openclaw.json");
        assert_eq!(
            resolve_openclaw_config_path(None, state.clone(), Some(home.clone()), |q| q == real),
            real,
            "the legacy dir's config must win over a config-less modern dir"
        );
        let modern = state.join("openclaw.json");
        assert_eq!(
            resolve_openclaw_config_path(None, state.clone(), Some(home.clone()), |q| q == real
                || q == modern),
            modern
        );
        // `legacy_home: None` is what `default_config_path` passes when
        // OPENCLAW_STATE_DIR is set.
        let overridden = PathBuf::from("/custom/state");
        assert_eq!(
            resolve_openclaw_config_path(None, overridden.clone(), None, |q| q == real),
            overridden.join("openclaw.json"),
            "an overridden state dir outranks the legacy home sibling"
        );
    }

    #[test]
    fn openclaw_detect_probes_the_same_resolved_dirs_as_install() {
        let home = PathBuf::from("/home/u");
        assert!(resolve_openclaw_detect(
            Some(home.join("claw")),
            None,
            None,
            |q| q == home.join("claw"),
        ));
        assert!(!resolve_openclaw_detect(
            Some(home.join("claw")),
            None,
            None,
            |_| false
        ));
        let claw_home = PathBuf::from("/expanded/claw");
        assert!(resolve_openclaw_detect(
            None,
            Some(claw_home.clone()),
            Some(home.clone()),
            |q| q == claw_home.join(".clawdbot"),
        ));
        assert!(resolve_openclaw_detect(
            None,
            None,
            Some(home.clone()),
            |q| q == home.join(".openclaw")
        ));
        assert!(!resolve_openclaw_detect(None, None, None, |_| panic!(
            "exists() must not be consulted when no home resolves"
        )));
    }

    #[test]
    fn openclaw_state_dir_errors_when_nothing_resolves() {
        let err = resolve_openclaw_state_dir(None, None, None, |_| false).unwrap_err();
        assert!(
            err.to_string().ends_with("unset); set one and reconnect"),
            "unresolvable home must surface the actionable error: {err}"
        );
    }

    #[test]
    fn openclaw_events_plugin_decoder_and_const_agree() {
        use pixtuoid_core::source::openclaw::decode_openclaw_hook_payload;
        for ev in OPENCLAW_EVENTS {
            assert!(
                PLUGIN_TEMPLATE.contains(&format!("\"{ev}\"")),
                "plugin HOOKS is missing the registered event `{ev}`"
            );
        }
        // The names of the array `[…]` or object `{ name: handler, … }` `name` binds.
        let names =
            |name: &str, open: &str, close: &str| -> std::collections::HashSet<&'static str> {
                PLUGIN_TEMPLATE
                    .split_once(&format!("const {name} = {open}"))
                    .and_then(|(_, rest)| rest.split_once(close))
                    .map(|(inner, _)| inner)
                    .unwrap_or_else(|| panic!("plugin defines {name}"))
                    .split(',')
                    .map(|s| {
                        s.split(':')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .trim_matches('"')
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            };
        assert_eq!(
            names("HOOKS", "[", "];"),
            OPENCLAW_EVENTS.iter().copied().collect(),
            "plugin HOOKS drifted from OPENCLAW_EVENTS"
        );
        assert_eq!(
            names("VERDICT_HOOKS", "{", "};"),
            OPENCLAW_VERDICT_EVENTS.iter().copied().collect(),
            "plugin VERDICT_HOOKS drifted from OPENCLAW_VERDICT_EVENTS"
        );
        for ev in OPENCLAW_EVENTS {
            let payload = json!({ "type": ev, "gatewayPort": 18789 });
            let decoded = decode_openclaw_hook_payload(&payload).unwrap();
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
        // Serialized with the tests that unset HOME and USERPROFILE, under which
        // `home_first_dir()` returns None.
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
            assert!(!index.contains(HOOK_PLACEHOLDER), "placeholder replaced");
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
            let out = merge_install("{}", "/opt/bin/pixtuoid-hook").unwrap();
            assert!(out.changed);
            let v: Value = serde_json::from_str(&out.content).unwrap();
            let entry = &v["plugins"]["entries"]["pixtuoid"];
            assert_eq!(entry["enabled"], json!(true));
            assert_eq!(
                entry["hooks"]["allowConversationAccess"],
                json!(true),
                "the busy-tell grant"
            );
            let paths = v["plugins"]["load"]["paths"].as_array().unwrap();
            assert!(
                paths.iter().any(|p| {
                    p.as_str()
                        .unwrap()
                        .replace('\\', "/")
                        .ends_with("plugins/pixtuoid")
                }),
                "load.paths points at the plugin dir"
            );
        });
    }

    /// Drives the env-wiring WRAPPER, not the pure core the cases above inject
    /// into. Asserts a shape invariant that holds under any ambient env, so it
    /// needs no env mutation; the env→argument MAPPING itself is covered by the
    /// openclaw e2e scripts.
    #[test]
    fn default_config_path_is_always_a_real_openclaw_config_file() {
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
            let p = default_config_path().expect("a dev machine resolves a home");
            assert!(
                !p.as_os_str().is_empty(),
                "an empty path would be merged into, and created, at the filesystem root"
            );
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a config FILE, not a directory");
            assert!(
                CONFIG_FILES.contains(&name),
                "must be one of OpenClaw's own config filenames {CONFIG_FILES:?} — got {name}"
            );
            let parent = p.parent().expect("the config sits inside a state dir");
            let dir = parent
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a named state dir");
            assert!(
                STATE_DIRS.contains(&dir),
                "must sit in one of OpenClaw's own state dirs {STATE_DIRS:?} — got {dir}"
            );
            assert!(
                p.is_absolute(),
                "a relative path would resolve against the CLI's cwd at merge time"
            );
        });
    }

    #[test]
    fn merge_install_is_idempotent() {
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
            let a = merge_install("{}", "/x").unwrap();
            let b = merge_install(&a.content, "/x").unwrap();
            assert!(!b.changed, "re-install of the same state is a no-op");
        });
    }

    #[test]
    fn merge_install_preserves_foreign_config() {
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
            let installed = merge_install("{}", "/x").unwrap();
            let removed = merge_uninstall(&installed.content).unwrap();
            let v: Value = serde_json::from_str(&removed.content).unwrap();
            assert!(v["plugins"]["entries"].get("pixtuoid").is_none());
        });
    }

    #[test]
    fn empty_content_is_treated_as_empty_document() {
        temp_env::with_vars(Vec::<(&str, Option<&str>)>::new(), || {
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
