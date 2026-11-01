//! The office over a wire: the line a server writes per change, and the office
//! a device rebuilds from those lines to render with its own engine.
//!
//! Not [`SceneState`] itself, which is internal and moves with every release,
//! while a server and a device update on their own schedules. A field crosses
//! only where `project_agent` or `project_daemon` names it, and a desk never does:
//! desks follow each device's layout, so it takes its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AgentId;
use crate::source::decoder::{
    MAX_DECODED_FIELD_CHARS, cwd_basename_label, display_safe, ellipsize, label_prefix_for,
};
use crate::state::{
    ActivityState, AgentSlot, DaemonInstanceId, DaemonLiveness, DaemonPresence, DaemonRoster,
    DaemonState, EffortObservation, GlobalDeskIndex, MAX_FLOORS, SceneState, SlotLabel, ToolKind,
    ToolUse, TurnKey, UsageObservation,
};

/// The wire version this build writes and reads. Within a version the line only
/// gains fields and tags, which `remote.v1.golden.json` holds it to; a reader
/// ignores what it doesn't know and maps an unknown tag to its nearest state.
pub const VERSION: u32 = 1;

/// How far a line's clock may stray from the skew already held before it is
/// taken for a clock change rather than delivery latency.
const MAX_CLOCK_DRIFT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    v: u32,
    now_ms: u64,
    agents: Vec<Agent>,
    daemons: Vec<Daemon>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Agent {
    id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<AgentId>,
    source: String,
    /// The one project name that leaves the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd_basename: Option<String>,
    activity: Activity,
    state_since_ms: u64,
    created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exiting_ms: Option<u64>,
    tool_calls: u32,
    running_ms: u64,
    tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort: Option<Effort>,
}

/// Tagged so `other` can take a newer server's state
/// (<https://serde.rs/variant-attrs.html>), as are [`Tool`] and [`DaemonActivity`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Activity {
    Idle,
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<Tool>,
    },
    RequiresAction,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Tool {
    Task,
    Edit,
    Read,
    Bash,
    Search,
    #[serde(other)]
    Other,
}

