//! DeepSeek Harness install target: one zero-import plugin file plus one
//! mount row in the HOME-level patch file, which applies to every profile
//! (`$DSH_HOME/cordis.patch.yml` is the last user layer before `--patch`
//! overlays). The plugin is a single `.mjs` deliberately free of
//! `@deepseek-ai` imports, so mounting it by ABSOLUTE path needs no package
//! install, no pnpm, and cannot summon a second cordis instance — probed live
//! against a stock npm dsh (0.1.1-rc.2): the loader `import()`s a bare
//! absolute path and the `web` profile even hot-reloads the patch file.
//! ACCEPTED residual: uninstall removes the mount ROW only — the plugin file
//! stays under `$DSH_HOME/pixtuoid/` (`write_config_atomic` cannot delete),
//! and unmounted it is inert bytes.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use yaml_edit::{Document, Mapping, Sequence, YamlFile, YamlNode};

use crate::install::target::MergeOutcome;
use crate::install::verify::{SchemaParse, ShimRef, reconnect};
use crate::install::{io, merge};

/// The mount entry's `id` — uninstall and the one-entry verify key on it.
const PLUGIN_ID: &str = "pixtuoid";

pub(crate) const PLUGIN_TEMPLATE: &str = include_str!("dsh_plugin.mjs");

/// The upstream events the plugin forwards, read off its agent subscriptions and
/// its session-event switch's cases: the selftest can fire only the cases it is
/// told of, so a hand list would leave an added case unexercised.
#[cfg(test)]
pub(crate) fn plugin_events() -> Vec<&'static str> {
    let mut names = Vec::new();
    for marker in [r#"ctx.on(""#, r#"case ""#] {
        for (i, m) in PLUGIN_TEMPLATE.match_indices(marker) {
            if let Some((name, _)) = PLUGIN_TEMPLATE[i + m.len()..].split_once('"') {
                names.push(name);
            }
        }
    }
    // The channel the cases arrive on, not an event.
    names.retain(|name| *name != "session/event");
    names.sort_unstable();
    names.dedup();
    names
}

/// `$DSH_HOME` else `~/.dsh` — upstream `resolveDshHome`
/// (`packages/util/home-paths/src/index.ts:101-110,135-137` @ dsh-v0.2.2-rc.2)
/// resolves configured-path > `$DSH_HOME` > `~/.dsh` with no XDG arm, treats a
/// blank `$DSH_HOME` as unset, and expands a leading `~` (`expandHomePath`,
/// `:87-91`). No configured path reaches the mount, so the env axis is the whole
/// surface here.
fn dsh_home() -> Result<PathBuf> {
    let home = pixtuoid_core::platform::user_home_opt();
    if let Some(p) = pixtuoid_core::platform::path_env("DSH_HOME") {
        return Ok(io::expand_tilde(&p, home.as_deref()));
    }
    home.map(|h| h.join(".dsh"))
        .context("no home directory resolves — cannot locate ~/.dsh")
}

/// The mergeable config: the home-level patch file every profile layers in.
pub(crate) fn default_config_path() -> Result<PathBuf> {
    Ok(dsh_home()?.join("cordis.patch.yml"))
}

/// Where the plugin file itself lives (the patch row's `name` points here).
pub(crate) fn plugin_path() -> Result<PathBuf> {
    Ok(dsh_home()?.join(PLUGIN_ID).join("pixtuoid-dsh.mjs"))
}

/// Presence probe: dsh's own home, never our artifacts (chicken-and-egg). A
/// relative one names no dir pixtuoid can find; the install's refusal covers
/// writes only.
pub(crate) fn detect_installed() -> bool {
    dsh_home().is_ok_and(|h| h.is_absolute() && h.exists())
}

/// dsh runs plugins under Node and the plugin spawns the shim by embedded
/// path — always absolute, `_explicit` is irrelevant.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    merge::hook_path_str(resolved).map(str::to_string)
}

pub(crate) fn plugin_artifacts(hook_path: &str) -> Result<Vec<(PathBuf, String)>> {
    let rendered = merge::bake_hook_path(PLUGIN_TEMPLATE, hook_path, "dsh")?;
    Ok(vec![(plugin_path()?, rendered)])
}

fn patch_file(content: &str) -> Result<YamlFile> {
    merge::yaml_file(content, "cordis.patch.yml")
}

