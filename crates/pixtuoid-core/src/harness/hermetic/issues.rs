//! The nightly's hermetic issues: one per source a leg judged anything but
//! Pass or whose feed released past its pin, one per leg that lost its
//! verdicts, and what each night does to them. The workflow only carries a
//! [`Plan`] out with `gh`. Every input is untrusted, since the legs ran
//! just-published CLIs: what one could forge is a `hermetic`-labelled issue's
//! text and whether it is open, never code.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::verify::Label;
use super::{Leg, NOT_HERMETIC, OSES, Unprofiled, legs, shard_of};
use crate::harness::plan::{PROMPT_ENV, SEED_ENV};
use crate::harness::provenance::{NonEmpty, Provenance, Recorded};
use crate::source::registry;

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

/// An unprofiled source's latest release, as its feed named it, or why none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Freshness {
    /// The source, as [`NOT_HERMETIC`] names it.
    pub source: String,
    /// The release as its feed spells it, or why the feed named none.
    pub latest: Result<String, String>,
}

/// A `hermetic`-labelled issue, as `gh issue list --json number,state,body`
/// lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Issue {
    pub(crate) number: u64,
    pub(crate) state: State,
    /// Its body, whose marker says which of the plan's it is.
    pub(crate) body: String,
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
    pub(crate) actions: Vec<Action>,
    /// Why the night's verdicts are incomplete; any entry turns the filing red.
    pub(crate) lost: Vec<String>,
}

/// The `hermetic.yml` output `leg` names its uploaded runs' URL in.
pub(crate) fn runs_url_output(leg: Leg) -> String {
    format!("runs_url_{}_{}", leg.os, leg.shard)
}

/// What one of the plan's issues is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Subject {
    Source(String),
    Leg(Leg),
}

/// What opens the hidden line of a source's issue body, before the source.
const SOURCE_MARKER: &str = "<!-- hermetic-source: ";
/// What opens the hidden line of a leg's issue body, before `<os>-<shard>`.
const LEG_MARKER: &str = "<!-- hermetic-leg: ";
/// What closes either hidden line.
const MARKER_END: &str = " -->";

impl Subject {
    /// The hidden line that names it in its issue's body: a title someone
    /// else writes can look the same.
    fn marker(&self) -> String {
        match self {
            Self::Source(source) => format!("{SOURCE_MARKER}{source}{MARKER_END}"),
            Self::Leg(leg) => format!("{LEG_MARKER}{}-{}{MARKER_END}", leg.os, leg.shard),
        }
    }

    /// The subject `body`'s marker names, if it has one.
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
            legs()
                .find(|leg| leg.os == os && leg.shard == shard)
                .map(Self::Leg)
        })
    }

    fn title(&self) -> String {
        match self {
            Self::Source(source) => format!("hermetic: {source}"),
            Self::Leg(leg) => format!("hermetic: {leg} lost its verdicts"),
        }
    }
}

/// One leg's night, as its verdicts and the drift job's outputs tell it.
struct Night<'a> {
    reported: Reported<'a>,
    runs_url: Option<&'a str>,
}

/// What a leg reported.
enum Reported<'a> {
    Missing,
    /// Verdicts of the sources its shard runs.
    Valid(&'a Verdicts),
    /// Verdicts it could not have reported, and why: a leg's are untrusted.
    Invalid(String),
}

impl Night<'_> {
    fn verdicts(&self) -> Option<&Verdicts> {
        match self.reported {
            Reported::Valid(v) => Some(v),
            Reported::Missing | Reported::Invalid(_) => None,
        }
    }
}

