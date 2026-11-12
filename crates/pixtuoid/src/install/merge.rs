//! Install-WRITE shared helpers — the config parse/merge core every JSON/TOML/YAML
//! target's `merge_install`/`merge_uninstall` rides. These fns MUTATE;
//! `verify.rs` next door is the READ-only soundness detector. Per-target FORMAT
//! knowledge stays in each `install/<target>.rs` (invariant #3); only the
//! shape-shared machinery lives here.

use crate::install::target::MergeOutcome;
use anyhow::Context;
use saphyr::{LoadableYamlNode, YamlOwned};
use serde_json::{Map, Value, json};

/// Parse JSON config content, treating empty/whitespace-only as the empty
/// document (`{}`) — the shared rule every JSON target's merge relies on.
pub(crate) fn parse_json_or_empty(content: &str) -> anyhow::Result<Value> {
    if content.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(content).context("not valid JSON — refusing to overwrite")
}

/// Where a plugin template holds the shim's path until [`bake_hook_path`] bakes
/// it in: quoted, so the template stays valid source before rendering.
pub(crate) const HOOK_PLACEHOLDER: &str = "\"{{HOOK_PATH_JSON}}\"";

/// Bake `hook_path` (JSON-escaped) into a plugin `template` at
/// [`HOOK_PLACEHOLDER`]. A JSON string is a string literal in JS since ES2019's
/// [JSON superset](https://github.com/tc39/proposal-json-superset) and in Python
/// ([escapes](https://docs.python.org/3/reference/lexical_analysis.html#escape-sequences)).
pub(crate) fn bake_hook_path(
    template: &str,
    hook_path: &str,
    what: &str,
) -> anyhow::Result<String> {
    let json = serde_json::to_string(hook_path)
        .with_context(|| format!("serializing the hook path into the {what} plugin"))?;
    Ok(template.replace(HOOK_PLACEHOLDER, &json))
}

/// A code artifact pixtuoid renders whole, which [`Self::sentinel`] marks as ours.
pub(crate) struct SentinelPlugin {
    pub(crate) sentinel: &'static str,
    /// What an uninstall leaves: loadable, inert, and no longer ours.
    pub(crate) removed_stub: &'static str,
    /// How an issue names it ("the omp extension").
    pub(crate) what: &'static str,
    /// The source whose CLI an issue says to reconnect.
    pub(crate) source: &'static str,
}

impl SentinelPlugin {
    /// `changed` only when the content is ours (carries the sentinel) — a foreign
    /// file, the removed stub, or empty content is left untouched.
    pub(crate) fn uninstall(&self, content: &str) -> MergeOutcome {
        let ours = content.contains(self.sentinel);
        MergeOutcome {
            changed: ours,
            content: if ours { self.removed_stub } else { content }.to_string(),
        }
    }
}

/// The shim path as `&str`. A non-UTF-8 path is rejected rather than
/// `to_string_lossy`'d into a silently-dead hook.
pub(crate) fn hook_path_str(p: &std::path::Path) -> anyhow::Result<&str> {
    use anyhow::anyhow;
    p.to_str()
        .ok_or_else(|| anyhow!("pixtuoid-hook path is non-UTF-8: {}", p.display()))
}

fn parse_toml(content: &str) -> anyhow::Result<toml::Table> {
    toml::from_str(content).context("not valid TOML — refusing to overwrite")
}

/// The one YAML document `content` holds, `None` when it holds none; `file` names
/// it in the error. A second is refused: the CLIs whose YAML pixtuoid reads
/// load exactly one and reject the file otherwise (dsh's js-yaml `load`,
/// `lib/loader.js:1791-1799` @ 4.3.2; Hermes's ruamel `Composer.get_single_node`,
/// `composer.py:65-83` @ 0.18.16).
pub(crate) fn single_yaml_doc(content: &str, file: &str) -> anyhow::Result<Option<YamlOwned>> {
    let mut docs = YamlOwned::load_from_str(content)
        .map_err(|e| anyhow::anyhow!("{file} does not parse as YAML: {e}"))?;
    if docs.len() > 1 {
        anyhow::bail!("{file} holds more than one YAML document, which its CLI refuses to load");
    }
    Ok(docs.pop())
}