#[derive(Debug, Serialize, Deserialize)]
struct Usage {
    delta: u64,
    at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Effort {
    value: String,
    at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Daemon {
    source: String,
    instance: String,
    activity: DaemonActivity,
    active_sessions: u32,
    entered_ms: u64,
    last_seen_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum DaemonActivity {
    Idle,
    Running,
    Degraded,
    Down,
    #[serde(other)]
    Unknown,
}

impl From<ToolKind> for Tool {
    fn from(kind: ToolKind) -> Self {
        match kind {
            ToolKind::Task => Self::Task,
            ToolKind::Edit => Self::Edit,
            ToolKind::Read => Self::Read,
            ToolKind::Bash => Self::Bash,
            ToolKind::Search => Self::Search,
            ToolKind::Other => Self::Other,
        }
    }
}

impl From<Tool> for ToolKind {
    fn from(tool: Tool) -> Self {
        match tool {
            Tool::Task => Self::Task,
            Tool::Edit => Self::Edit,
            Tool::Read => Self::Read,
            Tool::Bash => Self::Bash,
            Tool::Search => Self::Search,
            Tool::Other => Self::Other,
        }
    }
}

fn epoch_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// No `..`: a field added to [`AgentSlot`] must be ruled in or out here.
fn project_agent(slot: &AgentSlot) -> Agent {
    let AgentSlot {
        agent_id,
        source,
        session_id: _,
        cwd,
        label: _,
        state,
        state_started_at,
        last_event_at: _,
        created_at,
        exiting_at,
        desk_index: _,
        floor_idx: _,
        tool_call_count,
        running_ms,
        unknown_cwd,
        parent_id,
        pid: _,
        model,
        effort,
        tokens_used,
        last_usage,
    } = slot;
    Agent {
        id: *agent_id,
        parent: *parent_id,
        source: source.to_string(),
        // Capped here, unlike the decoders' bounded fields: the label's cap is on
        // its own derivation, and a crafted cwd can be one long component.
        cwd_basename: (!unknown_cwd)
            .then(|| cwd.file_name())
            .flatten()
            .map(|name| ellipsize(&name.to_string_lossy(), MAX_DECODED_FIELD_CHARS)),
        activity: match state {
            ActivityState::Idle => Activity::Idle,
            ActivityState::Running { item } => Activity::Running {
                tool: item.as_ref().map(|use_| use_.kind.into()),
            },
            ActivityState::RequiresAction { reason: _ } => Activity::RequiresAction,
        },
        state_since_ms: epoch_ms(*state_started_at),
        created_ms: epoch_ms(*created_at),
        exiting_ms: exiting_at.map(epoch_ms),
        tool_calls: *tool_call_count,
        running_ms: *running_ms,
        tokens: *tokens_used,
        last_usage: last_usage.map(|u| Usage {
            delta: u.delta,
            at_ms: epoch_ms(u.seen_at),
        }),
        model: model.as_deref().map(model_name).map(str::to_owned),
        effort: effort.as_ref().map(|e| Effort {
            value: e.value.to_string(),
            at_ms: epoch_ms(e.seen_at),
        }),
    }
}

/// `model`, less the directories when a local model goes by its path.
fn model_name(model: &str) -> &str {
    let path = Path::new(model);
    path.is_absolute()
        .then(|| path.file_name()?.to_str())
        .flatten()
        .unwrap_or(model)
}

/// Through [`DaemonPresence::display_state`], the one ranking of its axes; a
/// turn's key stays home.
fn project_daemon(source: &str, instance: &DaemonInstanceId, presence: &DaemonPresence) -> Daemon {
    let DaemonPresence {
        liveness: _,
        active_sessions,
        last_seen,
        entered_at,
        in_flight_turns: _,
        current_pid: _,
    } = presence;
    Daemon {
        source: source.to_owned(),
        instance: instance.as_str().to_owned(),
        activity: match presence.display_state() {
            DaemonState::Idle => DaemonActivity::Idle,
            DaemonState::Running => DaemonActivity::Running,
            DaemonState::Degraded => DaemonActivity::Degraded,
            DaemonState::Down => DaemonActivity::Down,
        },
        active_sessions: *active_sessions,
        entered_ms: epoch_ms(*entered_at),
        last_seen_ms: epoch_ms(*last_seen),
    }
}

/// `scene` as one newline-terminated wire line, stamped with the server's `now`.
///
/// # Errors
///
/// If JSON encoding fails.
pub fn encode(scene: &SceneState, now: SystemTime) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(&Snapshot {
        v: VERSION,
        now_ms: epoch_ms(now),
        agents: scene.agents.values().map(project_agent).collect(),
        daemons: scene
            .daemons()
            .map(|(source, instance, presence)| project_daemon(source, instance, presence))
            .collect(),
    })?;
    line.push('\n');
    Ok(line)
}

/// Why [`RemoteOffice::apply`] refused a line. The office is unchanged.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ApplyError {
    /// The server writes a wire version this build doesn't read.
    #[error("the server writes wire version {v}, this build reads {VERSION}")]
    Incompatible {
        /// The version the line carries.
        v: u32,
    },
    /// The line isn't one of the version it claims.
    #[error("malformed office line")]
    Malformed(#[from] serde_json::Error),
}

/// The office a device rebuilds from a server's lines, ready to render.
#[derive(Debug)]
pub struct RemoteOffice {
    scene: SceneState,
    /// The device's clock minus the server's, in ms; held from line to line.
    skew_ms: Option<i64>,
}

impl RemoteOffice {
    /// An empty office with `capacities` desks a floor, the device's own layout.
    pub fn new(capacities: [usize; MAX_FLOORS]) -> Self {
        Self {
            scene: SceneState::new(capacities),
            skew_ms: None,
        }
    }

    /// The office to render, its times on the device's clock.
    pub fn scene(&self) -> &SceneState {
        &self.scene
    }

    /// Take a new layout: everyone takes a new desk, oldest first, since the old
    /// one means nothing here. Whoever doesn't fit returns with a later line.
    pub fn set_capacities(&mut self, capacities: [usize; MAX_FLOORS]) {
        self.scene.floor_capacities = capacities;
        let mut agents: Vec<_> = std::mem::take(&mut self.scene.agents)
            .into_values()
            .collect();
        agents.sort_by_key(|slot| (slot.created_at, slot.agent_id));
        for mut slot in agents {
            if let Some((desk_index, floor_idx)) = self.free_desk() {
                slot.desk_index = desk_index;
                slot.floor_idx = floor_idx;
                self.scene.agents.insert(slot.agent_id, slot);
            }
        }
    }

    /// Take one line `encode` wrote, received at the device's `now`. An agent
    /// keeps its desk from line to line, and takes one when it first fits.
    ///
    /// # Errors
    ///
    /// If the line is another wire version, or malformed.
    pub fn apply(&mut self, line: &[u8], now: SystemTime) -> Result<(), ApplyError> {
        #[derive(Deserialize)]
        struct Version {
            v: u32,
        }
        let Version { v } = serde_json::from_slice(line)?;
        if v != VERSION {
            return Err(ApplyError::Incompatible { v });
        }
        let Snapshot {
            now_ms,
            mut agents,
            daemons,
            ..
        } = serde_json::from_slice(line)?;

        let skew = self.skew_for(now_ms, now);
        // A time this far ahead means nothing, and near a clock's end it would
        // panic the first `+` that reads it.
        let latest = now + MAX_CLOCK_DRIFT;
        let local = |ms: u64| {
            UNIX_EPOCH
                .checked_add(Duration::from_millis(ms.saturating_add_signed(skew)))
                .filter(|time| *time <= latest)
                .unwrap_or(now)
        };

        agents.sort_by_key(|agent| (agent.created_ms, agent.id));
        self.scene
            .agents
            .retain(|id, _| agents.iter().any(|agent| agent.id == *id));
        for agent in agents {
            let desk = match self.scene.agents.get(&agent.id) {
                Some(slot) => Some((slot.desk_index, slot.floor_idx)),
                // One never seen alive was never in the room to leave it.
                None if agent.exiting_ms.is_some() => None,
                None => self.free_desk(),
            };
            if let Some(desk) = desk {
                self.scene
                    .agents
                    .insert(agent.id, inflate_agent(agent, desk, &local));
            }
        }

        self.scene.daemons = DaemonRoster::default();
        for daemon in daemons {
            if let Some(instance) = DaemonInstanceId::new(display_safe(&daemon.instance)) {
                let presence = inflate_daemon(&daemon, &local);
                (self.scene.daemons).insert(&display_safe(&daemon.source), instance, presence);
            }
        }
        Ok(())
    }

    /// The skew to read a line sent at `sent_ms` by. Held while it stays within
    /// [`MAX_CLOCK_DRIFT`]: a walk restarts when `state_started_at` moves, so a
    /// line's own latency must not move it.
    fn skew_for(&mut self, sent_ms: u64, now: SystemTime) -> i64 {
        let measured = i64::try_from(epoch_ms(now))
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(sent_ms).unwrap_or(i64::MAX));
        match self.skew_ms {
            Some(held) if Duration::from_millis(held.abs_diff(measured)) <= MAX_CLOCK_DRIFT => held,
            _ => {
                self.skew_ms = Some(measured);
                measured
            }
        }
    }

    fn free_desk(&self) -> Option<(GlobalDeskIndex, usize)> {
        let desk = self.scene.next_free_desk()?;
        Some((desk, self.scene.floor_of(desk)))
    }
}

/// Every string here is a stranger's: the decoders' bounds a scene relies on hold
/// from this one entry.
fn inflate_agent(
    agent: Agent,
    (desk_index, floor_idx): (GlobalDeskIndex, usize),
    local: &impl Fn(u64) -> SystemTime,
) -> AgentSlot {
    let source = display_safe(&agent.source);
    let prefix = label_prefix_for(&source);
    let basename = agent.cwd_basename.as_deref().map(display_safe);
    let cwd = (basename.as_deref())
        .and_then(|name| Path::new(name).file_name())
        .map(PathBuf::from);
    let unknown_cwd = cwd.is_none();
    let cwd = cwd.unwrap_or_default();
    let label = cwd_basename_label(prefix, &cwd).map_or_else(
        || SlotLabel::prefix_fallback(prefix),
        SlotLabel::cwd_derived,
    );
    let state_started_at = local(agent.state_since_ms);
    AgentSlot {
        agent_id: agent.id,
        source: Arc::from(source.as_str()),
        // What `badge::disambig_suffix` tells two agents of one repo apart by.
        session_id: Arc::from(agent.id.to_string()),
        unknown_cwd,
        cwd: Arc::from(cwd.as_path()),
        label,
        state: match agent.activity {
            Activity::Idle | Activity::Unknown => ActivityState::Idle,
            Activity::Running { tool } => ActivityState::Running {
                item: tool.map(|tool| ToolUse {
                    tool_use_id: None,
                    detail: None,
                    kind: tool.into(),
                }),
            },
            Activity::RequiresAction => ActivityState::RequiresAction { reason: None },
        },
        state_started_at,
        last_event_at: state_started_at,
        created_at: local(agent.created_ms),
        exiting_at: agent.exiting_ms.map(local),
        desk_index,
        floor_idx,
        tool_call_count: agent.tool_calls,
        running_ms: agent.running_ms,
        parent_id: agent.parent,
        pid: None,
        model: agent.model.as_deref().map(display_safe).map(Arc::from),
        effort: agent
            .effort
            .map(|e| EffortObservation::new(Arc::from(display_safe(&e.value)), local(e.at_ms))),
        tokens_used: agent.tokens,
        last_usage: agent
            .last_usage
            .map(|u| UsageObservation::new(u.delta, local(u.at_ms))),
    }
}

fn inflate_daemon(daemon: &Daemon, local: &impl Fn(u64) -> SystemTime) -> DaemonPresence {
    let last_seen = local(daemon.last_seen_ms);
    let (liveness, running) = match daemon.activity {
        DaemonActivity::Idle | DaemonActivity::Unknown => (DaemonLiveness::UP, false),
        DaemonActivity::Running => (DaemonLiveness::UP, true),
        DaemonActivity::Degraded => (DaemonLiveness::Up { degraded: true }, false),
        DaemonActivity::Down => (DaemonLiveness::Down, false),
    };
    DaemonPresence {
        liveness,
        active_sessions: daemon.active_sessions,
        last_seen,
        entered_at: local(daemon.entered_ms),
        // `display_state` reads Running off the turn set, so one stands in, and
        // the mascot draws one turn's bubbles however many the server has.
        in_flight_turns: running
            .then_some((TurnKey::Seq(0), last_seen))
            .into_iter()
            .collect(),
        current_pid: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const GOLDEN: &str = include_str!("remote.v1.golden.json");
    const AT: u64 = 1_800_000_000_000;

    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    const ONE_DESK: [usize; MAX_FLOORS] = {
        let mut capacities = [0; MAX_FLOORS];
        capacities[0] = 1;
        capacities
    };

    fn caps(desks: usize) -> [usize; MAX_FLOORS] {
        [desks; MAX_FLOORS]
    }

    fn slot(name: &str, state: ActivityState) -> AgentSlot {
        AgentSlot {
            agent_id: AgentId::from_parts("claude-code", name),
            source: Arc::from("claude-code"),
            session_id: Arc::from(format!("session-of-{name}")),
            cwd: Arc::from(Path::new(&format!("/Users/someone/work/{name}"))),
            label: SlotLabel::cwd_derived(format!("cc·{name}")),
            state,
            state_started_at: at(AT - 2_000),
            last_event_at: at(AT - 1_000),
            created_at: at(AT - 60_000),
            exiting_at: None,
            desk_index: GlobalDeskIndex(0),
            floor_idx: 0,
            tool_call_count: 3,
            running_ms: 4_000,
            unknown_cwd: false,
            parent_id: None,
            pid: None,
            model: None,
            effort: None,
            tokens_used: 0,
            last_usage: None,
        }
    }

    /// Every wire field and tag, as `remote.v1.golden.json` froze them.
    fn full_scene() -> SceneState {
        let mut scene = SceneState::uniform(4);
        let running = ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: Some(Arc::from("toolu_1")),
                detail: Some(Arc::from("Edit /Users/someone/work/secret.rs")),
                kind: ToolKind::Edit,
            }),
        };
        let mut lead = slot("lead", running);
        lead.model = Some(Arc::from("some-model"));
        lead.effort = Some(EffortObservation::new(Arc::from("high"), at(AT - 500)));
        lead.tokens_used = 12_000;
        lead.last_usage = Some(UsageObservation::new(800, at(AT - 700)));
        let mut child = slot(
            "child",
            ActivityState::RequiresAction {
                reason: Some(Arc::from("approve rm -rf /Users/someone")),
            },
        );
        child.parent_id = Some(lead.agent_id);
        child.exiting_at = Some(at(AT - 100));
        let mut ghost = slot("ghost", ActivityState::Idle);
        ghost.unknown_cwd = true;
        for s in [lead, child, ghost] {
            scene.agents.insert(s.agent_id, s);
        }
        for (name, kind) in [
            ("task", ToolKind::Task),
            ("read", ToolKind::Read),
            ("bash", ToolKind::Bash),
            ("search", ToolKind::Search),
            ("other", ToolKind::Other),
        ] {
            let item = Some(ToolUse {
                tool_use_id: None,
                detail: None,
                kind,
            });
            let s = slot(name, ActivityState::Running { item });
            scene.agents.insert(s.agent_id, s);
        }
        scene.insert_daemon(
            "openclaw",
            DaemonInstanceId::new("18789").expect("not blank"),
            DaemonPresence {
                liveness: DaemonLiveness::UP,
                active_sessions: 2,
                last_seen: at(AT - 300),
                entered_at: at(AT - 90_000),
                in_flight_turns: std::collections::HashMap::from([(
                    TurnKey::Id(Arc::from("run-1")),
                    at(AT - 300),
                )]),
                current_pid: Some(4242),
            },
        );
        for (instance, liveness) in [
            ("18790", DaemonLiveness::UP),
            ("18791", DaemonLiveness::Up { degraded: true }),
            ("18792", DaemonLiveness::Down),
        ] {
            scene.insert_daemon(
                "openclaw",
                DaemonInstanceId::new(instance).expect("not blank"),
                DaemonPresence {
                    liveness,
                    active_sessions: 0,
                    last_seen: at(AT - 300),
                    entered_at: at(AT - 90_000),
                    in_flight_turns: std::collections::HashMap::new(),
                    current_pid: None,
                },
            );
        }
        scene
    }

    fn sent(scene: &SceneState) -> String {
        encode(scene, at(AT)).expect("a scene encodes")
    }

    fn received(office: &mut RemoteOffice, scene: &SceneState, device_now: u64) {
        let line = sent(scene);
        office
            .apply(line.as_bytes(), at(device_now))
            .expect("it applies");
    }

    fn desk_of(office: &RemoteOffice, name: &str) -> GlobalDeskIndex {
        office.scene().agents[&AgentId::from_parts("claude-code", name)].desk_index
    }

    fn crowd(names: &[&str]) -> SceneState {
        let mut scene = SceneState::uniform(16);
        for (i, name) in names.iter().enumerate() {
            let mut s = slot(name, ActivityState::Idle);
            s.created_at = at(AT - 1_000 + i as u64);
            scene.agents.insert(s.agent_id, s);
        }
        scene
    }

    #[test]
    fn a_v1_line_still_applies() {
        let mut office = RemoteOffice::new(caps(4));
        office.apply(GOLDEN.as_bytes(), at(AT)).expect("v1 applies");
        let scene = office.scene();
        let exiting_unseen = 1;
        assert_eq!(scene.agents.len(), 8 - exiting_unseen);
        assert_eq!(scene.daemons().count(), 4);
        let lead = &scene.agents[&AgentId::from_parts("claude-code", "lead")];
        assert_eq!(&*lead.label, "cc·lead");
        assert_eq!(lead.tokens_used, 12_000);
        assert!(matches!(
            lead.state,
            ActivityState::Running {
                item: Some(ToolUse {
                    kind: ToolKind::Edit,
                    ..
                })
            }
        ));
    }

    #[test]
    fn a_time_from_the_future_reads_as_now() {
        let mut office = RemoteOffice::new(caps(4));
        let mut line: Value = serde_json::from_str(GOLDEN).unwrap();
        for agent in line["agents"].as_array_mut().unwrap() {
            agent["created_ms"] = u64::MAX.into();
        }
        let line = line.to_string();
        office.apply(line.as_bytes(), at(AT)).expect("it applies");
        let agents = office.scene().agents.values();
        assert!(agents.map(|a| a.created_at).all(|t| t == at(AT)));
    }

    #[test]
    fn a_hostile_line_is_bounded_and_stripped_where_it_enters() {
        let dirty = format!(
            "x\u{1b}[2J\u{202e}{}",
            "y".repeat(MAX_DECODED_FIELD_CHARS * 2)
        );
        let mut line: Value = serde_json::from_str(GOLDEN).unwrap();
        for agent in line["agents"].as_array_mut().unwrap() {
            agent["source"] = dirty.clone().into();
            agent["cwd_basename"] = dirty.clone().into();
            agent["model"] = dirty.clone().into();
            agent["effort"] = serde_json::json!({"value": dirty, "at_ms": AT});
        }
        for daemon in line["daemons"].as_array_mut().unwrap() {
            daemon["source"] = dirty.clone().into();
            daemon["instance"] = dirty.clone().into();
        }
        let line = line.to_string();

        let mut office = RemoteOffice::new(caps(4));
        office.apply(line.as_bytes(), at(AT)).expect("it applies");

        let clean = |text: &str| {
            let is_clean = !text.contains(['\u{1b}', '\u{202e}']);
            assert!(is_clean && text.chars().count() <= MAX_DECODED_FIELD_CHARS + 1);
        };
        let scene = office.scene();
        for agent in scene.agents.values() {
            clean(&agent.source);
            clean(agent.cwd.to_str().unwrap());
            clean(agent.model.as_deref().unwrap());
            clean(&agent.effort.as_ref().unwrap().value);
        }
        for (source, instance, _) in scene.daemons() {
            clean(source);
            clean(instance.as_str());
        }
    }

    #[test]
    fn a_basename_with_a_path_in_it_stays_one_component() {
        for (sent, kept) in [
            ("/etc/passwd", Some("passwd")),
            ("../x", Some("x")),
            ("..", None),
            ("/", None),
        ] {
            let mut line: Value = serde_json::from_str(GOLDEN).unwrap();
            line["agents"][0]["cwd_basename"] = sent.into();
            let id: AgentId = serde_json::from_value(line["agents"][0]["id"].clone()).unwrap();
            let line = line.to_string();
            let mut office = RemoteOffice::new(caps(4));
            office.apply(line.as_bytes(), at(AT)).expect("it applies");
            let agent = &office.scene().agents[&id];
            assert_eq!(agent.cwd.to_str(), Some(kept.unwrap_or("")), "{sent}");
            assert_eq!(agent.unknown_cwd, kept.is_none(), "{sent}");
        }
    }

    /// `old`'s every key, value type and string, at the same place in `new`:
    /// what a reader built on `old` goes on to read, a tag above all.
    fn assert_keeps(old: &Value, new: &Value, at: &str) {
        match (old, new) {
            (Value::Object(old), Value::Object(new)) => {
                for (k, v) in old {
                    let found = new.get(k).unwrap_or_else(|| panic!("{at}.{k} is gone"));
                    assert_keeps(v, found, &format!("{at}.{k}"));
                }
            }
            (Value::Array(old), Value::Array(new)) => {
                assert!(new.len() >= old.len(), "{at} shrank");
                for (i, (o, n)) in old.iter().zip(new).enumerate() {
                    assert_keeps(o, n, &format!("{at}[{i}]"));
                }
            }
            (Value::String(old), Value::String(new)) => assert_eq!(old, new, "{at} changed"),
            _ => assert_eq!(
                std::mem::discriminant(old),
                std::mem::discriminant(new),
                "{at} changed type"
            ),
        }
    }

    #[test]
    fn this_version_writes_everything_a_v1_reader_reads() {
        let golden: Value = serde_json::from_str(GOLDEN).unwrap();
        let line: Value = serde_json::from_str(&sent(&full_scene())).unwrap();
        assert_keeps(&golden, &line, "line");
    }

    #[test]
    fn nothing_but_the_allowlist_reaches_the_wire() {
        let mut scene = full_scene();
        let id = AgentId::from_parts("claude-code", "lead");
        let lead = scene.agents.get_mut(&id).unwrap();
        lead.label = SlotLabel::renamed("/Users/someone/work/renamed-from-a-title");
        lead.pid = Some(crate::source::PidIdentity {
            pid: 919_191,
            started: None,
        });
        let local_model = std::env::temp_dir()
            .join("someone")
            .join("local-model.gguf");
        lead.model = Some(Arc::from(local_model.to_str().unwrap()));
        let line = sent(&scene);
        assert!(line.contains("\"model\":\"local-model.gguf\""), "{line}");
        for private in [
            "/Users",
            "someone",
            "session-of",
            "toolu_1",
            "secret.rs",
            "rm -rf",
            "renamed-from",
            "919191",
            "4242",
            "run-1",
        ] {
            assert!(!line.contains(private), "{private} left: {line}");
        }
    }

    /// `crowd` runs oldest first, against the id order the wire is in.
    #[test]
    fn desks_follow_the_devices_layout_not_the_servers() {
        let mut scene = crowd(&["c", "b", "a"]);
        for (i, slot) in scene.agents.values_mut().enumerate() {
            slot.desk_index = GlobalDeskIndex(9 + i);
        }
        let mut office = RemoteOffice::new([2, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        received(&mut office, &scene, AT);
        assert_eq!(desk_of(&office, "c"), GlobalDeskIndex(0));
        assert_eq!(desk_of(&office, "b"), GlobalDeskIndex(1));
        assert_eq!(desk_of(&office, "a"), GlobalDeskIndex(2));
        let a = AgentId::from_parts("claude-code", "a");
        assert_eq!(office.scene().agents[&a].floor_idx, 1);
    }

    #[test]
    fn a_desk_outlives_its_neighbours_and_a_leavers_desk_is_reused() {
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &crowd(&["a", "b", "c"]), AT);
        received(&mut office, &crowd(&["b", "c"]), AT);
        assert_eq!(desk_of(&office, "b"), GlobalDeskIndex(1));
        assert_eq!(office.scene().agents.len(), 2);
        received(&mut office, &crowd(&["b", "c", "d"]), AT);
        assert_eq!(desk_of(&office, "d"), GlobalDeskIndex(0));
    }

    #[test]
    fn an_agent_past_the_devices_desks_waits_for_one() {
        let mut office = RemoteOffice::new(ONE_DESK);
        received(&mut office, &crowd(&["b", "a"]), AT);
        assert_eq!(office.scene().agents.len(), 1);
        assert_eq!(desk_of(&office, "b"), GlobalDeskIndex(0));
        received(&mut office, &crowd(&["a"]), AT);
        assert_eq!(desk_of(&office, "a"), GlobalDeskIndex(0));
    }

    #[test]
    fn one_never_seen_alive_takes_no_desk_to_leave_it() {
        let mut office = RemoteOffice::new(ONE_DESK);
        received(&mut office, &crowd(&["a", "b", "c"]), AT);

        let mut scene = crowd(&["b", "c"]);
        let leaving = AgentId::from_parts("claude-code", "b");
        scene.agents.get_mut(&leaving).unwrap().exiting_at = Some(at(AT));
        received(&mut office, &scene, AT);

        assert!(!office.scene().agents.contains_key(&leaving));
        assert_eq!(desk_of(&office, "c"), GlobalDeskIndex(0));
    }

    #[test]
    fn a_new_layout_seats_the_oldest_and_the_rest_return_with_a_line() {
        let scene = crowd(&["c", "b", "a"]);
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &scene, AT);

        office.set_capacities(ONE_DESK);
        assert_eq!(office.scene().agents.len(), 1);
        assert_eq!(desk_of(&office, "c"), GlobalDeskIndex(0));
        assert_eq!(office.scene().floor_capacities, ONE_DESK);

        office.set_capacities(caps(2));
        received(&mut office, &scene, AT);
        assert_eq!(office.scene().agents.len(), 3);
        assert_eq!(desk_of(&office, "c"), GlobalDeskIndex(0));
    }

    #[test]
    fn a_clock_off_the_servers_shifts_every_time_by_the_skew() {
        for skew in [5_000, -5_000_i64] {
            let shifted = |ms: u64| at(ms.saturating_add_signed(skew));
            let now = AT.saturating_add_signed(skew);
            let child_id = AgentId::from_parts("claude-code", "child");
            let mut alive = full_scene();
            alive.agents.get_mut(&child_id).unwrap().exiting_at = None;
            let mut office = RemoteOffice::new(caps(4));
            received(&mut office, &alive, now);
            received(&mut office, &full_scene(), now);
            let scene = office.scene();
            let lead = &scene.agents[&AgentId::from_parts("claude-code", "lead")];
            assert_eq!(lead.state_started_at, shifted(AT - 2_000));
            assert_eq!(lead.created_at, shifted(AT - 60_000));
            assert_eq!(lead.last_usage.unwrap().seen_at, shifted(AT - 700));
            assert_eq!(lead.effort.as_ref().unwrap().seen_at, shifted(AT - 500));
            assert_eq!(scene.agents[&child_id].exiting_at, Some(shifted(AT - 100)));
            let (_, _, gateway) = scene.daemons().next().unwrap();
            assert_eq!(gateway.entered_at, shifted(AT - 90_000));
            assert_eq!(gateway.last_seen, shifted(AT - 300));
        }
    }

    #[test]
    fn jitter_leaves_an_agents_times_alone_and_a_clock_step_does_not() {
        let scene = full_scene();
        let id = AgentId::from_parts("claude-code", "lead");
        let drift = MAX_CLOCK_DRIFT.as_millis() as u64;
        for (inside, outside) in [(AT + drift, AT + drift + 1), (AT - drift, AT - drift - 1)] {
            let mut office = RemoteOffice::new(caps(4));
            received(&mut office, &scene, AT);
            let first = office.scene().agents[&id].state_started_at;

            received(&mut office, &scene, inside);
            assert_eq!(office.scene().agents[&id].state_started_at, first);

            received(&mut office, &scene, outside);
            assert_ne!(office.scene().agents[&id].state_started_at, first);
        }
    }

    #[test]
    fn a_running_gateway_is_still_running_on_the_device() {
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &full_scene(), AT);
        let (source, instance, gateway) = office.scene().daemons().next().unwrap();
        assert_eq!((source, instance.as_str()), ("openclaw", "18789"));
        assert_eq!(gateway.display_state(), DaemonState::Running);
        assert_eq!(gateway.active_sessions, 2);
    }

    #[test]
    fn every_daemon_state_survives_the_wire() {
        for (liveness, turns, state) in [
            (DaemonLiveness::UP, 0, DaemonState::Idle),
            (DaemonLiveness::UP, 1, DaemonState::Running),
            (
                DaemonLiveness::Up { degraded: true },
                0,
                DaemonState::Degraded,
            ),
            (DaemonLiveness::Down, 0, DaemonState::Down),
        ] {
            let mut scene = SceneState::uniform(1);
            scene.insert_daemon(
                "openclaw",
                DaemonInstanceId::new("1").expect("not blank"),
                DaemonPresence {
                    liveness,
                    active_sessions: 0,
                    last_seen: at(AT),
                    entered_at: at(AT),
                    in_flight_turns: (0..turns).map(|n| (TurnKey::Seq(n), at(AT))).collect(),
                    current_pid: None,
                },
            );
            let mut office = RemoteOffice::new(caps(1));
            received(&mut office, &scene, AT);
            let (_, _, got) = office.scene().daemons().next().unwrap();
            assert_eq!(got.display_state(), state);
        }
    }

    #[test]
    fn a_blank_gateway_id_drops_only_that_gateway() {
        let mut line: Value = serde_json::from_str(GOLDEN).unwrap();
        let mut blank = line["daemons"][0].clone();
        blank["instance"] = " ".into();
        line["daemons"].as_array_mut().unwrap().insert(0, blank);
        let line = line.to_string();

        let mut office = RemoteOffice::new(caps(4));
        office.apply(line.as_bytes(), at(AT)).expect("it applies");

        let daemons: Vec<_> = (office.scene().daemons())
            .map(|(_, instance, _)| instance.as_str())
            .collect();
        assert_eq!(daemons, ["18789", "18790", "18791", "18792"]);
    }

    #[test]
    fn a_gateway_that_left_the_line_leaves_the_office() {
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &full_scene(), AT);
        received(&mut office, &crowd(&[]), AT);
        assert_eq!(office.scene().daemons().count(), 0);
    }

    #[test]
    fn what_a_newer_server_adds_is_read_as_the_nearest_known_thing() {
        let line = format!(
            "{{\"v\":{VERSION},\"now_ms\":{AT},\"new_field\":1,\"agents\":[\
             {{\"id\":7,\"source\":\"a-new-cli\",\"activity\":{{\"state\":\"thinking\"}},\
             \"state_since_ms\":{AT},\"created_ms\":{AT},\"tool_calls\":0,\"running_ms\":0,\
             \"tokens\":0,\"new_field\":true}},\
             {{\"id\":8,\"source\":\"a-new-cli\",\
             \"activity\":{{\"state\":\"running\",\"tool\":{{\"kind\":\"a-new-tool\"}}}},\
             \"state_since_ms\":{AT},\"created_ms\":{AT},\"tool_calls\":0,\"running_ms\":0,\
             \"tokens\":0}}],\
             \"daemons\":[{{\"source\":\"openclaw\",\"instance\":\"1\",\
             \"activity\":{{\"state\":\"napping\"}},\"active_sessions\":0,\
             \"entered_ms\":{AT},\"last_seen_ms\":{AT}}}]}}\n"
        );
        let mut office = RemoteOffice::new(caps(4));
        office.apply(line.as_bytes(), at(AT)).expect("it applies");
        let agents: Vec<_> = office.scene().agents.values().collect();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].state, ActivityState::Idle);
        assert!(matches!(
            agents[1].state,
            ActivityState::Running {
                item: Some(ToolUse {
                    kind: ToolKind::Other,
                    ..
                })
            }
        ));
        let (_, _, gateway) = office.scene().daemons().next().unwrap();
        assert_eq!(gateway.display_state(), DaemonState::Idle);
    }

    #[test]
    fn another_version_is_refused_by_name_and_leaves_the_office_alone() {
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &crowd(&["a"]), AT);
        let line = format!("{{\"v\":{},\"changed\":\"everything\"}}\n", VERSION + 1);
        let err = office.apply(line.as_bytes(), at(AT)).unwrap_err();
        assert!(matches!(err, ApplyError::Incompatible { v } if v == VERSION + 1));
        assert_eq!(office.scene().agents.len(), 1);
    }

    #[test]
    fn a_broken_line_is_an_error_and_leaves_the_office_alone() {
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &crowd(&["a"]), AT);
        for line in ["", "not json", "{\"v\":1}"] {
            let err = office.apply(line.as_bytes(), at(AT)).unwrap_err();
            assert!(matches!(err, ApplyError::Malformed(_)), "{line:?}");
        }
        assert_eq!(office.scene().agents.len(), 1);
    }
}