/// What to do with the `hermetic` issues `issues`, given the drift job's
/// outputs, every verdicts file the legs uploaded, the unprofiled sources'
/// `fresh` releases and the run's URL. A leg whose verdicts are missing, or
/// name a leg or a source the matrix gives it no part in, is lost: it files an
/// issue of its own, and no profiled source's issue closes that night. A
/// profiled source's closes only on a night no leg is lost and every leg
/// running it reported its runs, each Pass; an unprofiled one's, on a night its
/// feed named its pin; a leg's, once it reported again.
pub fn plan(
    drift: &Map<String, Value>,
    reported: &[std::io::Result<Verdicts>],
    fresh: &std::io::Result<Vec<Freshness>>,
    issues: &[Issue],
    run_url: &str,
) -> Plan {
    let mut nights: BTreeMap<Leg, Night<'_>> = legs()
        .map(|leg| {
            let runs_url = drift
                .get(&runs_url_output(leg))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty());
            let night = Night {
                reported: Reported::Missing,
                runs_url,
            };
            (leg, night)
        })
        .collect();
    let mut lost = Vec::new();
    for v in reported {
        let v = match v {
            Ok(v) => v,
            Err(e) => {
                lost.push(format!("unreadable verdicts: {e}"));
                continue;
            }
        };
        let Some((leg, night)) = nights
            .iter_mut()
            .find(|(leg, _)| leg.os == v.os && Some(leg.shard) == v.shard)
        else {
            let shard = v.shard.map_or("no shard".into(), |n| format!("shard {n}"));
            lost.push(format!(
                "verdicts named no leg of the matrix: {}, {shard}",
                v.os
            ));
            continue;
        };
        let stray = v
            .runs
            .iter()
            .find(|r| shard_of(&r.source) != Some(leg.shard));
        night.reported = match (&night.reported, stray) {
            (Reported::Missing, None) => Reported::Valid(v),
            (Reported::Missing, Some(r)) => Reported::Invalid(format!(
                "a run of {}, which its shard does not run",
                r.source
            )),
            (Reported::Valid(_) | Reported::Invalid(_), _) => {
                Reported::Invalid("its verdicts twice".into())
            }
        };
    }

    let mut ours: BTreeMap<Subject, &Issue> = BTreeMap::new();
    for issue in issues {
        let Some(subject) = Subject::of(&issue.body) else {
            continue;
        };
        let kept = ours.entry(subject).or_insert(issue);
        let open = |i: &Issue| i.state == State::Open;
        if (open(issue), issue.number) > (open(kept), kept.number) {
            *kept = issue;
        }
    }

    let mut filed: BTreeMap<Subject, String> = BTreeMap::new();
    let mut failing: BTreeMap<&str, Vec<(Leg, &RunVerdict)>> = BTreeMap::new();
    for (leg, night) in &nights {
        for r in night.verdicts().iter().flat_map(|v| &v.runs) {
            if r.label > Label::Pass {
                failing.entry(&r.source).or_default().push((*leg, r));
            }
        }
    }
    for (source, rows) in failing {
        filed.insert(
            Subject::Source(source.to_owned()),
            source_note(&rows, &nights, run_url),
        );
    }
    for (leg, night) in &nights {
        let why = match &night.reported {
            Reported::Missing => "reported no verdicts".to_owned(),
            Reported::Invalid(what) => format!("reported {what}"),
            Reported::Valid(_) => continue,
        };
        lost.push(format!("{leg} {why}"));
        let runs = night
            .runs_url
            .map_or(String::new(), |url| format!(" [Its runs.]({url})"));
        filed.insert(
            Subject::Leg(*leg),
            format!("**Lost** on {leg}, in {run_url}: the leg {why}, so no verdict says what it ran.{runs}\n"),
        );
    }

    // Kept apart from the legs' losses: a lost freshness says nothing of what a
    // leg judged.
    let mut fresh_lost = Vec::new();
    let mut at_pin: BTreeSet<&str> = BTreeSet::new();
    match fresh {
        Err(e) => fresh_lost.push(format!("unreadable freshness: {e}")),
        Ok(rows) => {
            for row in rows {
                if !NOT_HERMETIC.iter().any(|u| u.source == row.source) {
                    fresh_lost.push(format!("freshness of {}, which has no feed", row.source));
                }
            }
            for u in NOT_HERMETIC {
                let mine: Vec<&Freshness> = rows.iter().filter(|r| r.source == u.source).collect();
                let [row] = mine[..] else {
                    fresh_lost.push(format!(
                        "freshness named {} releases of {}",
                        mine.len(),
                        u.source
                    ));
                    continue;
                };
                match freshness_note(u, &row.latest, run_url) {
                    None => {
                        at_pin.insert(u.source);
                    }
                    Some(note) => {
                        filed.insert(Subject::Source(u.source.to_owned()), note);
                    }
                }
            }
        }
    }

    let mut actions = Vec::new();
    for (subject, note) in &filed {
        actions.push(match ours.get(subject) {
            Some(issue) if issue.state == State::Open => Action::Comment {
                number: issue.number,
                body: note.clone(),
            },
            Some(issue) => Action::Reopen {
                number: issue.number,
                body: note.clone(),
            },
            None => Action::Open {
                title: subject.title(),
                body: format!("{}\n{note}", subject.marker()),
            },
        });
    }
    for (subject, issue) in &ours {
        if issue.state != State::Open || filed.contains_key(subject) {
            continue;
        }
        let body = match subject {
            // Not filed tonight, so it reported.
            Subject::Leg(leg) => {
                format!("{leg} reported its verdicts in {run_url}.")
            }
            Subject::Source(source) if at_pin.contains(source.as_str()) => {
                format!("{source}'s latest release is its pin, in {run_url}.")
            }
            Subject::Source(source)
                if lost.is_empty() && reported_on_every_leg(source, &nights) =>
            {
                format!("Every leg that runs {source} judged each of its runs Pass in {run_url}.")
            }
            _ => continue,
        };
        actions.push(Action::Close {
            number: issue.number,
            body,
        });
    }
    lost.extend(fresh_lost);
    Plan { actions, lost }
}

