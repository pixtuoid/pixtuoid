//! Install-WRITE shared helpers — the config parse/merge core every JSON/TOML
//! target's `merge_install`/`merge_uninstall` rides. These fns MUTATE;
//! `verify.rs` next door is the READ-only soundness detector. Per-target FORMAT
//! knowledge stays in each `install/<target>.rs` (invariant #3); only the
//! shape-shared machinery lives here.

use crate::install::target::MergeOutcome;
use anyhow::Context;
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

/// The TOML analog: `mutate` edits the user's own [`toml_edit::DocumentMut`], so
/// what it leaves unopened keeps its bytes.
pub(crate) fn toml_merge_outcome(
    content: &str,
    mutate: impl FnOnce(&mut toml_edit::DocumentMut) -> anyhow::Result<()>,
) -> anyhow::Result<MergeOutcome> {
    let before = parse_toml(content)?;
    let mut doc: toml_edit::DocumentMut = content.parse()?;
    mutate(&mut doc)?;
    let mut out = doc.to_string();
    let changed = parse_toml(&out)? != before;
    // toml_edit writes LF; a file that was all-CRLF stays so.
    if content.contains("\r\n") && !content.replace("\r\n", "").contains('\n') {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    Ok(MergeOutcome {
        content: out,
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::install::{codewhale, codex, jcode, kimi};
        for uninstall in [
            codewhale::merge_uninstall,
            codex::merge_uninstall,
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
    fn a_crlf_file_stays_crlf() {
        let out = toml_merge_outcome("# note\r\na = 1\r\n", |d| {
            d["b"] = toml_edit::value(2);
            Ok(())
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