/// `item` as a table, an inline one opened up so a nested array of tables fits in
/// it; any other value stays as found.
pub(crate) fn open_toml_table(item: &mut toml_edit::Item) -> Option<&mut toml_edit::Table> {
    *item = match std::mem::take(item).into_table() {
        Ok(table) => toml_edit::Item::Table(table),
        Err(other) => other,
    };
    item.as_table_mut()
}

/// [`open_toml_table`] for an array of tables.
pub(crate) fn open_toml_tables(
    item: &mut toml_edit::Item,
) -> Option<&mut toml_edit::ArrayOfTables> {
    *item = match std::mem::take(item).into_array_of_tables() {
        Ok(tables) => toml_edit::Item::ArrayOfTables(tables),
        Err(other) => other,
    };
    item.as_array_of_tables_mut()
}

/// `parent[key]` as a table an install can fill, starting empty when absent. A value
/// of any other shape is the owner's, so the merge refuses it.
pub(crate) fn install_toml_table<'a>(
    parent: &'a mut toml_edit::Table,
    key: &str,
) -> anyhow::Result<&'a mut toml_edit::Table> {
    open_toml_table(parent.entry(key).or_insert(toml_edit::table()))
        .with_context(|| format!("`{key}` is not a table — refusing to overwrite"))
}

/// [`install_toml_table`] for an array of tables; an empty inline one has nothing to
/// keep and no table to open.
pub(crate) fn install_toml_tables<'a>(
    parent: &'a mut toml_edit::Table,
    key: &str,
) -> anyhow::Result<&'a mut toml_edit::ArrayOfTables> {
    let empty = || toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    let item = parent.entry(key).or_insert_with(empty);
    if item.as_array().is_some_and(toml_edit::Array::is_empty) {
        *item = empty();
    }
    open_toml_tables(item)
        .with_context(|| format!("`{key}` is not an array of tables — refusing to overwrite"))
}

/// Merge managed hook entries into `root`: for each `event`, drop any prior
/// managed entry (keyed on `sentinel`) and push a fresh one built by
/// `make_entry`. The entry SHAPE stays the caller's — the merge treats the entry
/// opaquely, keying only on the sentinel.
pub(crate) fn flat_json_merge_install(
    mut root: Map<String, Value>,
    events: &[&str],
    sentinel: &str,
    make_entry: impl Fn(&str) -> Value,
    hook_command: &str,
) -> anyhow::Result<Map<String, Value>> {
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("`hooks` is not an object — refusing to overwrite")?;
    for ev in events {
        let entries = hooks
            .entry(*ev)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("`hooks.{ev}` is not an array — refusing to overwrite"))?;
        entries.retain(|entry| !is_flat_managed(entry, sentinel));
        entries.push(make_entry(hook_command));
    }
    Ok(root)
}

/// Drop `key` from `parent` when it is an EMPTY object/array. Anything with
/// content (a foreign plugin's entry, the user's own allowlist members) is left
/// exactly as found.
pub(crate) fn prune_empty(parent: &mut serde_json::Map<String, Value>, key: &str) {
    let empty = match parent.get(key) {
        Some(Value::Object(m)) => m.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    };
    if empty {
        parent.remove(key);
    }
}

pub(crate) fn prune_empty_root(root: &mut Value, key: &str) {
    if let Some(obj) = root.as_object_mut() {
        prune_empty(obj, key);
    }
}

/// Remove managed hook entries (keyed on `sentinel`) from `doc`, then drop any
/// event key whose array went empty and the `hooks` object if it emptied. A
/// target-specific key the install set (Cursor's `version`) is deliberately
/// preserved — this only touches `hooks`.
pub(crate) fn flat_json_merge_uninstall(mut doc: Value, sentinel: &str) -> Value {
    let Some(root) = doc.as_object_mut() else {
        return doc;
    };
    let Some(Value::Object(hooks_obj)) = root.get_mut("hooks") else {
        return doc;
    };
    for (_ev, list) in hooks_obj.iter_mut() {
        if let Some(arr) = list.as_array_mut() {
            arr.retain(|entry| !is_flat_managed(entry, sentinel));
        }
    }
    let to_remove: Vec<String> = hooks_obj
        .iter()
        .filter_map(|(k, v)| match v.as_array() {
            Some(a) if a.is_empty() => Some(k.clone()),
            _ => None,
        })
        .collect();
    for k in to_remove {
        hooks_obj.remove(&k);
    }
    prune_empty(root, "hooks");
    doc
}

