//! The nightly's hermetic issues; the rules are
//! `pixtuoid_core::harness::hermetic::issues`'.
//!
//! `plan <drift.json> <issues.json> <verdicts> <run-url>`: what to do with the
//! issues `gh issue list --json number,state,body` names, as JSON, given the
//! drift job's outputs and every leg's verdicts under the `verdicts` dir.
//!
//! Exits 0 on success, 1 when the drift outputs or the issues can't be read,
//! and 2 on a usage error.

use std::path::Path;

use pixtuoid_core::harness::hermetic::issues::{self, Issue, VERDICTS, Verdicts};
use pixtuoid_core::harness::{files_under, read_json};

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

fn plan(drift: &Path, issues: &Path, verdicts: &Path, run: &str) -> std::io::Result<()> {
    // One leg's download lands flat, several each in its artifact's dir
    // (download-artifact v8.0.2 `src/download-artifact.ts:188-194`).
    let reported: Vec<std::io::Result<Verdicts>> = files_under(verdicts)
        .into_iter()
        .filter(|file| file.file_name().is_some_and(|n| n == VERDICTS))
        .map(|file| read_json(&file))
        .collect();
    let issues: Vec<Issue> = read_json(issues)?;
    let plan = issues::plan(&read_json(drift)?, &reported, &issues, run);
    println!("{}", serde_json::to_string(&plan)?);
    Ok(())
}
