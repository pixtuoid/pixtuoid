//! The nightly's pin bumps, one subcommand per job; the rules are
//! `pixtuoid_core::harness::hermetic::bump`'s.
//!
//! - `plan <taken.json> <drift.json>`: the proposals, as JSON, given the titles
//!   `gh pr list --json title` names and the drift job's outputs.
//! - `patch <source> <out>`: the tree's bump of `source` as a patch at `out`;
//!   nothing is written when the tree is unchanged.
//! - `check <proposals.json> <patches>`: each proposal's patch in `patches`,
//!   applied to the clean tree and judged by what git says it changed, then
//!   wiped; prints what publish opens, as JSON.
//!
//! Exits 0 on success, 1 on a refusal (any patch refused refuses them all), and
//! 2 on a usage error.

use std::path::{Path, PathBuf};
use std::process::Command;

use pixtuoid_core::harness::hermetic::bump::{self, Proposal};
use pixtuoid_core::harness::hermetic::{self, Profile, pin};
use semver::Version;
use serde::{Deserialize, Serialize};

const USAGE: &str = "usage: bump plan <taken.json> <drift.json>\n       \
                     bump patch <source> <out>\n       \
                     bump check <proposals.json> <patches>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["plan", taken, drift] => plan(Path::new(taken), Path::new(drift)),
        ["patch", source, out] => patch(source, Path::new(out)),
        ["check", proposals, patches] => check(Path::new(proposals), Path::new(patches)),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = done {
        eprintln!("bump: {e}");
        std::process::exit(1);
    }
}

#[derive(Deserialize)]
struct Titled {
    title: String,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path:?}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path:?}: {e}"))
}

fn plan(taken: &Path, drift: &Path) -> Result<(), String> {
    let taken: Vec<String> = read_json::<Vec<Titled>>(taken)?
        .into_iter()
        .map(|t| t.title)
        .collect();
    let proposals = bump::plan(&read_json(drift)?, &taken)?;
    println!(
        "{}",
        serde_json::to_string(&proposals).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn profile(source: &str) -> Result<&'static Profile, String> {
    hermetic::profile(source).ok_or_else(|| format!("{source:?} has no profile"))
}

/// `git` at the repository root; its stdout.
fn git(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .current_dir(bump::repo_root())
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {args:?} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The paths in git's `-z` output.
fn paths(z: &[u8]) -> Vec<PathBuf> {
    z.split(|&b| b == 0)
        .filter(|n| !n.is_empty())
        .map(|n| PathBuf::from(String::from_utf8_lossy(n).into_owned()))
        .collect()
}

/// What the tree changed against HEAD, once each untracked file `marks` is in
/// the diff (intent-to-add, so an untracked file the bump doesn't admit stays out).
fn changes(marks: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>, String> {
    let untracked: Vec<PathBuf> =
        paths(&git(&["ls-files", "--others", "--exclude-standard", "-z"])?)
            .into_iter()
            .filter(|path| marks(path))
            .collect();
    if !untracked.is_empty() {
        let mut add = vec!["add", "--intent-to-add", "--"];
        add.extend(untracked.iter().filter_map(|p| p.to_str()));
        git(&add)?;
    }
    changed()
}

/// The paths the tree changed against HEAD.
fn changed() -> Result<Vec<PathBuf>, String> {
    Ok(paths(&git(&[
        "diff",
        "HEAD",
        "--name-only",
        "--no-renames",
        "-z",
    ])?))
}

fn patch(source: &str, out: &Path) -> Result<(), String> {
    let p = profile(source)?;
    // An untracked file the bump doesn't admit (CI's `npm ci` leaves
    // `node_modules`) stays out of the patch.
    let changed = changes(|path| bump::admits(p, path))?;
    if let Some(stray) = changed.iter().find(|path| !bump::admits(p, path)) {
        return Err(format!("{stray:?} changed, outside {source}'s bump"));
    }
    if changed.is_empty() {
        println!("{source}: the tree is unchanged, so there is no patch");
        return Ok(());
    }
    let diff = git(&[
        "diff",
        "HEAD",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ])?;
    std::fs::write(out, diff).map_err(|e| format!("{out:?}: {e}"))
}

/// What publish opens: the proposal, and the paths `add-paths` stages.
#[derive(Serialize)]
struct Admitted {
    #[serde(flatten)]
    proposal: Proposal,
    paths: Vec<String>,
}

fn check(proposals: &Path, patches: &Path) -> Result<(), String> {
    // Each patch is judged against HEAD and wiped after, so anything else in
    // the tree would be judged or lost.
    if !git(&["status", "--porcelain"])?.is_empty() {
        return Err("check needs a clean tree".into());
    }
    let registry = std::fs::read_to_string(pin::registry_path()).map_err(|e| e.to_string())?;
    let mut admitted = Vec::new();
    for proposal in read_json::<Vec<Proposal>>(proposals)? {
        let file = patches.join(bump::patch_name(&proposal.source));
        if !file.is_file() {
            eprintln!("{}: no patch, so its bump leg failed", proposal.source);
            continue;
        }
        let p = profile(&proposal.source)?;
        let version =
            Version::parse(&proposal.version).map_err(|e| format!("{proposal:?}: {e}"))?;
        let file = file
            .to_str()
            .ok_or_else(|| format!("{file:?} is not UTF-8"))?;
        git(&["apply", file])?;
        let judged = git(&["add", "--intent-to-add", "--all"])
            .and_then(|_| changed())
            .and_then(|changed| {
                let after =
                    std::fs::read_to_string(pin::registry_path()).map_err(|e| e.to_string())?;
                bump::judge(p, &version, &registry, &after, &changed)?;
                Ok(changed)
            });
        git(&["reset", "-q", "--hard", "HEAD"])?;
        git(&["clean", "-fdq"])?;
        admitted.push(Admitted {
            proposal,
            paths: judged?
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        });
    }
    println!(
        "{}",
        serde_json::to_string(&admitted).map_err(|e| e.to_string())?
    );
    Ok(())
}