/// What an unprofiled source's feed naming `latest` files: nothing at its pin;
/// past it, every capture of the source, which only a billed run re-records.
fn freshness_note(
    u: &Unprofiled,
    latest: &Result<String, String>,
    run_url: &str,
) -> Option<String> {
    let d = registry::descriptor_for(u.source)?;
    let release = match latest {
        Err(why) => {
            return Some(format!(
                "**Blocked** in {run_url}: its feed named no release: {why}\n"
            ));
        }
        Ok(text) => text,
    };
    let version = |s: &str| registry::parse_version(s, d.version_hyphen);
    // One token, as a feed spells a release: the body echoes it.
    let parsed = version(release).filter(|_| {
        release
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    });
    let Some(latest) = parsed else {
        return Some(format!(
            "**Blocked** in {run_url}: its feed named {release:?}, not a release\n"
        ));
    };
    let pin = d.verified_version;
    match version(pin).map(|p| latest.cmp_precedence(&p)) {
        Some(std::cmp::Ordering::Equal) => None,
        Some(std::cmp::Ordering::Greater) => {
            let root = crate::harness::sources_root();
            let mut note = format!(
                "**Outdated** in {run_url}: {} {release} is released, and its captures are \
                 recorded at its pin, {pin}. Each is owed a billed re-capture at the new \
                 release, and `verified_version` moves with them:\n",
                u.source
            );
            for c in crate::harness::captures(&root)
                .into_iter()
                .filter(|c| c.source == u.source)
            {
                let at = crate::harness::slashed_under(&root, &c.dir)
                    .unwrap_or_else(|_| c.dir.display().to_string());
                let line = Provenance::read(&c.provenance_path()).ok().map_or_else(
                    || "nothing recorded".to_owned(),
                    |p| recapture_line(u.source, &c.dir, p.recorded()),
                );
                note += &format!("- `{at}`: `{line}`\n");
            }
            Some(note)
        }
        _ => Some(format!(
            "**Blocked** in {run_url}: its feed's latest, {release}, is below its pin, {pin}\n"
        )),
    }
}

