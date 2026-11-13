//! A demo office any front end plays: authored [`Cue`]s a [`Playhead`]
//! lands by the clock through the REAL [`Reducer`], so a demo shows only
//! states the reducer produces; each cast keeps to what its CLI's decoder
//! emits. Each front end authors its own cast.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::source::daemon::{self, DaemonInstanceKey, DaemonPresenceUpdate};
use pixtuoid_core::source::{RequestKeys, Resolution, registry};
use pixtuoid_core::state::reducer::Reducer;
use pixtuoid_core::state::{SceneState, TurnKey};
use pixtuoid_core::{AgentEvent, AgentId, ToolDetail, Transport};

/// One authored instant of a demo timeline: what lands, `at` after the
/// timeline's origin.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[expect(missing_docs, reason = "a field is named for what it holds")]
pub enum Cue {
    /// An agent event, as a live source sends it.
    Event {
        at: Duration,
        /// What hook-wins dedup reads: the transport the live source uses.
        transport: Transport,
        event: AgentEvent,
    },
    /// A daemon mascot's presence delta, which rides its own lane: the one
    /// event channel is `AgentId`-pure.
    Presence {
        at: Duration,
        key: DaemonInstanceKey,
        update: DaemonPresenceUpdate,
    },
}

impl Cue {
    /// Its authored offset from its timeline's origin.
    pub fn at(&self) -> Duration {
        match self {
            Cue::Event { at, .. } | Cue::Presence { at, .. } => *at,
        }
    }

    /// Land it at `origin` + [`Cue::at`], as lap 0: its call ids, usage
    /// reading ids and `Id` turns take a `#0` suffix, so an event a host
    /// applies by hand pairs with the suffixed id.
    pub fn land(&self, origin: SystemTime, reducer: &mut Reducer, scene: &mut SceneState) {
        self.land_on(origin + self.at(), Lap::FIRST, reducer, scene);
    }

    fn land_on(&self, now: SystemTime, lap: Lap, reducer: &mut Reducer, scene: &mut SceneState) {
        match self {
            Cue::Event {
                transport, event, ..
            } => {
                let mut event = event.clone();
                lap.name(&mut event);
                reducer.apply(scene, event, now, *transport);
            }
            Cue::Presence { key, update, .. } => {
                // What `Reducer::apply` does for exits: a delta landing late, in
                // a catch-up, finds a Down entry removed as a live sweep would.
                daemon::sweep_registered(scene, now, |_| true);
                daemon::apply_presence(scene, key, update.clone(), now);
            }
        }
    }
}

/// A loop played, which names its ids and turns apart from every other lap's:
/// the reducer counts an id once and refuses a closed turn's replay, so a
/// replayed lap would add no work.
#[derive(Debug, Clone, Copy)]
struct Lap {
    n: u64,
    /// One past the script's highest `Seq` turn, so each lap's `Seq` turns
    /// come after the last lap's.
    seq_stride: u64,
}

impl Lap {
    const FIRST: Self = Self {
        n: 0,
        seq_stride: 0,
    };

    fn name(self, event: &mut AgentEvent) {
        if let Some(id) = counted_id(event) {
            id.push_str(&format!("#{}", self.n));
        }
        match turn(event) {
            Some(TurnKey::Id(id)) => *id = format!("{id}#{}", self.n).into(),
            Some(TurnKey::Seq(seq)) => {
                *seq = seq.saturating_add(self.seq_stride.saturating_mul(self.n));
            }
            None => {}
        }
    }
}

/// The turn `event` names, wherever it carries one.
fn turn(event: &mut AgentEvent) -> Option<&mut TurnKey> {
    match event {
        AgentEvent::TurnStart { turn, .. }
        | AgentEvent::TurnEnd {
            turn: Some(turn), ..
        }
        | AgentEvent::ToolUsePending {
            turn: Some(turn), ..
        }
        | AgentEvent::UserActionRequest {
            turn: Some(turn), ..
        }
        | AgentEvent::UserActionResolved {
            by: Resolution::Turn(turn),
            ..
        } => Some(turn),
        _ => None,
    }
}

/// The id `event` names that the reducer counts once: a call's, wherever the
/// event carries it, or a reading's.
fn counted_id(event: &mut AgentEvent) -> Option<&mut String> {
    match event {
        AgentEvent::ToolUseStart {
            tool_use_id: Some(id),
            ..
        }
        | AgentEvent::ToolUseEnd {
            tool_use_id: Some(id),
            ..
        }
        | AgentEvent::UserActionRequest {
            tool_use_id: Some(id),
            ..
        }
        | AgentEvent::ToolUsePending {
            tool_use_id: id, ..
        }
        | AgentEvent::UserActionResolved {
            by: Resolution::Call(id),
            ..
        }
        | AgentEvent::Usage {
            reading_id: Some(id),
            ..
        } => Some(id),
        _ => None,
    }
}

