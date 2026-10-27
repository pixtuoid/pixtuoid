mod activity;
mod child_ledger;
mod display;
mod lifecycle;
mod liveness;
mod snapshot;
mod tasks;

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pixtuoid_core::AgentId;
use pixtuoid_core::source::{AgentEvent, Transport};
use pixtuoid_core::state::SceneState;
use pixtuoid_core::state::reducer::Reducer;

/// The suppression tests' source: its subagents' hooks arrive under the
/// parent's key, naming no child.
const UNNAMED_CHILD_HOOKS: &str = "kimi";

fn start(reducer: &mut Reducer, scene: &mut SceneState, id: AgentId) {
    start_as(reducer, scene, id, "claude-code");
}

fn start_as(reducer: &mut Reducer, scene: &mut SceneState, id: AgentId, source: &str) {
    reducer.apply(
        scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: source.into(),
            session_id: "abc".into(),
            cwd: PathBuf::from("/"),
            parent_id: None,
        },
        SystemTime::now(),
        Transport::Hook,
    );
}

const HOOK_AND_TRANSCRIPT: &str = "omp";

/// The b1 tests' source: claude-code's caps, but a Task's drain arms b1.
const DRAIN_INFERRED: &str = "omp";

fn delegating_pair(
    r: &mut Reducer,
    scene: &mut SceneState,
    slug: &str,
    t0: SystemTime,
) -> (AgentId, AgentId) {
    delegating_pair_on(r, scene, slug, t0, DRAIN_INFERRED)
}

fn delegating_pair_on(
    r: &mut Reducer,
    scene: &mut SceneState,
    slug: &str,
    t0: SystemTime,
    source: &str,
) -> (AgentId, AgentId) {
    let parent = AgentId::from_parts(source, &format!("/p/{slug}.jsonl"));
    let child = AgentId::from_parts(source, &format!("/p/{slug}/subagents/agent-1.jsonl"));
    r.apply(
        scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: source.into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        scene,
        AgentEvent::SessionStart {
            agent_id: child,
            source: source.into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(parent),
        },
        t0 + Duration::from_millis(100),
        Transport::Jsonl,
    );
    (parent, child)
}

fn tool_use_start(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    tool_use_id: Option<&str>,
    detail: Option<&str>,
    at: SystemTime,
    tp: Transport,
) {
    r.apply(
        scene,
        AgentEvent::ToolUseStart {
            agent_id: id,
            tool_use_id: tool_use_id.map(Into::into),
            detail: detail.map(Into::into),
        },
        at,
        tp,
    );
}

fn tool_use_end(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    tool_use_id: Option<&str>,
    at: SystemTime,
    tp: Transport,
) {
    r.apply(
        scene,
        AgentEvent::ToolUseEnd {
            agent_id: id,
            tool_use_id: tool_use_id.map(Into::into),
        },
        at,
        tp,
    );
}

fn turn_end(r: &mut Reducer, scene: &mut SceneState, id: AgentId, at: SystemTime, tp: Transport) {
    r.apply(scene, AgentEvent::TurnEnd { agent_id: id }, at, tp);
}

fn user_action_request(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    reason: &str,
    at: SystemTime,
    tp: Transport,
) {
    r.apply(
        scene,
        AgentEvent::UserActionRequest {
            agent_id: id,
            reason: Some(reason.into()),
            tool_use_id: None,
        },
        at,
        tp,
    );
}

/// `user_action_request` with the wire-named gated call id (#951 approval rounds).
fn user_action_request_for(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    reason: &str,
    tool_use_id: Option<&str>,
    at: SystemTime,
    tp: Transport,
) {
    r.apply(
        scene,
        AgentEvent::UserActionRequest {
            agent_id: id,
            reason: Some(reason.into()),
            tool_use_id: tool_use_id.map(Into::into),
        },
        at,
        tp,
    );
}

fn proof_of_life(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    at: SystemTime,
    tp: Transport,
) {
    r.apply(scene, AgentEvent::ProofOfLife { agent_id: id }, at, tp);
}

fn sess_end(
    r: &mut Reducer,
    scene: &mut SceneState,
    id: AgentId,
    as_child: bool,
    at: SystemTime,
    tp: Transport,
) {
    r.apply(
        scene,
        AgentEvent::SessionEnd {
            agent_id: id,
            as_child,
        },
        at,
        tp,
    );
}
