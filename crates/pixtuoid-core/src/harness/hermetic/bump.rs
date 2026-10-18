//! A pin bump as the nightly proposes it: which moved releases become bot PRs,
//! and what the tree a bump leg's patch leaves may hold before publish applies
//! it. The patch is untrusted, since the leg ran a just-published CLI: it may
//! carry data a decoder reads, never code a same-repo PR's CI would run. The
//! threat is a poisoned release, not one crafted against these checks (the
//! owner's call): a bypass that needs input built for this pipeline is out of
//! scope.

use std::path::{Component, Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{PROFILES, Profile, pin};
use crate::harness::sources_root;

/// The nightly's drift legs, as `hermetic.yml`'s matrix names them: a release
/// is proposed only once it moved on every one.
const OSES: &[&str] = &["macos", "windows"];

/// The file beside a run's reports naming each source that moved.
pub const MOVED: &str = "moved.json";

/// The `hermetic.yml` output a drift leg on `os` reports its [`MOVED`] rows as.
fn moved_output(os: &str) -> String {
    format!("moved_{os}")
}

/// The file a bump leg writes its patch for `source` to, and check reads it from.
pub fn patch_name(source: &str) -> String {
    format!("{source}.patch")
}

/// A source whose `latest` passed every scenario, every run reporting it, above
/// its pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    /// The registered source id.
    pub source: String,
    /// The release it moved to.
    pub version: String,
}

/// Every bump PR's title opens with it, which is how plan finds the releases
/// taken.
pub(crate) const TITLE_PREFIX: &str = "chore(hermetic): pin";

/// A bot PR the nightly opens, or one a patch was admitted for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// The registered source id.
    pub source: String,
    /// The release its pin moves to.
    pub version: String,
    /// One per source, so a newer release updates the open PR.
    pub(crate) branch: String,
    /// Also the key a declined release is remembered by.
    pub(crate) title: String,
}

impl Proposal {
    /// The proposal moving `source` to `version`.
    pub(crate) fn new(source: &str, version: &Version) -> Self {
        Self {
            source: source.to_string(),
            version: version.to_string(),
            branch: format!("bot/bump-{source}"),
            title: format!("{TITLE_PREFIX} {source} at {version}"),
        }
    }
}

/// The releases to propose, given the drift job's outputs: each profiled source
/// that moved to one release on every OS in `OSES`, above its pin, under a
/// title no PR has taken (an open one proposes it, a closed one declined it). A
/// leg that reported nothing moved nothing.
///
/// # Errors
///
/// When a leg's rows are not rows, or the one release every leg moved a source
/// to is not a release or is below its pin.
pub fn plan(drift: &Map<String, Value>, taken: &[String]) -> Result<Vec<Proposal>, String> {
    let mut moved = Vec::new();
    for os in OSES {
        let rows: Vec<Moved> = match drift.get(&moved_output(os)).and_then(Value::as_str) {
            None | Some("") => Vec::new(),
            Some(json) => serde_json::from_str(json).map_err(|e| format!("{os}: {e}"))?,
        };
        moved.push(rows);
    }
    let mut proposals = Vec::new();
    for p in PROFILES {
        let versions: Vec<Option<&str>> = moved
            .iter()
            .map(|rows| {
                rows.iter()
                    .find(|r| r.source == p.source)
                    .map(|r| r.version.as_str())
            })
            .collect();
        let Some(&Some(first)) = versions.first() else {
            continue;
        };
        if versions.iter().any(|v| *v != Some(first)) {
            continue;
        }
        let version = Version::parse(first).map_err(|e| format!("{}: {first:?}: {e}", p.source))?;
        if !pin::moves(p.descriptor(), &version)? {
            continue;
        }
        let proposal = Proposal::new(p.source, &version);
        if !taken.contains(&proposal.title) {
            proposals.push(proposal);
        }
    }
    Ok(proposals)
}

/// The repository root, which a patch's paths are relative to.
pub fn repo_root() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .ancestors()
        .nth(2)
        .unwrap_or(crate_dir)
        .to_path_buf()
}

fn relative(abs: &Path) -> PathBuf {
    abs.strip_prefix(repo_root()).unwrap_or(abs).to_path_buf()
}

fn registry_relative() -> PathBuf {
    relative(&pin::registry_path())
}

/// Whether a bump of `p` may change `path`, relative to the repository: the
/// registry, a file under one of its scenarios' fixtures, or one of their
/// conformance snapshots, named as insta derives it (`conformance.rs`).
pub fn admits(p: &Profile, path: &Path) -> bool {
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    let sources = relative(&sources_root());
    path == registry_relative()
        || p.scenarios.iter().any(|s| {
            let fixture = sources.join(s.fixture);
            let snapshot = fixture.file_name().map(|scenario| {
                sources.join("snapshots").join(format!(
                    "sources__conformance__{}__{}.snap",
                    p.source,
                    scenario.to_string_lossy()
                ))
            });
            path.starts_with(&fixture) || snapshot.is_some_and(|s| path == s)
        })
}