/// The file's top-level patch list, `None` when it holds no document. A
/// non-list top level is a config we must not touch.
fn rows(file: &YamlFile) -> Result<Option<Sequence>> {
    let Some(doc) = file.documents().next() else {
        return Ok(None);
    };
    match doc.as_sequence() {
        Some(rows) => Ok(Some(rows)),
        None => bail!("cordis.patch.yml's top level is not a patch list"),
    }
}

/// [`patch_file`] and [`rows`] for a rewrite, which a malformed `insert:` refuses too.
fn rows_to_rewrite(content: &str) -> Result<(YamlFile, Option<Sequence>)> {
    let file = patch_file(content).context("refusing to overwrite cordis.patch.yml")?;
    let rows = rows(&file).context("refusing to overwrite cordis.patch.yml")?;
    if rows.as_ref().is_some_and(malformed_insert) {
        bail!(
            "cordis.patch.yml has an `insert:` that is not a list — dsh's patch \
             apply spreads it into an array push, so neither dsh nor pixtuoid \
             can read the entries there; fix the row by hand first"
        );
    }
    Ok((file, rows))
}

/// Our whole patch row, `- insert: [{id: pixtuoid, name: <plugin path>}]`, as a
/// list of its own; yaml-edit quotes the path as YAML needs.
fn mount_list(plugin: &str) -> Result<Document> {
    let list = Document::from_str(&format!(
        "- insert:\n    - id: {PLUGIN_ID}\n      name: ''\n"
    ))
    .map_err(|e| anyhow::anyhow!("rendering the mount row: {e}"))?;
    let entry = list
        .as_sequence()
        .and_then(|rows| rows.get(0))
        .and_then(|row| mapping(&row)?.get("insert"))
        .and_then(|insert| match insert {
            YamlNode::Sequence(entries) => entries.get(0),
            _ => None,
        })
        .and_then(|entry| mapping(&entry));
    entry
        .context("the mount row template holds no entry")?
        .set("name", plugin);
    Ok(list)
}

fn mapping(node: &YamlNode) -> Option<Mapping> {
    node.as_mapping().cloned()
}

fn insert_of(row: &YamlNode) -> Option<YamlNode> {
    mapping(row)?.get("insert")
}

/// A row whose `insert:` is a truthy non-list: dsh's own patch apply
/// spreads the value into an array push
/// (`@deepseek-ai/cordis-plugin-include` 1.0.7, vendored in dsh 0.1.1-rc.2
/// — `vendor/include/src/index.ts:94`, read 2026-09-01), so it throws at
/// boot — and a mapping could HOLD an entry `our_entries`/
/// `strip_our_entries` would silently not-see. Refuse loud instead. A
/// JS-FALSY scalar (`null`/`false`/`0`/`""`) passes: upstream's truthiness
/// gate skips it before the spread, an override row (`- id: x` +
/// `enabled: false`) carries a null one legitimately, and no scalar can
/// hide an entry.
fn malformed_insert(rows: &Sequence) -> bool {
    fn falsy(v: &YamlNode) -> bool {
        matches!(v, YamlNode::Scalar(s)
            if s.is_null()
                || s.as_bool() == Some(false)
                || s.as_i64() == Some(0)
                || (s.is_quoted() && s.as_string().is_empty()))
    }
    rows.values()
        .filter_map(|row| insert_of(&row))
        .any(|insert| !matches!(insert, YamlNode::Sequence(_)) && !falsy(&insert))
}

/// Is this patch-list entry OUR mount entry (`id: pixtuoid`)?
///
/// Ownership is an ENTRY, never a row: one `insert:` op may batch several
/// plugins, so a row-granular rewrite or removal would silently eat a
/// foreign entry a user hand-batched beside ours.
fn is_our_entry(entry: &YamlNode) -> bool {
    mapping(entry).is_some_and(
        |m| matches!(m.get("id"), Some(YamlNode::Scalar(id)) if id.as_string() == PLUGIN_ID),
    )
}

/// The plugin path an entry names, `None` when it names none.
fn name_of(entry: &Mapping) -> Option<String> {
    match entry.get("name") {
        Some(YamlNode::Scalar(name)) if !name.is_null() => Some(name.as_string()),
        _ => None,
    }
}

