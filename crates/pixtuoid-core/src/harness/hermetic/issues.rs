//! The nightly's hermetic issues: one per source a leg judged anything but
//! Pass, one per leg that reported no verdicts, and what each night does to
//! them. The workflow only carries a [`Plan`] out with `gh`. Every input is
//! untrusted, since the legs ran just-published CLIs: what one could forge is
//! a `hermetic`-labelled issue's text, never code.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::bump::moved_output;
use super::verify::Label;
use super::{OSES, SHARDS};

/// The file beside a leg's report holding its [`Verdicts`].
pub const VERDICTS: &str = "verdicts.json";

/// What one leg's runs came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdicts {
    /// Its OS, as [`std::env::consts::OS`] spells it.
    pub os: String,
    /// Its shard, counting from 1 as `--shard` does; none for a run of every
    /// shard.
    pub shard: Option<usize>,
    /// Each run, in the order it ran.
    pub runs: Vec<RunVerdict>,
}

/// One run's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunVerdict {
    /// The profiled source it ran.
    pub source: String,
    /// What the report heads it with: `<source>/<scenario>`, or the source's
    /// runs together.
    pub key: String,
    /// The CLI's `--version`, for one run.
    pub version: Option<String>,
    /// Its worst finding's.
    pub label: Label,
    /// Every finding, worst first.
    pub findings: Vec<(Label, String)>,
}

/// A `hermetic`-labelled issue, as `gh issue list --json number,state,body`
/// lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Issue {
    /// Its number.
    pub number: u64,
    /// Whether it is open.
    pub state: State,
    /// Its body, whose marker says which of the plan's it is.
    pub body: String,
}

/// An issue's state, as `gh` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum State {
    /// Open.
    Open,
    /// Closed.
    Closed,
}

/// What a night does to one issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "do", rename_all = "lowercase")]
pub enum Action {
    /// Open an issue.
    Open {
        /// Its title.
        title: String,
        /// Its body, marker first.
        body: String,
    },
    /// Comment on an open one.
    Comment {
        /// Which.
        number: u64,
        /// The comment.
        body: String,
    },
    /// Reopen a closed one, then comment on it.
    Reopen {
        /// Which.
        number: u64,
        /// The comment.
        body: String,
    },
    /// Comment on an open one, then close it.
    Close {
        /// Which.
        number: u64,
        /// The comment.
        body: String,
    },
}

/// A night's filing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Each issue's action, in order.
    pub actions: Vec<Action>,
    /// Each leg that reported no verdicts, and why, which turns the filing red.
    pub lost: Vec<String>,
}

/// One leg of `hermetic.yml`'s matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Leg {
    os: &'static str,
    shard: usize,
}

impl fmt::Display for Leg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, shard {}", self.os, self.shard)
    }
}

/// Every leg, [`OSES`] by [`SHARDS`].
fn matrix() -> impl Iterator<Item = Leg> {
    OSES.iter()
        .flat_map(|&os| (1..=SHARDS.len()).map(move |shard| Leg { os, shard }))
}

/// The `hermetic.yml` output the leg on `os` running [`SHARDS`]' `shard`th
/// names its uploaded runs' URL in.
pub(crate) fn artifact_output(os: &str, shard: usize) -> String {
    format!("artifact_{os}_{shard}")
}

/// What one of the plan's issues is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Source(String),
    Leg(Leg),
}

/// What opens the hidden line of a source's issue body, before the source.
const SOURCE_MARKER: &str = "<!-- hermetic-source: ";
/// What opens the hidden line of a leg's issue body, before `<os>-<shard>`.
const LEG_MARKER: &str = "<!-- hermetic-leg: ";
/// What closes either hidden line.
const MARKER_END: &str = " -->";

impl Key {
    /// The hidden line that names it in its issue's body: a title someone
    /// else writes can look the same.
    fn marker(&self) -> String {
        match self {
            Self::Source(source) => format!("{SOURCE_MARKER}{source}{MARKER_END}"),
            Self::Leg(leg) => format!("{LEG_MARKER}{}-{}{MARKER_END}", leg.os, leg.shard),
        }
    }