pub(crate) fn is_flat_managed(entry: &Value, sentinel: &str) -> bool {
    entry.get(sentinel).and_then(|v| v.as_bool()) == Some(true)
}

/// [`is_flat_managed`] for a TOML entry.
pub(crate) fn is_toml_managed(entry: &toml_edit::Table) -> bool {
    entry
        .get(crate::install::SENTINEL_KEY)
        .and_then(toml_edit::Item::as_bool)
        == Some(true)
}

/// Parse flat-JSON `content`, REFUSE a valid-but-non-object root (which would
/// otherwise be coerced to `{}`, dropping the user's document), run `mutate` on
/// its object (`null` reads as `{}`), and package a `MergeOutcome` whose `changed`
/// is a SEMANTIC parsed-doc diff — a byte diff would churn the user's formatting.
/// The guard lives HERE, once, so a future flat-JSON target cannot forget it.
pub(crate) fn flat_json_merge_outcome_install(
    content: &str,
    what: &str,
    mutate: impl FnOnce(Map<String, Value>) -> anyhow::Result<Map<String, Value>>,
) -> anyhow::Result<MergeOutcome> {
    let doc = parse_json_or_empty(content)?;
    let root = match doc.clone() {
        Value::Object(m) => m,
        Value::Null => Map::new(),
        _ => anyhow::bail!("{what} is valid JSON but not an object — refusing to overwrite"),
    };
    let merged = Value::Object(mutate(root)?);
    let changed = merged != doc;
    Ok(MergeOutcome {
        content: emit_json(content, &merged)?,
        changed,
    })
}

/// The uninstall twin, deliberately UNGUARDED on a non-object root — uninstall
/// must stay a clean no-op on a foreign non-object document, never error.
pub(crate) fn flat_json_merge_outcome_uninstall(
    content: &str,
    mutate: impl FnOnce(Value) -> Value,
) -> anyhow::Result<MergeOutcome> {
    let doc = parse_json_or_empty(content)?;
    let cleaned = mutate(doc.clone());
    let changed = cleaned != doc;
    Ok(MergeOutcome {
        content: emit_json(content, &cleaned)?,
        changed,
    })
}

/// The TOML analog: `mutate` edits the user's own [`toml_edit::DocumentMut`], so
/// what it leaves unopened keeps its bytes.
pub(crate) fn toml_merge_outcome(
    content: &str,
    mutate: impl FnOnce(&mut toml_edit::DocumentMut) -> anyhow::Result<()>,
) -> anyhow::Result<MergeOutcome> {
    let before = parse_toml(content)?;
    let mut doc: toml_edit::DocumentMut = content.parse()?;
    mutate(&mut doc)?;
    let out = doc.to_string();
    let changed = parse_toml(&out)? != before;
    Ok(MergeOutcome {
        content: end_lines_like(content, out),
        changed,
    })
}

/// `value` as pretty JSON for the file `content` held, by [`end_lines_like`].
pub(crate) fn emit_json(content: &str, value: &Value) -> anyhow::Result<String> {
    Ok(end_lines_like(
        content,
        serde_json::to_string_pretty(value)?,
    ))
}

/// `content` with every hook entry whose `command` runs pixtuoid's shim removed
/// ([`crate::install::hook_cmd::invokes_shim`]), and every list item, member or map
/// that removal emptied, whatever shape the release that wrote it used.
pub(crate) fn strip_shim_json(content: &str) -> anyhow::Result<MergeOutcome> {
    flat_json_merge_outcome_uninstall(content, |mut doc| {
        strip_shim_value(&mut doc);
        doc
    })
}

/// What stripping a node did: whether it removed anything, and whether it dropped
/// a member that held only shim entries, so a list item left holds none of its own.
#[derive(Default)]
struct Stripped {
    removed: bool,
    hollowed: bool,
}

fn json_shim_entry(v: &Value) -> bool {
    v.get("command")
        .and_then(Value::as_str)
        .is_some_and(crate::install::hook_cmd::invokes_shim)
}