/// Every `id: pixtuoid` entry across all `insert:` rows, in document order.
fn our_entries(rows: &Sequence) -> Vec<Mapping> {
    rows.values()
        .filter_map(|row| match insert_of(&row) {
            Some(YamlNode::Sequence(entries)) => Some(entries),
            _ => None,
        })
        .flat_map(|entries| entries.values().collect::<Vec<_>>())
        .filter(is_our_entry)
        .filter_map(|entry| mapping(&entry))
        .collect()
}

/// Whether `row` is our mount row alone, at `plugin`.
fn is_mount_row(row: &YamlNode, plugin: &str) -> bool {
    let Some(row) = mapping(row).filter(|m| m.len() == 1) else {
        return false;
    };
    let Some(YamlNode::Sequence(entries)) = row.get("insert") else {
        return false;
    };
    entries.len() == 1
        && entries.get(0).is_some_and(|entry| {
            is_our_entry(&entry)
                && mapping(&entry)
                    .is_some_and(|m| m.len() == 2 && name_of(&m).as_deref() == Some(plugin))
        })
}

/// Strip OUR entries from every `insert:` row in place; a foreign entry sharing
/// a row stays in it, and a row goes only when ours were all it held. How many
/// entries it removed.
fn strip_our_entries(rows: &Sequence) -> usize {
    let mut removed = 0;
    for r in (0..rows.len()).rev() {
        let Some(row) = rows.get(r).as_ref().and_then(mapping) else {
            continue;
        };
        let Some(YamlNode::Sequence(entries)) = row.get("insert") else {
            continue;
        };
        let held = entries.len();
        for i in (0..held).rev() {
            if entries.get(i).is_some_and(|e| is_our_entry(&e)) {
                entries.remove(i);
            }
        }
        removed += held - entries.len();
        if entries.is_empty() && held > 0 && row.len() == 1 {
            rows.remove(r);
        }
    }
    removed
}

/// `changed` is a semantic diff: exactly one of our entries, already a row
/// of its own at the wanted path → no rewrite. Anything else — a stale
/// path, a batched entry, duplicates — strips every one of our entries and
/// mounts one fresh standalone row; a non-list `insert:` is refused first
/// ([`malformed_insert`]).
pub(crate) fn merge_install(content: &str, _hook_cmd: &str) -> Result<MergeOutcome> {
    let plugin = plugin_path()?;
    let plugin = plugin.to_str().context("plugin path is not UTF-8")?;
    let (file, rows) = rows_to_rewrite(content)?;
    let mount = mount_list(plugin)?;
    let Some(rows) = rows else {
        // No list to edit in place: the file is empty or comments only.
        let lead = if content.is_empty() || content.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return Ok(MergeOutcome {
            content: merge::end_lines_like(content, format!("{content}{lead}{mount}")),
            changed: true,
        });
    };
    if our_entries(&rows).len() == 1 && rows.values().any(|row| is_mount_row(&row, plugin)) {
        return Ok(MergeOutcome {
            content: content.to_string(),
            changed: false,
        });
    }
    strip_our_entries(&rows);
    let row = mount
        .as_sequence()
        .and_then(|list| list.get(0))
        .context("the mount row template holds no row")?;
    rows.push(row);
    Ok(MergeOutcome {
        content: file.to_string(),
        changed: true,
    })
}

/// Remove only our ENTRIES; every foreign patch op — one batched into the
/// same `insert:` row included — survives byte for byte.
pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    let (file, rows) = rows_to_rewrite(content)?;
    if !rows
        .as_ref()
        .is_some_and(|rows| strip_our_entries(rows) > 0)
    {
        return Ok(MergeOutcome {
            content: content.to_string(),
            changed: false,
        });
    }
    Ok(MergeOutcome {
        content: file.to_string(),
        changed: true,
    })
}