/// Whether the tree a patch left is a bump of `p` to `version` and nothing
/// else: every path git says `changed` [`admits`], and the registry `after`
/// exactly as [`pin::repinned`] writes `before`.
///
/// # Errors
///
/// What the tree holds that a bump doesn't.
pub fn judge(
    p: &Profile,
    version: &Version,
    before: &str,
    after: &str,
    changed: &[PathBuf],
) -> Result<(), String> {
    let Some(expected) = pin::repinned(before, p.descriptor(), version)? else {
        return Err(format!("{}: {version} is its pin already", p.source));
    };
    if after != expected {
        return Err(format!(
            "the registry is not {}'s pin moving to {version}, and nothing else",
            p.source
        ));
    }
    match changed.iter().find(|path| !admits(p, path)) {
        Some(stray) => Err(format!("{stray:?} is outside {}'s bump", p.source)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::hermetic::profile;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("a test version")
    }

    fn codex() -> &'static Profile {
        profile("codex").expect("codex's profile")
    }

    fn registry() -> String {
        std::fs::read_to_string(pin::registry_path()).expect("registry.rs")
    }

    fn above(p: &Profile) -> Version {
        let pin = pin::pinned(p.descriptor()).expect("a pin");
        Version::new(pin.major, pin.minor, pin.patch + 1)
    }

    /// The drift job's outputs, each leg's rows as `hermetic.yml` reports them.
    fn drift(legs: &[(&str, &[(&str, &str)])]) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("runs-stem".into(), Value::from("hermetic-runs-1-1-"));
        for (os, rows) in legs {
            let rows: Vec<Moved> = rows
                .iter()
                .map(|(source, version)| Moved {
                    source: (*source).to_string(),
                    version: (*version).to_string(),
                })
                .collect();
            let json = serde_json::to_string(&rows).expect("rows");
            out.insert(moved_output(os), Value::from(json));
        }
        out
    }

    #[test]
    fn a_release_moved_on_every_os_is_proposed_once() {
        let up = above(codex()).to_string();
        let both = drift(&[("macos", &[("codex", &up)]), ("windows", &[("codex", &up)])]);
        let proposals = plan(&both, &[]).expect("a plan");
        assert_eq!(proposals, [Proposal::new("codex", &v(&up))]);
        let noisy = drift(&[
            ("macos", &[("nope", &up), ("codex", &up), ("codex", &up)]),
            ("windows", &[("codex", &up)]),
        ]);
        assert_eq!(
            plan(&noisy, &[]).as_ref(),
            Ok(&proposals),
            "rows no profile reads"
        );
        let taken = [proposals[0].title.clone()];
        assert_eq!(plan(&both, &taken), Ok(vec![]), "proposed or declined");
    }

    #[test]
    fn a_release_moved_on_one_os_or_at_two_versions_is_not() {
        let up = above(codex());
        let next = Version::new(up.major, up.minor, up.patch + 1).to_string();
        let up = up.to_string();
        let mut silent = drift(&[("macos", &[("codex", &up)]), ("windows", &[])]);
        silent.insert(moved_output("windows"), Value::from(""));
        for legs in [
            drift(&[("macos", &[("codex", &up)]), ("windows", &[])]),
            drift(&[("macos", &[("codex", &up)])]),
            silent,
            drift(&[
                ("macos", &[("codex", &up)]),
                ("windows", &[("codex", &next)]),
            ]),
        ] {
            assert_eq!(plan(&legs, &[]), Ok(vec![]), "{legs:?}");
        }
        let pin = codex().descriptor().verified_version;
        let at_pin = drift(&[("macos", &[("codex", pin)]), ("windows", &[("codex", pin)])]);
        assert_eq!(plan(&at_pin, &[]), Ok(vec![]));
    }

    #[test]
    fn a_plan_refuses_rows_that_are_not_releases_above_the_pin() {
        let pin = pin::pinned(codex().descriptor()).expect("a pin");
        let below = Version {
            pre: semver::Prerelease::new("rc.1").expect("a prerelease"),
            ..pin
        }
        .to_string();
        let mut garbled = drift(&[("macos", &[]), ("windows", &[])]);
        garbled.insert(
            moved_output("macos"),
            Value::from("[{\"source\":\"codex\"}]"),
        );
        for legs in [
            garbled,
            drift(&[
                ("macos", &[("codex", "1.x")]),
                ("windows", &[("codex", "1.x")]),
            ]),
            drift(&[
                ("macos", &[("codex", &below)]),
                ("windows", &[("codex", &below)]),
            ]),
        ] {
            assert!(plan(&legs, &[]).is_err(), "{legs:?}");
        }
    }

    #[test]
    fn hermetic_yml_reports_moved_rows_from_every_os_plan_reads() {
        let yml = std::fs::read_to_string(repo_root().join(".github/workflows/hermetic.yml"))
            .expect("hermetic.yml");
        assert!(
            yml.contains(&format!("runs/{MOVED}")),
            "the file the tier writes"
        );
        assert_eq!(
            yml.matches("- { os: ").count(),
            OSES.len(),
            "one leg per OS"
        );
        for os in OSES {
            assert!(yml.contains(&format!("- {{ os: {os}, ")), "{os}'s leg");
            assert_eq!(
                yml.matches(&format!("{}:", moved_output(os))).count(),
                2,
                "{os}'s rows, as the job's output and the workflow's"
            );
        }
    }

    #[test]
    fn a_bump_admits_its_registry_fixtures_and_snapshots_alone() {
        let p = codex();
        let sources = relative(&sources_root());
        let fixture = sources.join(p.scenarios[0].fixture);
        let name = fixture.file_name().expect("a name").to_string_lossy();
        assert!(registry_relative().is_relative());
        assert_eq!(repo_root().join(registry_relative()), pin::registry_path());
        assert!(admits(p, &registry_relative()));
        assert!(admits(p, &fixture.join("provenance.json")));
        let snap = format!("sources__conformance__codex__{name}.snap");
        assert!(admits(p, &sources.join("snapshots").join(&snap)));
        let other = profile("claude-code").expect("claude-code's profile");
        assert!(!admits(
            p,
            &sources
                .join(other.scenarios[0].fixture)
                .join("provenance.json")
        ));
        let theirs = format!("sources__conformance__claude-code__{name}.snap");
        assert!(!admits(p, &sources.join("snapshots").join(theirs)));
        assert!(!admits(p, &fixture.join("..").join("..").join("build.rs")));
        assert!(!admits(p, Path::new("justfile")));
        assert!(!admits(
            p,
            &relative(&sources_root().join("conformance.rs"))
        ));
    }

    #[test]
    fn every_profile_admits_a_conformance_snapshot_insta_wrote() {
        let root = repo_root();
        let snapshots: Vec<PathBuf> = std::fs::read_dir(sources_root().join("snapshots"))
            .expect("the snapshots")
            .map(|e| relative(&e.expect("an entry").path()))
            .collect();
        for p in PROFILES {
            assert!(
                snapshots
                    .iter()
                    .any(|s| admits(p, s) && root.join(s).is_file()),
                "{}: no committed snapshot carries the name `admits` derives",
                p.source
            );
        }
    }

    fn nightly() -> String {
        std::fs::read_to_string(repo_root().join(".github/workflows/hermetic-nightly.yml"))
            .expect("hermetic-nightly.yml")
    }

    #[test]
    fn the_nightly_names_each_patch_where_check_reads_it() {
        let yml = nightly();
        for (at, count, sites) in [
            ("$SOURCE", 2, "the bump leg's write and publish's apply"),
            ("${{ matrix.source }}", 1, "the bump leg's upload"),
        ] {
            assert_eq!(yml.matches(&patch_name(at)).count(), count, "{sites}");
        }
    }

    #[test]
    fn the_nightly_finds_taken_releases_by_the_title_bumps_open_with() {
        assert!(nightly().contains(&format!("in:title \"{TITLE_PREFIX}\"")));
        assert!(
            Proposal::new("codex", &v("1.0.0"))
                .title
                .starts_with(TITLE_PREFIX)
        );
    }

    fn fixture_path(name: &str) -> PathBuf {
        relative(&sources_root().join(codex().scenarios[0].fixture).join(name))
    }

    #[test]
    fn a_bump_is_judged_by_the_tree_it_left() {
        let before = registry();
        let up = above(codex());
        let after = pin::repinned(&before, codex().descriptor(), &up)
            .expect("a repin")
            .expect("a move");
        let ok = [
            registry_relative(),
            fixture_path("hook-payloads.jsonl"),
            fixture_path("new.jsonl"),
        ];
        assert_eq!(judge(codex(), &up, &before, &after, &ok), Ok(()));
        let next = Version::new(up.major, up.minor, up.patch + 1);
        assert!(
            judge(codex(), &next, &before, &after, &ok).is_err(),
            "another release"
        );
        let pin = pin::pinned(codex().descriptor()).expect("a pin");
        assert!(
            judge(codex(), &pin, &before, &before, &ok).is_err(),
            "no move"
        );
        let injected = format!("{after}pub fn injected() {{}}\n");
        assert!(
            judge(codex(), &up, &before, &injected, &ok).is_err(),
            "the pin, and code"
        );
        let stray = [registry_relative(), PathBuf::from("justfile")];
        assert!(
            judge(codex(), &up, &before, &after, &stray).is_err(),
            "a path no bump owns"
        );
    }
}
