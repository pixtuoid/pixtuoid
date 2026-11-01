//! The office over a wire: the line a server writes per change, and the office
//! a device rebuilds from those lines to render with its own engine.
//!
//! Not [`SceneState`] itself, which is internal and moves with every release,
//! while a server and an installed App update on their own schedules. A field
//! crosses only where `project_agent` names it, and a seat never does: seats
//! follow each viewer's layout, so the device takes its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AgentId;
use crate::source::decoder::{cwd_basename_label, label_prefix_for};
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
    /// The working directory's last component, the one project name that leaves
    /// the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
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

/// Internally tagged because only such an enum may carry `other`
/// (<https://serde.rs/variant-attrs.html>), as is [`Tool`] and [`DaemonActivity`].
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

fn millis(t: SystemTime) -> u64 {
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
        repo: (!unknown_cwd)
            .then(|| cwd.file_name())
            .flatten()
            .map(|name| name.to_string_lossy().into_owned()),
        activity: match state {
            ActivityState::Idle => Activity::Idle,
            ActivityState::Running { item } => Activity::Running {
                tool: item.as_ref().map(|use_| use_.kind.into()),
            },
            ActivityState::RequiresAction { reason: _ } => Activity::RequiresAction,
        },
        state_since_ms: millis(*state_started_at),
        created_ms: millis(*created_at),
        exiting_ms: exiting_at.map(millis),
        tool_calls: *tool_call_count,
        running_ms: *running_ms,
        tokens: *tokens_used,
        last_usage: last_usage.map(|u| Usage {
            delta: u.delta,
            at_ms: millis(u.seen_at),
        }),
        model: model.as_deref().map(str::to_owned),
        effort: effort.as_ref().map(|e| Effort {
            value: e.value.to_string(),
            at_ms: millis(e.seen_at),
        }),
    }
}

/// Projected rather than the stored axes: `in_flight_turns` never serializes.
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
        entered_ms: millis(*entered_at),
        last_seen_ms: millis(*last_seen),
    }
}