fn json_emptied(v: &Value) -> bool {
    matches!(v, Value::Array(a) if a.is_empty()) || matches!(v, Value::Object(m) if m.is_empty())
}

fn strip_shim_value(v: &mut Value) -> Stripped {
    let mut done = Stripped::default();
    match v {
        Value::Array(items) => items.retain_mut(|item| {
            if json_shim_entry(item) {
                done.removed = true;
                return false;
            }
            let inner = strip_shim_value(item);
            done.removed |= inner.removed;
            !(inner.removed && (inner.hollowed || json_emptied(item)))
        }),
        Value::Object(members) => members.retain(|_, child| {
            let inner = strip_shim_value(child);
            done.removed |= inner.removed;
            let emptied = inner.removed && json_emptied(child);
            done.hollowed |= emptied;
            !emptied
        }),
        _ => {}
    }
    done
}

/// [`strip_shim_json`] for a YAML config, edited in place so every other byte
/// stays as written; `file` names it in an error.
pub(crate) fn strip_shim_yaml(content: &str, file: &str) -> anyhow::Result<MergeOutcome> {
    let unchanged = || MergeOutcome {
        content: content.to_string(),
        changed: false,
    };
    let yaml = yaml_file(content, file).context("refusing to overwrite")?;
    let Some(doc) = yaml.documents().next() else {
        return Ok(unchanged());
    };
    let Some(root) = doc.as_mapping() else {
        return Ok(unchanged());
    };
    if !strip_shim_node(&yaml_edit::YamlNode::Mapping(root)).removed {
        return Ok(unchanged());
    }
    Ok(MergeOutcome {
        content: yaml.to_string(),
        changed: true,
    })
}

fn yaml_shim_entry(node: &yaml_edit::YamlNode) -> bool {
    node.as_mapping()
        .and_then(|m| m.get("command"))
        .is_some_and(|c| {
            matches!(c, yaml_edit::YamlNode::Scalar(s)
                if crate::install::hook_cmd::invokes_shim(&s.as_string()))
        })
}

fn yaml_emptied(node: &yaml_edit::YamlNode) -> bool {
    node.as_sequence()
        .is_some_and(yaml_edit::Sequence::is_empty)
        || node.as_mapping().is_some_and(yaml_edit::Mapping::is_empty)
}

fn strip_shim_node(node: &yaml_edit::YamlNode) -> Stripped {
    let mut done = Stripped::default();
    if let Some(items) = node.as_sequence() {
        for i in (0..items.len()).rev() {
            let Some(item) = items.get(i) else { continue };
            let inner = if yaml_shim_entry(&item) {
                Stripped {
                    removed: true,
                    hollowed: true,
                }
            } else {
                strip_shim_node(&item)
            };
            done.removed |= inner.removed;
            if inner.removed && (inner.hollowed || yaml_emptied(&item)) {
                items.remove(i);
            }
        }
    } else if let Some(members) = node.as_mapping() {
        for key in members.keys().collect::<Vec<_>>() {
            let Some(child) = members.get(&key) else {
                continue;
            };
            let inner = strip_shim_node(&child);
            done.removed |= inner.removed;
            if inner.removed && yaml_emptied(&child) {
                members.remove(&key);
                done.hollowed = true;
            }
        }
    }
    done
}

/// [`strip_shim_json`] for a TOML config, through `toml_edit`, which keeps every
/// other byte.
pub(crate) fn strip_shim_toml(content: &str) -> anyhow::Result<MergeOutcome> {
    toml_merge_outcome(content, |doc| {
        strip_shim_table(doc.as_table_mut());
        Ok(())
    })
}

fn toml_shim_entry(command: Option<&toml_edit::Value>) -> bool {
    command
        .and_then(toml_edit::Value::as_str)
        .is_some_and(crate::install::hook_cmd::invokes_shim)
}

fn toml_emptied(item: &toml_edit::Item) -> bool {
    match item {
        toml_edit::Item::Table(t) => t.is_empty(),
        toml_edit::Item::ArrayOfTables(a) => a.is_empty(),
        toml_edit::Item::Value(v) => toml_value_emptied(v),
        toml_edit::Item::None => false,
    }
}

