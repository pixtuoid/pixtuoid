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

use anyhow::{Context, Result, bail};
use saphyr::{LoadableYamlNode, MappingOwned, ScalarOwned, Yaml, YamlEmitter, YamlOwned};

use crate::install::target::MergeOutcome;
use crate::install::verify::{SchemaParse, ShimRef};
#[cfg(feature = "upstream")]
use pixtuoid_core::source::dsh::REPO;
#[cfg(feature = "upstream")]
use pixtuoid_core::source::upstream::{Finding, Read, Watch, When};

/// The mount entry's `id` — uninstall and the one-entry verify key on it.
const PLUGIN_ID: &str = "pixtuoid";

const HOOK_PLACEHOLDER: &str = "\"{{HOOK_PATH_JSON}}\"";

pub(crate) const PLUGIN_TEMPLATE: &str = include_str!("dsh_plugin.mjs");

/// The upstream events the plugin forwards: its literal `ctx.on("…")`
/// subscriptions and the `case "…"` labels of its session-event switch,
/// scanned from the template so a hand-kept list can't claim coverage the code
/// doesn't have.
#[cfg(any(test, feature = "upstream"))]
pub(crate) fn plugin_events() -> Vec<&'static str> {
    let mut names = Vec::new();
    for marker in [r#"ctx.on(""#, r#"case ""#] {
        for (i, m) in PLUGIN_TEMPLATE.match_indices(marker) {
            if let Some((name, _)) = PLUGIN_TEMPLATE[i + m.len()..].split_once('"') {
                names.push(name);
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

/// What the plugin reads off a tool result's message.
#[cfg(feature = "upstream")]
pub(crate) const TOOL_RESULT_FIELDS: &[&str] = &["toolCallId"];

#[cfg(feature = "upstream")]
pub(crate) const UPSTREAM: &[Watch] = &[
    Watch {
        id: "dsh.plugin_events",
        ours: plugin_events,
        // dsh declares its events as keys of the cordis `Events` and the
        // session's `SessionEventMap`, each augmented across packages, so one
        // key may live in any of these.
        docs: &[
            REPO.doc(
                "packages/core/agent/src/runtime-types.ts",
                r"declare module '@deepseek-ai/cordis' \{\s*interface Events \{",
                "the cordis `Events` augmentation",
            ),
            REPO.doc(
                "packages/core/session/src/types.ts",
                r"export interface SessionEventMap\b",
                "`SessionEventMap`",
            ),
            REPO.doc(
                "packages/interaction/user-approval/src/types.ts",
                r"declare module '@deepseek-ai/dsh-session/types' \{\s*interface SessionEventMap \{",
                "the `SessionEventMap` augmentation",
            ),
            REPO.doc(
                "packages/core/session/src/index.ts",
                r"declare module '@deepseek-ai/cordis' \{",
                "the cordis augmentation",
            ),
        ],
        read: Read::TsMembers {
            of: &["Events", "SessionEventMap"],
        },
        when: When::Vanishes,
        finding: Finding::Breaking(
            "the plugin listens to an event dsh never fires, so that slice of the office silently \
         stops updating",
        ),
        siblings: None,
    },
    Watch {
        id: "dsh.tool_result_call_id",
        ours: || TOOL_RESULT_FIELDS.to_vec(),
        docs: &[REPO.doc(
            "packages/llm/llm/src/message.ts",
            r"(?m)^export interface ToolResultMessage\b",
            "`ToolResultMessage`",
        )],
        read: Read::TsFields {
            name: "ToolResultMessage",
        },
        when: When::Vanishes,
        finding: Finding::Breaking(
            "every tool result reaches the decoder without its call id: doctor reports it as \
             drift, and its end cannot be paired with its call",
        ),
        siblings: None,
    },
];

/// `$DSH_HOME` else `~/.dsh` — upstream `resolveDshHome`
/// (`packages/util/home-paths/src/index.ts`, fetched 2026-09-01) resolves
/// configured-path > `$DSH_HOME` > `~/.dsh` with no XDG arm and treats a
/// blank `$DSH_HOME` as unset; it also expands a `~/` value and resolves a
/// relative one against CWD (`resolve(expandHomePath(..))`) — we refuse both
/// instead of mirroring, because a cwd-dependent mount row would point
/// somewhere new each launch. No configured path reaches the mount, so the
/// env axis is the whole surface here.
fn dsh_home() -> Result<PathBuf> {
    if let Some(p) = pixtuoid_core::platform::path_env("DSH_HOME") {
        if !p.is_absolute() {
            bail!(
                "DSH_HOME={} is not absolute — dsh would expand `~` and \
                 resolve the rest against its own CWD, so a pixtuoid-written \
                 mount row would drift per launch; set an absolute path",
                p.display()
            );
        }
        return Ok(p);
    }
    pixtuoid_core::platform::user_home_opt()
        .map(|h| h.join(".dsh"))
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

/// Presence probe: dsh's own home, never our artifacts (chicken-and-egg).
pub(crate) fn detect_installed() -> bool {
    dsh_home().map(|h| h.exists()).unwrap_or(false)
}

/// dsh runs plugins under Node and the plugin spawns the shim by embedded
/// path — always absolute, `_explicit` is irrelevant.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    crate::install::merge::hook_path_str(resolved).map(str::to_string)
}

pub(crate) fn plugin_artifacts(hook_path: &str) -> Result<Vec<(PathBuf, String)>> {
    let rendered =
        crate::install::merge::bake_hook_path(PLUGIN_TEMPLATE, HOOK_PLACEHOLDER, hook_path, "dsh")?;
    Ok(vec![(plugin_path()?, rendered)])
}

fn ystr(s: &str) -> YamlOwned {
    YamlOwned::Value(ScalarOwned::String(s.to_string()))
}

/// Parse the patch file as its top-level array; empty content is the empty
/// document. A non-array top level is a config we must not touch.
fn parse_rows(content: &str) -> Result<Vec<YamlOwned>> {
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    let docs = YamlOwned::load_from_str(content)
        .map_err(|e| anyhow::anyhow!("cordis.patch.yml does not parse as YAML: {e}"))?;
    match docs.into_iter().next() {
        None => Ok(Vec::new()),
        Some(YamlOwned::Sequence(rows)) => Ok(rows),
        Some(_) => bail!(
            "cordis.patch.yml's top level is not a patch list — refusing to rewrite a \
             config pixtuoid does not understand"
        ),
    }
}

fn emit_rows(rows: &[YamlOwned]) -> Result<String> {
    // dsh reads an empty file as no array and refuses to boot; only a missing
    // one is "no patches" (`@deepseek-ai/dsh-app-boot` `parsePatchList`, dsh-v0.2.0-rc.2).
    if rows.is_empty() {
        return Ok("[]\n".into());
    }
    let doc = YamlOwned::Sequence(rows.to_vec());
    let borrowed = Yaml::from(&doc);
    let mut out = String::new();
    YamlEmitter::new(&mut out)
        .dump(&borrowed)
        .map_err(|e| anyhow::anyhow!("emitting cordis.patch.yml: {e}"))?;
    let mut out = out.strip_prefix("---\n").map(str::to_string).unwrap_or(out);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Our whole patch row: `- insert: [{id: pixtuoid, name: <plugin path>}]`.
fn mount_row(plugin: &str) -> YamlOwned {
    let mut entry = MappingOwned::new();
    entry.insert(ystr("id"), ystr(PLUGIN_ID));
    entry.insert(ystr("name"), ystr(plugin));
    let mut row = MappingOwned::new();
    row.insert(
        ystr("insert"),
        YamlOwned::Sequence(vec![YamlOwned::Mapping(entry)]),
    );
    YamlOwned::Mapping(row)
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
fn malformed_insert(rows: &[YamlOwned]) -> bool {
    fn falsy(v: &YamlOwned) -> bool {
        matches!(
            v,
            YamlOwned::Value(ScalarOwned::Null)
                | YamlOwned::Value(ScalarOwned::Boolean(false))
                | YamlOwned::Value(ScalarOwned::Integer(0))
        ) || matches!(v, YamlOwned::Value(ScalarOwned::String(s)) if s.is_empty())
    }
    rows.iter().any(|r| {
        matches!(r, YamlOwned::Mapping(m)
            if matches!(m.get(&ystr("insert")),
                Some(v) if !matches!(v, YamlOwned::Sequence(_)) && !falsy(v)))
    })
}

fn refuse_malformed(rows: &[YamlOwned]) -> Result<()> {
    if malformed_insert(rows) {
        bail!(
            "cordis.patch.yml has an `insert:` that is not a list — dsh's patch \
             apply spreads it into an array push, so neither dsh nor pixtuoid \
             can read the entries there; fix the row by hand first"
        );
    }
    Ok(())
}

/// Is this patch-list entry OUR mount entry (`id: pixtuoid`)?
///
/// Ownership is an ENTRY, never a row: one `insert:` op may batch several
/// plugins, so a row-granular rewrite or removal would silently eat a
/// foreign entry a user hand-batched beside ours.
fn is_our_entry(e: &YamlOwned) -> bool {
    matches!(e, YamlOwned::Mapping(em) if em.get(&ystr("id")) == Some(&ystr(PLUGIN_ID)))
}

/// Every `id: pixtuoid` entry across all `insert:` rows, in document order.
fn our_entries(rows: &[YamlOwned]) -> Vec<&MappingOwned> {
    rows.iter()
        .filter_map(|r| match r {
            YamlOwned::Mapping(m) => match m.get(&ystr("insert")) {
                Some(YamlOwned::Sequence(entries)) => Some(entries),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .filter_map(|e| match e {
            YamlOwned::Mapping(em) if em.get(&ystr("id")) == Some(&ystr(PLUGIN_ID)) => Some(em),
            _ => None,
        })
        .collect()
}

/// Strip OUR entries from every `insert:` row; a foreign entry sharing a row
/// stays in place, and a row vanishes only when ours were all it held.
/// Returns the surviving rows and how many entries were removed.
fn strip_our_entries(rows: &[YamlOwned]) -> (Vec<YamlOwned>, usize) {
    let mut removed = 0;
    let mut kept = Vec::new();
    for row in rows {
        let YamlOwned::Mapping(m) = row else {
            kept.push(row.clone());
            continue;
        };
        let Some(YamlOwned::Sequence(entries)) = m.get(&ystr("insert")) else {
            kept.push(row.clone());
            continue;
        };
        let filtered: Vec<YamlOwned> = entries
            .iter()
            .filter(|e| !is_our_entry(e))
            .cloned()
            .collect();
        removed += entries.len() - filtered.len();
        if filtered.len() == entries.len() {
            kept.push(row.clone());
        } else if !filtered.is_empty() || m.len() > 1 {
            let mut only_foreign = m.clone();
            only_foreign.insert(ystr("insert"), YamlOwned::Sequence(filtered));
            kept.push(YamlOwned::Mapping(only_foreign));
        }
    }
    (kept, removed)
}

/// `changed` is a semantic diff: exactly one of our entries, already a row
/// of its own at the wanted path → no rewrite. Anything else — a stale
/// path, a batched entry, duplicates — strips every one of our entries and
/// mounts one fresh standalone row; a non-list `insert:` is refused first
/// ([`malformed_insert`]).
pub(crate) fn merge_install(content: &str, _hook_cmd: &str) -> Result<MergeOutcome> {
    let plugin = plugin_path()?;
    let plugin = plugin.to_str().context("plugin path is not UTF-8")?;
    let rows = parse_rows(content)?;
    refuse_malformed(&rows)?;
    let wanted = mount_row(plugin);
    if our_entries(&rows).len() == 1 && rows.contains(&wanted) {
        return Ok(MergeOutcome {
            content: content.to_string(),
            changed: false,
        });
    }
    let (mut kept, _) = strip_our_entries(&rows);
    kept.push(wanted);
    Ok(MergeOutcome {
        content: emit_rows(&kept)?,
        changed: true,
    })
}

/// Remove only our ENTRIES; every foreign patch op — one batched into the
/// same `insert:` row included — survives as data (comments do not survive
/// saphyr — the hermes target's documented trade).
pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    let rows = parse_rows(content)?;
    refuse_malformed(&rows)?;
    let (kept, removed) = strip_our_entries(&rows);
    if removed == 0 {
        return Ok(MergeOutcome {
            content: content.to_string(),
            changed: false,
        });
    }
    Ok(MergeOutcome {
        content: emit_rows(&kept)?,
        changed: true,
    })
}

/// The patch file carries the MOUNT, not the shim (that is baked inside the
/// plugin file) — so `shim` stays `Unknown` here. The schema's own, stat-free half
/// is the row-vs-home path match below.
pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    let rows = match parse_rows(content) {
        Ok(rows) => rows,
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
            let name = ours[0].get(&ystr("name")).and_then(|n| match n {
                YamlOwned::Value(ScalarOwned::String(s)) => Some(s.clone()),
                _ => None,
            });
            match name {
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
                                 plugin ({}) — reconnect dsh to re-mount",
                            crate::display_path(Path::new(&n)),
                            crate::display_path(&expected)
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
            "{n} pixtuoid mount entries — duplicates are ours; reconnect dsh to collapse them"
        )),
    }
    parse
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let evs = decode_dsh_payload(&payload).expect("decodes");
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

    /// The call id is read off the message, whose `ToolResultMessage` field
    /// `dsh.tool_result_call_id` watches, never off a content block.
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

    #[test]
    fn uninstalling_the_only_row_leaves_an_empty_list() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("DSH_HOME", Some(home.path()), || {
            let merged = merge_install("", "/unused").unwrap();
            let removed = merge_uninstall(&merged.content).unwrap();
            assert!(removed.changed);
            assert_eq!(removed.content, "[]\n");
            assert!(parse_rows(&removed.content).unwrap().is_empty());
            assert!(
                !merge_install(&removed.content, "/unused")
                    .unwrap()
                    .content
                    .is_empty()
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
            // Structural, not string: saphyr quote-escapes a Windows path in the
            // emitted YAML, so a lossy-string `contains` passes only on Unix. A
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
                    .any(|i| i.contains("reconnect dsh to collapse"))
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
            assert!(!content.contains(HOOK_PLACEHOLDER));
        });
    }

    #[test]
    fn dsh_home_refuses_a_relative_override() {
        temp_env::with_var("DSH_HOME", Some("relative/home"), || {
            assert!(dsh_home().is_err());
        });
    }
}
