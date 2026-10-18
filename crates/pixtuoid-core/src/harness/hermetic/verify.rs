//! Judging hermetic runs: each scenario's run alone ([`verify`]), and each
//! source's runs together ([`verify_source`]). A run's bytes ride the one
//! pipeline (`Drive`), and the scene a headless pixtuoid painted from the same
//! run is read as text. Ids, timestamps and model text change every run, so
//! nothing here compares bytes or goldens: only shapes, events and the fields
//! a decoder reads.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{MANIFEST, Manifest, Profile, SCENE_LOG, Scenario, Shape, profile};
use crate::harness::captures::jsonl_files_under;
use crate::harness::reads::{blank_of, child_pointer, is_hook_envelope, is_shim_stamp, strips};
use crate::harness::{Drive, Driven, HOOK_PAYLOADS, PROVENANCE, Reach, captures};
use crate::source::registry::ChildEndInference;
use crate::{AgentEvent, AgentId};

/// The tracing target every drift breadcrumb rides (`source::drift::TARGET`).
const BREADCRUMB_TARGET: &str = crate::source::drift::TARGET;

/// A verdict's outcome, ordered by severity: a verdict is its worst finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Label {
    /// Every check held.
    Pass,
    /// A `not_yet` hook fired or a `not_yet_fields` field was carried: good
    /// news, the list shrinks.
    Stale,
    /// The CLI could not run (install, network, a login wall): not judged.
    Blocked,
    /// The CLI asked the model something no script line answers: its request
    /// flow changed upstream.
    Script,
    /// pixtuoid read the run wrong, or upstream stopped sending what it reads.
    Fail,
}

/// One verdict: its findings, worst first.
#[derive(Debug)]
pub struct Verdict {
    findings: Vec<(Label, String)>,
}

impl Verdict {
    /// A verdict of `findings`, in any order.
    pub fn new(mut findings: Vec<(Label, String)>) -> Self {
        findings.sort_by_key(|f| std::cmp::Reverse(f.0));
        Self { findings }
    }

    /// The worst finding's label; Pass with none.
    pub fn label(&self) -> Label {
        self.findings.first().map_or(Label::Pass, |f| f.0)
    }

