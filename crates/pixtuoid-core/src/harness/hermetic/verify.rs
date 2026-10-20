//! Judging hermetic runs: each scenario's run alone ([`verify`]), each
//! source's runs together ([`verify_source`]), and an install's lifecycle
//! ([`verify_lifecycle`]). A run's bytes ride the one
//! pipeline (`Drive`), and the scene a headless pixtuoid painted from the same
//! run is read as text. Ids, timestamps and model text change every run, so
//! nothing here compares bytes or goldens: only shapes, events and the fields
//! a decoder reads.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::dropped::{self, Unjudged};
use super::{
    LIFECYCLE_RECORD, MANIFEST, Manifest, Profile, SCENE_LOG, STEPS, Scenario, Shape, StepRecord,
    Surface, literals, profile, tui,
};
use crate::harness::captures::{json_lines, jsonl_files_under};
use crate::harness::provenance::{Dropped, Provenance};
use crate::harness::reads::{
    blank_of, child_pointer, embedded_json, is_hook_envelope, is_shim_stamp, string_leaves,
};
use crate::harness::{Drive, Driven, HOOK_PAYLOADS, Reach, captures};
use crate::source::registry::ChildEndInference;
use crate::{AgentEvent, AgentId};

/// The tracing target every drift breadcrumb rides (`source::drift::TARGET`).
const BREADCRUMB_TARGET: &str = crate::source::drift::TARGET;

/// A verdict's outcome, ordered by severity: a verdict is its worst finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Label {
    /// Every check held.
    Pass,
    /// A decoder dropped a type the recording never did (`dropped`): a review
    /// for a decoder arm, never a failure.
    New,
    /// An owed gap closed (`not_yet`, `not_yet_fields`, `not_yet_literals`,
    /// `result_unseen`): good news, the profile shrinks.
    Stale,
    /// The CLI could not run (install, network, a login wall): not judged.
    Blocked,
    /// The CLI asked the model something no script line answers: its request
    /// flow changed upstream.
    Script,
    /// pixtuoid read the run wrong, upstream stopped sending what it reads, or
    /// its install no longer connects and disconnects as pixtuoid drives it.
    Fail,
}

/// One verdict: its findings, worst first, and why a run could not judge new
/// types here.
#[derive(Debug)]
pub struct Verdict {
    findings: Vec<(Label, String)>,
    unjudged: Option<Unjudged>,
}

impl Verdict {
    /// A verdict of `findings`, in any order.
    pub fn new(mut findings: Vec<(Label, String)>) -> Self {
        findings.sort_by_key(|f| std::cmp::Reverse(f.0));
        Self {
            findings,
            unjudged: None,
        }
    }

    /// This verdict, of a run that reached the new-type check and could not
    /// judge it here, for `why`.
    #[must_use]
    pub fn unjudged_because(self, why: Unjudged) -> Self {
        Self {
            unjudged: Some(why),
            ..self
        }
    }

    /// Why its run reached the new-type check and could not judge it here.
    pub fn unjudged(&self) -> Option<&Unjudged> {
        self.unjudged.as_ref()
    }

    /// The worst finding's label; Pass with none.
    pub fn label(&self) -> Label {
        self.findings.first().map_or(Label::Pass, |f| f.0)
    }

    /// Every finding, worst first.
    pub fn findings(&self) -> &[(Label, String)] {
        &self.findings
    }

    /// Whether the run passed: no finding worse than [`Label::New`].
    pub fn passed(&self) -> bool {
        self.label() <= Label::New
    }

    /// Whether the run was judged: a Blocked finding returns before any check,
    /// though a Script found first may outrank it.
    pub fn judged(&self) -> bool {
        !self.findings.iter().any(|f| f.0 == Label::Blocked)
    }
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

/// Judge one scenario's run in `dir`.
///
/// # Errors
///
/// If the manifest or a capture file cannot be read, or the manifest names no
/// profiled scenario.
pub fn verify(dir: &Path) -> std::io::Result<Verdict> {
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST))?)?;
    let p = profile(&manifest.source)
        .ok_or_else(|| invalid(format!("{}: not a profiled source", manifest.source)))?;
    let scenario = p
        .scenario(&manifest.scenario)
        .ok_or_else(|| invalid(format!("{}: no scenario {}", p.source, manifest.scenario)))?;
    let scene = std::fs::read_to_string(dir.join(SCENE_LOG)).unwrap_or_default();
    let mut findings = Vec::new();
    let unjudged = judge(p, scenario, &manifest, dir, &scene, &mut findings)?;
    let verdict = Verdict::new(findings);
    Ok(match unjudged {
        Some(why) => verdict.unjudged_because(why),
        None => verdict,
    })
}

/// Judge an install lifecycle's run in `dir`: each of [`STEPS`] reported its
/// outcome, each connect left an install `sources --json` finds sound, and the
/// turn after the last uninstall delivered no hook.
///
/// # Errors
///
/// If its record is missing, a present file can't be read, or its scenario is unprofiled.
pub fn verify_lifecycle(dir: &Path) -> std::io::Result<Verdict> {
    let records: Vec<StepRecord> =
        serde_json::from_str(&std::fs::read_to_string(dir.join(LIFECYCLE_RECORD))?)?;
    let mut findings = Vec::new();
    for (i, want) in STEPS.iter().enumerate() {
        let n = i + 1;
        let Some(got) = records.get(i).filter(|r| r.step == *want) else {
            findings.push((
                Label::Fail,
                format!("step {n} ({}) left no record", want.command()),
            ));
            continue;
        };
        if got.outcome.as_deref() != Some(want.outcome()) {
            findings.push((
                Label::Fail,
                format!(
                    "{} (step {n}) reported {}, not {}: {}",
                    want.command(),
                    got.outcome.as_deref().unwrap_or("no row"),
                    want.outcome(),
                    got.message.as_deref().unwrap_or("no message")
                ),
            ));
        }
        if let Some(health) = &got.health {
            findings.push((
                Label::Fail,
                format!(
                    "{} (step {n}) left the install unsound: {health}",
                    want.command()
                ),
            ));
        }
    }
    let manifest: Manifest = match std::fs::read_to_string(dir.join(MANIFEST)) {
        Ok(text) => serde_json::from_str(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            findings.push((
                Label::Blocked,
                "the turn after the last uninstall did not run".into(),
            ));
            return Ok(Verdict::new(findings));
        }
        Err(e) => return Err(e),
    };
    let shape = profile(&manifest.source)
        .and_then(|p| p.scenario(&manifest.scenario))
        .ok_or_else(|| {
            invalid(format!(
                "{}: no scenario {}",
                manifest.source, manifest.scenario
            ))
        })?
        .shape;
    // The scenario's own turn just completed in this sandbox, so one that
    // cannot now is the uninstall's doing; with no turn, no hook proves nothing.
    if !completed_turn(&manifest, shape, &mut findings) {
        findings.push((
            Label::Fail,
            format!(
                "the turn after the last uninstall did not complete (exit {:?}, {} model \
                 requests): the uninstall left the CLI unable to run",
                manifest.capture.exit_code, manifest.requests
            ),
        ));
        return Ok(Verdict::new(findings));
    }
    let mut delivered = 0;
    for file in jsonl_files_under(dir)
        .into_iter()
        .filter(|f| is_hook_envelope(f))
    {
        delivered += json_lines(&file)?.len();
    }
    if delivered > 0 {
        findings.push((
            Label::Fail,
            format!("{delivered} hook envelopes arrived after the last uninstall"),
        ));
    }
    Ok(Verdict::new(findings))
}

/// Judge `p`'s runs together, `dirs` being all of them, for what no one
/// scenario owns: a `not_yet` hook any run fired is Stale; the field census
/// over `p`'s committed captures, where a kind or a field upstream renamed or
/// dropped fails though the decoder still parses the line; and the literal
/// census (`literal_census`) over the fixtures of the scenarios that ran,
/// where a value upstream renamed fails though every path still arrives.
///
/// # Errors
///
/// If a run's or a committed capture's file cannot be read, or the decoders
/// cannot be parsed.
pub fn verify_source(p: &Profile, dirs: &[PathBuf]) -> std::io::Result<Verdict> {
    let mut fired = BTreeSet::new();
    let mut carried = Fields::default();
    // What the runs would leave once stripped as a fixture is: the at-rest
    // census judges fixtures, so only this makes an owed literal Stale.
    let mut kept = Fields::default();
    let mut ran = Vec::new();
    for dir in dirs {
        let manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST))?)?;
        ran.extend(p.scenarios.iter().filter(|s| s.name == manifest.scenario));
        for file in jsonl_files_under(dir) {
            let mut lines = json_lines(&file)?;
            for line in &lines {
                if is_hook_envelope(&file)
                    && let Some(event) = (p.event_name)(line)
                {
                    fired.insert(event.into_owned());
                }
                carried.add(p, &file, line, true);
            }
            if crate::harness::reads::strip_lines(p.source, &file, &mut lines) {
                for line in &lines {
                    kept.add(p, &file, line, false);
                }
            }
        }
    }
    let mut findings = census(
        &committed_reads(p)?,
        &carried,
        p.not_yet,
        p.not_yet_fields,
        p.source,
    );
    for hook in p.not_yet {
        if fired.contains(*hook) {
            findings.push((
                Label::Stale,
                format!("{hook} fired: it leaves the profile's not_yet"),
            ));
        }
    }
    findings.extend(literal_census(
        p,
        &literals::population()?,
        &scenario_reads(p, ran.into_iter())?,
        &carried,
        &kept,
    ));
    Ok(Verdict::new(findings))
}

/// The literal census over runs: each value a decoder's literal matches in
/// the `committed` fixtures must arrive in some run at the same kind and
/// pointer, and each key it names there in some run at all; a
/// `not_yet_literals` entry the runs would keep once stripped (`kept`) is Stale.
fn literal_census(
    p: &Profile,
    population: &BTreeSet<literals::Literal>,
    committed: &Fields,
    carried: &Fields,
    kept: &Fields,
) -> Vec<(Label, String)> {
    let mine: Vec<&literals::Literal> = population.iter().filter(|l| l.is_of(p.source)).collect();
    let mut findings = Vec::new();
    for (kind, values) in &committed.values {
        let seen = carried.values.get(kind);
        for (at, value) in values {
            for l in mine.iter().filter(|l| l.matches(value)) {
                let arrived = seen.is_some_and(|s| s.iter().any(|(q, v)| q == at && l.matches(v)));
                if !arrived {
                    findings.push((
                        Label::Fail,
                        format!(
                            "{kind} {at}: `{}` ({}): a decoder reads it in a scenario fixture, \
                             and no run of {} carried it there",
                            l.text, l.module, p.source
                        ),
                    ));
                }
            }
        }
    }
    for l in mine.iter().filter(|l| l.how == literals::Match::Exact) {
        if committed.keys.contains(&l.text) && !carried.keys.contains(&l.text) {
            findings.push((
                Label::Fail,
                format!(
                    "`{}` ({}): a decoder reads this key in a scenario fixture, and no run of {} \
                     carried it",
                    l.text, l.module, p.source
                ),
            ));
        }
    }
    let held = kept.keys_and_values();
    for owed in p.not_yet_literals {
        if mine
            .iter()
            .any(|l| l.text == *owed && held.iter().any(|s| l.matches(s)))
        {
            findings.push((
                Label::Stale,
                format!("`{owed}` was carried: it leaves the profile's not_yet_literals"),
            ));
        }
    }
    findings
}

/// The field census proper: each kind a committed capture has must be produced
/// by some run, unless it is a hook `owed_hooks` names or a transcript kind
/// whose every read field, `/type` included, `owed_fields` names; each read field of
/// a produced kind carried, unless `owed_fields` names it; an owed field
/// carried is Stale. A committed transcript line the strip left untyped has no
/// kind to match: some run's transcript line must carry all of its read fields
/// together, as a reader that keys on those fields alone takes any line that
/// has them (a head scan's `cwd_extractor`).
fn census(
    reads: &Fields,
    carried: &Fields,
    owed_hooks: &[&str],
    owed_fields: &[(&str, &str)],
    source: &str,
) -> Vec<(Label, String)> {
    let mut findings = Vec::new();
    for (kind, paths) in &reads.by_kind {
        let Some(seen) = carried.by_kind.get(kind) else {
            let owed = match kind.strip_prefix(HOOK) {
                Some(h) => owed_hooks.contains(&h),
                None => {
                    !paths.is_empty()
                        && paths
                            .iter()
                            .all(|p| owed_fields.contains(&(kind.as_str(), p.as_str())))
                }
            };
            if !owed {
                findings.push((
                    Label::Fail,
                    format!(
                        "{kind}: a committed capture has it, and no run of {source} produced it"
                    ),
                ));
            }
            continue;
        };
        for path in paths {
            let is_owed = owed_fields.contains(&(kind.as_str(), path.as_str()));
            match (seen.contains(path), is_owed) {
                (true, true) => findings.push((
                    Label::Stale,
                    format!("{kind} {path} was carried: it leaves the profile's not_yet_fields"),
                )),
                (false, false) => findings.push((
                    Label::Fail,
                    format!(
                        "{kind} {path}: a decoder reads it in a committed capture, \
                         and no run of {source} carried it"
                    ),
                )),
                _ => {}
            }
        }
    }
    for line in &reads.lines {
        if !carried.lines.iter().any(|l| line.is_subset(l)) {
            let paths: Vec<&str> = line.iter().map(String::as_str).collect();
            findings.push((
                Label::Fail,
                format!(
                    "a committed transcript line with no type reads {}, \
                     and no run of {source} carried them in one line",
                    paths.join(" ")
                ),
            ));
        }
    }
    findings
}