    /// The key `body`'s marker names, if it has one.
    fn of(body: &str) -> Option<Self> {
        body.lines().find_map(|line| {
            let line = line.trim();
            if let Some(source) = line
                .strip_prefix(SOURCE_MARKER)
                .and_then(|l| l.strip_suffix(MARKER_END))
            {
                return Some(Self::Source(source.to_owned()));
            }
            let (os, shard) = line
                .strip_prefix(LEG_MARKER)?
                .strip_suffix(MARKER_END)?
                .rsplit_once('-')?;
            let shard: usize = shard.parse().ok()?;
            matrix()
                .find(|leg| leg.os == os && leg.shard == shard)
                .map(Self::Leg)
        })
    }

    fn title(&self) -> String {
        match self {
            Self::Source(source) => format!("hermetic: {source}"),
            Self::Leg(leg) => format!("hermetic: {leg} reported no verdicts"),
        }
    }
}

/// One leg's night, as the drift job's outputs and its upload tell it.
struct Night<'a> {
    finished: bool,
    reported: Option<&'a Verdicts>,
    runs: Option<&'a str>,
}

/// What to do with the `hermetic` issues `issues`, given the drift job's
/// outputs, every leg's [`Verdicts`] and the run's URL. A source's issue
/// closes only on a night every leg reported and both legs running it judged
/// each of its runs Pass; a leg's, once it reported again.
///
/// # Errors
///
/// When verdicts come from a leg the matrix doesn't run, or twice from one.
pub fn plan(
    drift: &Map<String, Value>,
    verdicts: &[Verdicts],
    issues: &[Issue],
    run: &str,
) -> Result<Plan, String> {
    let output = |name: String| {
        drift
            .get(&name)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    let mut nights: BTreeMap<Leg, Night<'_>> = matrix()
        .map(|leg| {
            let night = Night {
                finished: output(moved_output(leg.os, leg.shard)).is_some(),
                reported: None,
                runs: output(artifact_output(leg.os, leg.shard)),
            };
            (leg, night)
        })
        .collect();
    for v in verdicts {
        let night = nights
            .iter_mut()
            .find(|(leg, _)| leg.os == v.os && Some(leg.shard) == v.shard)
            .map(|(_, night)| night)
            .ok_or_else(|| {
                format!(
                    "verdicts from no leg the matrix runs: {} {:?}",
                    v.os, v.shard
                )
            })?;
        if night.reported.replace(v).is_some() {
            return Err(format!("{} shard {:?} reported twice", v.os, v.shard));
        }
    }

    let mut ours: BTreeMap<Key, &Issue> = BTreeMap::new();
    for issue in issues {
        let Some(key) = Key::of(&issue.body) else {
            continue;
        };
        let kept = ours.entry(key).or_insert(issue);
        let open = |i: &Issue| i.state == State::Open;
        if (open(issue), issue.number) > (open(kept), kept.number) {
            *kept = issue;
        }
    }

    let mut filed: BTreeMap<Key, String> = BTreeMap::new();
    let mut failing: BTreeMap<&str, Vec<(Leg, &RunVerdict)>> = BTreeMap::new();
    for (leg, night) in &nights {
        for r in night.reported.iter().flat_map(|v| &v.runs) {
            if r.label > Label::Pass {
                failing.entry(&r.source).or_default().push((*leg, r));
            }
        }
    }
    for (source, rows) in failing {
        filed.insert(
            Key::Source(source.to_owned()),
            source_note(&rows, &nights, run),
        );
    }
    let mut lost = Vec::new();
    for (leg, night) in &nights {
        let why = match (night.finished, night.reported) {
            (false, _) => "never finished its run",
            (true, None) => "finished its run but reported no verdicts",
            (true, Some(_)) => continue,
        };
        lost.push(format!("{leg} {why}"));
        let runs = night
            .runs
            .map_or(String::new(), |url| format!(" [Its runs.]({url})"));
        filed.insert(
            Key::Leg(*leg),
            format!("**Unfinished** on {leg}, in {run}: the leg {why}, so no verdict says what it ran.{runs}\n"),
        );
    }

    let mut actions = Vec::new();
    for (key, note) in &filed {
        actions.push(match ours.get(key) {
            Some(issue) if issue.state == State::Open => Action::Comment {
                number: issue.number,
                body: note.clone(),
            },
            Some(issue) => Action::Reopen {
                number: issue.number,
                body: note.clone(),
            },
            None => Action::Open {
                title: key.title(),
                body: format!("{}\n{note}", key.marker()),
            },
        });
    }
    for (key, issue) in &ours {
        if issue.state != State::Open || filed.contains_key(key) {
            continue;
        }
        let body = match key {
            // Not filed tonight, so it reported.
            Key::Leg(leg) => format!("{leg} finished its run and reported its verdicts in {run}."),
            Key::Source(source) if lost.is_empty() && passed(source, &nights) => {
                format!("Every leg that runs {source} judged each of its runs Pass in {run}.")
            }
            _ => continue,
        };
        actions.push(Action::Close {
            number: issue.number,
            body,
        });
    }
    Ok(Plan { actions, lost })
}

/// Whether both legs of the shard that runs `source` reported its runs, and
/// judged each Pass.
fn passed(source: &str, nights: &BTreeMap<Leg, Night<'_>>) -> bool {
    let Some(shard) = SHARDS.iter().position(|s| s.contains(&source)) else {
        return false;
    };
    OSES.iter().all(|&os| {
        let runs: Vec<&RunVerdict> = nights[&Leg {
            os,
            shard: shard + 1,
        }]
            .reported
            .iter()
            .flat_map(|v| &v.runs)
            .filter(|r| r.source == source)
            .collect();
        !runs.is_empty() && runs.iter().all(|r| r.label == Label::Pass)
    })
}

/// A failing source's note: its worst label and legs, then each run that did
/// not pass with each leg's version, label, findings and runs.
fn source_note(
    rows: &[(Leg, &RunVerdict)],
    nights: &BTreeMap<Leg, Night<'_>>,
    run: &str,
) -> String {
    let worst = rows
        .iter()
        .map(|(_, r)| r.label)
        .max()
        .unwrap_or(Label::Pass);
    let mut legs: Vec<String> = rows.iter().map(|(leg, _)| leg.to_string()).collect();
    legs.dedup();
    let mut by_key: BTreeMap<&str, Vec<&(Leg, &RunVerdict)>> = BTreeMap::new();
    for row in rows {
        by_key.entry(&row.1.key).or_default().push(row);
    }
    let mut note = format!("**{worst:?}** on {}, in {run}.\n", legs.join("; "));
    for (key, rows) in by_key {
        note += &format!("\n### {key}\n");
        for (leg, r) in rows {
            let version = r
                .version
                .as_ref()
                .map_or(String::new(), |v| format!(" ({v})"));
            let runs = nights[leg]
                .runs
                .map_or(String::new(), |url| format!(" · [its runs]({url})"));
            note += &format!("\n#### {leg}{version}: {:?}{runs}\n", r.label);
            for (label, finding) in &r.findings {
                note += &format!("- {label:?}: {finding}\n");
            }
        }
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "https://github.com/o/r/actions/runs/1";

    /// The drift outputs of a night every leg finished, those in `uploaded`
    /// having uploaded their runs.
    fn drift(uploaded: &[(&str, usize)]) -> Map<String, Value> {
        let mut drift = Map::new();
        for leg in matrix() {
            drift.insert(moved_output(leg.os, leg.shard), "[]".into());
            let url = if uploaded.contains(&(leg.os, leg.shard)) {
                format!("https://a/{}-{}", leg.os, leg.shard)
            } else {
                String::new()
            };
            drift.insert(artifact_output(leg.os, leg.shard), url.into());
        }
        drift
    }

    fn run(source: &str, scenario: &str, label: Label, finding: &str) -> RunVerdict {
        RunVerdict {
            source: source.into(),
            key: format!("{source}/{scenario}"),
            version: Some("1.0".into()),
            label,
            findings: if label == Label::Pass {
                Vec::new()
            } else {
                vec![(label, finding.into())]
            },
        }
    }

    /// The shard `source` runs on.
    fn shard_of(source: &str) -> usize {
        SHARDS
            .iter()
            .position(|s| s.contains(&source))
            .expect("a sharded source")
            + 1
    }

    /// Every leg's verdicts: each source of its shard Pass but those `edit`
    /// rewrites.
    fn night(edit: impl Fn(&str, &mut Vec<RunVerdict>)) -> Vec<Verdicts> {
        matrix()
            .map(|leg| {
                let mut runs: Vec<RunVerdict> = SHARDS[leg.shard - 1]
                    .iter()
                    .map(|source| run(source, "tool-run", Label::Pass, ""))
                    .collect();
                edit(leg.os, &mut runs);
                Verdicts {
                    os: leg.os.into(),
                    shard: Some(leg.shard),
                    runs,
                }
            })
            .collect()
    }

    fn issue(number: u64, state: State, key: &Key) -> Issue {
        Issue {
            number,
            state,
            body: format!("{}\nbody", key.marker()),
        }
    }

    fn hermes() -> Key {
        Key::Source("hermes".into())
    }

    /// On hermes's legs: its tool-run Fails on both OSes, and its permission
    /// is Blocked on macOS alone.
    fn hermes_fails(os: &str, runs: &mut Vec<RunVerdict>) {
        if !runs.iter().any(|r| r.source == "hermes") {
            return;
        }
        runs.retain(|r| r.source != "hermes");
        runs.push(run(
            "hermes",
            "tool-run",
            Label::Fail,
            "no on_session_finalize",
        ));
        if os == "macos" {
            runs.push(run(
                "hermes",
                "permission",
                Label::Blocked,
                "the turn's driver gave up on it (exit 3, 3 model requests): the screen stood still",
            ));
        }
    }

    #[test]
    fn a_failing_source_opens_one_issue_naming_each_run_its_legs_and_their_runs() {
        let shard = shard_of("hermes");
        let plan =
            plan(&drift(&[("macos", shard)]), &night(hermes_fails), &[], RUN).expect("a plan");
        let [Action::Open { title, body }] = &plan.actions[..] else {
            panic!("one issue for the one failing source: {:?}", plan.actions);
        };
        assert_eq!(title, "hermetic: hermes");
        assert!(body.starts_with(&hermes().marker()), "{body}");
        for line in [
            format!("**Fail** on macos, shard {shard}; windows, shard {shard}, in {RUN}."),
            "### hermes/tool-run".into(),
            "### hermes/permission".into(),
            format!("#### macos, shard {shard} (1.0): Fail · [its runs](https://a/macos-{shard})"),
            format!("#### windows, shard {shard} (1.0): Fail"),
            "- Fail: no on_session_finalize".into(),
            "- Blocked: the turn's driver gave up on it (exit 3, 3 model requests): the screen stood still".into(),
        ] {
            assert!(body.lines().any(|l| l == line), "no `{line}` in:\n{body}");
        }
        assert!(plan.lost.is_empty());
    }

    #[test]
    fn a_wide_outage_opens_one_issue_per_source() {
        let outage = night(|_, runs| {
            for r in runs.iter_mut() {
                *r = run(
                    &r.source.clone(),
                    "tool-run",
                    Label::Blocked,
                    "npm view failed",
                );
            }
            let extra: Vec<RunVerdict> = runs
                .iter()
                .map(|r| {
                    run(
                        &r.source,
                        "lifecycle",
                        Label::Blocked,
                        "its tool-run run was not judged",
                    )
                })
                .collect();
            runs.extend(extra);
        });
        let plan = plan(&drift(&[]), &outage, &[], RUN).expect("a plan");
        assert_eq!(
            plan.actions.len(),
            super::super::PROFILES.len(),
            "{:?}",
            plan.actions
        );
    }

    #[test]
    fn a_source_with_an_issue_comments_on_it_or_reopens_it() {
        let open = [
            issue(7, State::Open, &hermes()),
            issue(3, State::Closed, &hermes()),
        ];
        let plan = plan(&drift(&[]), &night(hermes_fails), &open, RUN).expect("a plan");
        assert!(
            matches!(&plan.actions[..], [Action::Comment { number: 7, .. }]),
            "the open one of the key: {:?}",
            plan.actions
        );
        let closed = [
            issue(3, State::Closed, &hermes()),
            issue(5, State::Closed, &hermes()),
        ];
        let plan = super::plan(&drift(&[]), &night(hermes_fails), &closed, RUN).expect("a plan");
        assert!(
            matches!(&plan.actions[..], [Action::Reopen { number: 5, .. }]),
            "the newest closed one, never a duplicate: {:?}",
            plan.actions
        );
    }

    #[test]
    fn a_source_issue_closes_only_on_a_pass_every_leg_reported() {
        let open = [issue(7, State::Open, &hermes())];
        let closes = |verdicts: &[Verdicts], drift: &Map<String, Value>| {
            plan(drift, verdicts, &open, RUN)
                .expect("a plan")
                .actions
                .iter()
                .any(|a| matches!(a, Action::Close { number: 7, .. }))
        };
        let all = drift(&[]);
        assert!(
            closes(&night(|_, _| {}), &all),
            "both its legs judged it Pass"
        );
        let mut lost_leg = all.clone();
        lost_leg.insert(moved_output("windows", 1), "".into());
        assert!(
            !closes(&night(|_, _| {}), &lost_leg),
            "a lost leg's runs are unknown"
        );
        let unreported: Vec<Verdicts> = night(|_, _| {})
            .into_iter()
            .filter(|v| !(v.os == "windows" && v.shard == Some(shard_of("hermes"))))
            .collect();
        assert!(!closes(&unreported, &all), "a leg that reported nothing");
        let absent = night(|os, runs| {
            if os == "windows" {
                runs.retain(|r| r.source != "hermes");
            }
        });
        assert!(!closes(&absent, &all), "a leg that never judged it");
        let renamed = [issue(8, State::Open, &Key::Source("gone".into()))];
        assert!(
            plan(&all, &night(|_, _| {}), &renamed, RUN)
                .expect("a plan")
                .actions
                .is_empty(),
            "a source no shard runs has no leg to pass on"
        );
    }

    #[test]
    fn a_leg_that_reported_no_verdicts_files_its_own_issue_and_closes_nothing() {
        let mut lost = drift(&[("macos", 1)]);
        lost.insert(moved_output("macos", 1), "".into());
        let unreported: Vec<Verdicts> = night(|_, _| {})
            .into_iter()
            .filter(|v| !(v.os == "windows" && v.shard == Some(2)))
            .collect();
        let open = [issue(7, State::Open, &hermes())];
        let plan = plan(&lost, &unreported, &open, RUN).expect("a plan");
        assert_eq!(
            plan.lost,
            [
                "macos, shard 1 never finished its run",
                "windows, shard 2 finished its run but reported no verdicts",
            ]
        );
        let titles: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Open { title, .. } => Some(title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            titles,
            [
                "hermetic: macos, shard 1 reported no verdicts",
                "hermetic: windows, shard 2 reported no verdicts",
            ]
        );
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| matches!(a, Action::Close { .. }))
        );
    }

    #[test]
    fn a_legs_issue_closes_once_it_reports_again() {
        let leg = Key::Leg(Leg {
            os: "windows",
            shard: 1,
        });
        let plan = plan(
            &drift(&[]),
            &night(|_, _| {}),
            &[issue(10, State::Open, &leg)],
            RUN,
        )
        .expect("a plan");
        assert_eq!(
            plan.actions,
            [Action::Close {
                number: 10,
                body: format!(
                    "windows, shard 1 finished its run and reported its verdicts in {RUN}."
                ),
            }]
        );
    }

    #[test]
    fn an_issue_without_the_marker_is_never_touched() {
        let theirs = Issue {
            number: 1658,
            state: State::Open,
            body: "hermetic: a pin bump failed".into(),
        };
        let plan = plan(&drift(&[]), &night(|_, _| {}), &[theirs], RUN).expect("a plan");
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    }

    #[test]
    fn verdicts_from_no_leg_of_the_matrix_or_twice_from_one_are_refused() {
        let mut stray = night(|_, _| {});
        stray[0].shard = None;
        assert!(plan(&drift(&[]), &stray, &[], RUN).is_err());
        let mut twice = night(|_, _| {});
        twice.push(twice[0].clone());
        assert!(plan(&drift(&[]), &twice, &[], RUN).is_err());
    }

    #[test]
    fn a_marker_names_its_key_and_nothing_else_does() {
        for key in [
            hermes(),
            Key::Leg(Leg {
                os: "macos",
                shard: 2,
            }),
        ] {
            assert_eq!(Key::of(&format!("text\n{}\n", key.marker())), Some(key));
        }
        assert_eq!(Key::of("<!-- hermetic-leg: linux-1 -->"), None);
        assert_eq!(Key::of("hermetic: hermes"), None);
    }
}
