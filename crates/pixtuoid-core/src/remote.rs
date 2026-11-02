//! The office over a wire: the handshake that opens a stream, the line a server
//! writes per change, the office a device rebuilds from those lines to render
//! with its own engine, and what a device may ask between them.
//!
//! Not [`SceneState`] itself, which is internal and moves with every release,
//! while a server and a device update on their own schedules. A field crosses
//! only where `project_agent`, `project_daemon` or `project_today` names it, and a
//! desk never does: desks follow each device's layout, so it takes its own.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::AgentId;
use crate::source::decoder::{cwd_basename_label, display_safe, label_prefix_for};
use crate::state::{
    ActivityState, AgentSlot, DaemonInstanceId, DaemonLiveness, DaemonPresence, DaemonRoster,
    DaemonState, EffortObservation, GlobalDeskIndex, MAX_FLOORS, SceneState, SlotLabel, Today,
    ToolKind, ToolUse, TurnKey, UsageObservation,
};

mod change;

pub use change::{Changed, Outcome, not_detected, nothing_to_do, plugin_left_registered};

/// The wire version this build writes and reads. Within a version the line only
/// gains fields and tags, which `remote.v1.golden.json` holds it to; a reader
/// ignores what it doesn't know and maps an unknown tag to its nearest state.
pub const VERSION: u32 = 1;

/// How often a server writes its line however quiet the office: a device takes
/// silence past this for a dead link, and whoever didn't fit returns with a line.
pub const HEARTBEAT: Duration = Duration::from_secs(15);

/// How far a line's clock may stray from the skew already held before it is
/// taken for a clock change rather than delivery latency.
const MAX_CLOCK_DRIFT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    v: u32,
    now_ms: u64,
    agents: Vec<Agent>,
    daemons: Vec<Daemon>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    today: Option<Day>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Day {
    day: NaiveDate,
    tokens: u64,
    tool_calls: u64,
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
        cwd_basename: (!unknown_cwd).then(|| cwd_basename(cwd)).flatten(),
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

/// `model`, less its directories when it is an absolute path: a relative `a/b`
/// reads as a provider's id.
fn model_name(model: &str) -> &str {
    let path = Path::new(model);
    path.is_absolute()
        .then(|| path.file_name()?.to_str())
        .flatten()
        .unwrap_or(model)
}

/// The one name of `cwd` that crosses the wire, a single display-safe path
/// component, so a server and a device key an agent on the same string: a crafted
/// cwd can be one long component, and no decoder bounds it. `None` when nothing
/// survives.
pub fn cwd_basename(cwd: &Path) -> Option<String> {
    let name = display_safe(&cwd.file_name()?.to_string_lossy());
    Some(Path::new(&name).file_name()?.to_str()?.to_owned())
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

fn project_today(today: &Today) -> Day {
    let Today {
        day,
        tokens,
        tool_calls,
    } = *today;
    Day {
        day,
        tokens,
        tool_calls,
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
        today: scene.today.as_ref().map(project_today),
    })?;
    line.push('\n');
    Ok(line)
}

/// The entropy of a pairing code and of a device token.
pub const SECRET_BYTES: usize = 32;

/// SHA-256's output in bytes, which [`PairLink::spki`] carries as hex.
const PIN_BYTES: usize = 32;

const PAIR_LINK_PREFIX: &str = "pixtuoid://pair?";

/// What a device scans to reach a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairLink {
    /// A name or IPv4 address on the local network.
    pub host: String,
    /// The port `host` listens on.
    pub port: u16,
    /// The SHA-256 of the server key's SPKI in uppercase hex, which the TLS
    /// handshake must present: there is no CA to vouch for a server on a LAN.
    pub spki: String,
    /// The one-time code a [`Hello::Pair`] redeems, in uppercase hex.
    pub code: String,
}

impl fmt::Display for PairLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            host,
            port,
            spki,
            code,
        } = self;
        write!(
            f,
            "{PAIR_LINK_PREFIX}host={host}&port={port}&spki={spki}&code={code}"
        )
    }
}

/// Whether `host` can go in a [`PairLink`]: nothing that could end the field or
/// reach past a host.
pub fn is_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