/// What a set of captures holds, for the field and literal censuses.
#[derive(Debug, Default)]
pub(crate) struct Fields {
    /// Every leaf of each kind of line.
    by_kind: BTreeMap<String, BTreeSet<String>>,
    /// The leaves of each transcript line taken whole: every one of a run's,
    /// or a committed capture's untyped ones.
    lines: BTreeSet<BTreeSet<String>>,
    /// Every string leaf of each kind of line, as `(pointer, value)`; a string
    /// that holds JSON (codex's `arguments`) is read on, under its pointer.
    values: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Every key any line spells, blank or not, and each `.`-joined chain of
    /// nested ones (`location.directory`): a key the strip blanked still
    /// arrived.
    keys: BTreeSet<String>,
}

impl Fields {
    /// Add `line` of `file`; one with no value, as the strip leaves a line no
    /// decoder reads, adds only its keys. A committed (`run == false`) transcript
    /// line the strip left untyped goes to `lines` alone, since its blank
    /// `type` is no kind a run's line could have.
    fn add(&mut self, p: &Profile, file: &Path, line: &Value, run: bool) {
        key_chains(line, &mut Vec::new(), &mut self.keys);
        let stamped = is_hook_envelope(file);
        let leaves: BTreeSet<String> = leaves(line, stamped).into_iter().collect();
        if leaves.is_empty() {
            return;
        }
        let kind = kind_of(p, file, line);
        let untyped = !stamped && kind == TX;
        if run && !stamped || untyped {
            self.lines.insert(leaves.clone());
        }
        if run || !untyped {
            let values = self.values.entry(kind.clone()).or_default();
            string_leaves(line, "", values);
            self.by_kind.entry(kind).or_default().extend(leaves);
        }
    }

    /// Every key it holds, and every string value.
    pub(crate) fn keys_and_values(&self) -> BTreeSet<String> {
        let mut out = self.keys.clone();
        out.extend(self.values.values().flatten().map(|(_, v)| v.clone()));
        out
    }

    /// Whether a line of `kind` carried `path`.
    #[cfg(test)]
    pub(crate) fn has(&self, kind: &str, path: &str) -> bool {
        self.by_kind.get(kind).is_some_and(|p| p.contains(path))
    }
}

/// A transcript line's kind prefix, and the whole kind of one whose `type`
/// is blank.
const TX: &str = "tx:";
const HOOK: &str = "hook:";

/// The fields a decoder reads in `p`'s committed captures: a committed
/// recording is `reads::strip_unread`'s fixed point, so its non-blank leaves
/// are the read fields it recorded with a value, the shim's stamps aside; one
/// read but recorded blank is not seen.
/// A capture the strip skips proves nothing about reads.
pub(crate) fn committed_reads(p: &Profile) -> std::io::Result<Fields> {
    let corpus = crate::harness::sources_root();
    let mut reads = Fields::default();
    for capture in captures(&corpus) {
        if capture.source != p.source {
            continue;
        }
        if !Provenance::read(&capture.provenance_path())?.strips() {
            continue;
        }
        for file in capture.jsonl_files() {
            for line in json_lines(&file)? {
                reads.add(p, &file, &line, false);
            }
        }
    }
    Ok(reads)
}

/// A line's kind: the hook event of an envelope, the `type` of a transcript
/// line.
fn kind_of(p: &Profile, file: &Path, line: &Value) -> String {
    if is_hook_envelope(file) {
        format!("{HOOK}{}", (p.event_name)(line).as_deref().unwrap_or("?"))
    } else {
        format!("{TX}{}", line["type"].as_str().unwrap_or("?"))
    }
}

/// Every leaf of `v` holding a value (`reads::blank_of`), by its
/// pointer with `*` for an array index. A `stamped` hook envelope's shim
/// stamps are skipped, as the probe skips them (`reads::is_shim_stamp`).
fn leaves(v: &Value, stamped: bool) -> Vec<String> {
    fn walk(v: &Value, at: &str, stamped: bool, out: &mut Vec<String>) {
        if blank_of(v).is_none() {
            return;
        }
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if !(stamped && is_shim_stamp(at, k)) {
                        walk(x, &child_pointer(at, k), stamped, out);
                    }
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, &format!("{at}/*"), stamped, out);
                }
            }
            _ => out.push(at.to_owned()),
        }
    }
    let mut out = Vec::new();
    walk(v, "", stamped, &mut out);
    out
}

/// Each key of `v`, and each `.`-joined chain of nested keys ending in it,
/// `path` the keys above.
fn key_chains(v: &Value, path: &mut Vec<String>, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                path.push(k.clone());
                for i in 0..path.len() {
                    out.insert(path[i..].join("."));
                }
                key_chains(x, path, out);
                path.pop();
            }
        }
        Value::Array(a) => a.iter().for_each(|x| key_chains(x, path, out)),
        Value::String(s) => {
            if let Some(inner) = embedded_json(s) {
                key_chains(&inner, path, out);
            }
        }
        _ => {}
    }
}

/// The reads of `p`'s fixtures of `scenarios`, each a recording `--record`
/// wrote.
///
/// # Errors
///
/// If a fixture file cannot be read.
pub(crate) fn scenario_reads<'a>(
    p: &Profile,
    scenarios: impl Iterator<Item = &'a Scenario>,
) -> std::io::Result<Fields> {
    let mut reads = Fields::default();
    for s in scenarios {
        for file in jsonl_files_under(&crate::harness::sources_root().join(s.fixture)) {
            for line in json_lines(&file)? {
                reads.add(p, &file, &line, false);
            }
        }
    }
    Ok(reads)
}

/// Whether `manifest`'s CLI ran its turn of `shape` as scripted, against the
/// scripted model where the shape asks it anything, and the turn resuming it
/// where one ran; a request no script line matched is Script either way.
fn completed_turn(manifest: &Manifest, shape: Shape, findings: &mut Vec<(Label, String)>) -> bool {
    if manifest.unmatched > 0 {
        findings.push((
            Label::Script,
            format!(
                "{} model request(s) matched no script line: the request flow changed",
                manifest.unmatched
            ),
        ));
    }
    manifest.capture.exited_as_scripted(shape) && (manifest.requests > 0 || !shape.asks_the_model())
}

fn judge(
    p: &Profile,
    scenario: &Scenario,
    manifest: &Manifest,
    dir: &Path,
    scene: &str,
    findings: &mut Vec<(Label, String)>,
) -> std::io::Result<Option<Unjudged>> {
    // Before the Blocked return: a CLI the script lost may then exit non-zero.
    let completed = completed_turn(manifest, scenario.shape, findings);
    let unrefused = matches!(scenario.surface, Surface::Tui { .. })
        && manifest.capture.exit_code == Some(tui::UNREFUSED);
    if unrefused && manifest.requests > 0 && manifest.unmatched == 0 {
        findings.push((
            Label::Fail,
            "the turn raised no gate the driver knows to refuse".into(),
        ));
        return Ok(None);
    }
    // A CLI that never ran says nothing about pixtuoid, so nothing below is judged.
    if !completed {
        let resumed = manifest
            .capture
            .resumed
            .as_ref()
            .map_or(String::new(), |r| {
                format!(", resumed exit {:?}", r.exit_code)
            });
        let exit = manifest.capture.exit_code;
        let why = if scenario.shape.asks_the_model() {
            format!(
                "the CLI did not complete a turn against the scripted model (exit {exit:?}{resumed}, \
                 {} model requests): it could not run, or its model calls went elsewhere \
                 through a home the sandbox did not redirect",
                manifest.requests
            )
        } else {
            format!("the CLI did not complete its turn (exit {exit:?}{resumed}): it could not run")
        };
        findings.push((Label::Blocked, why));
        return Ok(None);
    }
    // A resumed turn that never ran had no session the first turn's wire named,
    // which is pixtuoid's to name; one that ran `completed_turn` held.
    if scenario.turn.resume.is_some() && manifest.capture.resumed.is_none() {
        findings.push((
            Label::Fail,
            "the first turn's wire named no session to resume".into(),
        ));
        return Ok(None);
    }
    if p.result_unseen && manifest.results_seen > 0 {
        findings.push((
            Label::Stale,
            "aimock saw a tool result: the profile leaves result_unseen, and its reply \
             waits on the note"
                .into(),
        ));
    }

    let payloads = hook_lines(dir)?;
    let observed: BTreeSet<String> = payloads
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| (p.event_name)(&v).map(Cow::into_owned))
        .collect();
    for want in scenario.expects {
        if !observed.contains(*want) {
            findings.push((
                Label::Fail,
                format!("the scenario must fire {want}; it did not"),
            ));
        }
    }

    let mut events: Vec<AgentEvent> = Vec::new();
    let mut reaches: Vec<(AgentId, Reach)> = Vec::new();
    let mut absorb = |what: &str, d: Driven, findings: &mut Vec<(Label, String)>| {
        for f in d.decode_errors.iter().chain(&d.panics) {
            findings.push((Label::Fail, format!("{what}: {f}")));
        }
        if d.unparseable > 0 {
            findings.push((
                Label::Fail,
                format!("{what}: {} line(s) not JSON", d.unparseable),
            ));
        }
        reaches.extend(d.reaches.iter().copied());
        events.extend(d.events.into_iter().skip(d.seed_events));
    };
    if !payloads.is_empty() {
        absorb(HOOK_PAYLOADS, Drive::hooks().lines(&payloads), findings);
    }
    for t in jsonl_files_under(dir)
        .into_iter()
        .filter(|f| !is_hook_envelope(f))
    {
        let Some(drive) = Drive::transcript_at(p.source, &t) else {
            continue;
        };
        let text = std::fs::read_to_string(&t)?;
        absorb(
            &t.display().to_string(),
            drive.seeded().lines(text.lines()),
            findings,
        );
    }

    // A daemon renders presence, not a desk sprite, so its turn is judged there.
    if p.descriptor().is_daemon() {
        match settled(scenario.shape) {
            Some(settled) => judge_presence(p, scene, settled, findings),
            None => findings.push((
                Label::Fail,
                format!("presence cannot show a {} turn", scenario.name),
            )),
        }
        return Ok(None);
    }
    let sessions = match scenario.shape {
        Shape::ToolRun | Shape::Permission | Shape::ModelError => 1,
        Shape::Subagent | Shape::ChildError | Shape::Succession => 2,
    };
    started_in(
        &events,
        &manifest.capture.workspace,
        sessions,
        p.children_inherit_cwd,
        findings,
    );
    let reached: Vec<Reach> = reaches.iter().map(|&(_, class)| class).collect();
    match scenario.shape {
        Shape::ToolRun => tool_run(&events, &reached, findings),
        Shape::Permission => permission(&events, &reached, findings),
        Shape::Subagent => subagent(
            &events,
            &reaches,
            p.descriptor().caps().child_end_inference,
            findings,
        ),
        Shape::ModelError => model_error(&events, findings),
        Shape::ChildError => child_error(
            &events,
            &reaches,
            p.descriptor().caps().child_end_inference,
            findings,
        ),
        Shape::Succession => succession(&events, findings),
    }
    let shown = match scenario.shape {
        Shape::ToolRun | Shape::Subagent | Shape::ChildError => "active",
        Shape::Permission => "waiting",
        Shape::ModelError | Shape::Succession => "idle",
    };
    judge_scene(p, scene, shown, findings);
    new_types(p, dir, dropped::baseline_of(scenario)?.as_ref(), findings)
}

/// Each type a run in `dir` left unread that `baseline`, its recording's,
/// never did: New. With no baseline to judge by here, why not.
fn new_types(
    p: &Profile,
    dir: &Path,
    baseline: Option<&Dropped>,
    findings: &mut Vec<(Label, String)>,
) -> std::io::Result<Option<Unjudged>> {
    let b = match baseline {
        None => return Ok(Some(Unjudged::NoBaseline)),
        Some(b) if !b.judges_here() => return Ok(Some(Unjudged::RecordedOn(b.os.to_string()))),
        Some(b) => b,
    };
    for (at, value) in b.new_in(&dropped::pairs(p.source, &jsonl_files_under(dir))?) {
        findings.push((
            Label::New,
            format!("{at}: `{value}` went unread, as it never did in the recording"),
        ));
    }
    Ok(None)
}