/// `scene` as one newline-terminated wire line, stamped with the sender's `now`.
///
/// # Errors
///
/// If JSON encoding fails.
pub fn encode(scene: &SceneState, now: SystemTime) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(&Snapshot {
        v: VERSION,
        now_ms: millis(now),
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
    /// The sender writes a wire version this build doesn't read.
    #[error("the sender writes wire version {v}, this build reads {VERSION}")]
    Incompatible {
        /// The version the line carries.
        v: u32,
    },
    /// The line isn't a frame of the version it claims.
    #[error("malformed office line")]
    Malformed(#[from] serde_json::Error),
}

/// The office a device rebuilds from a server's lines, ready to render.
#[derive(Debug)]
pub struct RemoteOffice {
    scene: SceneState,
    /// The device's clock minus the sender's, in ms; held from line to line.
    skew_ms: Option<i64>,
}

impl RemoteOffice {
    /// An empty office whose floors seat `capacities`, the device's own layout.
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

    /// Take a new layout: everyone is reseated, oldest first, since a desk means
    /// nothing in the old one. Whoever doesn't fit returns with a later line.
    pub fn set_capacities(&mut self, capacities: [usize; MAX_FLOORS]) {
        self.scene.floor_capacities = capacities;
        let mut agents: Vec<_> = std::mem::take(&mut self.scene.agents)
            .into_values()
            .collect();
        agents.sort_by_key(|slot| (slot.created_at, slot.agent_id));
        for mut slot in agents {
            if let Some((desk_index, floor_idx)) = self.free_seat() {
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
    /// If the line is another wire version, or isn't a frame.
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
        let local = |ms: u64| UNIX_EPOCH + Duration::from_millis(ms.saturating_add_signed(skew));

        agents.sort_by_key(|agent| (agent.created_ms, agent.id));
        self.scene
            .agents
            .retain(|id, _| agents.iter().any(|agent| agent.id == *id));
        for agent in agents {
            let seat = match self.scene.agents.get(&agent.id) {
                Some(slot) => Some((slot.desk_index, slot.floor_idx)),
                None => self.free_seat(),
            };
            if let Some(seat) = seat {
                self.scene
                    .agents
                    .insert(agent.id, inflate(agent, seat, &local));
            }
        }

        self.scene.daemons = DaemonRoster::default();
        for daemon in daemons {
            if let Some(instance) = DaemonInstanceId::new(daemon.instance.as_str()) {
                let presence = inflate_daemon(&daemon, &local);
                self.scene.insert_daemon(&daemon.source, instance, presence);
            }
        }
        Ok(())
    }

    /// The skew to read a line sent at `sent_ms` by. Held while it stays within
    /// [`MAX_CLOCK_DRIFT`]: a walk restarts when `state_started_at` moves, so a
    /// line's own latency must not move it.
    fn skew_for(&mut self, sent_ms: u64, now: SystemTime) -> i64 {
        let measured = i64::try_from(millis(now))
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(sent_ms).unwrap_or(i64::MAX));
        let drift = u64::try_from(MAX_CLOCK_DRIFT.as_millis()).unwrap_or(u64::MAX);
        match self.skew_ms {
            Some(held) if held.abs_diff(measured) <= drift => held,
            _ => {
                self.skew_ms = Some(measured);
                measured
            }
        }
    }

    fn free_seat(&self) -> Option<(GlobalDeskIndex, usize)> {
        let desk = self.scene.next_free_desk()?;
        Some((desk, self.scene.floor_of(desk)))
    }
}

fn inflate(
    agent: Agent,
    (desk_index, floor_idx): (GlobalDeskIndex, usize),
    local: &impl Fn(u64) -> SystemTime,
) -> AgentSlot {
    let prefix = label_prefix_for(&agent.source);
    let cwd = agent
        .repo
        .as_ref()
        .map_or_else(PathBuf::new, |repo| Path::new("/").join(repo));
    let label = cwd_basename_label(prefix, &cwd).map_or_else(
        || SlotLabel::prefix_fallback(prefix),
        SlotLabel::cwd_derived,
    );
    let state_started_at = local(agent.state_since_ms);
    AgentSlot {
        agent_id: agent.id,
        source: Arc::from(agent.source.as_str()),
        // What `badge::disambig_suffix` tells two agents of one repo apart by.
        session_id: Arc::from(format!("{:016x}", agent.id.raw())),
        unknown_cwd: agent.repo.is_none(),
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
        model: agent.model.map(Arc::from),
        effort: agent
            .effort
            .map(|e| EffortObservation::new(Arc::from(e.value), local(e.at_ms))),
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
        // `display_state` reads Running off the turn set, so one stands in.
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

    /// Every wire field and variant, as `remote.v1.golden.json` froze them.
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
        scene
    }

    fn sent(scene: &SceneState) -> String {
        encode(scene, at(AT)).expect("a scene encodes")
    }

    /// `office` after `scene` arrives on a device whose clock reads `now`.
    fn received(office: &mut RemoteOffice, scene: &SceneState, now: u64) {
        let line = encode(scene, at(AT)).expect("a scene encodes");
        office.apply(line.as_bytes(), at(now)).expect("it applies");
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
    fn a_v1_frame_still_applies() {
        let mut office = RemoteOffice::new(caps(4));
        office.apply(GOLDEN.as_bytes(), at(AT)).expect("v1 applies");
        let scene = office.scene();
        assert_eq!(scene.agents.len(), 3);
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

    /// `old`'s every key and value type, at the same place in `new`: what a
    /// reader built on `old` goes on to read.
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
        assert_keeps(&golden, &line, "frame");
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
        let line = sent(&scene);
        for secret in [
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
            assert!(!line.contains(secret), "{secret} leaked: {line}");
        }
    }

    #[test]
    fn a_rename_does_not_reach_the_device() {
        let mut scene = crowd(&["repo"]);
        let id = AgentId::from_parts("claude-code", "repo");
        scene.agents.get_mut(&id).unwrap().label = SlotLabel::renamed("a session title");
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &scene, AT);
        assert_eq!(&*office.scene().agents[&id].label, "cc·repo");
    }

    #[test]
    fn seats_follow_the_devices_layout_not_the_senders() {
        let mut scene = crowd(&["a", "b", "c"]);
        for (i, slot) in scene.agents.values_mut().enumerate() {
            slot.desk_index = GlobalDeskIndex(9 + i);
        }
        let mut office = RemoteOffice::new([2, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        received(&mut office, &scene, AT);
        assert_eq!(desk_of(&office, "a"), GlobalDeskIndex(0));
        assert_eq!(desk_of(&office, "b"), GlobalDeskIndex(1));
        assert_eq!(desk_of(&office, "c"), GlobalDeskIndex(2));
        let c = AgentId::from_parts("claude-code", "c");
        assert_eq!(office.scene().agents[&c].floor_idx, 1);
    }

    #[test]
    fn a_seat_outlives_its_neighbours_and_a_leavers_seat_is_reused() {
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
        let mut office = RemoteOffice::new([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        received(&mut office, &crowd(&["a", "b"]), AT);
        assert_eq!(office.scene().agents.len(), 1);
        received(&mut office, &crowd(&["b"]), AT);
        assert_eq!(desk_of(&office, "b"), GlobalDeskIndex(0));
    }

    #[test]
    fn a_new_layout_reseats_everyone_oldest_first() {
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &crowd(&["a", "b", "c"]), AT);
        received(&mut office, &crowd(&["b", "c"]), AT);
        office.set_capacities(caps(2));
        assert_eq!(desk_of(&office, "b"), GlobalDeskIndex(0));
        assert_eq!(desk_of(&office, "c"), GlobalDeskIndex(1));
        assert_eq!(office.scene().floor_capacities, caps(2));
    }

    #[test]
    fn a_device_clock_ahead_of_the_sender_shifts_every_time_by_the_skew() {
        const SKEW: u64 = 5_000;
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &full_scene(), AT + SKEW);
        let scene = office.scene();
        let lead = &scene.agents[&AgentId::from_parts("claude-code", "lead")];
        assert_eq!(lead.state_started_at, at(AT - 2_000 + SKEW));
        assert_eq!(lead.created_at, at(AT - 60_000 + SKEW));
        assert_eq!(lead.last_usage.unwrap().seen_at, at(AT - 700 + SKEW));
        assert_eq!(lead.effort.as_ref().unwrap().seen_at, at(AT - 500 + SKEW));
        let child = &scene.agents[&AgentId::from_parts("claude-code", "child")];
        assert_eq!(child.exiting_at, Some(at(AT - 100 + SKEW)));
        let (_, _, gateway) = scene.daemons().next().unwrap();
        assert_eq!(gateway.entered_at, at(AT - 90_000 + SKEW));
        assert_eq!(gateway.last_seen, at(AT - 300 + SKEW));
    }

    // A walk restarts when `state_started_at` moves, so a frame's own latency
    // must not move it.
    #[test]
    fn jitter_leaves_an_agents_times_alone_and_a_clock_step_does_not() {
        let mut office = RemoteOffice::new(caps(4));
        let scene = full_scene();
        let id = AgentId::from_parts("claude-code", "lead");
        received(&mut office, &scene, AT);
        let first = office.scene().agents[&id].state_started_at;

        let late = AT + MAX_CLOCK_DRIFT.as_millis() as u64;
        received(&mut office, &scene, late);
        assert_eq!(office.scene().agents[&id].state_started_at, first);

        received(&mut office, &scene, late + 1);
        assert_ne!(office.scene().agents[&id].state_started_at, first);
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
    fn a_gateway_that_left_the_frame_leaves_the_office() {
        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &full_scene(), AT);
        received(&mut office, &crowd(&[]), AT);
        assert_eq!(office.scene().daemons().count(), 0);
    }

    // A newer sender's vocabulary and fields, which an installed App must read.
    #[test]
    fn what_a_newer_sender_adds_is_read_as_the_nearest_known_thing() {
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