    /// Every finding, worst first.
    pub fn findings(&self) -> &[(Label, String)] {
        &self.findings
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
        .scenarios
        .iter()
        .find(|s| s.shape.name() == manifest.scenario)
        .ok_or_else(|| invalid(format!("{}: no scenario {}", p.source, manifest.scenario)))?;
    let scene = std::fs::read_to_string(dir.join(SCENE_LOG)).unwrap_or_default();
    let mut findings = Vec::new();
    judge(p, scenario, &manifest, dir, &scene, &mut findings)?;
    Ok(Verdict::new(findings))
}

/// Judge `p`'s runs together, `dirs` being all of them, for what no one
/// scenario owns: a `not_yet` hook any run fired is Stale, and the field census
/// over `p`'s committed captures, where a kind or a field upstream renamed or
/// dropped fails though the decoder still parses the line.
///
/// # Errors
///
/// If a run's or a committed capture's file cannot be read.
pub fn verify_source(p: &Profile, dirs: &[PathBuf]) -> std::io::Result<Verdict> {
    let mut fired = BTreeSet::new();
    let mut carried = Fields::default();
    for dir in dirs {
        for file in jsonl_files_under(dir) {
            for line in json_lines(&file)? {
                if is_hook_envelope(&file)
                    && let Some(event) = (p.event_name)(&line)
                {
                    fired.insert(event.into_owned());
                }
                carried.add(p, &file, &line, true);
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
    Ok(Verdict::new(findings))
}

/// The field census proper: each kind a committed capture has must be produced
/// by some run, unless it is a hook `owed_hooks` names, and each read field of
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
            if !kind
                .strip_prefix(HOOK)
                .is_some_and(|h| owed_hooks.contains(&h))
            {
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

/// The non-blank leaves a set of captures holds, for the census.
#[derive(Debug, Default)]
pub(crate) struct Fields {
    /// Every leaf of each kind of line.
    by_kind: BTreeMap<String, BTreeSet<String>>,
    /// The leaves of each transcript line taken whole: every one of a run's,
    /// or a committed capture's untyped ones.
    lines: BTreeSet<BTreeSet<String>>,
}

impl Fields {
    /// Add `line` of `file`; one with no value, as the strip leaves a line no
    /// decoder reads, adds nothing. A committed (`run == false`) transcript
    /// line the strip left untyped goes to `lines` alone, since its blank
    /// `type` is no kind a run's line could have.
    fn add(&mut self, p: &Profile, file: &Path, line: &Value, run: bool) {
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
            self.by_kind.entry(kind).or_default().extend(leaves);
        }
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
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(capture.dir.join(PROVENANCE))?)?;
        if !strips(&record) {
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

/// A file's lines that parse; the Drive reports the ones that don't.
fn json_lines(file: &Path) -> std::io::Result<Vec<Value>> {
    Ok(std::fs::read_to_string(file)?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

fn judge(
    p: &Profile,
    scenario: &Scenario,
    manifest: &Manifest,
    dir: &Path,
    scene: &str,
    findings: &mut Vec<(Label, String)>,
) -> std::io::Result<()> {
    // Before the Blocked return: a CLI the script lost may then exit non-zero.
    if manifest.unmatched > 0 {
        findings.push((
            Label::Script,
            format!(
                "{} model request(s) matched no script line: the request flow changed",
                manifest.unmatched
            ),
        ));
    }
    // A CLI that never ran says nothing about pixtuoid, so nothing below is judged.
    if manifest.capture.exit_code != Some(0) || manifest.requests == 0 {
        findings.push((
            Label::Blocked,
            format!(
                "the CLI did not complete a turn against the scripted model (exit {:?}, {} \
                 model requests): it could not run, or its model calls went elsewhere \
                 through a home the sandbox did not redirect",
                manifest.capture.exit_code, manifest.requests
            ),
        ));
        return Ok(());
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
    let mut reached: Vec<Reach> = Vec::new();
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
        reached.extend(d.reached.iter().copied());
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

    let sessions = match scenario.shape {
        Shape::ToolRun | Shape::Permission => 1,
        Shape::Subagent => 2,
    };
    started_in(&events, &manifest.capture.workspace, sessions, findings);
    match scenario.shape {
        Shape::ToolRun => tool_run(&events, &reached, findings),
        Shape::Permission => permission(&events, &reached, findings),
        Shape::Subagent => subagent(
            &events,
            &reached,
            p.descriptor().caps().child_end_inference,
            findings,
        ),
    }
    let shown = match scenario.shape {
        Shape::ToolRun | Shape::Subagent => "active",
        Shape::Permission => "waiting",
    };
    judge_scene(p, scene, shown, findings);
    Ok(())
}

fn started_in(
    events: &[AgentEvent],
    workspace: &Path,
    sessions: usize,
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
        if own.is_empty() {
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
    let waited: BTreeSet<&String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Waiting {
                tool_use_id: Some(t),
                ..
            } => Some(t),
            _ => None,
        })
        .collect();
    let ended = events.iter().any(
        |e| matches!(e, AgentEvent::ActivityEnd { tool_use_id: Some(t), .. } if waited.contains(t)),
    );
    if !ended {
        findings.push((
            Label::Fail,
            "the tool the slot waited on never ended".into(),
        ));
    }
}

fn subagent(
    events: &[AgentEvent],
    reached: &[Reach],
    inference: ChildEndInference,
    findings: &mut Vec<(Label, String)>,
) {
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
    let ended = |child: AgentId, parent: AgentId| {
        events.iter().any(|e| match e {
            AgentEvent::SessionEnd { agent_id, .. } => *agent_id == child,
            // The reducer's b1 cascade walks a drained Task's subtree out.
            AgentEvent::ActivityEnd {
                agent_id,
                tool_use_id: Some(t),
            } if inference != ChildEndInference::None => {
                *agent_id == parent && tasks.contains(&(parent, t.as_str()))
            }
            _ => false,
        })
    };
    if children.is_empty() {
        findings.push((Label::Fail, "no session started as another's child".into()));
    } else if !children.iter().all(|&(child, parent)| ended(child, parent)) {
        findings.push((Label::Fail, "the child session never ended".into()));
    }
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
    if !reached.contains(&Reach::Delegating) {
        findings.push((Label::Fail, "the parent never went Delegating".into()));
    }
    if !reached.contains(&Reach::Active) {
        findings.push((Label::Fail, "the child's tool never went Active".into()));
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
    let frames: Vec<&str> = scene
        .lines()
        .filter_map(|l| l.split_once("agents=[").map(|(_, agents)| agents))
        .collect();
    let state = format!(":{shown}");
    let shown_in = |frame: &str| {
        frame
            .split(", ")
            .any(|a| a.starts_with(&prefix) && a.contains(&state))
    };
    if !frames.iter().any(|f| shown_in(f)) {
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

/// macOS's `/tmp` is `/private/tmp`: a CLI may report either.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
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
        run_of(&src, source, Shape::ToolRun, scene)
    }

    /// A run directory built from the recording `source`'s `shape` scenario
    /// writes, judged as that scenario's run.
    fn recorded(source: &str, shape: Shape, scene: &str) -> tempfile::TempDir {
        let fixture = profile(source)
            .and_then(|p| p.scenarios.iter().find(|s| s.shape == shape))
            .map(|s| s.fixture)
            .expect("a profiled scenario");
        let src = crate::harness::sources_root().join(fixture);
        run_of(&src, source, shape, scene)
    }

    fn run_of(src: &Path, source: &str, shape: Shape, scene: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        for f in jsonl_files_under(src) {
            let to = d.path().join(f.strip_prefix(src).expect("under"));
            std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
            std::fs::copy(&f, &to).expect("copy");
        }
        write_manifest(d.path(), source, |m| m.scenario = shape.name().into());
        std::fs::write(d.path().join(SCENE_LOG), scene).expect("scene");
        d
    }

    fn write_manifest(dir: &Path, source: &str, edit: impl FnOnce(&mut Manifest)) {
        let mut m = Manifest {
            source: source.into(),
            scenario: Shape::ToolRun.name().into(),
            capture: CaptureRecord {
                workspace: PathBuf::from(RECORDED_WORKSPACE),
                cli: "test".into(),
                version: "test".into(),
                exit_code: Some(0),
            },
            aimock: "test".into(),
            requests: 2,
            unmatched: 0,
        };
        edit(&mut m);
        std::fs::write(dir.join(MANIFEST), serde_json::to_string(&m).expect("json"))
            .expect("write");
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

    fn blank_transcripts(dir: &Path) {
        for t in jsonl_files_under(dir)
            .into_iter()
            .filter(|f| !is_hook_envelope(f))
        {
            std::fs::write(&t, "").expect("blank");
        }
    }

    fn has(v: &Verdict, label: Label, text: &str) -> bool {
        v.findings()
            .iter()
            .any(|f| f.0 == label && f.1.contains(text))
    }

    const CX_SCENE: &str = "agents=[cx\u{b7}proj@0:active(Bash: cat NOTE.txt)]\n\
                            agents=[cx\u{b7}proj@0:idle]\nagents=[] daemons=[]\n";
    const CC_SCENE: &str = "agents=[cc\u{b7}proj@0:active(Read: NOTE.txt)]\nagents=[] daemons=[]\n";

    const DS_WAIT_SCENE: &str =
        "agents=[ds\u{b7}proj@0:waiting(bash: escalate)]\nagents=[] daemons=[]\n";
    const DS_DELEGATE_SCENE: &str = "agents=[ds\u{b7}proj@0:active(Delegating), \
                                     ds\u{b7}proj@1:active(read)]\nagents=[] daemons=[]\n";
    const GK_SCENE: &str = "agents=[gk\u{b7}proj@0:active(read_file)]\nagents=[] daemons=[]\n";
    const GK_DELEGATE_SCENE: &str = "agents=[gk\u{b7}proj@0:active(Delegating), \
                                     Read the note@1:active(read_file)]\nagents=[] daemons=[]\n";
    const RX_DELEGATE_SCENE: &str = "agents=[rx\u{b7}proj@0:active(Delegating), \
                                     rx\u{b7}proj@1:active(read_file)]\nagents=[] daemons=[]\n";

    #[test]
    fn a_recorded_dsh_permission_run_passes() {
        let d = recorded("dsh", Shape::Permission, DS_WAIT_SCENE);
        let v = verify(d.path()).expect("verify");
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
    }

    #[test]
    fn a_permission_run_that_never_waits_fails() {
        let d = recorded("dsh", Shape::Permission, DS_WAIT_SCENE);
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
            Shape::Permission,
            "agents=[ds\u{b7}proj@0:active(bash)]\n",
        );
        let v = verify(d.path()).expect("verify");
        assert!(has(&v, Label::Fail, "agent waiting"), "{:?}", v.findings());
    }

    #[test]
    fn a_wait_whose_tool_never_ends_fails() {
        let d = recorded("dsh", Shape::Permission, DS_WAIT_SCENE);
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
        let d = recorded("dsh", Shape::Permission, DS_WAIT_SCENE);
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
        let d = recorded("dsh", Shape::Permission, DS_WAIT_SCENE);
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
        let clean = recorded("claude-code", Shape::Subagent, CC_DELEGATE_SCENE);
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
        let d = recorded("claude-code", Shape::Subagent, CC_DELEGATE_SCENE);
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
        let d = recorded("dsh", Shape::Subagent, DS_DELEGATE_SCENE);
        edit_hooks(d.path(), |v| (!delegation(&v)).then_some(v));
        let v = verify(d.path()).expect("verify");
        assert!(
            has(&v, Label::Fail, "never went Delegating"),
            "{:?}",
            v.findings()
        );
        let d = recorded("dsh", Shape::Subagent, DS_DELEGATE_SCENE);
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
        let d = recorded("dsh", Shape::Subagent, DS_DELEGATE_SCENE);
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

    #[test]
    fn a_recorded_dsh_subagent_run_passes() {
        let d = recorded("dsh", Shape::Subagent, DS_DELEGATE_SCENE);
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
                let d = recorded(source, Shape::Subagent, scene);
                edit_hooks(d.path(), |v| {
                    let dropped = ends.iter().enumerate().any(|(i, e)| i != keep && e(&v));
                    (!dropped).then_some(v)
                });
                let v = verify(d.path()).expect("verify");
                assert!(!has(&v, Label::Fail, never), "{source}: {:?}", v.findings());
            }
            let d = recorded(source, Shape::Subagent, scene);
            edit_hooks(d.path(), |v| (!ends.iter().any(|e| e(&v))).then_some(v));
            let v = verify(d.path()).expect("verify");
            assert!(has(&v, Label::Fail, never), "{source}: {:?}", v.findings());
        }
        let d = recorded("dsh", Shape::Subagent, DS_DELEGATE_SCENE);
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
        let d = recorded("dsh", Shape::Subagent, scene);
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
    fn a_recorded_grok_run_passes_each_scenario() {
        for (shape, scene) in [
            (Shape::ToolRun, GK_SCENE),
            (Shape::Subagent, GK_DELEGATE_SCENE),
        ] {
            let d = recorded("grok", shape, scene);
            let v = verify(d.path()).expect("verify");
            assert_eq!(
                v.label(),
                Label::Pass,
                "{}: {:?}",
                shape.name(),
                v.findings()
            );
        }
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
    fn only_a_childs_registration_may_name_no_cwd_and_its_own_event_must() {
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
        let judged = |events: &[AgentEvent]| {
            let mut f = Vec::new();
            started_in(events, ws, 2, &mut f);
            Verdict::new(f)
        };
        let root = start("p", "/w", None);
        // grok's spawn line registers the child with no cwd.
        let child = start("c", "", Some("p"));
        let v = judged(&[root.clone(), child.clone(), backfill("c", "/w")]);
        assert_eq!(v.label(), Label::Pass, "{:?}", v.findings());
        // The root naming the workspace does not stand in for the child.
        let v = judged(&[root.clone(), child.clone()]);
        assert!(has(&v, Label::Fail, "never named"), "{:?}", v.findings());
        // A back-filled cwd is held to the workspace too.
        let v = judged(&[root, child, backfill("c", "/elsewhere")]);
        assert!(
            has(&v, Label::Fail, "is not the workspace"),
            "{:?}",
            v.findings()
        );
        // A root that loses its cwd is no child awaiting one.
        let v = judged(&[start("p", "", None), start("c", "/w", Some("p"))]);
        assert!(
            has(&v, Label::Fail, "is not the workspace"),
            "{:?}",
            v.findings()
        );
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
    fn a_not_yet_hook_any_run_fires_is_stale() {
        let d = run_from("codex", "tool-run-recorded", CX_SCENE);
        let mut first = true;
        edit_hooks(d.path(), |mut v| {
            if std::mem::take(&mut first) {
                v["hook_event_name"] = "SubagentStart".into();
            }
            Some(v)
        });
        let v = verify_source(
            profile("codex").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(has(&v, Label::Stale, "SubagentStart"), "{:?}", v.findings());
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

    #[test]
    fn a_not_yet_field_a_run_carries_is_stale() {
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        edit_hooks(d.path(), |mut v| {
            if v["hook_event_name"] == "SessionStart" {
                v["model"] = "claude-test".into();
            }
            Some(v)
        });
        let v = verify_source(
            profile("claude-code").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
        assert!(
            has(&v, Label::Stale, "hook:SessionStart /model"),
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
    fn a_committed_hook_the_profile_owes_needs_no_run() {
        // fixtures/claude-code/permission-recorded fires both; a tool run neither.
        let owed = ["hook:Notification", "hook:PermissionRequest"];
        let d = run_from("claude-code", "tool-run-recorded", CC_SCENE);
        let v = verify_source(
            profile("claude-code").expect("profile"),
            &[d.path().to_path_buf()],
        )
        .expect("verify");
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