/// A string that isn't a [`PairLink`].
#[derive(Debug, Error)]
#[error("not a pixtuoid pairing link")]
pub struct PairLinkError;

impl FromStr for PairLink {
    type Err = PairLinkError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let query = text.strip_prefix(PAIR_LINK_PREFIX).ok_or(PairLinkError)?;
        let field = |key: &str| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
                .ok_or(PairLinkError)
        };
        let hex = |key: &str, bytes: usize| {
            let value = field(key)?;
            let uppercase_hex = |b: u8| matches!(b, b'0'..=b'9' | b'A'..=b'F');
            (value.len() == bytes * 2 && value.bytes().all(uppercase_hex))
                .then(|| value.to_owned())
                .ok_or(PairLinkError)
        };
        let host = field("host")?;
        if !is_host(host) {
            return Err(PairLinkError);
        }
        let link = Self {
            host: host.to_owned(),
            port: field("port")?
                .parse()
                .ok()
                .filter(|port| *port != 0)
                .ok_or(PairLinkError)?,
            spki: hex("spki", PIN_BYTES)?,
            code: hex("code", SECRET_BYTES)?,
        };
        // A field read twice, or one a server never writes, reads as a forgery.
        (link.to_string() == text)
            .then_some(link)
            .ok_or(PairLinkError)
    }
}

/// The first line a device writes, once the TLS handshake is done. All it may
/// write after an acceptance is an [`Ask`].
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "hello", rename_all = "snake_case")]
pub enum Hello {
    /// First contact, redeeming a [`PairLink`]'s code.
    Pair {
        /// [`PairLink::code`].
        code: String,
    },
    /// Every later contact.
    Resume {
        /// What [`Reply::Paired`] gave this device.
        token: String,
    },
}

/// The server's reply to a [`Hello`]. Its [`encode`] lines follow an acceptance.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// The code was good, and this token stands in for it from now on.
    Paired {
        /// What a device sends as [`Hello::Resume`]; shown once, stored hashed.
        token: String,
    },
    /// The token is still good.
    Resumed,
    /// Pair again: the code was spent or stale, or the token forgotten.
    Refused,
    /// A reply from a newer server, which says nothing against the pairing.
    #[serde(other)]
    Unknown,
}

/// A line a device may write after an accepted [`Hello`], answered in the order
/// asked by one [`Answer`] among the server's [`encode`] lines.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum Ask {
    /// Every agent CLI the server knows, installed on its machine or not.
    Sources,
    /// Connect `source` when `on`, else disconnect it: the state wanted, so a
    /// retry changes nothing more.
    Change {
        /// Its [`SourceRow::id`].
        source: String,
        /// Whether it should be connected.
        on: bool,
    },
    /// An ask from a newer device, which keeps its stream.
    #[serde(other)]
    Unknown,
}

/// What a server writes for an [`Ask`], so a device that never asks never reads one.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// Every agent CLI this server knows, installed or not.
    Sources {
        /// One per CLI.
        rows: Vec<SourceRow>,
    },
    /// What an [`Ask::Change`] did.
    Changed(Changed),
    /// This server doesn't know the ask, or the source it names.
    Unsupported,
    /// An answer from a newer server.
    #[serde(other)]
    Unknown,
}

impl Answer {
    /// The answer `line` holds, or `None` for any other line: an [`encode`] line
    /// goes to [`RemoteOffice::apply`], which also refuses a broken one.
    pub fn from_line(line: &[u8]) -> Option<Self> {
        serde_json::from_slice(line).ok()
    }
}

/// One agent CLI the server knows, whether or not it is installed there.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRow {
    #[serde(deserialize_with = "display_safe_text")]
    id: String,
    #[serde(deserialize_with = "display_safe_text")]
    display_name: String,
    state: SourceState,
    attention: bool,
}

/// A row's text, a stranger's once it has crossed the wire, made safe to
/// display as it is read.
fn display_safe_text<'de, D: Deserializer<'de>>(text: D) -> Result<String, D::Error> {
    String::deserialize(text).map(|text| display_safe(&text))
}

impl SourceRow {
    /// `&'static str`s, so that no text a run reads can cross in a row.
    pub fn new(
        id: &'static str,
        display_name: &'static str,
        state: SourceState,
        attention: bool,
    ) -> Self {
        Self {
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            state,
            attention,
        }
    }