fn started_in(
    events: &[AgentEvent],
    workspace: &Path,
    sessions: usize,
    children_inherit_cwd: bool,
    findings: &mut Vec<(Label, String)>,
) {
    let ids: BTreeSet<_> = events.iter().map(AgentEvent::agent_id).collect();
    if ids.len() != sessions {
        findings.push((
            Label::Fail,
            format!(
                "{sessions} session(s) must decode to {sessions} agent(s); got {}",
                ids.len()
            ),
        ));
    }
    let want = canonical(workspace);
    // Every cwd an event keyed on `agent` names, wherever it sits in the run.
    let named = |agent: &AgentId| -> Vec<PathBuf> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::SessionStart { agent_id, cwd, .. } if agent_id == agent => {
                    Some(cwd.as_path())
                }
                AgentEvent::Identity {
                    agent_id,
                    cwd: Some(cwd),
                    ..
                } if agent_id == agent => Some(cwd.as_path()),
                _ => None,
            })
            .filter(|c| !c.as_os_str().is_empty())
            .map(canonical)
            .collect()
    };
    let mut cwds: Vec<PathBuf> = Vec::new();
    let mut started = false;
    for e in events {
        let AgentEvent::SessionStart {
            agent_id,
            cwd,
            parent_id,
            ..
        } = e
        else {
            continue;
        };
        started = true;
        if parent_id.is_none() || !cwd.as_os_str().is_empty() {
            cwds.push(canonical(cwd));
            continue;
        }
        // A child's registration may carry no cwd when another event keyed on
        // it names one (grok's spawn line, whose `subagent_start` does).
        let own = named(agent_id);
        if own.is_empty() && !children_inherit_cwd {
            findings.push((Label::Fail, "a child's cwd was never named".into()));
        }
        cwds.extend(own);
    }
    if !started {
        findings.push((Label::Fail, "no SessionStart decoded".into()));
    } else if let Some(bad) = cwds.iter().find(|c| **c != want) {
        findings.push((
            Label::Fail,
            format!(
                "cwd {} is not the workspace {}",
                bad.display(),
                want.display()
            ),
        ));
    }
}

fn tool_run(events: &[AgentEvent], reached: &[Reach], findings: &mut Vec<(Label, String)>) {
    let named = events.iter().any(|e| {
        matches!(e, AgentEvent::ActivityStart { detail: Some(d), .. }
            if !d.display().trim().is_empty() && d.display() != "?")
    });
    if !named {
        findings.push((
            Label::Fail,
            "no tool start decoded with a detail naming the tool".into(),
        ));
    }
    if !reached.contains(&Reach::Active) {
        findings.push((Label::Fail, "the slot never went Active".into()));
    }
    if !events
        .iter()
        .any(|e| matches!(e, AgentEvent::ActivityEnd { .. }))
    {
        findings.push((Label::Fail, "the tool never ended".into()));
    }
}

fn permission(events: &[AgentEvent], reached: &[Reach], findings: &mut Vec<(Label, String)>) {
    if !reached.contains(&Reach::Waiting) {
        findings.push((Label::Fail, "the slot never went Waiting".into()));
    }
    // In any order: a hook payload's arrival order is its shim process's, not
    // the CLI's, so the wait, the decision and the result can land in any order.
    let calls = |pick: fn(&AgentEvent) -> Option<&String>| -> BTreeSet<&String> {
        events.iter().filter_map(pick).collect()
    };
    let waited = calls(|e| match e {
        AgentEvent::Waiting {
            tool_use_id: Some(t),
            ..
        } => Some(t),
        _ => None,
    });
    let ended_calls = calls(|e| match e {
        AgentEvent::ActivityEnd {
            tool_use_id: Some(t),
            ..
        } => Some(t),
        _ => None,
    });
    let started = calls(|e| match e {
        AgentEvent::ActivityStart {
            tool_use_id: Some(t),
            ..
        } => Some(t),
        _ => None,
    });
    let ended = if !waited.is_empty() {
        waited.iter().any(|t| ended_calls.contains(t))
    } else if !events
        .iter()
        .any(|e| matches!(e, AgentEvent::ActivityStart { .. }))
    {
        // Asked before the call started (reasonix): the refused call never ran,
        // so its turn's end.
        events.iter().any(|e| {
            matches!(
                e,
                AgentEvent::ActivityEnd {
                    tool_use_id: None,
                    ..
                }
            )
        })
    } else {
        // A wait that names no call (claude-code's `PermissionRequest`): every
        // started tool ends.
        !started.is_empty() && started.is_subset(&ended_calls)
    };
    if !ended {
        findings.push((
            Label::Fail,
            "the call or turn the slot waited on never ended".into(),
        ));
    }
}

/// The failed turn ends. Its script answers the turn only with an error
/// (`a_model_error_script_errs_and_calls_no_tool`), so a run that reached the
/// model failed.
fn model_error(events: &[AgentEvent], findings: &mut Vec<(Label, String)>) {
    if !events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ActivityEnd {
                tool_use_id: None,
                ..
            }
        )
    }) {
        findings.push((Label::Fail, "the failed turn never ended".into()));
    }
}

/// Each (child, parent) of a delegating run, once what every delegation shows
/// is judged: the child registered under its parent and ended, and the parent
/// went Delegating.
fn delegated(
    events: &[AgentEvent],
    reaches: &[(AgentId, Reach)],
    inference: ChildEndInference,
    findings: &mut Vec<(Label, String)>,
) -> BTreeSet<(AgentId, AgentId)> {
    let ids: BTreeSet<_> = events.iter().map(AgentEvent::agent_id).collect();
    let children: BTreeSet<(AgentId, AgentId)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStart {
                agent_id,
                parent_id: Some(parent),
                ..
            } if parent != agent_id && ids.contains(parent) => Some((*agent_id, *parent)),
            _ => None,
        })
        .collect();
    let tasks: BTreeSet<(AgentId, &str)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ActivityStart {
                agent_id,
                tool_use_id: Some(t),
                detail: Some(d),
                ..
            } if d.is_task() => Some((*agent_id, t.as_str())),
            _ => None,
        })
        .collect();
    let durable = match inference {
        ChildEndInference::Durable => true,
        ChildEndInference::None | ChildEndInference::TaskDrain | ChildEndInference::AnyTaskEnd => {
            false
        }
    };
    // The reducer's untracked site (`apply_activity_end`): an id-less end while
    // an id-less Task stands.
    let mut standing = BTreeSet::new();
    let mut untracked_ends = BTreeSet::new();
    for e in events {
        match e {
            AgentEvent::ActivityStart {
                agent_id,
                tool_use_id: None,
                detail: Some(d),
            } if d.is_task() => {
                standing.insert(*agent_id);
            }
            AgentEvent::ActivityStart { agent_id, .. } => {
                standing.remove(agent_id);
            }
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id,
            } => {
                if tool_use_id.is_none() && standing.remove(agent_id) {
                    untracked_ends.insert(*agent_id);
                }
                standing.remove(agent_id);
            }
            _ => {}
        }
    }
    let ended = |child: AgentId, parent: AgentId| {
        (inference.dispatch_end_ends_child(false) && untracked_ends.contains(&parent))
            || events.iter().any(|e| match e {
                AgentEvent::SessionEnd { agent_id, .. } => *agent_id == child,
                // The reducer's b1 cascade walks a drained Task's subtree out.
                AgentEvent::ActivityEnd {
                    agent_id,
                    tool_use_id: Some(t),
                } if inference.dispatch_end_ends_child(true) => {
                    *agent_id == parent && tasks.contains(&(parent, t.as_str()))
                }
                // No run deletes a durable child, so its turn's end is the end a run sees.
                AgentEvent::ActivityEnd {
                    agent_id,
                    tool_use_id: None,
                } if durable => *agent_id == child,
                _ => false,
            })
    };
    if children.is_empty() {
        findings.push((Label::Fail, "no session started as another's child".into()));
    } else if !children.iter().all(|&(child, parent)| ended(child, parent)) {
        findings.push((Label::Fail, "the child session never ended".into()));
    }
    if !children
        .iter()
        .all(|&(_, parent)| reaches.contains(&(parent, Reach::Delegating)))
    {
        findings.push((Label::Fail, "the parent never went Delegating".into()));
    }
    children
}

fn subagent(
    events: &[AgentEvent],
    reaches: &[(AgentId, Reach)],
    inference: ChildEndInference,
    findings: &mut Vec<(Label, String)>,
) {
    let children = delegated(events, reaches, inference, findings);
    // A call shown on both is the child's work on the parent's sprite (#1701).
    let calls = |id: AgentId| -> BTreeSet<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ActivityStart {
                    agent_id,
                    tool_use_id: Some(t),
                    ..
                } if *agent_id == id => Some(t.as_str()),
                _ => None,
            })
            .collect()
    };
    if children
        .iter()
        .any(|&(child, parent)| !calls(child).is_disjoint(&calls(parent)))
    {
        findings.push((
            Label::Fail,
            "the parent showed a tool of the child's".into(),
        ));
    }
    let works = |id: AgentId| {
        events.iter().any(|e| {
            matches!(e, AgentEvent::ActivityStart { agent_id, detail, .. }
                if *agent_id == id && !detail.as_ref().is_some_and(|d| d.is_task()))
        })
    };
    if !children.iter().all(|&(child, _)| works(child)) {
        findings.push((
            Label::Fail,
            "a child's tool never landed on the child".into(),
        ));
    }
    // Per slot: a parent's own tool going Active proves nothing of its child.
    if !children
        .iter()
        .all(|&(child, _)| reaches.contains(&(child, Reach::Active)))
    {
        findings.push((Label::Fail, "the child's tool never went Active".into()));
    }
}

/// The child's failure is the script's to show: the parent's reply matches
/// only its error (`a_child_error_script_errs_the_childs_request_and_answers_only_its_error`).
fn child_error(
    events: &[AgentEvent],
    reaches: &[(AgentId, Reach)],
    inference: ChildEndInference,
    findings: &mut Vec<(Label, String)>,
) {
    // Its turn's end, or its session's where the end of a headless run is
    // the session's own (copilot's plain `-p` writes no `session.task_complete`).
    let ended = |parent: AgentId| {
        events.iter().any(|e| match e {
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: None,
            }
            | AgentEvent::SessionEnd { agent_id, .. } => *agent_id == parent,
            _ => false,
        })
    };
    if !delegated(events, reaches, inference, findings)
        .iter()
        .all(|&(_, parent)| ended(parent))
    {
        findings.push((
            Label::Fail,
            "the parent's turn or session never ended".into(),
        ));
    }
}

/// The first session to start ends, and another starts as a root; one process
/// runs the scenario, so its pid is not judged.
fn succession(events: &[AgentEvent], findings: &mut Vec<(Label, String)>) {
    let mut starts = events.iter().filter_map(|e| match e {
        AgentEvent::SessionStart {
            agent_id,
            parent_id,
            ..
        } => Some((*agent_id, *parent_id)),
        _ => None,
    });
    // No start at all is `started_in`'s finding.
    let Some((first, _)) = starts.next() else {
        return;
    };
    match starts.find(|(id, _)| *id != first) {
        None => findings.push((Label::Fail, "no session succeeded the first".into())),
        Some((_, Some(_))) => {
            findings.push((Label::Fail, "the successor started as a child".into()))
        }
        Some((_, None)) => {}
    }
    if !events
        .iter()
        .any(|e| matches!(e, AgentEvent::SessionEnd { agent_id, .. } if *agent_id == first))
    {
        findings.push((
            Label::Fail,
            "the session switched away from never ended".into(),
        ));
    }
}

/// The scene a headless pixtuoid painted, and every drift breadcrumb it logged.
/// The agent must be seen in the `shown` state its shape is about, and every
/// agent, a child under its own label included, gone once the CLI has exited:
/// every profiled CLI's clean exit ends its sprites, whether by an end event,
/// a drain or its process's death, so one left standing is a session pixtuoid
/// lost track of.
fn judge_scene(p: &Profile, scene: &str, shown: &str, findings: &mut Vec<(Label, String)>) {
    let prefix = format!("{}\u{b7}", p.descriptor().label_prefix);
    let frames = rosters(scene, "agents");
    if !frames.iter().any(|f| holds(f, &prefix, shown)) {
        findings.push((
            Label::Fail,
            format!("the scene never showed a {prefix} agent {shown}"),
        ));
    }
    if frames.last().is_some_and(|last| !last.starts_with(']')) {
        findings.push((
            Label::Fail,
            "an agent was still in the scene after the CLI exited".into(),
        ));
    }
    breadcrumbs(scene, findings);
}

/// The presence a daemon's turn of `shape` settles to, where presence can show
/// one.
pub(super) fn settled(shape: Shape) -> Option<&'static str> {
    match shape {
        Shape::ToolRun => Some("idle"),
        // `RunFailed` latches it (`crate::source::daemon`).
        Shape::ModelError => Some("degraded"),
        Shape::Permission | Shape::Subagent | Shape::ChildError | Shape::Succession => None,
    }
}

