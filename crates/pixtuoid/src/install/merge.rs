//! Install-WRITE shared helpers — the config parse/merge core every JSON/TOML
//! target's `merge_install`/`merge_uninstall` rides. These fns MUTATE;
//! `verify.rs` next door is the READ-only soundness detector. Per-target FORMAT
//! knowledge stays in each `install/<target>.rs` (invariant #3); only the
//! shape-shared machinery lives here.

use crate::install::target::MergeOutcome;
use serde_json::{Map, Value, json};

/// Parse JSON config content, treating empty/whitespace-only as the empty
/// document (`{}`) — the shared rule every JSON target's merge relies on.
pub(crate) fn parse_json_or_empty(content: &str) -> anyhow::Result<Value> {
    if content.trim().is_empty() {
        return Ok(json!({}));
    }
    use anyhow::Context;
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
    use anyhow::Context;
    let json = serde_json::to_string(hook_path)
        .with_context(|| format!("serializing the hook path into the {what} plugin"))?;
    Ok(template.replace(HOOK_PLACEHOLDER, &json))
}

/// A code artifact pixtuoid renders whole, which [`Self::sentinel`] marks as ours.
pub(crate) struct SentinelPlugin {
    pub(crate) sentinel: &'static str,
    /// What an uninstall leaves: loadable, inert, and no longer ours.
    pub(crate) removed_stub: &'static str,
    /// How an issue names it ("the omp extension"), and the CLI a reconnect names.
    pub(crate) what: &'static str,
    pub(crate) cli: &'static str,
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

pub(crate) fn parse_toml_or_empty(content: &str) -> anyhow::Result<toml::Table> {
    if content.trim().is_empty() {
        return Ok(toml::Table::new());
    }
    use anyhow::Context;
    toml::from_str(content).context("not valid TOML — refusing to overwrite")
}

/// Edit `parent[key]` as an object in place; an absent or wrongly-typed value
/// starts empty. Taken out and put back, so the key keeps its position.
fn with_json_object(
    parent: &mut Map<String, Value>,
    key: &str,
    edit: impl FnOnce(&mut Map<String, Value>),
) {
    let slot = parent.entry(key).or_insert(Value::Null);
    let mut obj = match slot.take() {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    edit(&mut obj);
    *slot = Value::Object(obj);
}

/// [`with_json_object`] for an array.
fn with_json_array(parent: &mut Map<String, Value>, key: &str, edit: impl FnOnce(&mut Vec<Value>)) {
    let slot = parent.entry(key).or_insert(Value::Null);
    let mut arr = match slot.take() {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    edit(&mut arr);
    *slot = Value::Array(arr);
}

/// [`with_json_object`] for a TOML table.
pub(crate) fn with_toml_table(
    parent: &mut toml::Table,
    key: &str,
    edit: impl FnOnce(&mut toml::Table),
) {
    let slot = parent
        .entry(key)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let mut table = match std::mem::replace(slot, toml::Value::Boolean(false)) {
        toml::Value::Table(t) => t,
        _ => toml::Table::new(),
    };
    edit(&mut table);
    *slot = toml::Value::Table(table);
}

/// [`with_json_object`] for a TOML array.
pub(crate) fn with_toml_array(
    parent: &mut toml::Table,
    key: &str,
    edit: impl FnOnce(&mut Vec<toml::Value>),
) {
    let slot = parent
        .entry(key)
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let mut arr = match std::mem::replace(slot, toml::Value::Boolean(false)) {
        toml::Value::Array(a) => a,
        _ => Vec::new(),
    };
    edit(&mut arr);
    *slot = toml::Value::Array(arr);
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
) -> Map<String, Value> {
    with_json_object(&mut root, "hooks", |hooks| {
        for ev in events {
            with_json_array(hooks, ev, |arr| {
                arr.retain(|entry| !is_flat_managed(entry, sentinel));
                arr.push(make_entry(hook_command));
            });
        }
    });
    root
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

/// Parse flat-JSON `content`, REFUSE a valid-but-non-object root (which would
/// otherwise be coerced to `{}`, dropping the user's document), run `mutate` on
/// its object (`null` reads as `{}`), and package a `MergeOutcome` whose `changed`
/// is a SEMANTIC parsed-doc diff — a byte diff would churn the user's formatting.
/// The guard lives HERE, once, so a future flat-JSON target cannot forget it.
pub(crate) fn flat_json_merge_outcome_install(
    content: &str,
    what: &str,
    mutate: impl FnOnce(Map<String, Value>) -> Map<String, Value>,
) -> anyhow::Result<MergeOutcome> {
    let doc = parse_json_or_empty(content)?;
    let root = match doc.clone() {
        Value::Object(m) => m,
        Value::Null => Map::new(),
        _ => anyhow::bail!("{what} is valid JSON but not an object — refusing to overwrite"),
    };
    let merged = Value::Object(mutate(root));
    let changed = merged != doc;
    Ok(MergeOutcome {
        content: serde_json::to_string_pretty(&merged)?,
        changed,
    })
}

/// [`flat_json_merge_outcome_uninstall`] of the [`crate::install::SENTINEL_KEY`]
/// entries: the strip every flat-JSON config pixtuoid merged into shares.
pub(crate) fn flat_json_uninstall(content: &str) -> anyhow::Result<MergeOutcome> {
    flat_json_merge_outcome_uninstall(content, |doc| {
        flat_json_merge_uninstall(doc, crate::install::SENTINEL_KEY)
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
        content: serde_json::to_string_pretty(&cleaned)?,
        changed,
    })
}

/// The TOML analog. No non-object guard — a TOML root is always a table.
///
/// `mutate` merges on the plain table; the result is written back onto the
/// user's own [`toml_edit::DocumentMut`], so every key, table and array-of-tables
/// entry the merge left equal keeps its comments and layout — the file is
/// another CLI's, and its owner's notes in it are not ours to drop.
pub(crate) fn toml_merge_outcome(
    content: &str,
    mutate: impl FnOnce(toml::Table) -> toml::Table,
) -> anyhow::Result<MergeOutcome> {
    let table = parse_toml_or_empty(content)?;
    let merged = toml::Value::Table(mutate(table.clone()));
    let doc = toml::Value::Table(table);
    let changed = merged != doc;
    let mut edit: toml_edit::DocumentMut = content.parse()?;
    apply_toml(edit.as_item_mut(), &doc, &merged)?;
    let mut out = edit.to_string();
    // toml_edit writes LF; a file that was all-CRLF stays so.
    if content.contains("\r\n") && !content.replace("\r\n", "").contains('\n') {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    Ok(MergeOutcome {
        content: out,
        changed,
    })
}

/// Rewrite `item` from `old` to `new`, touching only what differs: a table
/// recurses key by key, an array of tables reuses each entry an equal one
/// survives as, and anything else is replaced whole.
fn apply_toml(
    item: &mut toml_edit::Item,
    old: &toml::Value,
    new: &toml::Value,
) -> anyhow::Result<()> {
    if old == new {
        return Ok(());
    }
    match (old, new, item) {
        (toml::Value::Table(old), toml::Value::Table(new), toml_edit::Item::Table(table)) => {
            for key in old.keys().filter(|k| !new.contains_key(*k)) {
                table.remove(key);
            }
            for (key, value) in new {
                match (old.get(key), table.get_mut(key)) {
                    (Some(was), Some(slot)) => apply_toml(slot, was, value)?,
                    _ => {
                        table.insert(key, toml_item(value)?);
                    }
                }
            }
        }
        (toml::Value::Array(old), toml::Value::Array(new), toml_edit::Item::ArrayOfTables(aot))
            if !new.is_empty() && new.iter().all(toml::Value::is_table) =>
        {
            let mut kept = vec![false; old.len()];
            let mut last_reused = None;
            let mut new_seen = false;
            let mut in_order = true;
            let mut rebuilt = toml_edit::ArrayOfTables::new();
            for value in new {
                let reuse = (0..old.len()).find(|&i| !kept[i] && old[i] == *value);
                let table = match reuse.and_then(|i| aot.get(i).map(|t| (i, t.clone()))) {
                    Some((i, t)) => {
                        kept[i] = true;
                        in_order &= !new_seen && last_reused.is_none_or(|j| j < i);
                        last_reused = Some(i);
                        t
                    }
                    None => {
                        new_seen = true;
                        match toml_item(value)? {
                            toml_edit::Item::Table(t) => t,
                            _ => anyhow::bail!("a table entry did not render as a table"),
                        }
                    }
                };
                rebuilt.push(table);
            }
            // A reused entry keeps its place in the file, which wins over its
            // place in the array; reordered, or behind a new one, they take the
            // array's.
            if !in_order {
                rebuilt.iter_mut().for_each(clear_table_positions);
            }
            *aot = rebuilt;
        }
        (_, new, item) => *item = toml_item(new)?,
    }
    Ok(())
}

/// `value` as a fresh `toml_edit` item, positionless so it lays out after its
/// neighbours instead of claiming another document's place.
fn toml_item(value: &toml::Value) -> anyhow::Result<toml_edit::Item> {
    const KEY: &str = "v";
    let mut wrapper = toml::Table::new();
    wrapper.insert(KEY.into(), value.clone());
    let mut doc: toml_edit::DocumentMut = toml::to_string(&wrapper)?.parse()?;
    let mut item = doc
        .remove(KEY)
        .ok_or_else(|| anyhow::anyhow!("a rendered TOML value lost its key"))?;
    clear_positions(&mut item);
    Ok(item)
}

fn clear_positions(item: &mut toml_edit::Item) {
    match item {
        toml_edit::Item::Table(t) => clear_table_positions(t),
        toml_edit::Item::ArrayOfTables(aot) => aot.iter_mut().for_each(clear_table_positions),
        _ => {}
    }
}

fn clear_table_positions(table: &mut toml_edit::Table) {
    table.set_position(None);
    for (_, child) in table.iter_mut() {
        clear_positions(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_merge_writes_the_merged_order_and_keeps_empty_arrays() {
        let reorder = toml_merge_outcome("[[a]]\nx = 1\n\n[[a]]\nx = 2\n", |mut d: toml::Table| {
            d["a"].as_array_mut().unwrap().reverse();
            d
        })
        .unwrap();
        let xs: Vec<i64> = toml::from_str::<toml::Value>(&reorder.content).unwrap()["a"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["x"].as_integer().unwrap())
            .collect();
        assert_eq!(xs, [2, 1], "{}", reorder.content);
        let between = toml_merge_outcome("[[a]]\nx = 1\n\n[[a]]\nx = 2\n", |mut d: toml::Table| {
            let a = d["a"].as_array_mut().unwrap();
            a.insert(
                1,
                toml::Value::Table(toml::Table::from_iter([("x".into(), 9.into())])),
            );
            d
        })
        .unwrap();
        let xs: Vec<i64> = toml::from_str::<toml::Value>(&between.content).unwrap()["a"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["x"].as_integer().unwrap())
            .collect();
        assert_eq!(xs, [1, 9, 2], "{}", between.content);
        let emptied = toml_merge_outcome("[[a]]\nx = 1\n", |mut d: toml::Table| {
            d.insert("a".into(), toml::Value::Array(vec![]));
            d
        })
        .unwrap();
        assert_eq!(
            toml::from_str::<toml::Value>(&emptied.content).unwrap()["a"],
            toml::Value::Array(vec![])
        );
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

    #[test]
    fn a_crlf_file_stays_crlf() {
        let out = toml_merge_outcome("# note\r\na = 1\r\n", |mut d: toml::Table| {
            d.insert("b".into(), 2.into());
            d
        })
        .unwrap();
        assert_eq!(out.content, "# note\r\na = 1\r\nb = 2\r\n");
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
        let add = |mut d: toml::Table| {
            let stop = d["hooks"]["Stop"].as_array_mut().unwrap();
            stop.push(toml::Value::Table(toml::Table::from_iter([(
                "command".to_string(),
                toml::Value::from("ours"),
            )])));
            d
        };
        let out = toml_merge_outcome(user, add).unwrap();
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
        let back = toml_merge_outcome(&out.content, |mut d: toml::Table| {
            d["hooks"]["Stop"].as_array_mut().unwrap().pop();
            d
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
            let err = flat_json_merge_outcome_install(content, "settings", |d| d)
                .unwrap_err()
                .to_string();
            assert!(err.contains("not an object"), "{content}: {err}");
        }
        for ok in ["", "null", "{}"] {
            assert!(
                flat_json_merge_outcome_install(ok, "settings", |d| d).is_ok(),
                "{ok}"
            );
        }
    }

    #[test]
    fn flat_json_merge_outcome_install_reads_a_null_root_as_an_empty_object() {
        let out = flat_json_merge_outcome_install("null", "settings", |mut d| {
            d.insert("k".into(), json!(1));
            d
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
        let noop = toml_merge_outcome("a = 1\n", |d| d).unwrap();
        assert!(!noop.changed, "identity mutate must be a semantic no-op");
        let changed = toml_merge_outcome("a = 1\n", |mut d| {
            d.insert("b".into(), toml::Value::Integer(2));
            d
        })
        .unwrap();
        assert!(changed.changed);
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
