//! The scriptable sources-CLI presenters over `crate::sources` (the TUI-free
//! core): `setup` / `sources [set]` / the shared `connect`/`disconnect` runner.
//! A SIBLING of `sources.rs`, kept out of it on purpose so the core stays
//! presenter-free. The `--json` row shape is the typed
//! [`crate::sources::OutcomeRow`] wire contract.

use std::io::Write;
use std::path::Path;

use crate::{config, sources};
use anyhow::Result;
use pixtuoid_core::remote::Changed;

/// One source's change in a batch: what it did, or why it failed.
type Done = (&'static str, Result<Changed>);

/// `pixtuoid setup [--yes]` — the headless onboarding twin. Without `--yes` it is
/// a DRY RUN: writing to another tool's config is opt-in. Exits non-zero if any
/// connect fails, so a `$?`-checking caller gets a real signal.
pub(crate) fn run_setup(yes: bool) -> Result<()> {
    let mut out = crate::cli_stdout();
    let detected = sources::detect();
    if detected.is_empty() {
        writeln!(
            out,
            "No agent CLIs detected on this machine \u{2014} nothing to set up."
        )?;
        return Ok(());
    }
    if !yes {
        writeln!(
            out,
            "Detected agent CLIs (run `pixtuoid setup --yes` to connect):"
        )?;
        for sid in &detected {
            writeln!(out, "  {sid}")?;
        }
        return Ok(());
    }
    let cfg = config::config_path();
    if emit_outcomes(&sources::connect_each(&cfg, &detected), false)? {
        anyhow::bail!("one or more sources failed to connect (see the rows above)");
    }
    Ok(())
}

/// `pixtuoid sources [--json]` — print every source's connection state. Read-only.
pub(crate) fn run_sources_list(json: bool) -> Result<()> {
    let mut out = crate::cli_stdout();
    let cfg = config::config_path();
    // An unreadable log leaves `health` under-reported, so say so instead of
    // returning a silent clean bill — via tracing, never stdout, because `--json`
    // stdout is the frozen Raycast array.
    let (log, log_warning) = crate::run_log::LogLocation::from_env().read();
    if let Some(w) = log_warning {
        tracing::warn!("{w}");
    }
    let rows = sources::status(&cfg, &log);
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
    } else {
        for r in &rows {
            let (mark, state) = if r.connected {
                ('\u{25cf}', "connected") // ●
            } else if r.cli_present {
                ('\u{25cb}', "disconnected") // ○
            } else {
                ('\u{00b7}', "not installed") // ·
            };
            writeln!(out, "{mark} {:<16} {state}", r.id)?;
            if let Some(h) = &r.health {
                writeln!(out, "    {h}")?;
            }
        }
    }
    Ok(())
}

/// `pixtuoid sources set <ids>` — declarative reconcile (connected set = exactly these).
pub(crate) fn run_sources_set(ids: &[String], json: bool) -> Result<()> {
    let cfg = config::config_path();
    // Validate every id up front so a typo can't partially apply.
    let desired: std::collections::HashSet<String> = ids
        .iter()
        .map(|id| sources::registered_id(id).map(String::from))
        .collect::<Result<_>>()?;
    report_batch(&sources::reconcile_to(&cfg, &desired), json)
}

/// Shared `connect`/`disconnect` presenter. An `Err` from `op` becomes a
/// `failed` row AND makes the whole command exit non-zero, after emitting all
/// rows.
pub(crate) fn run_change(
    ids: &[String],
    json: bool,
    op: impl Fn(&Path, &str) -> Result<Changed>,
) -> Result<()> {
    let cfg = config::config_path();
    let sids: Vec<&'static str> = ids
        .iter()
        .map(|id| sources::registered_id(id))
        .collect::<Result<_>>()?;
    let rows: Vec<Done> = sids.into_iter().map(|sid| (sid, op(&cfg, sid))).collect();
    report_batch(&rows, json)
}

/// Emit the batch, then fail the command if any row failed. Emits BEFORE bailing:
/// the `--json` rows must reach stdout even on the non-zero exit.
fn report_batch(rows: &[Done], json: bool) -> Result<()> {
    if emit_outcomes(rows, json)? {
        anyhow::bail!("one or more sources failed (see the rows above)");
    }
    Ok(())
}

