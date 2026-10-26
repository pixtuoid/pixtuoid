//! The nightly's hermetic issues; the rules are
//! `pixtuoid_core::harness::hermetic::issues`'.
//!
//! `plan <drift.json> <issues.json> <verdicts> <run-url>`: what to do with the
//! issues `gh issue list --json number,state,body` names, as JSON, given the
//! drift job's outputs and every leg's verdicts under the `verdicts` dir.
//!
//! Exits 0 on success, 1 when an input can't be read, and 2 on a usage error.

use std::path::Path;

use pixtuoid_core::harness::files_under;
use pixtuoid_core::harness::hermetic::issues::{self, Issue, VERDICTS, Verdicts};
use serde::Deserialize;

const USAGE: &str = "usage: issues plan <drift.json> <issues.json> <verdicts> <run-url>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["plan", drift, issues, verdicts, run] => plan(
            Path::new(drift),
            Path::new(issues),
            Path::new(verdicts),
            run,
        ),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = done {
        eprintln!("issues: {e}");
        std::process::exit(1);
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path:?}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path:?}: {e}"))
}

fn plan(drift: &Path, issues: &Path, verdicts: &Path, run: &str) -> Result<(), String> {
    // One leg's download lands flat, several each in its artifact's dir.
    let legs: Vec<Verdicts> = files_under(verdicts)
        .iter()
        .filter(|f| f.file_name().is_some_and(|n| n == VERDICTS))
        .map(|f| read_json(f))
        .collect::<Result<_, _>>()?;
    let issues: Vec<Issue> = read_json(issues)?;
    let plan = issues::plan(&read_json(drift)?, &legs, &issues, run)?;
    println!(
        "{}",
        serde_json::to_string(&plan).map_err(|e| e.to_string())?
    );
    Ok(())
}
