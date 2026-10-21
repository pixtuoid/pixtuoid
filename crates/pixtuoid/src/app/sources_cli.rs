//! The scriptable sources-CLI presenters over `crate::sources` (the TUI-free
//! core): `setup` / `sources [set]` / the shared `connect`/`disconnect` runner.
//! A SIBLING of `sources.rs`, kept out of it on purpose so the core stays
//! presenter-free. The `--json` row shape is the typed
//! [`crate::sources::OutcomeRow`] wire contract.

use std::io::Write;
use std::path::Path;

use crate::{config, sources};
use anyhow::Result;

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
    let mut any_failed = false;
    for (id, oc) in sources::connect_each(&cfg, &detected) {
        let oc = sources::ChangeOutcome::from(oc);
        if matches!(oc, sources::ChangeOutcome::Failed(_)) {
            any_failed = true;
        }
        let row = sources::OutcomeRow::new(id, &oc);
        writeln!(out, "{}", text_line(&row))?;
        if let Some(hint) = hint_line(&row) {
            writeln!(out, "{hint}")?;
        }
    }
    if any_failed {
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
    let rows: Vec<sources::OutcomeRow> = sources::reconcile_to(&cfg, &desired)
        .into_iter()
        .map(|(id, oc)| sources::OutcomeRow::new(id, &oc))
        .collect();
    report_batch(&rows, json)
}

/// Shared `connect`/`disconnect` presenter. `op` returns the SUCCESS outcome; an
/// `Err` becomes a `failed` row AND makes the whole command exit non-zero, after
/// emitting all rows.
pub(crate) fn run_change(
    ids: &[String],
    json: bool,
    op: impl Fn(&Path, &str) -> Result<sources::ChangeOutcome>,
) -> Result<()> {
    let cfg = config::config_path();
    let sids: Vec<&'static str> = ids
        .iter()
        .map(|id| sources::registered_id(id))
        .collect::<Result<_>>()?;
    let rows: Vec<sources::OutcomeRow> = sids
        .into_iter()
        .map(|sid| {
            let oc =
                op(&cfg, sid).unwrap_or_else(|e| sources::ChangeOutcome::Failed(format!("{e:#}")));
            sources::OutcomeRow::new(sid.to_string(), &oc)
        })
        .collect();
    report_batch(&rows, json)
}

/// Emit the batch, then fail the command if any row failed. Emits BEFORE bailing:
/// the `--json` rows must reach stdout even on the non-zero exit, and the failure
/// flag is DERIVED from the rows so the two can't disagree.
fn report_batch(rows: &[sources::OutcomeRow], json: bool) -> Result<()> {
    emit_outcomes(rows, json)?;
    if rows
        .iter()
        .any(|r| r.outcome == sources::WireOutcome::Failed)
    {
        // homebrew-core contract: the formula's `test do` expects the exit
        // `main` maps this error to (`ExitCode::FAILURE`).
        anyhow::bail!("one or more sources failed (see the rows above)");
    }
    Ok(())
}

/// Print an [`sources::OutcomeRow`] batch as a text table or the `--json` array —
/// the schema-backed envelope the Raycast extension parses back.
fn emit_outcomes(rows: &[sources::OutcomeRow], json: bool) -> Result<()> {
    let mut out = crate::cli_stdout();
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(rows)?)?;
    } else {
        for row in rows {
            writeln!(out, "{}", text_line(row))?;
            if let Some(hint) = hint_line(row) {
                writeln!(out, "{hint}")?;
            }
        }
    }
    Ok(())
}

/// The ONE human (non-`--json`) row form, shared by `emit_outcomes` and
/// `run_setup`: `id: token`, with the failure detail appended.
fn text_line(row: &sources::OutcomeRow) -> String {
    match &row.message {
        Some(m) => format!("{}: {}: {m}", row.id, row.outcome),
        None => format!("{}: {}", row.id, row.outcome),
    }
}

/// The optional SECOND human line under a row — the target's post-install step,
/// because connecting is not always the last one (OpenClaw's running gateway must
/// restart before it loads the plugin). HUMAN output only: in the `--json`
/// envelope `message` means FAILURE, so an advisory there would change its meaning.
fn hint_line(row: &sources::OutcomeRow) -> Option<String> {
    (row.outcome == sources::WireOutcome::Connected)
        .then(|| sources::post_install_hint(&row.id))
        .flatten()
        .map(|hint| format!("  \u{21b3} {hint}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_envelope_is_the_id_outcome_raycast_contract() {
        // Raycast is not the only consumer. homebrew-core contract: the
        // formula's `test do` parses `connect claude-code --json` with no agent CLI
        // and asserts the first row's `id` and `outcome` (`failed`). Reshaping this
        // envelope breaks Homebrew's CI on the next autobump.
        let rows = vec![
            sources::OutcomeRow::new("codewhale".to_string(), &sources::ChangeOutcome::Connected),
            sources::OutcomeRow::new(
                "cursor".to_string(),
                &sources::ChangeOutcome::Failed("boom".into()),
            ),
        ];
        assert_eq!(
            serde_json::to_string(&rows).unwrap(),
            r#"[{"id":"codewhale","outcome":"connected"},{"id":"cursor","outcome":"failed","message":"boom"}]"#
        );
    }

    #[test]
    fn the_post_install_hint_line_rides_a_connected_row_only() {
        let row =
            |id: &str, oc: sources::ChangeOutcome| sources::OutcomeRow::new(id.to_string(), &oc);

        let line = hint_line(&row("openclaw", sources::ChangeOutcome::Connected))
            .expect("openclaw declares a post-install step");
        assert!(
            line.starts_with("  \u{21b3} "),
            "the hint is an indented continuation of its row — got {line:?}"
        );
        assert_eq!(
            line.trim_start_matches([' ', '\u{21b3}']),
            sources::post_install_hint("openclaw").expect("declared"),
            "the line carries the target's own hint verbatim, never a re-worded copy"
        );

        // `no_op` especially: a re-run of `setup --yes` must not nag about a
        // gateway the user already restarted.
        for oc in [
            sources::ChangeOutcome::NoOp,
            sources::ChangeOutcome::Disconnected,
            sources::ChangeOutcome::Failed("boom".into()),
        ] {
            assert!(
                hint_line(&row("openclaw", oc.clone())).is_none(),
                "{oc:?} is not a completed install — no step to announce"
            );
        }

        assert!(
            hint_line(&row("claude-code", sources::ChangeOutcome::Connected)).is_none(),
            "claude-code's hooks take effect on its next run — nothing to add"
        );
    }
}