fn toml_value_emptied(v: &toml_edit::Value) -> bool {
    v.as_array().is_some_and(toml_edit::Array::is_empty)
        || v.as_inline_table()
            .is_some_and(toml_edit::InlineTable::is_empty)
}

fn strip_shim_table(t: &mut toml_edit::Table) -> Stripped {
    let mut done = Stripped::default();
    t.retain(|_, item| {
        let inner = strip_shim_item(item);
        done.removed |= inner.removed;
        let emptied = inner.removed && toml_emptied(item);
        done.hollowed |= emptied;
        !emptied
    });
    done
}

fn strip_shim_item(item: &mut toml_edit::Item) -> Stripped {
    let mut done = Stripped::default();
    match item {
        toml_edit::Item::Table(t) => return strip_shim_table(t),
        toml_edit::Item::Value(v) => return strip_shim_toml_value(v),
        toml_edit::Item::ArrayOfTables(tables) => {
            for i in (0..tables.len()).rev() {
                let Some(t) = tables.get_mut(i) else { continue };
                let gone = if toml_shim_entry(t.get("command").and_then(toml_edit::Item::as_value))
                {
                    true
                } else {
                    let inner = strip_shim_table(t);
                    done.removed |= inner.removed;
                    inner.removed && (inner.hollowed || t.is_empty())
                };
                if gone {
                    done.removed = true;
                    tables.remove(i);
                }
            }
        }
        toml_edit::Item::None => {}
    }
    done
}

fn strip_shim_toml_value(v: &mut toml_edit::Value) -> Stripped {
    let mut done = Stripped::default();
    if let Some(items) = v.as_array_mut() {
        for i in (0..items.len()).rev() {
            let Some(item) = items.get_mut(i) else {
                continue;
            };
            let gone = if toml_shim_entry(item.as_inline_table().and_then(|t| t.get("command"))) {
                true
            } else {
                let inner = strip_shim_toml_value(item);
                done.removed |= inner.removed;
                inner.removed && (inner.hollowed || toml_value_emptied(item))
            };
            if gone {
                done.removed = true;
                items.remove(i);
            }
        }
    } else if let Some(members) = v.as_inline_table_mut() {
        members.retain(|_, child| {
            let inner = strip_shim_toml_value(child);
            done.removed |= inner.removed;
            let emptied = inner.removed && toml_value_emptied(child);
            done.hollowed |= emptied;
            !emptied
        });
    }
    done
}

/// `content` held for edits in place. saphyr's strict parse gates it, as
/// yaml-edit's parser recovers from syntax errors (its README, "Error recovery").
pub(crate) fn yaml_file(content: &str, file: &str) -> anyhow::Result<yaml_edit::YamlFile> {
    single_yaml_doc(content, file)?;
    <yaml_edit::YamlFile as std::str::FromStr>::from_str(content)
        .map_err(|e| anyhow::anyhow!("{file} does not parse as YAML: {e}"))
}