/// A daemon's turn on the scene: its mascot busy, then `settled` once the turn
/// ended, and down or gone once the daemon stopped.
fn judge_presence(p: &Profile, scene: &str, settled: &str, findings: &mut Vec<(Label, String)>) {
    let mascot = format!("{}@", p.source);
    let frames = rosters(scene, "daemons");
    match frames.iter().position(|f| holds(f, &mascot, "busy")) {
        None => findings.push((
            Label::Fail,
            format!("the scene never showed a {mascot} daemon busy"),
        )),
        Some(at) if !frames[at..].iter().any(|f| holds(f, &mascot, settled)) => findings.push((
            Label::Fail,
            format!("the {mascot} daemon never settled {settled} after its turn"),
        )),
        Some(_) => {}
    }
    let up = |last: &&str| last.contains(&mascot) && !holds(last, &mascot, "down");
    if frames.last().is_some_and(up) {
        findings.push((
            Label::Fail,
            "a daemon was still up in the scene after it stopped".into(),
        ));
    }
    breadcrumbs(scene, findings);
}

/// Each line's `key=[…]` roster, in the scene's order.
fn rosters<'a>(scene: &'a str, key: &str) -> Vec<&'a str> {
    let key = format!("{key}=[");
    scene
        .lines()
        .filter_map(|l| l.split_once(key.as_str()).map(|(_, roster)| roster))
        .collect()
}

/// Whether `roster` holds an entry of `who`'s in `state`.
fn holds(roster: &str, who: &str, state: &str) -> bool {
    let state = format!(":{state}");
    roster
        .split(", ")
        .any(|e| e.starts_with(who) && e.contains(&state))
}

/// Each drift breadcrumb pixtuoid logged while rendering the run.
fn breadcrumbs(scene: &str, findings: &mut Vec<(Label, String)>) {
    for line in scene.lines().filter(|l| l.contains(BREADCRUMB_TARGET)) {
        findings.push((Label::Fail, format!("drift breadcrumb: {}", line.trim())));
    }
}