/// Print a batch as a text table or the `--json` array — the schema-backed
/// envelope the Raycast extension parses back — and say whether a row failed,
/// DERIVED from the rows printed so the two can't disagree.
fn emit_outcomes(rows: &[Done], json: bool) -> Result<bool> {
    let wire: Vec<sources::OutcomeRow> = rows
        .iter()
        .map(|(id, done)| sources::OutcomeRow::new(id, done))
        .collect();
    let mut out = crate::cli_stdout();
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&wire)?)?;
        let mut err = std::io::stderr();
        for note in json_notes(rows) {
            let _ = writeln!(err, "{note}");
        }
    } else {
        for line in text_lines(rows) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(wire
        .iter()
        .any(|r| r.outcome == sources::WireOutcome::Failed))
}

/// The ONE human (non-`--json`) form of a batch: each row as `id: token`, with
/// the failure detail appended, and under it what its change still asks of the
/// user or warns of (OpenClaw's running gateway must restart before it loads
/// the plugin).
fn text_lines(rows: &[Done]) -> Vec<String> {
    let mut lines = Vec::new();
    for (id, done) in rows {
        let row = sources::OutcomeRow::new(id, done);
        lines.push(match &row.message {
            Some(m) => format!("{}: {}: {m}", row.id, row.outcome),
            None => format!("{}: {}", row.id, row.outcome),
        });
        for note in done.iter().flat_map(Changed::notes) {
            lines.push(format!("  \u{21b3} {note}"));
        }
    }
    lines
}

/// A `--json` batch's notes, for stderr: in the envelope `message` means
/// FAILURE, so a note there would change its meaning.
fn json_notes(rows: &[Done]) -> Vec<String> {
    rows.iter()
        .flat_map(|(id, done)| {
            done.iter()
                .flat_map(Changed::notes)
                .map(move |note| format!("{id}: {note}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::remote::Outcome;

    #[test]
    fn outcome_envelope_is_the_id_outcome_raycast_contract() {
        let connected = Changed::new("codewhale", true, Outcome::Connected, false, None, None);
        let rows = vec![
            sources::OutcomeRow::new("codewhale", &Ok(connected)),
            sources::OutcomeRow::new("cursor", &Err(anyhow::anyhow!("boom"))),
        ];
        assert_eq!(
            serde_json::to_string(&rows).unwrap(),
            r#"[{"id":"codewhale","outcome":"connected"},{"id":"cursor","outcome":"failed","message":"boom"}]"#
        );
    }

    /// What a change still asks or warns of rides under its row: a connect
    /// isn't always the last step, and a hook off PATH or a plugin left
    /// registered never fires.
    #[test]
    fn a_changes_notes_ride_under_its_row() {
        let step = sources::post_install_hint("openclaw").expect("openclaw declares a step");
        let rows: Vec<Done> = vec![
            (
                "openclaw",
                Ok(Changed::new(
                    "openclaw",
                    true,
                    Outcome::Connected,
                    true,
                    Some(step),
                    None,
                )),
            ),
            (
                "codex",
                Ok(Changed::new(
                    "codex",
                    false,
                    Outcome::Disconnected,
                    false,
                    None,
                    Some("codex"),
                )),
            ),
            ("cursor", Err(anyhow::anyhow!("boom"))),
        ];
        assert_eq!(
            text_lines(&rows),
            [
                "openclaw: connected".to_owned(),
                "  \u{21b3} \u{26a0} pixtuoid-hook not on PATH".to_owned(),
                format!("  \u{21b3} {step}"),
                "codex: disconnected".to_owned(),
                "  \u{21b3} \u{26a0} plugin left registered (codex not on PATH)".to_owned(),
                "cursor: failed: boom".to_owned(),
            ]
        );
        assert_eq!(
            json_notes(&rows),
            [
                "openclaw: \u{26a0} pixtuoid-hook not on PATH".to_owned(),
                format!("openclaw: {step}"),
                "codex: \u{26a0} plugin left registered (codex not on PATH)".to_owned(),
            ]
        );
    }

    /// `no_op` especially: a re-run of `sources set` must not nag about a
    /// gateway the user already restarted.
    #[test]
    fn a_change_with_nothing_to_say_is_its_row_alone() {
        let rows: Vec<Done> = vec![
            (
                "openclaw",
                Ok(Changed::new(
                    "openclaw",
                    true,
                    Outcome::NoOp,
                    false,
                    None,
                    None,
                )),
            ),
            (
                "claude-code",
                Ok(Changed::new(
                    "claude-code",
                    true,
                    Outcome::Connected,
                    false,
                    None,
                    None,
                )),
            ),
        ];
        assert_eq!(
            text_lines(&rows),
            ["openclaw: no_op", "claude-code: connected"]
        );
        assert!(json_notes(&rows).is_empty());
    }
}