/// The patch file carries the MOUNT, not the shim (that is baked inside the
/// plugin file) — so `shim` stays `Unknown` here. The schema's own, stat-free half
/// is the row-vs-home path match below.
pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    let rows = match patch_file(content).and_then(|file| rows(&file)) {
        Ok(Some(rows)) => rows,
        Ok(None) => Sequence::new(),
        Err(e) => return SchemaParse::broken(e.to_string()),
    };
    let ours = our_entries(&rows);
    let mut parse = SchemaParse {
        shim: ShimRef::Unknown,
        ..Default::default()
    };
    // The one early return in an accumulate-into-issues fn: ours.len() is
    // not trustworthy here — a mapping-shaped row can HOLD our entry, so
    // falling through would report "no mount entry" about an entry that is
    // in the file.
    if malformed_insert(&rows) {
        parse.issues.push(
            "an `insert:` row is not a list — dsh cannot apply it and pixtuoid \
             cannot read it; fix cordis.patch.yml by hand"
                .to_string(),
        );
        return parse;
    }
    match ours.len() {
        0 => parse
            .issues
            .push("no pixtuoid mount entry — dsh never loads the plugin".to_string()),
        1 => {
            match name_of(&ours[0]) {
                // PURE over the content (`target.rs` schema contract): file
                // existence is `verify_beside`'s stat and staleness
                // `owned_state`'s, never checked here. The HOME match is
                // this check's own: `verify_beside` stats only the path
                // derived from the current `$DSH_HOME`, so a row carried
                // over from a relocated home would verify green while dsh
                // imports a path that is gone.
                Some(n) if Path::new(&n).is_absolute() => {
                    if let Ok(expected) = plugin_path()
                        && Path::new(&n) != expected
                    {
                        parse.issues.push(format!(
                            "the mount entry points at {}, not this home's \
                                 plugin ({}) — {} to re-mount",
                            crate::display_path(Path::new(&n)),
                            crate::display_path(&expected),
                            reconnect(pixtuoid_core::source::dsh::SOURCE_NAME)
                        ));
                    }
                }
                Some(n) => parse.issues.push(format!(
                    "the mount entry's plugin path {} is not absolute — dsh's loader only \
                     imports bare absolute paths",
                    crate::display_path(Path::new(&n))
                )),
                None => parse
                    .issues
                    .push("the pixtuoid mount entry carries no plugin path".to_string()),
            }
        }
        n => parse.issues.push(format!(
            "{n} pixtuoid mount entries — duplicates are ours; {} to collapse them",
            reconnect(pixtuoid_core::source::dsh::SOURCE_NAME)
        )),
    }
    parse
}

#[cfg(test)]
mod tests {
    use super::*;