fn hook_lines(dir: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read_to_string(dir.join(HOOK_PAYLOADS)) {
        Ok(text) => Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// macOS's `/tmp` is `/private/tmp`, and a CLI may report either: omp drops
/// the `/private` (`packages/utils/src/dirs.ts:144-155`, `:228` at
/// `v18.12.1`). A recording is judged on hosts where neither spelling exists,
/// so the alias folds by name, not through the filesystem.
fn canonical(p: &Path) -> PathBuf {
    const PRIVATE_TMP: &str = "/private/tmp";
    const TMP: &str = "/tmp";
    let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match real.strip_prefix(PRIVATE_TMP) {
        Ok(rest) => Path::new(TMP).join(rest),
        Err(_) => real,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::CaptureRecord;

    const RECORDED_WORKSPACE: &str = "/private/tmp/pixtuoid-capture/proj";

    /// A run directory built from a committed recording of `source`'s tool run,
    /// judged as if the tier had just produced it.
    fn run_from(source: &str, scenario_dir: &str, scene: &str) -> tempfile::TempDir {
        let src = crate::harness::sources_root()
            .join("fixtures")
            .join(source)
            .join(scenario_dir);
        run_of(&src, source, "tool-run", scene)
    }

    /// A run directory built from the recording `source`'s scenario `name`
    /// writes, judged as that scenario's run.
    fn recorded(source: &str, name: &str, scene: &str) -> tempfile::TempDir {
        let scenario = profile(source)
            .and_then(|p| p.scenario(name))
            .expect("a profiled scenario");
        let src = crate::harness::sources_root().join(scenario.fixture);
        let d = run_of(&src, source, name, scene);
        if scenario.turn.resume.is_some() {
            write_manifest(d.path(), source, |m| {
                m.scenario = name.into();
                m.capture.resumed = Some(crate::harness::Resumed { exit_code: Some(0) });
            });
        }
        d
    }

    fn run_of(src: &Path, source: &str, scenario: &str, scene: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        for f in jsonl_files_under(src) {
            let to = d.path().join(f.strip_prefix(src).expect("under"));
            std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
            std::fs::copy(&f, &to).expect("copy");
        }
        write_manifest(d.path(), source, |m| m.scenario = scenario.into());
        std::fs::write(d.path().join(SCENE_LOG), scene).expect("scene");
        d
    }

    fn write_manifest(dir: &Path, source: &str, edit: impl FnOnce(&mut Manifest)) {
        std::fs::write(
            dir.join(MANIFEST),
            serde_json::to_string(&manifest(source, edit)).expect("json"),
        )
        .expect("write");
    }

    fn manifest(source: &str, edit: impl FnOnce(&mut Manifest)) -> Manifest {
        let mut m = Manifest {
            source: source.into(),
            scenario: "tool-run".into(),
            capture: CaptureRecord {
                workspace: PathBuf::from(RECORDED_WORKSPACE),
                cli: "test".into(),
                version: "test".into(),
                exit_code: Some(0),
                resumed: None,
            },
            aimock: "test".into(),
            requests: 2,
            unmatched: 0,
            results_seen: 1,
        };
        edit(&mut m);
        m
    }

    /// Rewrite the hook lines through `f`, which may reorder them.
    fn reorder_hooks(dir: &Path, f: impl FnOnce(&mut Vec<Value>)) {
        let path = dir.join(HOOK_PAYLOADS);
        let mut lines: Vec<Value> = std::fs::read_to_string(&path)
            .expect("read")
            .lines()
            .map(|l| serde_json::from_str(l).expect("json"))
            .collect();
        f(&mut lines);
        let out: Vec<String> = lines.iter().map(Value::to_string).collect();
        std::fs::write(&path, out.join("\n") + "\n").expect("write");
    }

    /// Rewrite every hook line; `None` drops it.
    fn edit_hooks(dir: &Path, f: impl FnMut(Value) -> Option<Value>) {
        reorder_hooks(dir, |lines| {
            *lines = std::mem::take(lines).into_iter().filter_map(f).collect();
        });
    }

    /// Keep the transcript lines `keep` admits.
    fn edit_transcripts(dir: &Path, mut keep: impl FnMut(&str) -> bool) {
        for t in jsonl_files_under(dir)
            .into_iter()
            .filter(|f| !is_hook_envelope(f))
        {
            let text = std::fs::read_to_string(&t).expect("read");
            let kept: String = text
                .lines()
                .filter(|l| keep(l))
                .map(|l| format!("{l}\n"))
                .collect();
            std::fs::write(&t, kept).expect("write");
        }
    }

    fn blank_transcripts(dir: &Path) {
        edit_transcripts(dir, |_| false);
    }

    /// A lifecycle run in a fresh directory: `records` as its steps reported,
    /// its silent turn's manifest, and `envelopes` hook lines.
    fn lifecycle_run(records: &[StepRecord], envelopes: usize) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            d.path().join(LIFECYCLE_RECORD),
            serde_json::to_string(records).expect("records"),
        )
        .expect("record");
        write_manifest(d.path(), "codex", |_| {});
        std::fs::write(
            d.path().join(HOOK_PAYLOADS),
            "{\"hook_event_name\":\"Stop\"}\n".repeat(envelopes),
        )
        .expect("hooks");
        d
    }

    fn sound() -> Vec<StepRecord> {
        STEPS
            .iter()
            .map(|s| StepRecord {
                step: *s,
                outcome: Some(s.outcome().into()),
                message: None,
                health: None,
            })
            .collect()
    }

    #[test]
    fn a_sound_lifecycle_passes() {
        let d = lifecycle_run(&sound(), 0);
        let v = verify_lifecycle(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_hook_after_the_last_uninstall_fails() {
        let d = lifecycle_run(&sound(), 2);
        let v = verify_lifecycle(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "2 hook envelopes arrived"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_failed_step_or_an_unsound_install_fails() {
        let mut records = sound();
        records[1].outcome = Some("failed".into());
        records[1].message = Some("codex plugin remove: unknown command".into());
        records[2].health = Some("install broken: the plugin is not installed".into());
        let d = lifecycle_run(&records, 0);
        let v = verify_lifecycle(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "disconnect (step 2) reported failed"),
            "{:?}",
            v.findings()
        );
        assert!(
            has(&v, Label::Fail, "connect (step 3) left the install unsound"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_turn_that_never_ran_or_could_not_complete_proves_no_silence() {
        let d = lifecycle_run(&sound(), 0);
        std::fs::remove_file(d.path().join(MANIFEST)).expect("rm");
        let v = verify_lifecycle(d.path()).expect("verify");
        assert!(has(&v, Label::Blocked, "did not run"), "{:?}", v.findings());
        let d = lifecycle_run(&sound(), 0);
        write_manifest(d.path(), "codex", |m| m.capture.exit_code = Some(1));
        let v = verify_lifecycle(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "did not complete"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_step_that_left_no_record_fails() {
        let d = lifecycle_run(&sound()[..2], 0);
        let v = verify_lifecycle(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "step 3 (connect) left no record"),
            "{:?}",
            v.findings()
        );
    }

    /// Each step's outcome is a token the published `--json` contract lists,
    /// the copy of it this side of the crate boundary.
    #[test]
    fn every_step_outcome_is_a_contract_token() {
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../integrations/raycast/contract/outcome-row.schema.json"),
            )
            .expect("the contract reads"),
        )
        .expect("the contract parses");
        let tokens = &schema["$defs"]["WireOutcome"]["enum"];
        for s in STEPS {
            assert!(
                tokens
                    .as_array()
                    .is_some_and(|t| t.iter().any(|v| v == s.outcome())),
                "{} is no contract token: {tokens}",
                s.outcome()
            );
        }
    }

    fn has(v: &Verdict, label: Label, text: &str) -> bool {
        v.findings()
            .iter()
            .any(|f| f.0 == label && f.1.contains(text))
    }

    const CX_SCENE: &str = "agents=[cx\u{b7}proj@0:active(Bash: cat NOTE.txt)]\n\
                            agents=[cx\u{b7}proj@0:idle]\nagents=[] daemons=[]\n";
    const CX_DELEGATE_SCENE: &str = "agents=[cx\u{b7}proj@0:active(Delegating), \
                                     cx\u{b7}proj@1:active(Bash: cat NOTE.txt)]\n\
                                     agents=[] daemons=[]\n";
    const OC_DELEGATE_SCENE: &str = "agents=[oc\u{b7}proj@0:active(Delegating), \
                                     oc\u{b7}proj@1:active(read)]\n\
                                     agents=[] daemons=[]\n";
    const CC_SCENE: &str = "agents=[cc\u{b7}proj@0:active(Read: NOTE.txt)]\nagents=[] daemons=[]\n";
    const CC_WAIT_SCENE: &str =
        "agents=[cc\u{b7}proj@0:waiting(permission)]\nagents=[] daemons=[]\n";
    const CX_WAIT_SCENE: &str =
        "agents=[cx\u{b7}proj@0:waiting(permission)]\nagents=[] daemons=[]\n";
    const RX_WAIT_SCENE: &str = "agents=[rx\u{b7}proj@0:waiting(approval needed: bash rm NOTE.txt)]\n\
                                 agents=[] daemons=[]\n";

    const DS_WAIT_SCENE: &str =
        "agents=[ds\u{b7}proj@0:waiting(bash: escalate)]\nagents=[] daemons=[]\n";
    const DS_DELEGATE_SCENE: &str = "agents=[ds\u{b7}proj@0:active(Delegating), \
                                     ds\u{b7}proj@1:active(read)]\nagents=[] daemons=[]\n";
    const CP_SCENE: &str = "agents=[cp\u{b7}proj@0:active(view: NOTE.txt)]\nagents=[] daemons=[]\n";
    const CP_AUTOPILOT_SCENE: &str =
        "agents=[cp\u{b7}proj@0:active(task_complete)]\nagents=[] daemons=[]\n";
    const CP_WAIT_SCENE: &str =
        "agents=[cp\u{b7}proj@0:waiting(permission: shell)]\nagents=[] daemons=[]\n";
    const CP_DELEGATE_SCENE: &str = "agents=[cp\u{b7}proj@0:active(Delegating), \
                                     reader@1:active(view: NOTE.txt)]\nagents=[] daemons=[]\n";
    const CP_CHILD_ERROR_SCENE: &str = "agents=[cp\u{b7}proj@0:active(Delegating), \
                                        reader@1:idle]\nagents=[] daemons=[]\n";
    const GK_SCENE: &str = "agents=[gk\u{b7}proj@0:active(read_file)]\nagents=[] daemons=[]\n";
    const AG_SCENE: &str =
        "agents=[ag\u{b7}proj@0:active(view_file: NOTE.txt)]\nagents=[] daemons=[]\n";
    const GK_DELEGATE_SCENE: &str = "agents=[gk\u{b7}proj@0:active(Delegating), \
                                     Read the note@1:active(read_file)]\nagents=[] daemons=[]\n";
    const RX_DELEGATE_SCENE: &str = "agents=[rx\u{b7}proj@0:active(Delegating), \
                                     rx\u{b7}proj@1:active(read_file)]\nagents=[] daemons=[]\n";
    const OM_NEW_SCENE: &str = "agents=[om\u{b7}proj@0:idle]\n\
                                agents=[om\u{b7}proj@0:idle, om\u{b7}proj@1:idle]\n\
                                agents=[om\u{b7}proj@1:idle]\nagents=[] daemons=[]\n";

    /// omp's recorded `/new`, which asked the model nothing.
    fn recorded_succession(scene: &str) -> tempfile::TempDir {
        let d = recorded("omp", "new-session", scene);
        write_manifest(d.path(), "omp", |m| {
            m.scenario = "new-session".into();
            m.requests = 0;
            m.results_seen = 0;
        });
        d
    }

    #[test]
    fn a_recorded_omp_succession_passes() {
        let d = recorded_succession(OM_NEW_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_succession_that_could_not_run_is_blocked_without_blaming_the_model() {
        let d = recorded_succession(OM_NEW_SCENE);
        write_manifest(d.path(), "omp", |m| {
            m.scenario = "new-session".into();
            m.requests = 0;
            m.capture.exit_code = Some(1);
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(
                &v,
                Label::Blocked,
                "did not complete its turn (exit Some(1))"
            ),
            "{:?}",
            v.findings()
        );
        assert!(
            !v.findings().iter().any(|f| f.1.contains("model")),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_recorded_succession_cut_before_its_switch_fails() {
        // Killed before the command ran, or quit before it: one session, its
        // successor's transcript never written.
        for quit_first in [false, true] {
            let d = recorded_succession("agents=[om\u{b7}proj@0:idle]\nagents=[] daemons=[]\n");
            let mut first = None;
            edit_hooks(d.path(), |mut v| match v["type"].as_str() {
                Some("session_start") => {
                    first = Some(v["sessionFile"].clone());
                    Some(v)
                }
                Some("session_shutdown") if quit_first => {
                    v["sessionFile"] = first.clone().expect("started first");
                    Some(v)
                }
                _ => None,
            });
            blank_transcripts(d.path());
            let v = verify(d.path()).expect("verify");
            assert!(
                has(&v, Label::Fail, "no session succeeded"),
                "{quit_first}: {:?}",
                v.findings()
            );
        }
    }

    #[test]
    fn a_recorded_dsh_permission_run_passes() {
        let d = recorded("dsh", "permission", DS_WAIT_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_permission_run_that_never_waits_fails() {
        let d = recorded("dsh", "permission", DS_WAIT_SCENE);
        edit_hooks(d.path(), |v| (v["type"] != "approval_asked").then_some(v));
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "never went Waiting"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_permission_scene_that_never_shows_the_wait_fails() {
        let d = recorded(
            "dsh",
            "permission",
            "agents=[ds\u{b7}proj@0:active(bash)]\n",
        );
        let v = verify(d.path()).expect("verify");
        assert!(has(&v, Label::Fail, "agent waiting"), "{:?}", v.findings());
    }

    #[test]
    fn a_recorded_codex_tui_permission_run_passes() {
        let d = recorded("codex", "permission", CX_WAIT_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_refused_call_that_never_started_ends_with_its_turn() {
        let d = recorded("reasonix", "permission", RX_WAIT_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        edit_hooks(d.path(), |v| (v["event"] != "Stop").then_some(v));
        let v = verify(d.path()).expect("verify");
        assert!(has(&v, Label::Fail, "never ended"), "{:?}", v.findings());
        let d = recorded("reasonix", "permission", RX_WAIT_SCENE);
        reorder_hooks(d.path(), |lines| {
            let at = lines
                .iter()
                .position(|v| v["event"] == "PermissionRequest")
                .expect("the gate's request");
            let mut started = lines[at].clone();
            started["event"] = "PreToolUse".into();
            lines.insert(at, started);
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "never ended"),
            "a call that started is held to its own end: {:?}",
            v.findings()
        );
    }

    #[test]
    fn a_recorded_claude_code_tui_permission_run_passes() {
        let d = recorded("claude-code", "permission", CC_WAIT_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_wait_naming_no_call_fails_when_a_started_tool_never_ends() {
        let d = recorded("claude-code", "permission", CC_WAIT_SCENE);
        edit_transcripts(d.path(), |line| !line.contains("tool_result"));
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "the slot waited on never ended"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_wait_naming_no_call_fails_when_no_tool_started() {
        let d = recorded("claude-code", "permission", CC_WAIT_SCENE);
        edit_hooks(d.path(), |v| {
            (v["hook_event_name"] != "PreToolUse").then_some(v)
        });
        edit_transcripts(d.path(), |line| !line.contains("tool_use"));
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "the slot waited on never ended"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_wait_whose_tool_never_ends_fails() {
        let d = recorded("dsh", "permission", DS_WAIT_SCENE);
        edit_hooks(d.path(), |v| {
            (v["type"] != "tool_result" && v["type"] != "approval_decided").then_some(v)
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "the slot waited on never ended"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_wait_that_lands_after_every_end_of_its_tool_still_passes() {
        let d = recorded("dsh", "permission", DS_WAIT_SCENE);
        reorder_hooks(d.path(), |lines| {
            let asked = lines
                .iter()
                .position(|v| v["type"] == "approval_asked")
                .expect("recorded");
            let wait = lines.remove(asked);
            let end = lines
                .iter()
                .position(|v| v["type"] == "session_end")
                .expect("recorded");
            lines.insert(end, wait);
        });
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_wait_ended_only_by_another_tool_fails() {
        let d = recorded("dsh", "permission", DS_WAIT_SCENE);
        edit_hooks(d.path(), |mut v| {
            if v["type"] == "tool_result" {
                v["callId"] = "another-call".into();
            }
            (v["type"] != "approval_decided").then_some(v)
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "the slot waited on never ended"),
            "{:?}",
            v.findings()
        );
    }

    const CC_DELEGATE_SCENE: &str = "agents=[cc\u{b7}proj@0:active(Delegating), \
        general-purpose@1:active(Read: NOTE.txt)]\nagents=[] daemons=[]\n";

    /// The recorded CC delegation keys the child's tool hooks by `agent_id`;
    /// without it they land on the root while the child's transcript shows
    /// the same call (#1701), and with the transcript gone too the child shows
    /// no tool of its own.
    #[test]
    fn a_childs_tool_on_its_parent_or_on_neither_fails() {
        let clean = recorded("claude-code", "subagent", CC_DELEGATE_SCENE);
        let v = verify(clean.path()).expect("verify");
        assert!(v.findings().is_empty(), "{:?}", v.findings());
        // As a decoder that ignores `agent_id` keys them: on the root's session.
        let mut root = Value::Null;
        let unkeyed = |mut v: Value| {
            if v["hook_event_name"] == "SessionStart" {
                root = v["session_id"].clone();
            } else if v["hook_event_name"] != "SubagentStart"
                && v["hook_event_name"] != "SubagentStop"
            {
                let o = v.as_object_mut().expect("a payload");
                if o.remove("agent_id").is_some() {
                    o.insert("session_id".into(), root.clone());
                }
            }
            Some(v)
        };
        let d = recorded("claude-code", "subagent", CC_DELEGATE_SCENE);
        edit_hooks(d.path(), unkeyed);
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "parent showed a tool of the child's"),
            "{:?}",
            v.findings()
        );
        for t in jsonl_files_under(d.path()) {
            if t.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("agent-"))
            {
                std::fs::write(&t, "").expect("blank the child's transcript");
            }
        }
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "child's tool never landed on the child"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_parent_that_never_delegates_or_a_child_that_never_works_fails() {
        let delegation = |v: &Value| v["type"] == "tool_call" && v["toolName"] == "subagent";
        let d = recorded("dsh", "subagent", DS_DELEGATE_SCENE);
        edit_hooks(d.path(), |v| (!delegation(&v)).then_some(v));
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "never went Delegating"),
            "{:?}",
            v.findings()
        );
        let d = recorded("dsh", "subagent", DS_DELEGATE_SCENE);
        edit_hooks(d.path(), |v| {
            (v["type"] != "tool_call" || delegation(&v)).then_some(v)
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "child's tool never went Active"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_session_that_names_itself_its_parent_is_no_child() {
        let d = recorded("dsh", "subagent", DS_DELEGATE_SCENE);
        let mut parent = None;
        edit_hooks(d.path(), |mut v| {
            if let Some(p) = v.as_object_mut().and_then(|o| o.remove("parentSession")) {
                parent = Some(p);
            }
            Some(v)
        });
        let parent = parent.expect("the recording links its child");
        edit_hooks(d.path(), |mut v| {
            if v["type"] == "session_start" && v["sessionId"] == parent {
                v["parentSession"] = parent.clone();
            }
            Some(v)
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "as another's child"),
            "{:?}",
            v.findings()
        );
    }

    /// `events` judged with each child reaching Active and each parent
    /// Delegating.
    fn subagent_findings(
        events: &[AgentEvent],
        inference: ChildEndInference,
    ) -> Vec<(Label, String)> {
        let reaches: Vec<(AgentId, Reach)> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::SessionStart {
                    agent_id,
                    parent_id: Some(parent),
                    ..
                } => Some([(*agent_id, Reach::Active), (*parent, Reach::Delegating)]),
                _ => None,
            })
            .flatten()
            .collect();
        let mut findings = Vec::new();
        subagent(events, &reaches, inference, &mut findings);
        findings
    }

    fn child_unended(events: &[AgentEvent], inference: ChildEndInference) -> bool {
        subagent_findings(events, inference)
            .iter()
            .any(|f| f.1.contains("never ended"))
    }

    #[test]
    fn an_untracked_task_end_ends_the_child_only_where_any_task_end_does() {
        use crate::source::ToolDetail;
        let (parent, child) = (
            AgentId::from_parts("reasonix", "p"),
            AgentId::from_parts("reasonix", "p:subagent:sa_1"),
        );
        let session = |agent_id, id: &str, parent_id| AgentEvent::SessionStart {
            agent_id,
            source: "reasonix".into(),
            session_id: id.into(),
            cwd: PathBuf::from("/proj"),
            parent_id,
        };
        let started = |agent_id, detail| AgentEvent::ActivityStart {
            agent_id,
            tool_use_id: None,
            detail: Some(detail),
        };
        let end = |agent_id| AgentEvent::ActivityEnd {
            agent_id,
            tool_use_id: None,
        };
        let read = || ToolDetail::Generic {
            display: "read_file".into(),
        };
        // As a reasonix skill call decodes: no call id, and its return re-states
        // the Task its end closes.
        let run = vec![
            session(parent, "p", None),
            started(parent, ToolDetail::Task),
            session(child, "p:subagent:sa_1", Some(parent)),
            started(child, read()),
            end(child),
            started(parent, ToolDetail::Task),
            end(parent),
        ];
        assert_eq!(
            subagent_findings(&run, ChildEndInference::AnyTaskEnd),
            vec![]
        );
        for inference in [ChildEndInference::None, ChildEndInference::TaskDrain] {
            assert!(child_unended(&run, inference), "{inference:?}");
        }
        let mut own_tool = run.clone();
        own_tool[5] = started(parent, read());
        assert!(
            child_unended(&own_tool, ChildEndInference::AnyTaskEnd),
            "the parent's own tool ending is no dispatch's"
        );
    }

    #[test]
    fn a_child_error_takes_no_child_work_but_the_parents_end() {
        use crate::source::ToolDetail;
        let (parent, child) = (
            AgentId::from_parts("copilot", "root"),
            AgentId::from_parts("copilot", "call_1"),
        );
        let session = |agent_id, id: &str, parent_id| AgentEvent::SessionStart {
            agent_id,
            source: "copilot".into(),
            session_id: id.into(),
            cwd: PathBuf::from("/proj"),
            parent_id,
        };
        // A child whose request errs ends, then the dispatch, then the parent's
        // turn; copilot's `-p` ends its session instead (`shut_down` below).
        let run = vec![
            session(parent, "root", None),
            AgentEvent::ActivityStart {
                agent_id: parent,
                tool_use_id: Some("call_1".into()),
                detail: Some(ToolDetail::Task),
            },
            session(child, "call_1", Some(parent)),
            AgentEvent::SessionEnd {
                agent_id: child,
                as_child: true,
            },
            AgentEvent::ActivityEnd {
                agent_id: parent,
                tool_use_id: Some("call_1".into()),
            },
            AgentEvent::ActivityEnd {
                agent_id: parent,
                tool_use_id: None,
            },
        ];
        let judged = |events: &[AgentEvent], reaches: &[(AgentId, Reach)], inference| {
            let mut findings = Vec::new();
            child_error(events, reaches, inference, &mut findings);
            findings.into_iter().map(|f| f.1).collect::<Vec<_>>()
        };
        let (delegating, drain) = ([(parent, Reach::Delegating)], ChildEndInference::TaskDrain);
        assert_eq!(judged(&run, &delegating, drain), Vec::<String>::new());
        let unended = &run[..run.len() - 1];
        assert_eq!(
            judged(unended, &delegating, drain),
            ["the parent's turn or session never ended"]
        );
        let shut_down: Vec<_> = unended
            .iter()
            .cloned()
            .chain([AgentEvent::SessionEnd {
                agent_id: parent,
                as_child: false,
            }])
            .collect();
        assert_eq!(judged(&shut_down, &delegating, drain), Vec::<String>::new());
        // Where no dispatch end ends a child, only its own end does.
        let mut unended = run.clone();
        unended.remove(3);
        assert_eq!(
            judged(&unended, &delegating, ChildEndInference::None),
            ["the child session never ended"]
        );
        assert_eq!(
            judged(&run, &[], drain),
            ["the parent never went Delegating"]
        );
    }

    #[test]
    fn only_a_durable_child_ends_with_its_own_turn() {
        use crate::source::ToolDetail;
        let (parent, child) = (
            AgentId::from_parts("opencode", "ses_parent"),
            AgentId::from_parts("opencode", "ses_child"),
        );
        let started = |agent_id, id: &str, detail| AgentEvent::ActivityStart {
            agent_id,
            tool_use_id: Some(id.into()),
            detail: Some(detail),
        };
        let turn_end = |agent_id| AgentEvent::ActivityEnd {
            agent_id,
            tool_use_id: None,
        };
        let session = |agent_id, id: &str, parent_id| AgentEvent::SessionStart {
            agent_id,
            source: "opencode".into(),
            session_id: id.into(),
            cwd: PathBuf::from("/proj"),
            parent_id,
        };
        let run = vec![
            session(parent, "ses_parent", None),
            started(parent, "dispatch", ToolDetail::Task),
            session(child, "ses_child", Some(parent)),
            started(
                child,
                "read",
                ToolDetail::Generic {
                    display: "read".into(),
                },
            ),
            turn_end(child),
        ];
        assert_eq!(subagent_findings(&run, ChildEndInference::Durable), vec![]);
        for (reaches, missing) in [
            (
                [(parent, Reach::Active), (parent, Reach::Delegating)],
                "the child's tool never went Active",
            ),
            (
                [(child, Reach::Active), (child, Reach::Delegating)],
                "the parent never went Delegating",
            ),
        ] {
            let mut findings = Vec::new();
            subagent(&run, &reaches, ChildEndInference::Durable, &mut findings);
            assert!(
                findings.iter().any(|f| f.1 == missing),
                "a class one slot reached is not another's: {findings:?}"
            );
        }
        let parent_turn: Vec<_> = run[..run.len() - 1]
            .iter()
            .cloned()
            .chain([turn_end(parent)])
            .collect();
        assert!(
            child_unended(&parent_turn, ChildEndInference::Durable),
            "only the child's own turn ends it"
        );
        for inference in [
            ChildEndInference::None,
            ChildEndInference::TaskDrain,
            ChildEndInference::AnyTaskEnd,
        ] {
            assert!(child_unended(&run, inference), "{inference:?}");
            let mut ended = run.clone();
            ended.push(AgentEvent::SessionEnd {
                agent_id: child,
                as_child: true,
            });
            assert_eq!(
                subagent_findings(&ended, inference),
                vec![],
                "{inference:?}"
            );
        }
    }

    #[test]
    fn a_recorded_dsh_subagent_run_passes() {
        let d = recorded("dsh", "subagent", DS_DELEGATE_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_child_that_names_no_parent_or_never_ends_fails() {
        // The lines that end the child: its own end, and its parent's drain of
        // the Task that spawned it. Any one alone ends it.
        let dsh: &[fn(&Value) -> bool] = &[
            |v| v["type"] == "session_end" && v.get("parentSession").is_some(),
            |v| v["type"] == "tool_result" && v.get("parentSession").is_none(),
        ];
        let reasonix: &[fn(&Value) -> bool] = &[|v| v["event"] == "SubagentStop"];
        // claude-code infers no end from the drained Agent call: its Stop alone.
        let claude: &[fn(&Value) -> bool] = &[|v| v["hook_event_name"] == "SubagentStop"];
        let never = "child session never ended";
        for (source, scene, ends) in [
            ("dsh", DS_DELEGATE_SCENE, dsh),
            ("reasonix", RX_DELEGATE_SCENE, reasonix),
            ("claude-code", CC_DELEGATE_SCENE, claude),
        ] {
            for keep in 0..ends.len() {
                let d = recorded(source, "subagent", scene);
                edit_hooks(d.path(), |v| {
                    let dropped = ends.iter().enumerate().any(|(i, e)| i != keep && e(&v));
                    (!dropped).then_some(v)
                });
                let v = verify(d.path()).expect("verify");
                assert!(!has(&v, Label::Fail, never), "{source}: {:?}", v.findings());
            }
            let d = recorded(source, "subagent", scene);
            edit_hooks(d.path(), |v| (!ends.iter().any(|e| e(&v))).then_some(v));
            let v = verify(d.path()).expect("verify");
            assert!(has(&v, Label::Fail, never), "{source}: {:?}", v.findings());
        }
        let d = recorded("dsh", "subagent", DS_DELEGATE_SCENE);
        edit_hooks(d.path(), |mut v| {
            if let Some(o) = v.as_object_mut() {
                o.remove("parentSession");
            }
            Some(v)
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "as another's child"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_child_still_in_the_scene_after_the_cli_exited_fails() {
        // A child renamed after its task carries no source prefix (grok's).
        let scene = "agents=[ds\u{b7}proj@0:active(Delegating), \
                     Read the note@1:active(read)]\n\
                     agents=[Read the note@1:idle] daemons=[]\n";
        let d = recorded("dsh", "subagent", scene);
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "after the CLI exited"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_recorded_codex_tool_run_passes() {
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_recorded_codex_subagent_run_passes() {
        let d = recorded("codex", "subagent", CX_DELEGATE_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_recorded_opencode_subagent_run_passes() {
        let d = recorded("opencode", "subagent", OC_DELEGATE_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_recorded_grok_run_passes_each_scenario() {
        for (name, scene) in [("tool-run", GK_SCENE), ("subagent", GK_DELEGATE_SCENE)] {
            let d = recorded("grok", name, scene);
            let v = verify(d.path()).expect("verify");
            assert_eq!(v.label(), Label::Pass, "{name}: {:?}", v.findings());
        }
    }

    #[test]
    fn a_recorded_copilot_run_passes_each_scenario() {
        for (name, scene) in [
            ("tool-run", CP_SCENE),
            ("resume", CP_SCENE),
            ("autopilot", CP_AUTOPILOT_SCENE),
            ("permission", CP_WAIT_SCENE),
            ("subagent", CP_DELEGATE_SCENE),
            ("child-error", CP_CHILD_ERROR_SCENE),
        ] {
            let d = recorded("copilot", name, scene);
            let v = verify(d.path()).expect("verify");
            assert_eq!(v.label(), Label::Pass, "{name}: {:?}", v.findings());
        }
    }

    #[test]
    fn a_resumed_turn_passes_only_once_it_ran_and_exited_clean() {
        let src = crate::harness::sources_root().join("fixtures/claude-code/ultra-effort-hermetic");
        let judged = |exit_code, resumed: Option<crate::harness::Resumed>| {
            let d = run_of(&src, "claude-code", "ultra-effort", CC_SCENE);
            write_manifest(d.path(), "claude-code", |m| {
                m.scenario = "ultra-effort".into();
                m.capture.exit_code = exit_code;
                m.capture.resumed = resumed;
            });
            verify(d.path()).expect("verify")
        };
        let resumed = |exit_code| Some(crate::harness::Resumed { exit_code });
        let v = judged(Some(0), resumed(Some(0)));
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        assert!(has(
            &judged(Some(0), None),
            Label::Fail,
            "named no session to resume"
        ));
        assert!(has(
            &judged(Some(0), resumed(Some(1))),
            Label::Blocked,
            "resumed exit Some(1)"
        ));
        let v = judged(Some(1), None);
        assert_eq!(v.label(), Label::Blocked, "{:?}", v.findings());
        assert!(
            !has(&v, Label::Fail, "named no session"),
            "a first turn that never ran is the CLI's, not pixtuoid's: {:?}",
            v.findings()
        );
    }

    #[test]
    fn a_recorded_child_error_whose_parent_never_ends_fails() {
        let d = recorded("copilot", "child-error", CP_CHILD_ERROR_SCENE);
        edit_transcripts(d.path(), |line| {
            !line.contains(r#""type":"session.shutdown""#)
        });
        assert!(has(
            &verify(d.path()).expect("verify"),
            Label::Fail,
            "the parent's turn or session never ended"
        ));
    }

    #[test]
    fn a_recorded_claude_code_tool_run_passes() {
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_session_id_that_splits_the_run_into_two_agents_fails() {
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        let mut first = true;
        edit_hooks(d.path(), |mut v| {
            if std::mem::take(&mut first) {
                v["session_id"] = "another-session".into();
            }
            Some(v)
        });
        assert!(has(
            &verify(d.path()).expect("verify"),
            Label::Fail,
            "must decode to 1 agent(s)"
        ));
    }

    #[test]
    fn a_tmp_cwd_is_its_private_tmp_workspace_where_neither_exists() {
        let start = |cwd: &str| AgentEvent::SessionStart {
            agent_id: crate::AgentId::from_parts("omp", "s"),
            source: "omp".into(),
            session_id: "s".into(),
            cwd: cwd.into(),
            parent_id: None,
        };
        let ws = Path::new("/private/tmp/pixtuoid-judged-elsewhere/proj");
        for cwd in [
            "/tmp/pixtuoid-judged-elsewhere/proj",
            "/private/tmp/pixtuoid-judged-elsewhere/proj",
        ] {
            let mut f = Vec::new();
            started_in(&[start(cwd)], ws, 1, false, &mut f);
            assert!(f.is_empty(), "{cwd}: {f:?}");
        }
        let mut f = Vec::new();
        started_in(
            &[start("/tmp/pixtuoid-judged-elsewhere/other")],
            ws,
            1,
            false,
            &mut f,
        );
        assert!(has(&Verdict::new(f), Label::Fail, "is not the workspace"));
    }

    #[test]
    fn a_cwd_outside_the_workspace_fails() {
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        write_manifest(d.path(), "codex", |m| {
            m.capture.workspace = PathBuf::from("/elsewhere")
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "is not the workspace"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_childs_own_event_names_its_cwd_unless_children_inherit_it() {
        let id = |id: &str| crate::AgentId::from_parts("grok", id);
        let start = |agent: &str, cwd: &str, parent: Option<&str>| AgentEvent::SessionStart {
            agent_id: id(agent),
            source: "grok".into(),
            session_id: agent.into(),
            cwd: cwd.into(),
            parent_id: parent.map(id),
        };
        let backfill = |agent: &str, cwd: &str| AgentEvent::Identity {
            agent_id: id(agent),
            source: "grok".into(),
            session_id: agent.into(),
            cwd: Some(cwd.into()),
            pid: None,
        };
        let ws = Path::new("/w");
        let judged_inheriting = |events: &[AgentEvent], inherit: bool| {
            let mut f = Vec::new();
            started_in(events, ws, 2, inherit, &mut f);
            Verdict::new(f)
        };
        let judged = |events: &[AgentEvent]| judged_inheriting(events, false);
        let root = start("p", "/w", None);
        // grok's spawn line registers the child with no cwd.
        let child = start("c", "", Some("p"));
        let v = judged(&[root.clone(), child.clone(), backfill("c", "/w")]);
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        // The root naming the workspace does not stand in for the child, unless
        // the CLI's children run where their parent does.
        let v = judged(&[root.clone(), child.clone()]);
        assert!(has(&v, Label::Fail, "never named"), "{:?}", v.findings());
        let v = judged_inheriting(&[root.clone(), child.clone()], true);
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        // A back-filled cwd is held to the workspace too, inherited or not.
        for inherit in [false, true] {
            let v = judged_inheriting(
                &[root.clone(), child.clone(), backfill("c", "/elsewhere")],
                inherit,
            );
            assert!(
                has(&v, Label::Fail, "is not the workspace"),
                "{:?}",
                v.findings()
            );
        }
        // A root that loses its cwd is no child awaiting one.
        let v = judged(&[start("p", "", None), start("c", "/w", Some("p"))]);
        assert!(
            has(&v, Label::Fail, "is not the workspace"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_succession_completes_with_no_model_request_and_only_on_a_clean_exit() {
        let completed = |shape, exit_code, requests| {
            let m = manifest("omp", |m| {
                m.capture.exit_code = exit_code;
                m.requests = requests;
            });
            completed_turn(&m, shape, &mut Vec::new())
        };
        assert!(completed(Shape::Succession, Some(0), 0));
        assert!(!completed(Shape::Succession, Some(1), 0));
        assert!(
            !completed(Shape::Succession, None, 0),
            "a signal ends no turn"
        );
        assert!(
            !completed(Shape::ToolRun, Some(0), 0),
            "a turn that asks the model"
        );
    }

    #[test]
    fn a_succession_ends_its_first_session_and_starts_a_root_after_it() {
        let id = |id: &str| crate::AgentId::from_parts("omp", id);
        let start = |agent: &str, parent: Option<&str>| AgentEvent::SessionStart {
            agent_id: id(agent),
            source: "omp".into(),
            session_id: agent.into(),
            cwd: "/w".into(),
            parent_id: parent.map(id),
        };
        let end = |agent: &str| AgentEvent::SessionEnd {
            agent_id: id(agent),
            as_child: false,
        };
        let judged = |events: &[AgentEvent]| {
            let mut f = Vec::new();
            succession(events, &mut f);
            Verdict::new(f)
        };
        let v = judged(&[start("a", None), end("a"), start("b", None), end("b")]);
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        let v = judged(&[start("a", None), start("b", None), end("a")]);
        assert_eq!(
            v.label(),
            Label::Pass,
            "an end's arrival order is its shim's"
        );
        // A quit that won the race with the command: one session, ended.
        let v = judged(&[start("a", None), end("a")]);
        assert!(
            has(&v, Label::Fail, "no session succeeded"),
            "{:?}",
            v.findings()
        );
        let v = judged(&[start("a", None), start("b", None), end("b")]);
        assert!(has(&v, Label::Fail, "never ended"), "{:?}", v.findings());
        let v = judged(&[start("a", None), end("a"), start("b", Some("a"))]);
        assert!(has(&v, Label::Fail, "as a child"), "{:?}", v.findings());
    }

    #[test]
    fn a_run_with_no_session_start_fails() {
        // Not codex: its UserPromptSubmit decodes to a SessionStart too.
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        edit_hooks(d.path(), |v| {
            (v["hook_event_name"] != "SessionStart").then_some(v)
        });
        blank_transcripts(d.path());
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "no SessionStart decoded"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_tool_that_never_ends_fails() {
        // Not codex: its Stop ends the activity too.
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        edit_hooks(d.path(), |v| {
            (v["hook_event_name"] != "PostToolUse").then_some(v)
        });
        blank_transcripts(d.path());
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "the tool never ended"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_tool_start_with_no_name_fails_and_a_slot_with_no_tool_never_goes_active() {
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        // Both transports name the tool, so both lose it.
        edit_hooks(d.path(), |mut v| {
            if v.get("tool_name").is_some() {
                v["tool_name"] = "".into();
            }
            Some(v)
        });
        blank_transcripts(d.path());
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "detail naming the tool"),
            "{:?}",
            v.findings()
        );

        edit_hooks(d.path(), |v| {
            (v["hook_event_name"] != "PreToolUse").then_some(v)
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "never went Active"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_line_the_decoder_rejects_fails() {
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        edit_hooks(d.path(), |mut v| {
            if v["hook_event_name"] == "PreToolUse" {
                v["session_id"] = Value::Null;
            }
            Some(v)
        });
        let v = verify(d.path()).expect("verify");
        assert!(
            v.findings
                .iter()
                .any(|f| f.0 == Label::Fail && f.1.starts_with(HOOK_PAYLOADS)),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_hook_the_scenario_expects_but_never_fired_fails() {
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        edit_hooks(d.path(), |v| (v["hook_event_name"] != "Stop").then_some(v));
        let v = verify(d.path()).expect("verify");
        assert!(has(&v, Label::Fail, "must fire Stop"), "{:?}", v.findings());
    }

    #[test]
    fn a_tui_turn_that_raised_no_gate_to_refuse_fails() {
        let d = recorded("claude-code", "permission", CC_WAIT_SCENE);
        let unrefused = |requests, unmatched| {
            write_manifest(d.path(), "claude-code", |m| {
                m.scenario = "permission".into();
                m.capture.exit_code = Some(tui::UNREFUSED);
                m.requests = requests;
                m.unmatched = unmatched;
            });
            verify(d.path()).expect("verify")
        };
        let v = unrefused(2, 0);
        assert!(has(&v, Label::Fail, "no gate"), "{:?}", v.findings());
        assert_eq!(
            unrefused(0, 0).label(),
            Label::Blocked,
            "it never reached the model"
        );
        assert_eq!(
            unrefused(2, 1).label(),
            Label::Script,
            "a script that lost the turn never asked for the gated tool"
        );
        let h = run_from("codex", "tool-run-recorded", CX_SCENE);
        write_manifest(h.path(), "codex", |m| {
            m.capture.exit_code = Some(tui::UNREFUSED)
        });
        assert_eq!(
            verify(h.path()).expect("verify").label(),
            Label::Blocked,
            "a headless CLI's own exit code"
        );
    }

    #[test]
    fn a_run_the_script_lost_before_it_blocked_is_not_judged() {
        let v = Verdict::new(vec![
            (Label::Blocked, "exit 1".into()),
            (Label::Script, "1 unmatched".into()),
        ]);
        assert_eq!((v.label(), v.judged()), (Label::Script, false));
        assert!(Verdict::new(vec![(Label::Script, "1 unmatched".into())]).judged());
    }

    #[test]
    fn an_unmatched_model_request_is_script_and_a_cli_that_never_ran_is_blocked() {
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        write_manifest(d.path(), "codex", |m| m.unmatched = 1);
        assert_eq!(verify(d.path()).expect("verify").label(), Label::Script);
        write_manifest(d.path(), "codex", |m| m.capture.exit_code = Some(1));
        assert_eq!(verify(d.path()).expect("verify").label(), Label::Blocked);
        write_manifest(d.path(), "codex", |m| m.requests = 0);
        assert_eq!(verify(d.path()).expect("verify").label(), Label::Blocked);
        write_manifest(d.path(), "codex", |m| {
            m.capture.exit_code = Some(1);
            m.unmatched = 1;
        });
        assert_eq!(
            verify(d.path()).expect("verify").label(),
            Label::Script,
            "a CLI the script lost and that then exited is the script's finding"
        );
    }

    #[test]
    fn a_daemon_s_turn_shows_busy_then_idle_then_gone() {
        let judged = |scene: &str| {
            let d = recorded("openclaw", "tool-run", scene);
            verify(d.path()).expect("verify").findings().to_vec()
        };
        let frame = |state: &str| format!("agents=[] daemons=[openclaw@59871:{state}]\n");
        let gone = "agents=[] daemons=[]\n";
        let ran = [frame("idle"), frame("busy"), frame("idle"), frame("down")].concat();
        assert_eq!(judged(&(ran.clone() + gone)), vec![]);
        assert_eq!(
            judged(&ran),
            vec![],
            "down is stopped, before its linger ends"
        );
        let never_busy = [frame("idle"), frame("down")].concat() + gone;
        assert!(
            judged(&never_busy)[0].1.contains("busy"),
            "{:?}",
            judged(&never_busy)
        );
        let never_settled = [frame("busy"), frame("down")].concat() + gone;
        assert!(judged(&never_settled)[0].1.contains("idle"));
        let idle_only_before = [frame("idle"), frame("busy"), frame("down")].concat() + gone;
        assert!(judged(&idle_only_before)[0].1.contains("idle"));
        let lingering = [frame("busy"), frame("idle")].concat();
        assert!(judged(&lingering)[0].1.contains("still up"));
    }

    #[test]
    fn a_daemon_s_failed_turn_settles_degraded() {
        let judged = |scene: &str| {
            let d = recorded("openclaw", "model-error", scene);
            write_manifest(d.path(), "openclaw", |m| {
                m.scenario = "model-error".into();
                m.capture.exit_code = Some(1);
            });
            verify(d.path()).expect("verify").findings().to_vec()
        };
        let frame = |state: &str| format!("agents=[] daemons=[openclaw@55306:{state}]\n");
        let gone = "agents=[] daemons=[]\n";
        let failed = [
            frame("idle"),
            frame("busy"),
            frame("degraded"),
            frame("down"),
        ]
        .concat();
        assert_eq!(judged(&(failed + gone)), vec![]);
        let sound = [frame("busy"), frame("idle"), frame("down")].concat() + gone;
        assert!(
            judged(&sound)[0].1.contains("degraded"),
            "{:?}",
            judged(&sound)
        );
    }

    #[test]
    fn only_a_turn_scripted_to_fail_may_exit_non_zero() {
        let scene = "agents=[] daemons=[openclaw@55306:busy]\n\
                     agents=[] daemons=[openclaw@55306:degraded]\n\
                     agents=[] daemons=[openclaw@55306:down]\nagents=[] daemons=[]\n";
        let judged = |scenario: &str, exit_code, requests| {
            let d = recorded("openclaw", "model-error", scene);
            write_manifest(d.path(), "openclaw", |m| {
                m.scenario = scenario.into();
                m.capture.exit_code = exit_code;
                m.requests = requests;
            });
            has(
                &verify(d.path()).expect("verify"),
                Label::Blocked,
                "did not complete",
            )
        };
        assert!(!judged("model-error", Some(1), 1));
        assert!(
            judged("model-error", Some(1), 0),
            "it never reached the model"
        );
        assert!(judged("model-error", None, 1), "a signal ends no turn");
        assert!(judged("tool-run", Some(1), 1));
    }

    #[test]
    fn a_recorded_agent_model_error_run_passes_on_its_failure_hook() {
        let run = |edit: fn(&Path)| {
            let d = recorded(
                "opencode",
                "model-error",
                "agents=[oc\u{b7}proj@0:idle]\nagents=[] daemons=[]\n",
            );
            write_manifest(d.path(), "opencode", |m| {
                m.scenario = "model-error".into();
                m.capture.exit_code = Some(1);
            });
            edit(d.path());
            verify(d.path()).expect("verify")
        };
        let v = run(|_| {});
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        let v = run(|d| {
            edit_hooks(d, |v| {
                (v["type"] != "session.execution.failed").then_some(v)
            })
        });
        assert!(
            has(&v, Label::Fail, "session.execution.failed"),
            "{:?}",
            v.findings()
        );
        assert!(has(&v, Label::Fail, "never ended"), "{:?}", v.findings());
    }

    #[test]
    fn a_failed_turn_is_held_to_its_end() {
        let agent_id = AgentId::from_parts("kimi", "session");
        let judged = |tool_use_id: Option<&str>| {
            let mut findings = Vec::new();
            model_error(
                &[AgentEvent::ActivityEnd {
                    agent_id,
                    tool_use_id: tool_use_id.map(str::to_owned),
                }],
                &mut findings,
            );
            findings
        };
        assert_eq!(judged(None), vec![]);
        assert!(
            judged(Some("call"))
                .iter()
                .any(|f| f.1.contains("never ended"))
        );
    }

    #[test]
    fn a_scene_that_never_shows_the_agent_active_fails() {
        let d = run_from(
            "codex",
            "tool-run-recorded",
            "agents=[cx\u{b7}proj@0:idle]\nagents=[]\n",
        );
        let v = verify(d.path()).expect("verify");
        assert!(has(&v, Label::Fail, "never showed"), "{:?}", v.findings());
    }

    #[test]
    fn an_agent_still_in_the_scene_after_the_cli_exited_fails() {
        let scene =
            "agents=[cx\u{b7}proj@0:active(Bash: cat NOTE.txt)]\nagents=[cx\u{b7}proj@0:idle]\n";
        let d = run_from("codex", "tool-run-recorded", scene);
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "after the CLI exited"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_real_drift_breadcrumb_in_the_scene_log_fails() {
        let breadcrumb = crate::test_capture::capture_logs(|| {
            crate::source::drift::missing_field("codex", "PreToolUse", "tool_name");
        });
        let d = run_from(
            "codex",
            "tool-run-recorded",
            &format!("{breadcrumb}{CX_SCENE}"),
        );
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "drift breadcrumb"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_result_aimock_sees_for_a_profile_owing_one_is_stale() {
        let d = run_from("antigravity", "tool-run-hermetic", AG_SCENE);
        write_manifest(d.path(), "antigravity", |m| m.results_seen = 0);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        write_manifest(d.path(), "antigravity", |m| m.results_seen = 1);
        let v = verify(d.path()).expect("verify");
        assert!(has(&v, Label::Stale, "result_unseen"), "{:?}", v.findings());
    }

    /// claude-code owing `Notification`, a hook its committed captures fire.
    fn owing_notification() -> Profile {
        Profile {
            not_yet: &["Notification"],
            live: None,
            daemon: None,
            ..*profile("claude-code").expect("profile")
        }
    }

    #[test]
    fn a_not_yet_hook_any_run_fires_is_stale() {
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        let mut first = true;
        edit_hooks(d.path(), |mut v| {
            if std::mem::take(&mut first) {
                v["hook_event_name"] = "Notification".into();
            }
            Some(v)
        });
        let v = verify_source(&owing_notification(), &[d.path().to_path_buf()]).expect("verify");
        assert!(has(&v, Label::Stale, "Notification"), "{:?}", v.findings());
    }

    #[test]
    fn a_read_field_no_run_carries_fails_the_census() {
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        edit_hooks(d.path(), |mut v| {
            if let Some(name) = v.as_object_mut().and_then(|o| o.remove("tool_name")) {
                v["toolName"] = name;
            }
            Some(v)
        });
        let v = verify_source(
            profile("claude-code").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(
            has(&v, Label::Fail, "hook:PreToolUse /tool_name"),
            "{:?}",
            v.findings()
        );
    }

    /// Rewrite every `.jsonl` under `dir` with `from` spelled `to`.
    fn respell(dir: &Path, from: &str, to: &str) {
        for f in jsonl_files_under(dir) {
            let text = std::fs::read_to_string(&f).expect("read");
            std::fs::write(&f, text.replace(from, to)).expect("write");
        }
    }

    #[test]
    fn a_run_of_its_own_fixture_owes_no_value() {
        let d = recorded("codex", "tool-run", CX_SCENE);
        let v = verify_source(
            profile("codex").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(
            !v.findings()
                .iter()
                .any(|f| f.1.contains("in a scenario fixture")),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_value_upstream_renamed_fails_though_its_path_arrives() {
        let d = recorded("codex", "tool-run", CX_SCENE);
        respell(d.path(), "\"task_complete\"", "\"turn_complete\"");
        let v = verify_source(
            profile("codex").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(
            has(&v, Label::Fail, "`task_complete`"),
            "{:?}",
            v.findings()
        );
    }

    /// A literal a fixture holds must arrive though an owed field path names it:
    /// codex owes fields under `tx:event_msg`, and its kind is still read.
    #[test]
    fn a_held_literal_an_owed_path_names_must_still_arrive() {
        let d = recorded("codex", "tool-run", CX_SCENE);
        respell(d.path(), "\"event_msg\"", "\"event_message\"");
        let v = verify_source(
            profile("codex").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(
            has(&v, Label::Fail, "`event_msg` (codex.rs)"),
            "{:?}",
            v.findings()
        );
    }

    /// A copilot run against a baseline of its own recording: nothing New, until
    /// the run carries a type its decoder drops that the recording never did.
    #[test]
    fn a_type_a_run_drops_that_its_recording_never_did_is_new_and_passes() {
        let p = profile("copilot").expect("profile");
        let d = recorded("copilot", "tool-run", "");
        let files = jsonl_files_under(d.path());
        let baseline = Dropped::of_files("copilot", &files).expect("baseline");
        let check = |baseline: Option<&Dropped>| {
            let mut findings = Vec::new();
            let unjudged = new_types(p, d.path(), baseline, &mut findings).expect("judge");
            (Verdict::new(findings), unjudged)
        };
        let (v, unjudged) = check(Some(&baseline));
        assert!(
            v.findings().is_empty() && unjudged.is_none(),
            "{:?}",
            v.findings()
        );

        let transcript = files
            .iter()
            .find(|f| f.ends_with("events.jsonl"))
            .expect("a transcript");
        let mut text = std::fs::read_to_string(transcript).expect("read");
        text.push_str("{\"type\":\"assistant.brand_new\",\"data\":{\"x\":\"y\"}}\n");
        std::fs::write(transcript, text).expect("write");
        let (v, _) = check(Some(&baseline));
        assert!(
            has(&v, Label::New, "`assistant.brand_new`"),
            "{:?}",
            v.findings()
        );
        assert!(v.passed(), "New never fails a run");
        assert!(!Verdict::new(vec![(Label::Stale, String::new())]).passed());

        let elsewhere = Dropped {
            os: "plan9".try_into().expect("an OS"),
            ..baseline
        };
        let (v, unjudged) = check(Some(&elsewhere));
        assert!(
            v.findings().is_empty(),
            "a baseline recorded on another OS judges nothing here"
        );
        assert_eq!(unjudged, Some(Unjudged::RecordedOn("plan9".to_owned())));
        let (v, unjudged) = check(None);
        assert!(v.findings().is_empty());
        assert_eq!(unjudged, Some(Unjudged::NoBaseline));
    }

    /// `verify` carries why its run could not judge new types, as its
    /// scenario's baseline says, whichever baseline the fixture holds.
    #[test]
    fn a_verdict_says_why_its_run_judged_no_new_types() {
        let d = recorded("codex", "tool-run", CX_SCENE);
        let v = verify(d.path()).expect("verify");
        let scenario = profile("codex")
            .and_then(|p| p.scenarios.iter().find(|s| s.shape == Shape::ToolRun))
            .expect("codex's tool-run");
        let want = match dropped::baseline_of(scenario).expect("baseline") {
            None => Some(Unjudged::NoBaseline),
            Some(b) if !b.judges_here() => Some(Unjudged::RecordedOn(b.os.to_string())),
            Some(_) => None,
        };
        assert_eq!(v.unjudged(), want.as_ref(), "{:?}", v.findings());
    }

    /// A key a fixture holds, blank or not, must arrive: grok's `subagent_type`
    /// is held only blank, so no value census would see it renamed.
    #[test]
    fn a_key_upstream_renamed_fails_though_its_value_was_blank() {
        let d = recorded("grok", "subagent", GK_DELEGATE_SCENE);
        respell(d.path(), "\"subagent_type\"", "\"subagent_kind\"");
        let v = verify_source(profile("grok").expect("profile"), &[d.path().to_path_buf()])
            .expect("verify");
        assert!(
            has(&v, Label::Fail, "`subagent_type`"),
            "{:?}",
            v.findings()
        );
    }

    /// An owed literal is Stale only where the runs would keep it once stripped:
    /// the census at rest judges stripped fixtures, so a run's raw bytes alone
    /// would hold the profile at Stale with no way out.
    #[test]
    fn an_owed_literal_is_stale_only_where_the_strip_keeps_it() {
        let census = |source: &str, owed: &'static &'static str, edit: &dyn Fn(&Path)| {
            let p = Profile {
                not_yet_literals: std::slice::from_ref(owed),
                live: None,
                daemon: None,
                ..*profile(source).expect("profile")
            };
            let d = recorded(source, "tool-run", "");
            edit(d.path());
            let v = verify_source(&p, &[d.path().to_path_buf()]).expect("verify");
            has(&v, Label::Stale, &format!("`{owed}`"))
        };
        // A codex turn end the strip keeps.
        assert!(census("codex", &"web_search_end", &|d| {
            respell(d, "\"task_complete\"", "\"web_search_end\"");
        }));
        // An agy step whose line decodes to nothing, which the strip blanks whole.
        assert!(!census("antigravity", &"USER_INPUT", &|d| {
            let t = jsonl_files_under(d)
                .into_iter()
                .find(|f| f.ends_with("transcript.jsonl"))
                .expect("a transcript");
            let text = std::fs::read_to_string(&t).expect("read");
            std::fs::write(
                &t,
                format!("{{\"step_index\":9,\"type\":\"USER_INPUT\"}}\n{text}"),
            )
            .expect("write");
        }));
    }

    #[test]
    fn a_not_yet_field_a_run_carries_is_stale() {
        // A billed capture's usage carries the cache tokens aimock's never does.
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        let v = verify_source(
            profile("claude-code").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(
            has(
                &v,
                Label::Stale,
                "tx:assistant /message/usage/cache_creation_input_tokens"
            ),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_stamped_envelope_drops_its_top_level_shim_stamps_only() {
        let v = serde_json::json!({"_shim_ts_ms": 5, "_pid": 7, "a": {"_k": 1}});
        assert_eq!(leaves(&v, true), ["/a/_k"]);
        assert_eq!(leaves(&v, false).len(), 3);
    }

    fn fields(kinds: &[(&str, &[&str])], lines: &[&[&str]]) -> Fields {
        let set = |paths: &[&str]| {
            paths
                .iter()
                .map(|p| (*p).to_owned())
                .collect::<BTreeSet<_>>()
        };
        Fields {
            by_kind: kinds
                .iter()
                .map(|(k, p)| ((*k).to_owned(), set(p)))
                .collect(),
            lines: lines.iter().map(|l| set(l)).collect(),
            values: BTreeMap::new(),
            keys: BTreeSet::new(),
        }
    }

    #[test]
    fn the_census_fails_a_read_field_not_carried_unless_owed_and_stales_one_carried() {
        let reads = fields(&[("hook:A", &["/x", "/owed", "/owed2"])], &[]);
        let carried = fields(&[("hook:A", &["/owed2"])], &[]);
        let owed = [("hook:A", "/owed"), ("hook:A", "/owed2")];
        let got = census(&reads, &carried, &[], &owed, "s");
        assert_eq!(
            got,
            [
                (Label::Stale, "hook:A /owed2 was carried: it leaves the profile's not_yet_fields".to_owned()),
                (Label::Fail, "hook:A /x: a decoder reads it in a committed capture, and no run of s carried it".to_owned()),
            ],
            "an owed field not carried is silent"
        );
    }

    #[test]
    fn a_committed_kind_no_run_produced_fails_unless_a_not_yet_hook() {
        let reads = fields(
            &[
                ("hook:B", &["/y"]),
                ("hook:C", &["/y"]),
                ("tx:event_msg", &["/y"]),
            ],
            &[],
        );
        let got = census(&reads, &Fields::default(), &["B"], &[], "s");
        assert_eq!(
            got,
            [
                (
                    Label::Fail,
                    "hook:C: a committed capture has it, and no run of s produced it".to_owned()
                ),
                (
                    Label::Fail,
                    "tx:event_msg: a committed capture has it, and no run of s produced it"
                        .to_owned()
                ),
            ],
            "the owed hook B is silent; a renamed or unharvested transcript kind is not"
        );
    }

    #[test]
    fn a_transcript_kind_no_run_produced_is_owed_through_every_field_it_reads() {
        let reads = fields(
            &[
                ("tx:title_change", &["/title"]),
                ("tx:title", &["/title", "/v"]),
                ("tx:bare", &[]),
            ],
            &[],
        );
        let owed = [("tx:title_change", "/title"), ("tx:title", "/title")];
        let got = census(&reads, &Fields::default(), &[], &owed, "s");
        let produced = |kind: &str| {
            (
                Label::Fail,
                format!("{kind}: a committed capture has it, and no run of s produced it"),
            )
        };
        assert_eq!(got, [produced("tx:bare"), produced("tx:title")]);
    }

    #[test]
    fn a_committed_hook_the_profile_owes_needs_no_run() {
        // fixtures/claude-code/permission-recorded fires it; a tool run doesn't.
        let owed = ["hook:Notification"];
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        let v = verify_source(&owing_notification(), &[d.path().to_path_buf()]).expect("verify");
        assert!(
            !v.findings()
                .iter()
                .any(|f| owed.iter().any(|k| f.1.starts_with(&format!("{k}:")))),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_line_the_strip_blanked_whole_adds_no_kind() {
        let p = profile("codex").expect("codex");
        let mut f = Fields::default();
        f.add(p, Path::new("rollout.jsonl"), &serde_json::json!({}), false);
        f.add(
            p,
            Path::new("rollout.jsonl"),
            &serde_json::json!({"type": ""}),
            false,
        );
        assert!(f.by_kind.is_empty() && f.lines.is_empty(), "{f:?}");
    }

    #[test]
    fn an_untyped_committed_line_needs_one_run_line_carrying_all_its_reads() {
        let reads = fields(&[], &[&["/payload/cwd", "/payload/id"]]);
        let split = fields(&[], &[&["/payload/cwd"], &["/payload/id"]]);
        assert_eq!(
            census(&reads, &split, &[], &[], "s").len(),
            1,
            "two lines don't make one"
        );
        let whole = fields(&[], &[&["/payload/cwd", "/payload/id", "/payload/more"]]);
        assert!(census(&reads, &whole, &[], &[], "s").is_empty());
    }

    #[test]
    fn a_rollout_that_loses_its_cwd_fails_the_census() {
        // Every line: codex's head scan takes the first `payload.cwd` it finds.
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        for t in jsonl_files_under(d.path())
            .into_iter()
            .filter(|f| !is_hook_envelope(f))
        {
            let text = std::fs::read_to_string(&t).expect("read");
            let out: Vec<String> = text
                .lines()
                .map(|l| {
                    let mut v: Value = serde_json::from_str(l).expect("json");
                    if let Some(payload) = v["payload"].as_object_mut() {
                        payload.remove("cwd");
                    }
                    v.to_string()
                })
                .collect();
            std::fs::write(&t, out.join("\n") + "\n").expect("write");
        }
        let v = verify_source(
            profile("codex").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(
            has(&v, Label::Fail, "with no type reads"),
            "{:?}",
            v.findings()
        );
    }

    #[test]
    fn a_leaf_is_blank_exactly_where_the_probe_neutralizes() {
        let v = serde_json::json!({
            "s": "x", "n": 1, "f": 0.0, "b": true, "a": [{"k": "y"}], "l/m": "z", "z": null
        });
        let got: BTreeSet<String> = leaves(&v, false).into_iter().collect();
        let want: BTreeSet<String> = ["/s", "/n", "/f", "/b", "/a/*/k", "/l~1m"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(got, want, "0.0 is a value: the probe neutralizes it to 0");
        // What the probe writes over each leaf is blank to the census.
        for (_, x) in v.as_object().expect("an object") {
            if let Some(blank) = blank_of(x) {
                assert!(leaves(&blank, false).is_empty(), "{blank}");
            }
        }
    }
}