/// Where `at` falls in a loop `period` long, and how many wraps it crossed to
/// get there: the laps since the one its timeline began in.
fn place(at: Duration, period: Duration) -> (Duration, u128) {
    let (at, period) = (at.as_nanos(), period.as_nanos());
    (Duration::from_nanos_u128(at % period), at / period)
}

/// One scripted call's length. Calls chain back to back: the reducer settles a
/// slot the moment its call ends, so any gap between them shows as Idle.
const CALL: Duration = Duration::from_millis(1_200);

/// A scripted session: the identity every cue it plays names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    id: AgentId,
    source: &'static str,
    session: String,
    cwd: PathBuf,
    parent: Option<AgentId>,
    /// One transport for its whole lifecycle, its live source's: after a hook
    /// end, the reducer refuses a transcript start for the straggler window.
    lifecycle: Transport,
}

impl Actor {
    /// A session of `source`, a source module's `SOURCE_NAME`.
    ///
    /// # Panics
    ///
    /// If no registered source is named `source`: its label would fall back to
    /// the raw string (`claude_code·api`, not `cc·api`).
    pub fn new(source: &'static str, session: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        let Some(descriptor) = registry::descriptor_for(source) else {
            panic!("{source:?} names no registered source");
        };
        let session = session.into();
        Self {
            id: AgentId::from_parts(source, &session),
            source,
            session,
            cwd: cwd.into(),
            parent: None,
            lifecycle: if descriptor.hook().is_some() {
                Transport::Hook
            } else {
                Transport::Jsonl
            },
        }
    }

    /// A subagent this session dispatches, in its source and cwd.
    #[must_use]
    pub fn subagent(&self, session: impl Into<String>) -> Self {
        Self {
            parent: Some(self.id),
            ..Self::new(self.source, session, self.cwd.clone())
        }
    }

    /// The agent every cue it plays names.
    pub fn id(&self) -> AgentId {
        self.id
    }

    /// Its `SessionStart`, a subagent's naming its parent.
    pub fn start(&self, at: Duration) -> Cue {
        self.cue(
            at,
            self.lifecycle,
            AgentEvent::SessionStart {
                agent_id: self.id,
                source: self.source.to_owned(),
                session_id: self.session.clone(),
                cwd: self.cwd.clone(),
                parent_id: self.parent,
            },
        )
    }

    /// Its transcript reader has caught up: without it, a usage reading grows
    /// the desk's tower and never the sign's count.
    pub fn caught_up(&self, at: Duration) -> Cue {
        self.cue(
            at,
            Transport::Jsonl,
            AgentEvent::Backlog {
                agent_id: self.id,
                replaying: false,
            },
        )
    }

    /// `calls` calls back to back from `at`, displayed in turn as `tools`
    /// cycles: Running throughout, then Idle.
    pub fn spell(&self, at: Duration, calls: u32, tools: &[&str]) -> Vec<Cue> {
        (0..calls)
            .zip(tools.iter().cycle())
            .flat_map(|(k, tool)| self.call(at + CALL * k, CALL, Self::generic(tool)))
            .collect()
    }

    /// A `tool` call parked on a permission prompt from `at` until it ends
    /// `held` later.
    pub fn wait(&self, at: Duration, held: Duration, tool: &str) -> [Cue; 3] {
        let [start, end] = self.call(at, held, Self::generic(tool));
        let ask = self.cue(
            at,
            self.lifecycle,
            AgentEvent::user_action_request(
                self.id,
                Some("permission".to_owned()),
                RequestKeys::default(),
            ),
        );
        [start, ask, end]
    }

    /// A subagent dispatch running from `at` for `held`: the typed Task no
    /// tool display can spell.
    pub fn delegate(&self, at: Duration, held: Duration) -> [Cue; 2] {
        self.call(at, held, ToolDetail::Task)
    }

    /// The session's model, as its wire names it.
    pub fn model(&self, at: Duration, model: &str) -> Cue {
        self.cue(
            at,
            Transport::Jsonl,
            AgentEvent::ModelInfo {
                agent_id: self.id,
                model: Some(model.to_owned()),
                effort: None,
            },
        )
    }

    /// A fresh-token reading with no id: each one counts.
    pub fn usage(&self, at: Duration, fresh_tokens: u64) -> Cue {
        self.cue(
            at,
            Transport::Jsonl,
            AgentEvent::usage(self.id, fresh_tokens, None),
        )
    }

    /// Its `SessionEnd`, a subagent's ending as a child.
    pub fn end(&self, at: Duration) -> Cue {
        self.cue(
            at,
            self.lifecycle,
            AgentEvent::SessionEnd {
                agent_id: self.id,
                as_child: self.parent.is_some(),
            },
        )
    }

    fn cue(&self, at: Duration, transport: Transport, event: AgentEvent) -> Cue {
        Cue::Event {
            at,
            transport,
            event,
        }
    }

    fn generic(tool: &str) -> ToolDetail {
        ToolDetail::Generic {
            display: tool.to_owned(),
        }
    }

    /// One call from `at` for `held`, named by its start: one actor never
    /// starts two calls at once.
    fn call(&self, at: Duration, held: Duration, detail: ToolDetail) -> [Cue; 2] {
        let id = format!("{}@{}", self.session, at.as_millis());
        [
            self.cue(
                at,
                self.lifecycle,
                AgentEvent::tool_use_start(self.id, Some(id.clone()), Some(detail)),
            ),
            self.cue(
                at + held,
                self.lifecycle,
                AgentEvent::ToolUseEnd {
                    agent_id: self.id,
                    tool_use_id: Some(id),
                },
            ),
        ]
    }
}

