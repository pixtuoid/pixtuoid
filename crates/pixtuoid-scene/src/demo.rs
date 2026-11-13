//! A demo office any front end plays: authored [`Beat`]s a [`Playhead`] lands
//! by the clock through the REAL [`Reducer`], so a demo shows only states the
//! product produces. Each front end authors its own cast.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::source::daemon::{self, DaemonInstanceKey, DaemonPresenceUpdate};
use pixtuoid_core::source::{RequestKeys, registry};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;
use pixtuoid_core::{AgentEvent, AgentId, ToolDetail, Transport};

/// One authored moment of a demo timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    /// When it lands, after its timeline's origin.
    pub at: Duration,
    /// What lands.
    pub cue: Cue,
}

/// What a [`Beat`] lands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Cue {
    /// An agent event, as a live source sends it.
    Event {
        /// What hook-wins dedup reads: the transport the live source uses.
        transport: Transport,
        /// The event.
        event: AgentEvent,
    },
    /// A daemon mascot's presence delta, which rides its own lane: the one
    /// event channel is `AgentId`-pure.
    Presence {
        /// The daemon instance.
        key: DaemonInstanceKey,
        /// The delta.
        update: DaemonPresenceUpdate,
    },
}

impl Beat {
    /// Land it at `origin` + [`Beat::at`].
    pub fn land(&self, origin: SystemTime, reducer: &mut Reducer, scene: &mut SceneState) {
        let now = origin + self.at;
        match &self.cue {
            Cue::Event { transport, event } => reducer.apply(scene, event.clone(), now, *transport),
            Cue::Presence { key, update } => {
                daemon::apply_presence(scene, key, update.clone(), now)
            }
        }
    }
}

/// One scripted call's length. Calls chain back to back: the reducer settles a
/// slot the moment its call ends, so any gap between them shows as Idle.
const CALL: Duration = Duration::from_millis(1_200);

/// A scripted session: the identity every beat it plays names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    id: AgentId,
    source: &'static str,
    session: String,
    cwd: PathBuf,
    parent: Option<AgentId>,
}