    /// Its registry id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name to show for it.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Whether it is connected, and whether its CLI is known missing.
    pub fn state(&self) -> SourceState {
        self.state
    }

    /// Whether the server says this source wants a look there.
    pub fn attention(&self) -> bool {
        self.attention
    }
}

/// How a [`SourceRow`] stands. Tagged so `other` can take a newer server's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceState {
    /// Bound, its CLI not known to be missing.
    Connected,
    /// Not bound.
    Disconnected,
    /// Its CLI isn't on this machine.
    NoCli {
        /// Whether its hooks are in all the same: they live in a config, so a
        /// disconnect needs no CLI.
        connected: bool,
    },
    /// A state from a newer server.
    #[serde(other)]
    Unknown,
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
            today,
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

        self.scene.today = today.map(|d| Today::new(d.day, d.tokens, d.tool_calls));
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
    let cwd = (agent.cwd_basename.as_deref())
        .and_then(|name| cwd_basename(Path::new(name)))
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
    use crate::source::decoder::MAX_DECODED_FIELD_CHARS;
    use serde_json::Value;

    const GOLDEN: &str = include_str!("remote.v1.golden.json");
    const AT: u64 = 1_800_000_000_000;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2027, 1, 15).expect("a date")
    }

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
        scene.today = Some(Today::new(day(), 1_200_000, 38));
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
        assert_eq!(scene.today, Some(Today::new(day(), 1_200_000, 38)));
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

    /// A day with nothing counted sends no field, and a reader drops the number
    /// it held, as it does on a line from a server older than the count.
    #[test]
    fn a_line_with_no_count_gives_no_number() {
        let line = sent(&SceneState::uniform(4));
        assert!(!line.contains("today"), "{line}");

        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &full_scene(), AT);
        assert!(office.scene().today.is_some());
        office.apply(line.as_bytes(), at(AT)).expect("it applies");
        assert_eq!(office.scene().today, None, "a stale number stays behind");
    }

    #[test]
    fn a_time_past_the_drift_reads_as_now() {
        let drift = MAX_CLOCK_DRIFT.as_millis() as u64;
        for (created, read) in [
            (AT + drift, AT + drift),
            (AT + drift + 1, AT),
            (u64::MAX, AT),
        ] {
            let mut office = RemoteOffice::new(caps(4));
            let mut line: Value = serde_json::from_str(GOLDEN).unwrap();
            for agent in line["agents"].as_array_mut().unwrap() {
                agent["created_ms"] = created.into();
            }
            let line = line.to_string();
            office.apply(line.as_bytes(), at(AT)).expect("it applies");
            let agents = office.scene().agents.values();
            let times: Vec<_> = agents.map(|a| a.created_at).collect();
            let exiting_unseen = 1;
            assert_eq!(times, vec![at(read); 8 - exiting_unseen], "{created}");
        }
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
    fn a_server_sends_the_clean_basename() {
        let dirty = format!("x\u{1b}\u{202e}{}", "y".repeat(MAX_DECODED_FIELD_CHARS * 2));
        let cwd = Path::new("/work").join(dirty);
        let id = AgentId::from_parts("claude-code", "a");
        let mut scene = crowd(&["a"]);
        scene.agents.get_mut(&id).unwrap().cwd = Arc::from(cwd.as_path());

        let line: Value = serde_json::from_str(&sent(&scene)).unwrap();
        let name = line["agents"][0]["cwd_basename"].as_str().unwrap();

        assert_eq!(Some(name), cwd_basename(&cwd).as_deref());
        assert!(!name.contains(['\u{1b}', '\u{202e}']), "{name}");
        assert!(
            name.chars().count() <= MAX_DECODED_FIELD_CHARS + 1,
            "{name}"
        );
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

    /// The handshake's wire, frozen as the snapshot's golden freezes its own.
    #[test]
    fn handshake_lines_keep_their_shape() {
        let wire = |hello: &Hello| serde_json::to_string(hello).expect("it encodes");
        assert_eq!(
            wire(&Hello::Pair { code: "c".into() }),
            r#"{"hello":"pair","code":"c"}"#
        );
        assert_eq!(
            wire(&Hello::Resume { token: "t".into() }),
            r#"{"hello":"resume","token":"t"}"#
        );
        let reply = |line: &str| serde_json::from_str::<Reply>(line).expect("it decodes");
        assert_eq!(
            reply(r#"{"reply":"paired","token":"t"}"#),
            Reply::Paired { token: "t".into() }
        );
        assert_eq!(reply(r#"{"reply":"resumed"}"#), Reply::Resumed);
        assert_eq!(reply(r#"{"reply":"refused"}"#), Reply::Refused);
    }

    /// A new tag must not send every older device back to a QR.
    #[test]
    fn a_reply_this_build_does_not_know_is_no_refusal() {
        let reply: Reply =
            serde_json::from_str(r#"{"reply":"from_the_future","extra":1}"#).expect("it decodes");
        assert_eq!(reply, Reply::Unknown);
    }

    /// An ask's wire, frozen as the handshake's is.
    #[test]
    fn asks_and_answers_keep_their_shape() {
        fn wire(value: &impl Serialize) -> String {
            serde_json::to_string(value).expect("it encodes")
        }
        let line = wire(&Ask::Sources);
        assert_eq!(line, r#"{"ask":"sources"}"#);
        assert_eq!(serde_json::from_str::<Ask>(&line).unwrap(), Ask::Sources);

        let rows = || {
            vec![
                SourceRow::new("codex", "Codex", SourceState::Connected, false),
                SourceRow::new("cursor", "Cursor", SourceState::Disconnected, true),
                SourceRow::new(
                    "grok",
                    "Grok",
                    SourceState::NoCli { connected: true },
                    false,
                ),
                SourceRow::new(
                    "kimi",
                    "Kimi",
                    SourceState::NoCli { connected: false },
                    false,
                ),
            ]
        };
        let line = wire(&Answer::Sources { rows: rows() });
        assert_eq!(
            line,
            concat!(
                r#"{"answer":"sources","rows":["#,
                r#"{"id":"codex","display_name":"Codex","state":{"kind":"connected"},"attention":false},"#,
                r#"{"id":"cursor","display_name":"Cursor","state":{"kind":"disconnected"},"attention":true},"#,
                r#"{"id":"grok","display_name":"Grok","state":{"kind":"no_cli","connected":true},"attention":false},"#,
                r#"{"id":"kimi","display_name":"Kimi","state":{"kind":"no_cli","connected":false},"attention":false}"#,
                "]}"
            )
        );
        assert_eq!(
            Answer::from_line(line.as_bytes()),
            Some(Answer::Sources { rows: rows() })
        );

        let line = wire(&Answer::Unsupported);
        assert_eq!(line, r#"{"answer":"unsupported"}"#);
        assert_eq!(
            Answer::from_line(line.as_bytes()),
            Some(Answer::Unsupported)
        );
    }

    #[test]
    fn a_row_reads_back_through_its_accessors() {
        let row = SourceRow::new("cursor", "Cursor", SourceState::Connected, true);
        assert_eq!(row.id(), "cursor");
        assert_eq!(row.display_name(), "Cursor");
        assert_eq!(row.state(), SourceState::Connected);
        assert!(row.attention());
    }

    /// A newer peer's tag keeps the stream, whichever end is the newer.
    #[test]
    fn what_a_newer_peer_adds_is_read_as_unknown() {
        let ask: Ask = serde_json::from_str(r#"{"ask":"from_the_future","x":1}"#).expect("an ask");
        assert_eq!(ask, Ask::Unknown);
        assert_eq!(
            Answer::from_line(br#"{"answer":"from_the_future","x":1}"#),
            Some(Answer::Unknown)
        );
        let line = br#"{"answer":"sources","rows":[{"id":"a","display_name":"A","state":{"kind":"from_the_future","x":1},"attention":false}]}"#;
        let Some(Answer::Sources { rows }) = Answer::from_line(line) else {
            panic!("an answer");
        };
        assert_eq!(rows[0].state(), SourceState::Unknown);
    }

    #[test]
    fn an_office_line_is_no_answer_and_an_answer_is_no_office_line() {
        assert_eq!(Answer::from_line(GOLDEN.as_bytes()), None);
        assert_eq!(Answer::from_line(sent(&full_scene()).as_bytes()), None);
        for broken in [&b""[..], b"not json", br#"{"answer":"sources"}"#] {
            assert_eq!(Answer::from_line(broken), None);
        }

        let mut office = RemoteOffice::new(caps(4));
        received(&mut office, &crowd(&["a"]), AT);
        let answer = serde_json::to_string(&Answer::Unsupported).unwrap();
        let err = office.apply(answer.as_bytes(), at(AT)).unwrap_err();
        assert!(matches!(err, ApplyError::Malformed(_)));
        assert_eq!(office.scene().agents.len(), 1);
    }

    #[test]
    fn a_hostile_row_is_bounded_and_stripped_where_it_enters() {
        let dirty = format!(
            "x\u{1b}[2J\u{202e}{}",
            "y".repeat(MAX_DECODED_FIELD_CHARS * 2)
        );
        let line = serde_json::json!({
            "answer": "sources",
            "rows": [{"id": dirty, "display_name": dirty, "state": {"kind": "connected"}, "attention": false}],
        })
        .to_string();
        let Some(Answer::Sources { rows }) = Answer::from_line(line.as_bytes()) else {
            panic!("an answer");
        };
        for text in [rows[0].id(), rows[0].display_name()] {
            assert!(!text.contains(['\u{1b}', '\u{202e}']), "{text}");
            assert!(
                text.chars().count() <= MAX_DECODED_FIELD_CHARS + 1,
                "{text}"
            );
        }
    }

    const EVERY_DIGIT: &str = "0123456789ABCDEF";

    fn link() -> PairLink {
        PairLink {
            host: "studio.local".into(),
            port: 47820,
            spki: EVERY_DIGIT.repeat(PIN_BYTES * 2 / EVERY_DIGIT.len()),
            code: EVERY_DIGIT.repeat(SECRET_BYTES * 2 / EVERY_DIGIT.len()),
        }
    }

    #[test]
    fn a_pair_link_survives_its_text() {
        for port in [1, 47820, u16::MAX] {
            let link = PairLink { port, ..link() };
            let text = link.to_string();
            assert!(text.starts_with("pixtuoid://pair?"), "{text}");
            assert_eq!(text.parse::<PairLink>().expect("it parses"), link);
        }
    }

    /// A link can reach a device from anywhere a URL can, so nothing it carries
    /// is taken on trust.
    #[test]
    fn a_pair_link_refuses_what_a_server_never_writes() {
        let with = |key: &str, value: &str| {
            let mut fields = [
                ("host", "studio.local".to_owned()),
                ("port", "47820".to_owned()),
                ("spki", link().spki),
                ("code", link().code),
            ];
            let field = fields.iter_mut().find(|(k, _)| *k == key).expect("a field");
            field.1 = value.to_owned();
            let query: Vec<_> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("pixtuoid://pair?{}", query.join("&"))
        };
        // One character past each edge of the digits' ranges, at full length.
        let edges = ['/', ':', '@', 'G'].into_iter().flat_map(|edge| {
            let bad = |value: String| value.replacen('0', &edge.to_string(), 1);
            [
                with("spki", &bad(link().spki)),
                with("code", &bad(link().code)),
            ]
        });
        for bad in edges.chain([
            "https://pair?host=a".to_owned(),
            "pixtuoid://pair?host=a".to_owned(),
            with("host", ""),
            with("host", "evil.example/path"),
            with("host", "a@b"),
            with("port", "0"),
            with("port", "70000"),
            with("spki", &"A".repeat(PIN_BYTES * 2 - 1)),
            with("spki", &"A".repeat(PIN_BYTES * 2 + 1)),
            with("spki", &"zz".repeat(PIN_BYTES)),
            with("spki", &"ab".repeat(PIN_BYTES)),
            with("code", ""),
            with("code", &"C".repeat(SECRET_BYTES * 2 - 1)),
            with("code", &"C".repeat(SECRET_BYTES * 2 + 1)),
            with("code", &"cd".repeat(SECRET_BYTES)),
            format!("{}&port=1", link()),
            format!("{}&extra=1", link()),
            link()
                .to_string()
                .replace("host=studio.local", "host=a&port=1&host=b"),
        ]) {
            assert!(bad.parse::<PairLink>().is_err(), "{bad}");
        }
    }
}