/// The line that records `source`'s capture in `dir` again as `record` says it
/// was recorded, its prompt and seed in the env the recorder reads them from.
fn recapture_line(source: &str, dir: &std::path::Path, record: &Recorded) -> String {
    let quoted = |v: &NonEmpty| format!("'{}'", v.as_str().replace('\'', r"'\''"));
    let env: String = [(PROMPT_ENV, &record.prompt), (SEED_ENV, &record.seed)]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={} ", quoted(v))))
        .collect();
    let scenario = dir.file_name().unwrap_or_default().to_string_lossy();
    format!(
        "{env}just capture-fixture {source} {scenario} {}",
        record.command.as_str()
    )
}

/// Whether every leg of [`OSES`] on the shard that runs `source` reported its runs.
fn reported_on_every_leg(source: &str, nights: &BTreeMap<Leg, Night<'_>>) -> bool {
    let Some(shard) = shard_of(source) else {
        return false;
    };
    OSES.iter().all(|&os| {
        nights[&Leg { os, shard }]
            .verdicts()
            .is_some_and(|v| v.runs.iter().any(|r| r.source == source))
    })
}

/// A failing source's note: its worst label and legs, then each run not
/// labelled Pass with each leg's version, label, findings and runs.
fn source_note(
    rows: &[(Leg, &RunVerdict)],
    nights: &BTreeMap<Leg, Night<'_>>,
    run_url: &str,
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
    let mut note = format!("**{worst:?}** on {}, in {run_url}.\n", legs.join("; "));
    for (key, rows) in by_key {
        note += &format!("\n### {key}\n");
        for (leg, r) in rows {
            let version = r
                .version
                .as_ref()
                .map_or(String::new(), |v| format!(" ({v})"));
            let runs = nights[leg]
                .runs_url
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
    use super::super::SHARDS;
    use super::super::bump::repo_root;
    use super::*;

    const RUN: &str = "https://github.com/o/r/actions/runs/1";

    /// The planner on a night every unprofiled source's feed named its pin.
    fn plan(
        drift: &Map<String, Value>,
        reported: &[std::io::Result<Verdicts>],
        issues: &[Issue],
        run_url: &str,
    ) -> Plan {
        super::plan(drift, reported, &current(), issues, run_url)
    }

    fn current() -> std::io::Result<Vec<Freshness>> {
        Ok(NOT_HERMETIC
            .iter()
            .map(|u| fresh(u.source, Ok(pin_of(u.source))))
            .collect())
    }

    fn pin_of(source: &str) -> String {
        registry::descriptor_for(source)
            .expect("registered")
            .verified_version
            .to_owned()
    }

    fn fresh(source: &str, latest: Result<String, String>) -> Freshness {
        Freshness {
            source: source.into(),
            latest,
        }
    }

    fn cursor() -> Subject {
        Subject::Source("cursor".into())
    }

    /// The drift outputs of a night whose legs in `uploaded` uploaded their runs.
    fn drift(uploaded: &[(&str, usize)]) -> Map<String, Value> {
        legs()
            .map(|leg| {
                let url = if uploaded.contains(&(leg.os, leg.shard)) {
                    format!("https://a/{}-{}", leg.os, leg.shard)
                } else {
                    String::new()
                };
                (runs_url_output(leg), url.into())
            })
            .collect()
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

    /// Every leg's verdicts: each source of its shard Pass but those `edit`
    /// rewrites.
    fn night(edit: impl Fn(&str, &mut Vec<RunVerdict>)) -> Vec<std::io::Result<Verdicts>> {
        legs()
            .map(|leg| {
                let mut runs: Vec<RunVerdict> = SHARDS[leg.shard - 1]
                    .iter()
                    .map(|source| run(source, "tool-run", Label::Pass, ""))
                    .collect();
                edit(leg.os, &mut runs);
                Ok(Verdicts {
                    os: leg.os.into(),
                    shard: Some(leg.shard),
                    runs,
                })
            })
            .collect()
    }

    fn without(
        reported: Vec<std::io::Result<Verdicts>>,
        os: &str,
        shard: usize,
    ) -> Vec<std::io::Result<Verdicts>> {
        reported
            .into_iter()
            .filter(|v| !matches!(v, Ok(v) if v.os == os && v.shard == Some(shard)))
            .collect()
    }

    fn issue(number: u64, state: State, subject: &Subject) -> Issue {
        Issue {
            number,
            state,
            body: format!("{}\nbody", subject.marker()),
        }
    }

    fn hermes() -> Subject {
        Subject::Source("hermes".into())
    }

    fn hermes_shard() -> usize {
        shard_of("hermes").expect("a sharded source")
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

    fn closes(plan: &Plan, number: u64) -> bool {
        plan.actions
            .iter()
            .any(|a| matches!(a, Action::Close { number: n, .. } if *n == number))
    }

    #[test]
    fn a_failing_source_opens_one_issue_naming_each_run_its_legs_and_their_runs() {
        let shard = hermes_shard();
        let plan = plan(&drift(&[("macos", shard)]), &night(hermes_fails), &[], RUN);
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
        let plan = plan(&drift(&[]), &outage, &[], RUN);
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
            issue(3, State::Open, &hermes()),
            issue(7, State::Closed, &hermes()),
        ];
        let commented = plan(&drift(&[]), &night(hermes_fails), &open, RUN);
        assert!(
            matches!(&commented.actions[..], [Action::Comment { number: 3, .. }]),
            "the open one, though a closed one is newer: {:?}",
            commented.actions
        );
        let closed = [
            issue(3, State::Closed, &hermes()),
            issue(5, State::Closed, &hermes()),
        ];
        let reopened = plan(&drift(&[]), &night(hermes_fails), &closed, RUN);
        assert!(
            matches!(&reopened.actions[..], [Action::Reopen { number: 5, .. }]),
            "the newest closed one, never a duplicate: {:?}",
            reopened.actions
        );
    }

    #[test]
    fn a_source_issue_closes_only_on_a_pass_every_leg_reported() {
        let open = [issue(7, State::Open, &hermes())];
        let closes = |reported: &[std::io::Result<Verdicts>]| {
            closes(&plan(&drift(&[]), reported, &open, RUN), 7)
        };
        assert!(closes(&night(|_, _| {})), "both its legs judged it Pass");
        let other = 3 - hermes_shard();
        assert!(
            !closes(&without(night(|_, _| {}), "macos", other)),
            "a lost leg's runs are unknown"
        );
        assert!(
            !closes(&without(night(|_, _| {}), "windows", hermes_shard())),
            "a leg that reported nothing"
        );
        let absent = night(|os, runs| {
            if os == "windows" {
                runs.retain(|r| r.source != "hermes");
            }
        });
        assert!(!closes(&absent), "a leg that never judged it");
        let new = night(|os, runs| {
            if os == "windows" {
                for r in runs.iter_mut().filter(|r| r.source == "hermes") {
                    *r = run("hermes", "tool-run", Label::New, "/type: `x`");
                }
            }
        });
        assert!(!closes(&new), "a run that passed with a New finding");
        let renamed = [issue(8, State::Open, &Subject::Source("gone".into()))];
        assert!(
            plan(&drift(&[]), &night(|_, _| {}), &renamed, RUN)
                .actions
                .is_empty(),
            "a source no shard runs has no leg to pass on"
        );
    }

    #[test]
    fn a_leg_that_reported_no_verdicts_files_its_own_issue_and_closes_no_source_issue() {
        let unreported = without(without(night(|_, _| {}), "macos", 1), "windows", 2);
        let open = [issue(7, State::Open, &hermes())];
        let plan = plan(&drift(&[("macos", 1)]), &unreported, &open, RUN);
        assert_eq!(
            plan.lost,
            [
                "macos, shard 1 reported no verdicts",
                "windows, shard 2 reported no verdicts",
            ]
        );
        let opened: Vec<(&str, &str)> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Open { title, body } => Some((title.as_str(), body.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            opened.iter().map(|(title, _)| *title).collect::<Vec<_>>(),
            [
                "hermetic: macos, shard 1 lost its verdicts",
                "hermetic: windows, shard 2 lost its verdicts",
            ]
        );
        assert!(
            opened[0].1.contains(&format!(
                "**Lost** on macos, shard 1, in {RUN}: the leg reported no verdicts, so no \
                 verdict says what it ran. [Its runs.](https://a/macos-1)"
            )),
            "{}",
            opened[0].1
        );
        assert!(!closes(&plan, 7));
    }

    #[test]
    fn an_unreadable_verdicts_file_is_lost_and_closes_no_source_issue() {
        let mut reported = without(night(|_, _| {}), "macos", 1);
        reported.push(Err(std::io::Error::other(
            "verdicts.json: EOF while parsing",
        )));
        let open = [issue(7, State::Open, &hermes())];
        let plan = plan(&drift(&[]), &reported, &open, RUN);
        assert_eq!(
            plan.lost,
            [
                "unreadable verdicts: verdicts.json: EOF while parsing",
                "macos, shard 1 reported no verdicts",
            ]
        );
        assert!(!closes(&plan, 7));
    }

    #[test]
    fn a_legs_issue_closes_once_it_reports_again() {
        let leg = Subject::Leg(Leg {
            os: "windows",
            shard: 1,
        });
        let plan = plan(
            &drift(&[]),
            &night(|_, _| {}),
            &[issue(10, State::Open, &leg)],
            RUN,
        );
        assert_eq!(
            plan.actions,
            [Action::Close {
                number: 10,
                body: format!("windows, shard 1 reported its verdicts in {RUN}."),
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
        let plan = plan(&drift(&[]), &night(|_, _| {}), &[theirs], RUN);
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    }

    #[test]
    fn a_leg_that_reported_what_it_could_not_have_is_lost_and_closes_no_source_issue() {
        let open = [issue(7, State::Open, &hermes())];
        let forged = night(|os, runs| {
            if os == "windows" && runs[0].source == SHARDS[0][0] {
                runs.extend((0..500).map(|n| run(&format!("fake{n}"), "x", Label::Fail, "forged")));
            }
        });
        let p = plan(&drift(&[]), &forged, &open, RUN);
        assert_eq!(
            p.lost,
            ["windows, shard 1 reported a run of fake0, which its shard does not run"]
        );
        assert!(
            matches!(&p.actions[..], [Action::Open { title, .. }] if title == "hermetic: windows, shard 1 lost its verdicts"),
            "one issue for the leg, none for what it forged, and no close: {:?}",
            p.actions
        );
        let mut twice = night(|_, _| {});
        let first = twice[0].as_ref().map(Clone::clone).expect("verdicts");
        twice.push(Ok(first));
        assert_eq!(
            plan(&drift(&[]), &twice, &open, RUN).lost,
            ["macos, shard 1 reported its verdicts twice"]
        );
        let mut stray = night(|_, _| {});
        if let Ok(v) = &mut stray[0] {
            v.shard = None;
        }
        assert_eq!(
            plan(&drift(&[]), &stray, &open, RUN).lost,
            [
                "verdicts named no leg of the matrix: macos, no shard",
                "macos, shard 1 reported no verdicts",
            ]
        );
    }

    /// Freshness's plan alone: every leg reported its shard Pass.
    fn freshness(fresh: std::io::Result<Vec<Freshness>>, issues: &[Issue]) -> Plan {
        super::plan(&drift(&[]), &night(|_, _| {}), &fresh, issues, RUN)
    }

    #[test]
    fn an_unprofiled_release_past_its_pin_opens_one_issue_naming_each_capture_owed() {
        let plan = freshness(Ok(vec![fresh("cursor", Ok("2099.01.02-abc".into()))]), &[]);
        let [Action::Open { title, body }] = &plan.actions[..] else {
            panic!("one issue for cursor: {:?}", plan.actions);
        };
        assert_eq!(title, "hermetic: cursor");
        assert!(body.starts_with(&cursor().marker()), "{body}");
        assert!(
            body.contains(&format!(
                "**Outdated** in {RUN}: cursor 2099.01.02-abc is released, and its captures are recorded at its pin, {}.",
                pin_of("cursor")
            )),
            "{body}"
        );
        let owed = crate::harness::captures(&crate::harness::sources_root())
            .into_iter()
            .filter(|c| c.source == "cursor")
            .collect::<Vec<_>>();
        assert!(!owed.is_empty());
        for c in owed {
            let at = crate::harness::slashed_under(&crate::harness::sources_root(), &c.dir)
                .expect("under the corpus");
            let record = Provenance::read(&c.provenance_path()).expect("a record");
            let r = record.recorded();
            let scenario = c.dir.file_name().expect("a dir").to_string_lossy();
            let row = body
                .lines()
                .find(|l| l.starts_with(&format!("- `{at}`: `")))
                .unwrap_or_else(|| panic!("no `{at}` row in:\n{body}"));
            assert!(
                row.contains(&format!(
                    "just capture-fixture cursor {scenario} {}`",
                    r.command.as_str()
                )),
                "{row}"
            );
            assert_eq!(
                row.contains(&format!("{PROMPT_ENV}='")),
                r.prompt.is_some(),
                "a scenario's own prompt rides its line: {row}"
            );
        }
        assert!(plan.lost.is_empty(), "{:?}", plan.lost);
    }

    #[test]
    fn a_recapture_line_quotes_its_prompt_for_the_shell() {
        let record: Recorded = serde_json::from_value(serde_json::json!({
            "cli": "cursor-agent",
            "version": "1",
            "captured": "2026-10-27",
            "command": "cursor-agent -p {prompt}",
            "deidentified": {"method": "decoder-allowlist", "blanked": 0},
            "prompt": "Don't read it",
        }))
        .expect("a record");
        assert_eq!(
            recapture_line(
                "cursor",
                std::path::Path::new("/s/fixtures/cursor/x"),
                &record
            ),
            r"CAPTURE_PROMPT='Don'\''t read it' just capture-fixture cursor x cursor-agent -p {prompt}"
        );
    }

    #[test]
    fn an_unprofiled_sources_issue_is_one_across_nights_and_closes_at_its_pin() {
        let behind = || Ok(vec![fresh("cursor", Ok("2099.1.2".into()))]);
        let open = [issue(4, State::Open, &cursor())];
        assert!(
            matches!(
                &freshness(behind(), &open).actions[..],
                [Action::Comment { number: 4, .. }]
            ),
            "a later night comments on the one issue"
        );
        let closed = [issue(4, State::Closed, &cursor())];
        assert!(matches!(
            &freshness(behind(), &closed).actions[..],
            [Action::Reopen { number: 4, .. }]
        ));
        assert_eq!(
            freshness(current(), &open).actions,
            [Action::Close {
                number: 4,
                body: format!("cursor's latest release is its pin, in {RUN}."),
            }]
        );
        assert!(freshness(current(), &[]).actions.is_empty());
    }

    #[test]
    fn a_feed_that_names_no_release_is_blocked_and_closes_nothing() {
        let open = [issue(4, State::Open, &cursor())];
        for (latest, why) in [
            (
                Err("curl exited 22".to_owned()),
                "its feed named no release: curl exited 22",
            ),
            (
                Ok("not a version".to_owned()),
                "its feed named \"not a version\", not a release",
            ),
            (
                Ok("2000.1.1".to_owned()),
                "its feed's latest, 2000.1.1, is below its pin",
            ),
        ] {
            let plan = freshness(Ok(vec![fresh("cursor", latest)]), &open);
            let [Action::Comment { number: 4, body }] = &plan.actions[..] else {
                panic!("{why}: {:?}", plan.actions);
            };
            assert!(
                body.starts_with(&format!("**Blocked** in {RUN}: {why}")),
                "{body}"
            );
        }
    }

    #[test]
    fn freshness_that_is_unreadable_or_names_the_wrong_sources_is_lost_and_closes_no_source_of_its_own()
     {
        let open = [
            issue(4, State::Open, &cursor()),
            issue(7, State::Open, &hermes()),
        ];
        let unreadable = freshness(Err(std::io::Error::other("EOF")), &open);
        assert_eq!(unreadable.lost, ["unreadable freshness: EOF"]);
        assert!(!closes(&unreadable, 4));
        assert!(
            closes(&unreadable, 7),
            "a leg's verdicts are not freshness's to hold"
        );
        let mut forged = current().expect("rows");
        forged.push(fresh("hermes", Ok("9.9.9".into())));
        assert_eq!(
            freshness(Ok(forged), &open).lost,
            ["freshness of hermes, which has no feed"]
        );
        assert_eq!(
            freshness(Ok(Vec::new()), &open).lost,
            ["freshness named 0 releases of cursor"]
        );
        let mut twice = current().expect("rows");
        twice.extend(current().expect("rows"));
        let p = freshness(Ok(twice), &open);
        assert_eq!(p.lost, ["freshness named 2 releases of cursor"]);
        assert!(!closes(&p, 4));
    }

    #[test]
    fn a_marker_names_its_subject_and_nothing_else_does() {
        for subject in [
            hermes(),
            Subject::Leg(Leg {
                os: "macos",
                shard: 2,
            }),
        ] {
            assert_eq!(
                Subject::of(&format!("text\n{}\n", subject.marker())),
                Some(subject)
            );
        }
        assert_eq!(Subject::of("<!-- hermetic-leg: linux-1 -->"), None);
        assert_eq!(Subject::of("hermetic: hermes"), None);
    }

    fn file_step() -> String {
        let yml =
            std::fs::read_to_string(repo_root().join(".github/workflows/hermetic-nightly.yml"))
                .expect("hermetic-nightly.yml");
        let (_, step) = yml
            .split_once("- name: File the planned issues")
            .expect("the file step");
        step.to_owned()
    }

    #[test]
    fn the_file_step_reads_every_key_a_plan_holds_and_does_every_action() {
        let body = || "body".to_owned();
        let plan = Plan {
            actions: vec![
                Action::Open {
                    title: "hermetic: hermes".into(),
                    body: body(),
                },
                Action::Comment {
                    number: 1,
                    body: body(),
                },
                Action::Reopen {
                    number: 2,
                    body: body(),
                },
                Action::Close {
                    number: 3,
                    body: body(),
                },
            ],
            lost: vec!["macos, shard 1 reported no verdicts".into()],
        };
        let json = serde_json::to_value(&plan).expect("a plan");
        let step = file_step();
        let read = |key: &str| {
            assert!(
                step.contains(&format!(".{key}")),
                "the step never reads `.{key}`"
            )
        };
        json.as_object()
            .expect("an object")
            .keys()
            .for_each(|k| read(k));
        for action in json["actions"].as_array().expect("actions") {
            let action = action.as_object().expect("an action");
            action.keys().for_each(|k| read(k));
            let what = action["do"].as_str().expect("its do");
            assert!(
                step.contains(&format!("{what})")),
                "no case arm does `{what}`"
            );
        }
    }

    #[test]
    fn the_file_step_admits_only_an_issue_the_planner_would_mark() {
        let step = file_step();
        for (name, value) in [
            ("source", SOURCE_MARKER),
            ("leg", LEG_MARKER),
            ("end", MARKER_END),
        ] {
            assert!(
                step.contains(&format!("--arg {name} '{value}'")),
                "the step's `${name}` is not `{value}`"
            );
        }
    }
}