impl Actor {
    /// A session of `source`, a source module's `SOURCE_NAME`: a hand-typed
    /// name misses the registry, and the label falls back to the raw string
    /// (`claude_code·api`, not `cc·api`).
    pub fn new(source: &'static str, session: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        let session = session.into();
        Self {
            id: AgentId::from_parts(source, &session),
            source,
            session,
            cwd: cwd.into(),
            parent: None,
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

    /// The agent every beat it plays names.
    pub fn id(&self) -> AgentId {
        self.id
    }

    /// The session starts.
    pub fn start(&self, at: Duration) -> Beat {
        self.beat(
            at,
            Transport::Jsonl,
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
    pub fn caught_up(&self, at: Duration) -> Beat {
        self.beat(
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
    pub fn spell(&self, at: Duration, calls: u32, tools: &[&str]) -> Vec<Beat> {
        (0..calls)
            .zip(tools.iter().cycle())
            .flat_map(|(k, tool)| self.call(at + CALL * k, CALL, Self::generic(tool)))
            .collect()
    }

    /// A `tool` call parked on a permission prompt from `at` until it ends
    /// `held` later.
    pub fn wait(&self, at: Duration, held: Duration, tool: &str) -> [Beat; 3] {
        let [start, end] = self.call(at, held, Self::generic(tool));
        let ask = self.beat(
            at,
            Transport::Hook,
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
    pub fn delegate(&self, at: Duration, held: Duration) -> [Beat; 2] {
        self.call(at, held, ToolDetail::Task)
    }

    /// The session's model, as its wire names it.
    pub fn model(&self, at: Duration, model: &str) -> Beat {
        self.beat(
            at,
            Transport::Jsonl,
            AgentEvent::ModelInfo {
                agent_id: self.id,
                model: Some(model.to_owned()),
                effort: None,
            },
        )
    }

    /// A fresh-token reading.
    pub fn usage(&self, at: Duration, fresh_tokens: u64) -> Beat {
        self.beat(
            at,
            Transport::Jsonl,
            AgentEvent::usage(self.id, fresh_tokens, None),
        )
    }

    /// The session ends.
    pub fn end(&self, at: Duration) -> Beat {
        self.beat(
            at,
            Transport::Hook,
            AgentEvent::SessionEnd {
                agent_id: self.id,
                as_child: self.parent.is_some(),
            },
        )
    }

    fn beat(&self, at: Duration, transport: Transport, event: AgentEvent) -> Beat {
        Beat {
            at,
            cue: Cue::Event { transport, event },
        }
    }

    fn generic(tool: &str) -> ToolDetail {
        ToolDetail::Generic {
            display: tool.to_owned(),
        }
    }

    /// One call from `at` for `held`, named by its start: one actor never
    /// starts two calls at once.
    fn call(&self, at: Duration, held: Duration, detail: ToolDetail) -> [Beat; 2] {
        let id = format!("{}@{}", self.session, at.as_millis());
        [
            self.beat(
                at,
                Transport::Hook,
                AgentEvent::tool_use_start(self.id, Some(id.clone()), Some(detail)),
            ),
            self.beat(
                at + held,
                Transport::Hook,
                AgentEvent::ToolUseEnd {
                    agent_id: self.id,
                    tool_use_id: Some(id),
                },
            ),
        ]
    }
}

/// A looped timeline: its beats on a circle a period long, so a call that
/// straddles the seam ends early in the next loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// By phase, ties in authored order: a wait's prompt follows its call's start.
    beats: Vec<Beat>,
    period: Duration,
}

impl Script {
    /// `beats` looped every `period`, each at its [`Beat::at`] modulo `period`.
    ///
    /// # Panics
    ///
    /// If `period` is zero.
    pub fn new(period: Duration, beats: impl IntoIterator<Item = Beat>) -> Self {
        assert!(!period.is_zero(), "a looped script needs a period");
        let mut beats: Vec<Beat> = beats
            .into_iter()
            .map(|beat| Beat {
                at: Duration::from_nanos_u128(beat.at.as_nanos() % period.as_nanos()),
                ..beat
            })
            .collect();
        beats.sort_by_key(|beat| beat.at);
        Self { beats, period }
    }
}

/// Where a [`Script`] is: each beat lands once, at its own instant, into the
/// host's reducer and scene.
#[derive(Debug, Clone)]
pub struct Playhead {
    script: Script,
    /// The current loop's start.
    origin: SystemTime,
    /// The current loop's next beat.
    cursor: usize,
}

// The App holds one behind a UniFFI `Mutex`.
const _: () = {
    const fn shareable<T: Send + Sync>() {}
    shareable::<Playhead>();
};

impl Playhead {
    /// `script`, its first loop starting at `started`.
    pub fn new(script: Script, started: SystemTime) -> Self {
        Self {
            script,
            origin: started,
            cursor: 0,
        }
    }

    /// Land every beat due by `now` at its own instant, then sweep as a live
    /// office's reducer task would. A gap of two loops or more replays only the
    /// last whole loop and the current one, in phase: a fresh playhead's first
    /// call fills the office it would show at `now`.
    pub fn advance(&mut self, now: SystemTime, reducer: &mut Reducer, scene: &mut SceneState) {
        let period = self.script.period;
        let loops = now
            .duration_since(self.origin)
            .map_or(0, |elapsed| elapsed.as_nanos() / period.as_nanos());
        if loops >= 2 {
            self.origin += Duration::from_nanos_u128((loops - 1) * period.as_nanos());
            self.cursor = 0;
        }
        loop {
            while let Some(beat) = self.script.beats.get(self.cursor) {
                if self.origin + beat.at > now {
                    break;
                }
                beat.land(self.origin, reducer, scene);
                self.cursor += 1;
            }
            let next = self.origin + period;
            if next > now {
                break;
            }
            self.origin = next;
            self.cursor = 0;
        }
        sweep(reducer, scene, now);
    }
}

/// The binary's reconcile tick with every source connected.
fn sweep(reducer: &mut Reducer, scene: &mut SceneState, now: SystemTime) {
    reducer.tick(scene, now);
    // A `Down` gateway otherwise lingers to its next `GatewayUp`.
    for (source, ttl) in registry::daemon_sources() {
        daemon::sweep_presence_ttl(scene, source, ttl, now);
    }
}

#[cfg(test)]
mod tests;