/// `out` in `content`'s line ending, CRLF when `content` broke every line with one,
/// and ending in a newline, as npm's `package.json` save does (npm/package-json
/// `lib/index.js:249-257` @ v8.0.0); an empty `out` stays empty. Every config
/// rewrite but yaml-edit's in place (dsh's patch list, Hermes's [`strip_shim_yaml`])
/// rides it, pixtuoid's own `config.toml` save included.
pub(crate) fn end_lines_like(content: &str, mut out: String) -> String {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if content.contains("\r\n") && !content.replace("\r\n", "").contains('\n') {
        // Fold first: toml_edit 0.25.17 writes a multi-line string's raw CRLF as is
        // (`src/encode.rs:81-83`), every other break as LF (`src/raw_string.rs:95-114`).
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape a release wrote, Claude Code's group with the sentinel its own
    /// writer drops included, goes; a user's hook, mixed group and empty list stay.
    #[test]
    fn the_shim_strip_takes_every_releases_entry_and_leaves_the_users() {
        let ours = |cmd: &str| json!({ "command": cmd });
        let before = json!({
            "theme": "dark",
            "hooks": {
                "Stop": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "notify-send done"}]},
                    {"matcher": ".*", "hooks": [{"type": "command", "command": "PIXTUOID_SOURCE=claude-code 'pixtuoid-hook'"}]},
                    {"matcher": "x", "hooks": [{"command": "mine"}, ours("'/opt/pixtuoid-hook' --source claude-code")]},
                    ours("PIXTUOID_SOURCE=reasonix '/old/pixtuoid-hook'"),
                ],
                "PreToolUse": [
                    ours("PIXTUOID_SOURCE=reasonix '/old/pixtuoid-hook'"),
                    ours(r"C:\PROGRA~1\PIXTUO~1.EXE --source reasonix"),
                ],
                "Empty": [],
            },
            "lint": {"Stop": []},
            "pixtuoid": {"Stop": [{"matcher": "*", "hooks": [ours("PIXTUOID_SOURCE=antigravity '/old/pixtuoid-hook' --event Stop")]}]},
        });
        let after = json!({
            "theme": "dark",
            "hooks": {
                "Stop": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "notify-send done"}]},
                    {"matcher": "x", "hooks": [{"command": "mine"}]},
                ],
                "Empty": [],
            },
            "lint": {"Stop": []},
        });
        let pretty = |v: &Value| format!("{}\n", serde_json::to_string_pretty(v).unwrap());
        let out = strip_shim_json(&pretty(&before)).unwrap();
        assert!(out.changed);
        assert_eq!(out.content, pretty(&after));
        assert!(!strip_shim_json(&out.content).unwrap().changed);
    }

    /// The YAML strip edits in place: every byte but the shim's entries stays.
    #[test]
    fn the_yaml_shim_strip_leaves_every_other_byte() {
        let user = "# mine\nmodel: gpt\ndisplay:\n  compact: off\nhooks:\n  pre_tool_call:\n    - command: my-guard.sh   # kept\n";
        let ours = "    - command: \"'/old/pixtuoid-hook' --source hermes\"\n      timeout: 5\n      _pixtuoid: true\n  on_session_finalize:\n    - command: \"'/old/pixtuoid-hook' --source hermes\"\n      timeout: 5\n";
        for ending in ["\n", "\r\n"] {
            let out =
                strip_shim_yaml(&format!("{user}{ours}").replace('\n', ending), "c.yml").unwrap();
            assert!(out.changed);
            assert_eq!(out.content, user.replace('\n', ending));
        }
        assert!(!strip_shim_yaml(user, "c.yml").unwrap().changed);
        assert!(strip_shim_yaml("- 1\nb: 2\n", "c.yml").is_err());
    }

    /// An owner hook appended after ours: reinstalling moves ours to the end,
    /// and the next reinstall finds nothing to do.
    #[test]
    fn a_reinstall_after_an_owner_hook_settles() {
        use crate::install::codewhale::merge_install;
        let cmd = "PIXTUOID_SOURCE=codewhale /opt/bin/pixtuoid-hook";
        let ours = merge_install("", cmd).unwrap().content;
        let owner =
            format!("{ours}\n[[hooks.hooks]]\nevent = \"Stop\"\ncommand = \"notify-send done\"\n");
        let moved = merge_install(&owner, cmd).unwrap();
        assert!(moved.changed, "{}", moved.content);
        let again = merge_install(&moved.content, cmd).unwrap();
        assert!(!again.changed, "{}", moved.content);
        assert_eq!(again.content, moved.content);
    }

    /// A hand-inlined hooks value is the same value: ours go in beside a foreign
    /// entry and come back out without it.
    #[test]
    fn an_inlined_config_merges_like_a_table_one() {
        use crate::install::{codewhale, kimi};
        let kimi_doc = "hooks = [{ event = \"Stop\", command = \"keep\" }]\n";
        let whale_doc =
            "hooks = { enabled = true, hooks = [{ event = \"stop\", command = \"keep\" }] }\n";
        for (doc, cmd, install, uninstall) in [
            (
                kimi_doc,
                "PIXTUOID_SOURCE=kimi /opt/bin/pixtuoid-hook",
                kimi::merge_install as fn(&str, &str) -> anyhow::Result<MergeOutcome>,
                kimi::merge_uninstall as fn(&str) -> anyhow::Result<MergeOutcome>,
            ),
            (
                whale_doc,
                "PIXTUOID_SOURCE=codewhale /opt/bin/pixtuoid-hook",
                codewhale::merge_install,
                codewhale::merge_uninstall,
            ),
        ] {
            let installed = install(doc, cmd).unwrap();
            assert!(installed.changed, "{}", installed.content);
            let removed = uninstall(&installed.content).unwrap();
            assert!(removed.changed, "{}", removed.content);
            assert_eq!(
                parse_toml(&removed.content).unwrap(),
                parse_toml(doc).unwrap(),
                "{}",
                removed.content
            );
        }
    }

    #[test]
    fn an_install_fills_an_empty_inline_array() {
        let cmd = "PIXTUOID_SOURCE=kimi /opt/bin/pixtuoid-hook";
        let out = crate::install::kimi::merge_install("hooks = []\n", cmd).unwrap();
        assert!(out.changed);
        assert!(
            !parse_toml(&out.content).unwrap()["hooks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    /// A hooks value of no shape we wrote is the owner's, whatever it is.
    #[test]
    fn an_uninstall_leaves_a_foreign_hooks_value_alone() {
        use crate::install::{codewhale, jcode, kimi};
        for uninstall in [
            codewhale::merge_uninstall,
            strip_shim_toml,
            jcode::merge_uninstall,
            kimi::merge_uninstall,
        ] {
            for doc in ["hooks = 1\n", "hooks = []\n", "hooks = ['a']\n"] {
                let out = uninstall(doc).unwrap();
                assert!(!out.changed, "{doc}");
                assert_eq!(out.content, doc);
            }
        }
    }

    #[test]
    fn a_yaml_file_holds_at_most_one_document() {
        for none in ["", "  \n", "# only a note\n"] {
            assert_eq!(single_yaml_doc(none, "c.yml").unwrap(), None, "{none:?}");
        }
        assert!(single_yaml_doc("a: 1\n", "c.yml").unwrap().is_some());
        for two in ["a: 1\n---\nb: 2\n", "- a\n---\n"] {
            let err = single_yaml_doc(two, "c.yml").unwrap_err().to_string();
            assert!(
                err.starts_with("c.yml") && err.contains("more than one YAML document"),
                "{two:?}: {err}"
            );
        }
    }

    #[test]
    fn prune_empty_drops_only_an_empty_container() {
        let mut parent = json!({
            "paths": ["/some/other/plugin"],
            "entries": {"anthropic": {}},
            "none": [],
            "nothing": {},
            "scalar": 0,
        })
        .as_object()
        .unwrap()
        .clone();
        for key in ["paths", "entries", "none", "nothing", "scalar", "absent"] {
            prune_empty(&mut parent, key);
        }
        assert_eq!(
            Value::Object(parent),
            json!({"paths": ["/some/other/plugin"], "entries": {"anthropic": {}}, "scalar": 0})
        );
    }

    #[test]
    fn a_crlf_file_stays_crlf() {
        let out = toml_merge_outcome("# note\r\na = 1\r\ns = '''x\r\ny'''\r\n", |d| {
            d["b"] = toml_edit::value(2);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            out.content,
            "# note\r\na = 1\r\ns = '''x\r\ny'''\r\nb = 2\r\n"
        );
    }

    #[test]
    fn a_json_rewrite_ends_with_a_newline_in_the_files_line_ending() {
        let add = |mut d: Map<String, Value>| {
            d.insert("b".into(), json!(2));
            Ok(d)
        };
        for (before, kept, added) in [
            (
                "{\r\n  \"a\": 1\r\n}\r\n",
                "{\r\n  \"a\": 1\r\n}\r\n",
                "{\r\n  \"a\": 1,\r\n  \"b\": 2\r\n}\r\n",
            ),
            (
                "{\"a\": 1}",
                "{\n  \"a\": 1\n}\n",
                "{\n  \"a\": 1,\n  \"b\": 2\n}\n",
            ),
        ] {
            let out = flat_json_merge_outcome_install(before, "c", add).unwrap();
            assert_eq!(out.content, added);
            let out = flat_json_merge_outcome_uninstall(before, |d| d).unwrap();
            assert_eq!(out.content, kept);
        }
    }

    /// A merge rewrites only what it changed: the owner's comments, on the keys,
    /// tables and hook groups it left equal, survive both directions.
    #[test]
    fn toml_merge_keeps_what_it_did_not_change() {
        let user = "\
# my model, do not touch
model = \"o3\" # inline note

[hooks]
# my own hook
[[hooks.Stop]]
command = \"notify-send done\" # theirs
";
        let out = toml_merge_outcome(user, |d| {
            let mut ours = toml_edit::Table::new();
            ours["command"] = toml_edit::value("ours");
            d["hooks"]["Stop"]
                .as_array_of_tables_mut()
                .unwrap()
                .push(ours);
            Ok(())
        })
        .unwrap();
        assert!(out.changed);
        for kept in [
            "# my model, do not touch",
            "# inline note",
            "# my own hook",
            "# theirs",
        ] {
            assert!(
                out.content.contains(kept),
                "{kept:?} dropped:\n{}",
                out.content
            );
        }
        assert!(out.content.contains("\"ours\""), "{}", out.content);
        let back = toml_merge_outcome(&out.content, |d| {
            d["hooks"]["Stop"]
                .as_array_of_tables_mut()
                .unwrap()
                .remove(1);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            back.content, user,
            "uninstall must restore the owner's bytes"
        );
    }

    #[test]
    fn flat_json_merge_outcome_install_refuses_a_valid_non_object_root() {
        for content in ["[1, 2, 3]", "\"hello\"", "42", "true"] {
            let err = flat_json_merge_outcome_install(content, "settings", Ok)
                .unwrap_err()
                .to_string();
            assert!(err.contains("not an object"), "{content}: {err}");
        }
        for ok in ["", "null", "{}"] {
            assert!(
                flat_json_merge_outcome_install(ok, "settings", Ok).is_ok(),
                "{ok}"
            );
        }
    }

    #[test]
    fn flat_json_merge_outcome_install_reads_a_null_root_as_an_empty_object() {
        let out = flat_json_merge_outcome_install("null", "settings", |mut d| {
            d.insert("k".into(), json!(1));
            Ok(d)
        })
        .unwrap();
        assert!(out.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&out.content).unwrap(),
            json!({"k": 1})
        );
    }

    #[test]
    fn flat_json_merge_outcome_uninstall_is_a_clean_noop_on_a_non_object_root() {
        let out = flat_json_merge_outcome_uninstall("[1, 2, 3]", |d| d).unwrap();
        assert!(!out.changed);
    }

    #[test]
    fn toml_merge_outcome_reports_semantic_change_not_byte_change() {
        let set = |key: &'static str, value: i64| {
            move |d: &mut toml_edit::DocumentMut| {
                d[key] = toml_edit::value(value);
                Ok(())
            }
        };
        let noop = toml_merge_outcome("a  =  0x1\n", set("a", 1)).unwrap();
        assert!(!noop.changed, "an equal value must be a semantic no-op");
        assert!(toml_merge_outcome("a = 1\n", set("b", 2)).unwrap().changed);
    }

    #[test]
    fn flat_json_merge_uninstall_returns_non_object_unchanged() {
        let arr = json!([1, 2, 3]);
        assert_eq!(flat_json_merge_uninstall(arr.clone(), "_pixtuoid"), arr);
        let scalar = json!(42);
        assert_eq!(
            flat_json_merge_uninstall(scalar.clone(), "_pixtuoid"),
            scalar
        );
    }

    #[test]
    fn hook_path_str_returns_utf8_path() {
        let p = std::path::Path::new("/opt/bin/pixtuoid-hook");
        assert_eq!(hook_path_str(p).unwrap(), "/opt/bin/pixtuoid-hook");
    }

    #[test]
    fn hook_path_str_rejects_non_utf8() {
        let bad = non_utf8_path();
        let err = hook_path_str(&bad).unwrap_err().to_string();
        assert!(err.contains("non-UTF-8"), "{err}");
    }

    #[cfg(unix)]
    fn non_utf8_path() -> std::path::PathBuf {
        use std::os::unix::ffi::OsStrExt;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/x/\xff\xfehook"))
    }

    #[cfg(windows)]
    fn non_utf8_path() -> std::path::PathBuf {
        use std::os::windows::ffi::OsStringExt;
        // An unpaired surrogate (0xD800) is valid UTF-16 to the OS but not UTF-8.
        std::ffi::OsString::from_wide(&[0x005C, 0x0078, 0xD800, 0x0068]).into()
    }
}