    /// dsh expands a leading `~` (`expandHomePath`), and a relative home names no
    /// dir the probe can find.
    #[test]
    fn dsh_home_expands_its_tilde_and_a_relative_one_is_no_install() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("DSH_HOME", Some(std::path::Path::new("~/dsh"))),
                ("HOME", Some(home.path())),
                ("USERPROFILE", Some(home.path())),
            ],
            || {
                assert_eq!(
                    default_config_path().unwrap(),
                    home.path().join("dsh").join("cordis.patch.yml")
                );
            },
        );
        // `.` exists wherever the probe runs, so only the refusal keeps it out.
        temp_env::with_var("DSH_HOME", Some("."), || assert!(!detect_installed()));
    }

    #[test]
    fn the_presence_probe_reads_dsh_home() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("dsh");
        temp_env::with_vars(
            [
                ("DSH_HOME", Some(dir.as_path())),
                ("HOME", Some(home.path())),
                ("USERPROFILE", Some(home.path())),
            ],
            || {
                let probe = crate::install::target::DSH.presence_probe;
                assert!(!probe());
                std::fs::create_dir(&dir).unwrap();
                assert!(probe());
            },
        );
    }

    /// The template's leading marker — uninstall keys on the entry's `id`
    /// (not this), so the sentinel is a recognizer for humans and this test
    /// only.
    const SENTINEL: &str = "@pixtuoid-dsh-plugin";

    /// Every `type: "<wire name>"` the plugin sends, extracted from the template.
    fn plugin_wire_types() -> std::collections::BTreeSet<String> {
        let mut set = std::collections::BTreeSet::new();
        for part in PLUGIN_TEMPLATE.split("type: \"").skip(1) {
            set.insert(part.split('"').next().expect("closed quote").to_string());
        }
        set
    }

    #[test]
    fn the_plugin_sends_exactly_what_the_decoder_reads() {
        // The registered-events↔decoder-arms guard: a wire name only one side
        // knows is a silently dead event (sent-and-dropped, or read-and-never-sent).
        let decoder_reads: std::collections::BTreeSet<String> = [
            "session_start",
            "session_end",
            "turn_end",
            "tool_call",
            "tool_result",
            "approval_asked",
            "approval_decided",
            "model",
            "usage",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(plugin_wire_types(), decoder_reads);
    }

    #[test]
    fn every_plugin_payload_type_decodes_to_events() {
        use pixtuoid_core::source::dsh::decode_dsh_payload;
        let sid = "01b00000-0000-7000-8000-000000000001";
        for ty in plugin_wire_types() {
            let payload = serde_json::json!({
                "type": ty, "sessionId": sid, "cwd": "/r",
                "callId": "c1", "toolName": "bash",
                "outcome": "allowed-once", "model": "m",
                "inputTokens": 1, "outputTokens": 1,
            });
            let evs = decode_dsh_payload(payload.as_object().expect("an object")).expect("decodes");
            assert!(
                !evs.is_empty(),
                "{ty} decodes to nothing — a dead wire name"
            );
        }
    }

    #[test]
    fn the_plugin_subscribes_only_verified_channels_and_never_awaits() {
        // Allowlist, not denylist: upstream carries ~21 non-emit (waterfall/
        // serial/parallel) events and the set churns, so naming bad ones can
        // only sample the invariant. The enforceable form: every `ctx.on`
        // call site names a channel verified upstream — `@mode emit`, or
        // `agent/created`, `@mode serial` since dsh renamed `agent/session-start`
        // (deepseek-harness 9b7a8ccc), whose listener the no-await assert below
        // keeps synchronous (`runtime-types.ts` for the agent channels,
        // `session/src/index.ts` for `session/event`; fetched 2026-10-05).
        // A channel is a literal, or a const the template declares it by.
        let subscribed: std::collections::BTreeSet<&str> = PLUGIN_TEMPLATE
            .split(r#"ctx.on("#)
            .skip(1)
            .map(|p| match p.strip_prefix('"') {
                Some(literal) => literal.split('"').next().expect("closed quote"),
                None => {
                    let name = p.split(',').next().expect("an argument").trim();
                    PLUGIN_TEMPLATE
                        .split_once(&format!("const {name} = \""))
                        .and_then(|(_, rest)| rest.split('"').next())
                        .expect("a named channel's const")
                }
            })
            .collect();
        let allowed =
            std::collections::BTreeSet::from(["agent/created", "agent/disposed", "session/event"]);
        assert_eq!(subscribed, allowed);
        assert!(
            !PLUGIN_TEMPLATE.contains("await"),
            "an await inside a listener is a stall waiting to be awaited"
        );
        // The scanners key on the literal `ctx.on(` — a `ctx.on?.(` variant
        // would register a listener both they and the allowlist miss.
        assert_eq!(PLUGIN_TEMPLATE.matches("ctx.on").count(), 3);
    }

    #[test]
    fn the_plugin_guards_call_id_against_stringified_undefined() {
        // `String(undefined)` is the literal "undefined" on the wire, which
        // defeats the decoder's missing-field breadcrumb (JSON.stringify
        // drops undefined VALUES, but String() manufactures one).
        assert_eq!(
            PLUGIN_TEMPLATE
                .matches(r#"...(d.callId ? { callId: String(d.callId) } : {})"#)
                .count(),
            2,
            "tool/call and approval/asked both guard callId"
        );
    }

    /// The call id is read off the message, never off a content block.
    #[test]
    fn the_plugin_reads_a_tool_results_call_id_off_its_message() {
        assert!(
            PLUGIN_TEMPLATE.contains(
                r#"...(d.message?.toolCallId ? { callId: String(d.message.toolCallId) } : {})"#
            ),
            "tool/result lost its guarded read of the message's call id"
        );
        assert!(
            !PLUGIN_TEMPLATE.contains("content?.[0]"),
            "tool/result reads a content block, which declares no call id"
        );
    }

    #[test]
    fn the_plugin_forwards_parent_session_only_for_subagent_headers() {
        // `parentSession` alone is upstream's seed-lineage field — an ordinary
        // user branch (`ctx.sessions.fork()`) stamps it too; forwarded ungated,
        // a branched conversation renders as a delegation child.
        assert!(
            PLUGIN_TEMPLATE.contains(r#"header.origin === "subagent" && header.parentSession"#),
            "base() lost the subagent-discriminator gate on parentSession"
        );
    }

    /// A connect appends our row and a disconnect takes our entries out, every
    /// other byte as the user wrote it: dsh's `!!js` expressions, quoted strings
    /// its JSON schema would read as numbers, comments and line endings.
    #[test]
    fn a_connect_then_a_disconnect_touch_no_byte_but_ours() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let plugin = plugin_path().unwrap();
            let ours = format!(
                "- insert:\n    - id: pixtuoid\n      name: {}\n",
                plugin.display()
            );
            for ending in ["\n", "\r\n"] {
                let user = "# my patches\n- id: shell\n  config:\n    home: !!js process.env.HOME   # kept\n    mode: \"0b101\"\n    day: \"2026-01-02\"\n"
                    .replace('\n', ending);
                let merged = merge_install(&user, "/unused").unwrap();
                assert_eq!(
                    merged.content,
                    format!("{user}{}", ours.replace('\n', ending))
                );
                assert_eq!(merge_uninstall(&merged.content).unwrap().content, user);
            }
            let batched = "- insert:\n  - id: timer\n    name: t\n  - id: pixtuoid\n    name: /old/p.mjs\n# tail\n";
            assert_eq!(
                merge_uninstall(batched).unwrap().content,
                "- insert:\n  - id: timer\n    name: t\n# tail\n"
            );
            for (before, after) in [
                ("", ours.clone()),
                ("# none yet", format!("# none yet\n{ours}")),
            ] {
                assert_eq!(merge_install(before, "/unused").unwrap().content, after);
            }
            let shared = "- id: shell\n  insert:\n    - id: pixtuoid\n      name: /old/p.mjs\n";
            assert_eq!(
                merge_uninstall(shared).unwrap().content,
                "- id: shell\n  insert: []\n"
            );
        });
    }

    #[test]
    fn uninstalling_the_only_row_leaves_an_empty_list() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let merged = merge_install("", "/unused").unwrap();
            let removed = merge_uninstall(&merged.content).unwrap();
            assert!(removed.changed);
            assert_eq!(removed.content, "[]\n");
            assert!(
                rows(&patch_file(&removed.content).unwrap())
                    .unwrap()
                    .is_some_and(|r| r.is_empty())
            );
            let remounted = merge_install(&removed.content, "/unused").unwrap();
            assert!(remounted.changed);
            assert!(verify_schema(&remounted.content).issues.is_empty());
            assert!(
                !merge_install(&remounted.content, "/unused")
                    .unwrap()
                    .changed
            );
        });
    }

    #[test]
    fn install_mounts_once_idempotently_and_preserves_foreign_rows() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let foreign =
                "- insert:\n    - id: timer\n      name: '@deepseek-ai/cordis-plugin-timer'\n";
            let merged = merge_install(foreign, "/unused").unwrap();
            assert!(merged.changed);
            assert!(merged.content.contains("timer"), "foreign row preserved");
            assert!(merged.content.contains(PLUGIN_ID));
            assert!(merged.content.contains("pixtuoid-dsh.mjs"));

            let again = merge_install(&merged.content, "/unused").unwrap();
            assert!(!again.changed, "same path re-install is a no-op");

            let removed = merge_uninstall(&merged.content).unwrap();
            assert!(removed.changed);
            assert!(removed.content.contains("timer"));
            assert!(!removed.content.contains(PLUGIN_ID));
            assert!(!merge_uninstall(foreign).unwrap().changed);
        });
    }

    #[test]
    fn a_moved_home_re_mounts_the_new_plugin_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = temp_env::with_var("DSH_HOME", Some(a.path()), || {
            merge_install("", "/unused").unwrap()
        });
        temp_env::with_var("DSH_HOME", Some(b.path()), || {
            let second = merge_install(&first.content, "/unused").unwrap();
            assert!(second.changed, "a stale mount path must be rewritten");
            // Structural, not string: YAML may quote-escape a path, so a
            // lossy-string `contains` would pass only where it doesn't. A
            // re-merge under home B is a no-op exactly when the row already
            // carries B's plugin path.
            assert!(!merge_install(&second.content, "/unused").unwrap().changed);
        });
    }

    #[test]
    fn a_foreign_entry_batched_in_our_row_survives_connect_and_disconnect() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            // A user hand-batches a foreign plugin into the same `insert:` op as
            // an (old) pixtuoid mount — entry-granular ownership must not eat it.
            let batched = "- insert:\n    - id: timer\n      name: '@deepseek-ai/cordis-plugin-timer'\n    - id: pixtuoid\n      name: /old/pixtuoid-dsh.mjs\n";
            let merged = merge_install(batched, "/unused").unwrap();
            assert!(merged.changed);
            assert!(merged.content.contains("timer"), "{}", merged.content);
            assert!(verify_schema(&merged.content).issues.is_empty());
            let removed = merge_uninstall(&merged.content).unwrap();
            assert!(removed.changed);
            assert!(removed.content.contains("timer"), "{}", removed.content);
            assert!(!removed.content.contains(PLUGIN_ID));
        });
    }

    #[test]
    fn duplicate_our_entries_collapse_on_reconnect_as_the_doctor_line_says() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let one = merge_install("", "/unused").unwrap();
            let dup = format!("{}{}", one.content, one.content);
            assert!(
                verify_schema(&dup)
                    .issues
                    .iter()
                    .any(|i| i.contains("reconnect DeepSeek Harness to collapse"))
            );
            let collapsed = merge_install(&dup, "/unused").unwrap();
            assert!(collapsed.changed);
            assert!(verify_schema(&collapsed.content).issues.is_empty());
        });
    }

    #[test]
    fn merge_refuses_a_non_list_config_rather_than_rewriting_it() {
        assert!(merge_install("just: a mapping\n", "/unused").is_err());
        assert!(merge_uninstall("just: a mapping\n").is_err());
    }

    #[test]
    fn a_crlf_patch_file_stays_crlf() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let foreign = "- insert:\r\n    - id: timer\r\n      name: t\r\n";
            let merged = merge_install(foreign, "/unused").unwrap();
            let removed = merge_uninstall(&merged.content).unwrap();
            for out in [merged.content, removed.content] {
                assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
            }
        });
    }

    /// yaml-edit reads past what saphyr refuses, so only the strict parse keeps a
    /// connect off a list dsh's loader would reject.
    #[test]
    fn the_strict_parse_gates_what_yaml_edit_would_read_past() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            for bad in ["a: 1\n  b: 2\n", "- id: timer\nenabled: false\n"] {
                assert!(YamlFile::from_str(bad).is_ok(), "{bad:?}");
                assert!(merge::single_yaml_doc(bad, "c.yml").is_err(), "{bad:?}");
                let err = format!("{:#}", merge_install(bad, "/unused").unwrap_err());
                assert!(err.contains("does not parse as YAML"), "{bad:?}: {err}");
            }
        });
    }

    /// A merge refuses a patch file it can't edit, a second document's
    /// included; verify, which writes nothing, reports it broken without a refusal.
    #[test]
    fn a_patch_file_a_merge_refuses_reads_broken_to_verify() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let two = "- insert:\n    - id: pixtuoid\n      name: /old/pixtuoid-dsh.mjs\n---\n- id: timer\n  enabled: false\n";
            for (content, why) in [
                (two, "more than one YAML document"),
                ("just: a mapping\n", "not a patch list"),
            ] {
                for err in [
                    merge_install(content, "/unused").unwrap_err(),
                    merge_uninstall(content).unwrap_err(),
                ] {
                    let err = format!("{err:#}");
                    assert!(
                        err.contains("refusing to overwrite") && err.contains(why),
                        "{err}"
                    );
                }
                let v = verify_schema(content);
                assert!(
                    v.issues
                        .iter()
                        .any(|i| i.contains(why) && !i.contains("refusing")),
                    "{v:?}"
                );
            }
        });
    }

    #[test]
    fn a_mapping_shaped_insert_is_refused_loud_not_silently_unseen() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            // Message-anchored (the openclaw refusal-test standard): a different
            // error passing `is_err()` for the wrong reason must not count.
            let mapping = "- insert:\n    id: pixtuoid\n    name: /old/pixtuoid-dsh.mjs\n";
            let err = merge_install(mapping, "/unused").unwrap_err();
            assert!(err.to_string().contains("is not a list"), "{err}");
            let err = merge_uninstall(mapping).unwrap_err();
            assert!(err.to_string().contains("is not a list"), "{err}");
            let v = verify_schema(mapping);
            assert!(
                v.issues.iter().any(|i| i.contains("is not a list")),
                "{v:?}"
            );
            // A JS-falsy insert is upstream's own no-op (a legitimate override
            // row carries a null one) and no scalar can hide an entry — falsy
            // shapes pass through preserved, never refused.
            for over in [
                "- id: timer\n  insert:\n  enabled: false\n",
                "- insert: false\n",
                "- insert: 0\n",
                "- insert: ''\n",
            ] {
                let ok = merge_install(over, "/unused").unwrap();
                assert!(ok.changed, "our row still mounts beside {over:?}");
                assert!(verify_schema(&ok.content).issues.is_empty(), "{over:?}");
            }
            // A TRUTHY scalar still refuses: upstream spreads it (a number
            // throws at boot; a string spreads into per-char garbage entries).
            let err = merge_install("- insert: 42\n", "/unused").unwrap_err();
            assert!(err.to_string().contains("is not a list"), "{err}");
        });
    }

    #[test]
    fn verify_reports_the_missing_row_and_the_relative_path() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let none = verify_schema("");
            assert!(
                none.issues
                    .iter()
                    .any(|i| i.contains("no pixtuoid mount entry"))
            );

            // The plugin file does not exist on disk, yet the schema reads
            // clean: verify_schema is PURE over the content — a missing file is
            // `verify_beside`'s finding, pinned by the shared
            // missing-beside-file test in `install/tests.rs`.
            let merged = merge_install("", "/unused").unwrap();
            let ok = verify_schema(&merged.content);
            assert!(ok.issues.is_empty(), "{ok:?}");

            // A row carried over from a relocated home: the artifact loop stats
            // only the CURRENT home's derived path, so the schema owns this.
            let other = tempfile::tempdir().unwrap();
            temp_env::with_var("DSH_HOME", Some(other.path()), || {
                let stale = verify_schema(&merged.content);
                assert!(
                    stale
                        .issues
                        .iter()
                        .any(|i| i.contains("not this home's plugin")),
                    "{stale:?}"
                );
            });

            let relative = "- insert:\n    - id: pixtuoid\n      name: relative/plugin.mjs\n";
            let rel = verify_schema(relative);
            assert!(rel.issues.iter().any(|i| i.contains("not absolute")));
        });
    }

    /// The mount entry's `name` is a hand-editable YAML scalar and `$DSH_HOME`
    /// an env path; both reach `doctor` and the boot warning through these
    /// issues.
    #[test]
    fn a_hostile_mount_entry_reaches_the_issues_stripped() {
        let dir = tempfile::tempdir().unwrap();
        // Windows forbids the Cc half in a path component; U+202E it allows.
        let home = if cfg!(windows) {
            dir.path().join("home\u{202e}")
        } else {
            dir.path().join("home\u{1b}]0;pwned\u{7}\u{202e}")
        };
        // Absolute on each platform: Windows needs the drive prefix.
        let abs = if cfg!(windows) { "C:/abs" } else { "/abs" };
        let issues: Vec<String> = temp_env::with_var("DSH_HOME", Some(&home), || {
            [abs, "rel"]
                .iter()
                .map(|root| {
                    format!(
                        "- insert:\n    - id: pixtuoid\n      name: \"{root}\\e]0;pwned\\a\\u202E/p.mjs\"\n"
                    )
                })
                .flat_map(|content| verify_schema(&content).issues)
                .collect()
        });
        assert!(
            issues.iter().any(|i| i.contains("not this home's plugin"))
                && issues.iter().any(|i| i.contains("not absolute")),
            "both arms must fire: {issues:?}"
        );
        for issue in &issues {
            assert_eq!(&crate::strip_control_chars(issue), issue);
        }
    }

    #[test]
    fn plugin_artifacts_bake_the_hook_path_and_carry_the_sentinel() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let arts = plugin_artifacts("/opt/bin/pixtuoid-hook").unwrap();
            assert_eq!(arts.len(), 1);
            let (path, content) = &arts[0];
            assert!(path.starts_with(home.path()));
            assert!(content.contains(SENTINEL));
            assert!(content.contains("\"/opt/bin/pixtuoid-hook\""));
            assert!(!content.contains(crate::install::merge::HOOK_PLACEHOLDER));
        });
    }
}