/// A looped timeline: its cues on a circle a period long, so a call that
/// straddles the wrap ends early in the next loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// By phase, ties in authored order: a wait's prompt follows its call's start.
    cues: Vec<Cue>,
    period: Duration,
    /// [`Lap::seq_stride`].
    seq_stride: u64,
}

impl Script {
    /// `cues` looped every `period`, each at its [`Cue::at`] modulo `period`.
    ///
    /// # Panics
    ///
    /// If `period` is zero.
    pub fn new(period: Duration, cues: impl IntoIterator<Item = Cue>) -> Self {
        assert!(!period.is_zero(), "a looped script needs a period");
        let mut cues: Vec<Cue> = cues.into_iter().collect();
        cues.sort_by_key(|cue| place(cue.at(), period).0);
        let seq_stride = (cues.iter_mut())
            .filter_map(|cue| match cue {
                Cue::Event { event, .. } => turn(event),
                Cue::Presence { .. } => None,
            })
            .filter_map(|turn| match turn {
                TurnKey::Seq(seq) => Some(seq.saturating_add(1)),
                TurnKey::Id(_) => None,
            })
            .max()
            .unwrap_or_default();
        Self {
            cues,
            period,
            seq_stride,
        }
    }
}

/// Where a [`Script`] is: each cue lands once, at its own instant, into the
/// host's reducer and scene, but for the loops a jump skips
/// ([`Playhead::advance`]).
#[derive(Debug, Clone)]
pub struct Playhead {
    script: Script,
    /// The current loop's start.
    origin: SystemTime,
    /// The current loop's next cue.
    cursor: usize,
    /// Loops played, [`Lap::n`]. A jump plays on in the lap it interrupts, so
    /// a call open across it still ends.
    lap: u64,
}

impl Playhead {
    /// `script`, its first loop starting at `started`.
    pub fn new(script: Script, started: SystemTime) -> Self {
        Self {
            script,
            origin: started,
            cursor: 0,
            lap: 0,
        }
    }

    /// Land every cue due by `now` at its own instant, then sweep as a live
    /// office's reducer task would. Once `now` is two loops or more past the
    /// current loop's start, it jumps whole loops, in phase: the loop under way
    /// finishes in the last one before `now`, and the loops jumped never play,
    /// so a fresh playhead's first `advance` fills the office it would show then.
    pub fn advance(&mut self, now: SystemTime, reducer: &mut Reducer, scene: &mut SceneState) {
        let period = self.script.period;
        let loops = now
            .duration_since(self.origin)
            .map_or(0, |elapsed| elapsed.as_nanos() / period.as_nanos());
        if loops >= 2 {
            self.origin += Duration::from_nanos_u128((loops - 1) * period.as_nanos());
        }
        loop {
            while let Some(cue) = self.script.cues.get(self.cursor) {
                let (phase, wraps) = place(cue.at(), period);
                if self.origin + phase > now {
                    break;
                }
                // A cue past the wrap is its own timeline's, which began
                // `wraps` laps back: none did before `started`.
                if let Some(n) = u64::try_from(wraps)
                    .ok()
                    .and_then(|wraps| self.lap.checked_sub(wraps))
                {
                    let lap = Lap {
                        n,
                        seq_stride: self.script.seq_stride,
                    };
                    cue.land_on(self.origin + phase, lap, reducer, scene);
                }
                self.cursor += 1;
            }
            let next = self.origin + period;
            if next > now {
                break;
            }
            self.origin = next;
            self.lap += 1;
            self.cursor = 0;
        }
        reducer.tick(scene, now);
        // A `Down` gateway otherwise lingers to its next `GatewayUp`.
        daemon::sweep_registered(scene, now, |_| true);
    }
}

#[cfg(test)]
mod tests;
