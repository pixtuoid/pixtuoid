use super::label_prefix_for;
use crate::source::registry;

#[test]
fn every_registered_source_has_two_char_label_prefix() {
    for src in registry::registered_source_names() {
        let prefix = label_prefix_for(src);
        assert_eq!(
            prefix.chars().count(),
            2,
            "source {src:?} has no 2-char label prefix (got {prefix:?}) — fix its SourceDescriptor row in source/registry.rs"
        );
    }
}

/// Only a derivation fallback (ordinal ghost / bare prefix) may be upgraded by
/// a later back-fill; a cwd- or Rename-derived label is real information.
#[test]
fn rename_classification_and_upgradability_cover_each_provenance() {
    use super::classify_rename;
    use crate::state::{LabelProvenance, SlotLabel};
    for (label, source, expect) in [
        ("cx", "codex", LabelProvenance::PrefixFallback),
        ("cc·repo", "claude-code", LabelProvenance::Renamed),
        ("code-explorer", "claude-code", LabelProvenance::Renamed),
        ("cc#3", "claude-code", LabelProvenance::Renamed),
        ("", "claude-code", LabelProvenance::Renamed),
    ] {
        assert_eq!(
            classify_rename(label, source).provenance(),
            expect,
            "{label:?} under source {source:?} must classify as {expect:?}"
        );
    }
    assert!(SlotLabel::ordinal_ghost("cc#3").is_upgradable());
    assert!(SlotLabel::ordinal_ghost("#1").is_upgradable());
    assert!(SlotLabel::prefix_fallback("cx").is_upgradable());
    assert!(!SlotLabel::cwd_derived("cc·repo").is_upgradable());
    assert!(!SlotLabel::renamed("code-explorer").is_upgradable());
}

/// Every timing test derives its offsets FROM these constants, so mutating a
/// window also mutates each test's own expectation — only a direct pin catches
/// a collapsed or typo'd duration. Change it deliberately, never to pass.
#[test]
fn stale_timeout_constants_have_their_intended_durations() {
    use super::{
        PROOF_OF_LIFE_TTL, STALE_ACTIVE_TIMEOUT, STALE_IDLE_TIMEOUT, STALE_SHORT_IDLE_TIMEOUT,
        STALE_UNKNOWN_CWD_TIMEOUT, STALE_WAITING_TIMEOUT,
    };
    use std::time::Duration;
    assert_eq!(STALE_ACTIVE_TIMEOUT, Duration::from_secs(600));
    assert_eq!(STALE_IDLE_TIMEOUT, Duration::from_secs(1800));
    assert_eq!(STALE_WAITING_TIMEOUT, Duration::from_secs(3600));
    assert_eq!(STALE_UNKNOWN_CWD_TIMEOUT, Duration::from_secs(180));
    assert_eq!(STALE_SHORT_IDLE_TIMEOUT, Duration::from_secs(300));
    assert_eq!(PROOF_OF_LIFE_TTL, Duration::from_secs(150));
}

// Synthetic caps on an unregistered source, so the POLICY half stays covered
// for caps combinations the registered rows don't happen to spell.
#[test]
fn delegating_slot_with_hook_silent_caps_gets_waiting_window() {
    use super::{STALE_ACTIVE_TIMEOUT, STALE_WAITING_TIMEOUT, stale_threshold_with_caps};
    use crate::source::registry::SourceCaps;
    use crate::source::{AgentEvent, ToolDetail, Transport};
    use crate::{AgentId, Reducer, SceneState};
    use std::time::SystemTime;
    let caps = SourceCaps {
        has_exit_signal: true,
        resurrects_on_prompt: true,
        delegations_are_hook_silent: true,
        child_end_inference: crate::source::registry::ChildEndInference::TaskDrain,
        child_hooks_unnamed: false,
    };
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("hook-silent-cli", "/p");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "hook-silent-cli".into(),
            session_id: "/p".into(),
            cwd: "/p".into(),
            parent_id: None,
        },
        SystemTime::UNIX_EPOCH,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: None,
            detail: Some(ToolDetail::Task),
        },
        SystemTime::UNIX_EPOCH,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(
        stale_threshold_with_caps(slot, Some(caps)),
        STALE_WAITING_TIMEOUT,
        "hook-silent Delegating slot must get the Waiting-class window"
    );
    assert_eq!(
        stale_threshold_with_caps(slot, None),
        STALE_ACTIVE_TIMEOUT,
        "without the cap, Delegating reaps on the normal Active timer"
    );

    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: None,
            detail: Some(ToolDetail::Generic {
                display: "bash: ls".into(),
            }),
        },
        SystemTime::UNIX_EPOCH,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(
        stale_threshold_with_caps(slot, Some(caps)),
        STALE_ACTIVE_TIMEOUT,
        "caps-on but non-Task detail must keep the Active timer"
    );
}

#[test]
fn generic_tool_displaying_delegating_keeps_the_active_window() {
    use super::{STALE_ACTIVE_TIMEOUT, stale_threshold_with_caps};
    use crate::source::registry::SourceCaps;
    use crate::source::{AgentEvent, ToolDetail, Transport};
    use crate::{AgentId, Reducer, SceneState};
    use std::time::SystemTime;
    let caps = SourceCaps {
        has_exit_signal: true,
        resurrects_on_prompt: true,
        delegations_are_hook_silent: true,
        child_end_inference: crate::source::registry::ChildEndInference::TaskDrain,
        child_hooks_unnamed: false,
    };
    let mut scene = SceneState::uniform(4);
    let mut r = Reducer::new();
    let id = AgentId::from_parts("hook-silent-cli", "/p");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "hook-silent-cli".into(),
            session_id: "/p".into(),
            cwd: "/p".into(),
            parent_id: None,
        },
        SystemTime::UNIX_EPOCH,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: None,
            detail: Some(ToolDetail::Generic {
                display: "Delegating".into(),
            }),
        },
        SystemTime::UNIX_EPOCH,
        Transport::Hook,
    );
    let slot = scene.agents.get(&id).unwrap();
    assert_eq!(
        stale_threshold_with_caps(slot, Some(caps)),
        STALE_ACTIVE_TIMEOUT,
        "a Generic tool spelling 'Delegating' must not ride the delegation carve-out"
    );
}

#[test]
fn an_action_request_is_evicted_on_apply_path_sweep() {
    use crate::AgentId;
    use crate::source::{AgentEvent, ToolDetail, Transport};
    use crate::state::SceneState;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    let id = AgentId::from_transcript_path("/p/a.jsonl");
    let t0 = SystemTime::now();
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: id,
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: Some("toolT".into()),
            detail: Some(ToolDetail::from("Bash")),
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::Waiting {
            agent_id: id,
            reason: "perm".into(),
            tool_use_id: None,
        },
        t0,
        Transport::Hook,
    );
    assert!(
        r.corr.action_requests.contains(&id),
        "request recorded while Waiting mid-tool"
    );

    // The UNRELATED event below is what runs sweep_exited on the APPLY path.
    r.apply(
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: id,
            as_child: false,
        },
        t0,
        Transport::Hook,
    );
    let later = t0 + super::EXIT_GRACE_WINDOW + Duration::from_secs(1);
    let other = AgentId::from_transcript_path("/p/other.jsonl");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: other,
            source: "claude-code".into(),
            session_id: "s2".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        later,
        Transport::Hook,
    );

    assert!(
        !scene.agents.contains_key(&id),
        "exited slot swept on the apply path"
    );
    assert!(
        !r.corr.action_requests.contains(&id),
        "apply-path sweep_exited must evict the action request (not only tick's retain)"
    );
}

/// Every attempt against every end it can meet, on both sides of each window.
#[test]
fn authorizes_decides_each_attempt_by_the_end_it_meets() {
    use super::{Attempt, AttemptKind, Refusal, Verdict};
    use crate::AgentId;
    use crate::source::{AgentEvent, Transport};
    use crate::state::SceneState;
    use crate::state::correlation::{CHILD_END_GATE_TTL, STRAGGLER_TTL};
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    let id = AgentId::from_parts("codex", "s");
    let parent = AgentId::from_parts("codex", "p");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let ms = Duration::from_millis(1);
    let at = |kind, parent| Attempt { kind, parent };
    let (root, child) = (
        at(AttemptKind::Start, None),
        at(AttemptKind::Start, Some(parent)),
    );
    let (identity, activity) = (
        at(AttemptKind::Identity, None),
        at(AttemptKind::HookActivity, None),
    );
    let (hook, jsonl) = (Transport::Hook, Transport::Jsonl);
    use Verdict::{Create, Refuse};

    // No slot: the attempt meets only the recorded end.
    for (end, rows) in [
        (
            None,
            vec![
                (root, jsonl, t0, Create),
                (child, jsonl, t0, Create),
                (identity, hook, t0, Create),
                (activity, hook, t0, Create),
            ],
        ),
        (
            Some(false),
            vec![
                (root, hook, t0 + STRAGGLER_TTL - ms, Create),
                (
                    root,
                    jsonl,
                    t0 + STRAGGLER_TTL - ms,
                    Refuse(Refusal::HookOwnsRevival),
                ),
                (root, jsonl, t0 + STRAGGLER_TTL, Create),
                (
                    child,
                    jsonl,
                    t0 + STRAGGLER_TTL - ms,
                    Refuse(Refusal::Straggler),
                ),
                (
                    identity,
                    hook,
                    t0 + STRAGGLER_TTL - ms,
                    Refuse(Refusal::Straggler),
                ),
                (
                    activity,
                    hook,
                    t0 + STRAGGLER_TTL - ms,
                    Refuse(Refusal::Straggler),
                ),
                (activity, hook, t0 + STRAGGLER_TTL, Create),
                (identity, hook, t0 + STRAGGLER_TTL, Create),
            ],
        ),
        (
            Some(true),
            vec![
                (root, jsonl, t0 + STRAGGLER_TTL - ms, Create),
                (
                    activity,
                    hook,
                    t0 + STRAGGLER_TTL - ms,
                    Refuse(Refusal::Straggler),
                ),
                (
                    child,
                    jsonl,
                    t0 + STRAGGLER_TTL - ms,
                    Refuse(Refusal::Straggler),
                ),
                (
                    identity,
                    hook,
                    t0 + STRAGGLER_TTL - ms,
                    Refuse(Refusal::Straggler),
                ),
                (
                    activity,
                    hook,
                    t0 + STRAGGLER_TTL,
                    Refuse(Refusal::ChildEnded),
                ),
                (activity, hook, t0 + CHILD_END_GATE_TTL, Create),
                (
                    child,
                    jsonl,
                    t0 + STRAGGLER_TTL,
                    Refuse(Refusal::ChildEnded),
                ),
                (
                    identity,
                    hook,
                    t0 + CHILD_END_GATE_TTL - ms,
                    Refuse(Refusal::ChildEnded),
                ),
                (child, jsonl, t0 + CHILD_END_GATE_TTL, Create),
            ],
        ),
    ] {
        let mut r = super::Reducer::new();
        let scene = SceneState::uniform(4);
        if let Some(as_child) = end {
            r.corr.note_end(id, Transport::Hook, as_child, t0);
        }
        for (attempt, from, now, want) in rows {
            assert_eq!(
                r.authorizes(&scene, id, attempt, from, now),
                want,
                "end {end:?}, {from:?} at +{:?}",
                now.duration_since(t0)
            );
        }
    }

    // An end pixtuoid only inferred: no straggler window, and only a hook
    // re-admits the child, under its live parent, explicit or remembered.
    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: parent,
            source: "codex".into(),
            session_id: "p".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        },
        t0,
        Transport::Hook,
    );
    r.corr.link_applied_parent(id, parent);
    r.corr.note_removed(id, t0);
    let remembered = |kind| r.attempt(id, kind, None);
    assert!(
        remembered(AttemptKind::Start) == child,
        "a known child's parentless start is judged under its remembered parent"
    );
    for (attempt, from, want) in [
        (child, hook, Create),
        (child, jsonl, Refuse(Refusal::ChildEnded)),
        (remembered(AttemptKind::Identity), hook, Create),
        (remembered(AttemptKind::HookActivity), hook, Create),
    ] {
        assert_eq!(
            r.authorizes(&scene, id, attempt, from, t0 + ms),
            want,
            "{from:?}"
        );
    }

    // A slot: only a parentless start of an exiting root revives it, and a
    // transcript's only while no hook end owns its revival.
    let start = |parent_id| AgentEvent::SessionStart {
        agent_id: id,
        source: "codex".into(),
        session_id: "s".into(),
        cwd: PathBuf::from("/repo"),
        parent_id,
    };
    let end = AgentEvent::SessionEnd {
        agent_id: id,
        as_child: false,
    };
    let enrich = |revive| Verdict::Enrich { revive };
    for (ended_by, rows) in [
        (
            None,
            vec![(root, hook, enrich(false)), (root, jsonl, enrich(false))],
        ),
        (
            Some(hook),
            vec![
                (root, hook, enrich(true)),
                (root, jsonl, enrich(false)),
                (child, hook, enrich(false)),
            ],
        ),
        (Some(jsonl), vec![(root, jsonl, enrich(true))]),
    ] {
        let mut r = super::Reducer::new();
        let mut scene = SceneState::uniform(4);
        r.apply(&mut scene, start(None), t0, hook);
        if let Some(from) = ended_by {
            r.apply(&mut scene, end.clone(), t0, from);
        }
        for (attempt, from, want) in rows {
            assert_eq!(
                r.authorizes(&scene, id, attempt, from, t0 + ms),
                want,
                "ended by {ended_by:?}, {from:?}"
            );
        }
    }
    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    r.apply(&mut scene, start(None), t0, hook);
    let child_id = AgentId::from_parts("codex", "child");
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: child_id,
            source: "codex".into(),
            session_id: "c".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: Some(id),
        },
        t0,
        hook,
    );
    r.apply(&mut scene, end, t0, hook);
    assert_eq!(
        r.authorizes(
            &scene,
            child_id,
            r.attempt(child_id, AttemptKind::Start, None),
            hook,
            t0 + ms
        ),
        enrich(false),
        "a cascaded subagent never revives in place"
    );
}

/// A parentless start that adopts its remembered parent makes the slot a
/// child again, which leaves with its parent's cascade, never in place (#246).
#[test]
fn an_exiting_root_that_adopts_its_remembered_parent_does_not_revive() {
    use crate::AgentId;
    use crate::source::{AgentEvent, Transport};
    use crate::state::SceneState;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    let id = AgentId::from_parts("codex", "orphaned-child");
    let parent = AgentId::from_parts("codex", "parent");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let start = AgentEvent::SessionStart {
        agent_id: id,
        source: "codex".into(),
        session_id: "c".into(),
        cwd: PathBuf::from("/repo"),
        parent_id: None,
    };
    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    r.apply(&mut scene, start.clone(), t0, Transport::Hook);
    r.corr.link_applied_parent(id, parent);
    r.apply(
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: id,
            as_child: false,
        },
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        start,
        t0 + Duration::from_millis(1),
        Transport::Hook,
    );
    let slot = &scene.agents[&id];
    assert_eq!(
        slot.parent_id,
        Some(parent),
        "the remembered parent is adopted"
    );
    assert!(
        slot.exiting_at.is_some(),
        "an adopted child does not revive in place"
    );
}

/// #242 on a slot that existed: its hook end refuses the life's stragglers
/// past the sweep, for the window and no longer.
#[test]
fn a_known_slots_hook_end_refuses_its_stragglers_past_the_sweep() {
    use crate::AgentId;
    use crate::source::{AgentEvent, Transport};
    use crate::state::SceneState;
    use crate::state::correlation::STRAGGLER_TTL;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    let id = AgentId::from_parts("claude-code", "known");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let ms = Duration::from_millis(1);
    let straggle = |r: &mut super::Reducer, scene: &mut SceneState, at| {
        r.apply(
            scene,
            AgentEvent::identity(id, "claude-code", "known", Some(PathBuf::from("/repo"))),
            at,
            Transport::Hook,
        );
        r.apply(
            scene,
            AgentEvent::ActivityStart {
                agent_id: id,
                tool_use_id: Some("t".into()),
                detail: None,
            },
            at,
            Transport::Hook,
        );
    };
    for (at, registers) in [(t0 + STRAGGLER_TTL - ms, false), (t0 + STRAGGLER_TTL, true)] {
        let mut r = super::Reducer::new();
        let mut scene = SceneState::uniform(4);
        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: id,
                source: "claude-code".into(),
                session_id: "known".into(),
                cwd: PathBuf::from("/repo"),
                parent_id: None,
            },
            t0 - Duration::from_secs(1),
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            AgentEvent::SessionEnd {
                agent_id: id,
                as_child: false,
            },
            t0,
            Transport::Hook,
        );
        r.tick(&mut scene, t0 + super::EXIT_GRACE_WINDOW + ms);
        assert!(!scene.agents.contains_key(&id), "fixture: swept");
        straggle(&mut r, &mut scene, at);
        assert_eq!(
            scene.agents.contains_key(&id),
            registers,
            "at +{:?}",
            at.duration_since(t0)
        );
    }
}

/// The exit's own authority decides what may undo it: within the straggler
/// window a session a hook ended resurrects only on a hook start, while one
/// its transcript ended may still resume from that transcript.
#[test]
fn a_transcript_first_sight_resurrects_only_what_a_transcript_ended() {
    use crate::AgentId;
    use crate::source::{AgentEvent, Transport};
    use crate::state::SceneState;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    let id = AgentId::from_transcript_path("/p/ended.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let start = AgentEvent::SessionStart {
        agent_id: id,
        source: "codex".into(),
        session_id: "s".into(),
        cwd: PathBuf::from("/repo"),
        parent_id: None,
    };
    let end = AgentEvent::SessionEnd {
        agent_id: id,
        as_child: false,
    };
    let exiting = |scene: &SceneState| scene.agents.get(&id).map(|s| s.exiting_at.is_some());
    // The watcher's first sight lands inside the walkout (codex writes its
    // rollout at exit; grok's trailer follows its end line): its last instant.
    let end_at = t0 + Duration::from_secs(1);
    let sight = end_at + super::EXIT_GRACE_WINDOW - Duration::from_millis(1);

    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    r.apply(&mut scene, start.clone(), t0, Transport::Hook);
    r.apply(
        &mut scene,
        end.clone(),
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    r.apply(&mut scene, start.clone(), sight, Transport::Jsonl);
    assert_eq!(
        exiting(&scene),
        Some(true),
        "a file's first sight proves no life"
    );
    r.apply(&mut scene, start.clone(), sight, Transport::Hook);
    assert_eq!(exiting(&scene), Some(false), "a hook start resurrects");

    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    r.apply(&mut scene, start.clone(), t0, Transport::Jsonl);
    r.apply(
        &mut scene,
        end,
        t0 + Duration::from_secs(1),
        Transport::Jsonl,
    );
    r.apply(&mut scene, start, sight, Transport::Jsonl);
    assert_eq!(
        exiting(&scene),
        Some(false),
        "a transcript-ended session resumes from its transcript"
    );
}

#[test]
fn resurrect_in_place_evicts_correlation_maps_but_keeps_proof_of_life() {
    use crate::AgentId;
    use crate::source::{AgentEvent, ToolDetail, Transport};
    use crate::state::SceneState;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    // A source whose Task drain arms b1.
    let id = AgentId::from_parts("omp", "/p/res-maps.jsonl");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let session_start = |sid: &str| AgentEvent::SessionStart {
        agent_id: id,
        source: "omp".into(),
        session_id: sid.into(),
        cwd: PathBuf::from("/repo"),
        parent_id: None,
    };
    r.apply(&mut scene, session_start("s"), t0, Transport::Hook);
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: Some("t-gate".into()),
            detail: Some(ToolDetail::from("Bash")),
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::Waiting {
            agent_id: id,
            reason: "perm".into(),
            tool_use_id: None,
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    // A dispatch that fully drains arms the b1 cascade and leaves an (empty)
    // tasks_in_flight entry behind.
    r.apply(
        &mut scene,
        AgentEvent::ActivityStart {
            agent_id: id,
            tool_use_id: Some("task-1".into()),
            detail: Some(ToolDetail::from("Agent")),
        },
        t0 + Duration::from_secs(2),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ActivityEnd {
            agent_id: id,
            tool_use_id: Some("task-1".into()),
        },
        t0 + Duration::from_secs(3),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        AgentEvent::ProofOfLife { agent_id: id },
        t0 + Duration::from_secs(3),
        Transport::Hook,
    );
    assert!(
        r.corr.tasks_in_flight.contains_key(&id),
        "active task populated"
    );
    assert!(r.corr.action_requests.contains(&id), "request populated");
    assert!(r.pending_b1_cascades.contains_key(&id), "cascade armed");
    assert!(
        r.corr.recent_proof_of_life.contains_key(&id),
        "vouch recorded"
    );

    // The resurrect must land inside the walkout window AND before the armed
    // cascade's grace elapses.
    r.apply(
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: id,
            as_child: false,
        },
        t0 + Duration::from_secs(4),
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        session_start("s2"),
        t0 + Duration::from_millis(4_500),
        Transport::Hook,
    );

    assert!(
        scene
            .agents
            .get(&id)
            .is_some_and(|s| s.exiting_at.is_none()),
        "resurrected"
    );
    assert!(
        !r.corr.tasks_in_flight.contains_key(&id),
        "resurrect must evict the dead life's tasks_in_flight entry"
    );
    assert!(
        !r.corr.action_requests.contains(&id),
        "resurrect must evict the dead life's action request"
    );
    assert!(
        !r.pending_b1_cascades.contains_key(&id),
        "resurrect must disarm the dead life's pending b1 cascade"
    );
    assert!(
        r.corr.recent_proof_of_life.contains_key(&id),
        "the vouch must SURVIVE resurrection — the process is alive"
    );
}

/// Past the GATE's `CHILD_END_GATE_TTL` the entry is deliberately RETAINED:
/// a parentless revival re-links through its `parent_id` across an unbounded
/// turn gap, so the link rides the longer relink budget. (Pruning at the gate's
/// TTL is what made a multi-turn child idle >90s come back an orphan.)
#[test]
fn child_lifecycle_is_stamped_on_sweep_and_pruned_by_gc() {
    use crate::AgentId;
    use crate::source::{AgentEvent, Transport};
    use crate::state::SceneState;
    use crate::state::correlation::ChildEnd;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    let mut r = super::Reducer::new();
    let mut scene = SceneState::uniform(4);
    let parent = AgentId::from_parts("codex", "lifecycle-parent");
    let child = AgentId::from_parts("codex", "lifecycle-child");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let session_start = |agent_id, sid: &str, parent_id| AgentEvent::SessionStart {
        agent_id,
        source: "codex".into(),
        session_id: sid.into(),
        cwd: PathBuf::from("/repo"),
        parent_id,
    };
    r.apply(
        &mut scene,
        session_start(parent, "lifecycle-parent", None),
        t0,
        Transport::Hook,
    );
    r.apply(
        &mut scene,
        session_start(child, "lifecycle-child", Some(parent)),
        t0,
        Transport::Hook,
    );
    assert!(
        !r.corr.lifecycles.contains_key(&parent),
        "a root registration records no lifecycle"
    );
    let entry = r.corr.lifecycles.get(&child).expect("child link recorded");
    assert_eq!(entry.parent_id, Some(parent));
    assert!(entry.child_end.is_none(), "alive — no gc clock yet");

    // Neither end below is `as_child`, so only sweep_exited can stamp the clock.
    r.apply(
        &mut scene,
        AgentEvent::SessionEnd {
            agent_id: parent,
            as_child: false,
        },
        t0 + Duration::from_secs(1),
        Transport::Hook,
    );
    let swept = t0 + Duration::from_secs(1) + super::EXIT_GRACE_WINDOW + Duration::from_secs(1);
    r.tick(&mut scene, swept);
    assert!(!scene.agents.contains_key(&child), "child swept");
    assert!(
        r.corr
            .lifecycles
            .get(&child)
            .is_some_and(|e| matches!(e.child_end, Some(ChildEnd::Inferred(_)))),
        "sweep_exited must stamp an inferred end for a child whose end wasn't as_child"
    );

    r.tick(
        &mut scene,
        swept + super::CHILD_END_GATE_TTL + Duration::from_secs(1),
    );
    assert_eq!(
        r.corr.lifecycles.get(&child).and_then(|e| e.parent_id),
        Some(parent),
        "the parent link must outlive the end GATE it does not share a purpose with"
    );

    r.tick(
        &mut scene,
        swept + super::CHILD_END_RELINK_TTL + Duration::from_secs(1),
    );
    assert!(
        !r.corr.lifecycles.contains_key(&child),
        "gc must prune an ended entry past CHILD_END_RELINK_TTL"
    );
}

/// A synthetic ~1 event/s stream through the REAL reducer: a per-event leak
/// (a missed prune site) grows a map ~linearly and blows the bound, while a
/// healthy stream stays an order of magnitude under.
#[test]
fn correlation_maps_stay_bounded_across_a_long_stream() {
    use crate::AgentId;
    use crate::source::{AgentEvent, ToolDetail, Transport};
    use crate::state::SceneState;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    /// ~3× the widest steady-state working set (`CHILD_END_RELINK_TTL` at
    /// ~1 event/s) and FAR below ITERS. A bound that merely clears the steady
    /// state proves nothing about a slow leak.
    const MAX_CORR_ENTRIES: usize = 1024;
    const ITERS: u64 = 3_000;
    const MAP_NAMES: [&str; 7] = [
        "recent_hook_tool_uses",
        "tasks_in_flight",
        "recent_task_drains",
        "lifecycles",
        "recent_proof_of_life",
        "action_requests",
        "counted_calls",
    ];

    let mut r = super::Reducer::new();
    // 32 desks comfortably exceeds the un-swept working set: a slot lives until
    // `sweep_exited` removes it ~EXIT_GRACE_WINDOW after its end, so only a
    // handful are concurrent at ~1s/iter.
    let mut scene = SceneState::uniform(32);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

    let mut peak = [0usize; MAP_NAMES.len()];

    for i in 0..ITERS {
        let now = t0 + Duration::from_secs(i);
        let a = AgentId::from_parts("claude-code", &format!("s{i}"));

        r.apply(
            &mut scene,
            AgentEvent::SessionStart {
                agent_id: a,
                source: "claude-code".into(),
                session_id: format!("s{i}"),
                cwd: PathBuf::from("/repo"),
                parent_id: None,
            },
            now,
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            AgentEvent::ActivityStart {
                agent_id: a,
                tool_use_id: Some("task1".into()),
                detail: Some(ToolDetail::Task),
            },
            now,
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            AgentEvent::ActivityEnd {
                agent_id: a,
                tool_use_id: Some("task1".into()),
            },
            now,
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            AgentEvent::ActivityStart {
                agent_id: a,
                tool_use_id: Some("gate1".into()),
                detail: Some(ToolDetail::Generic {
                    display: "Bash".into(),
                }),
            },
            now,
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            AgentEvent::Waiting {
                agent_id: a,
                reason: "perm".into(),
                tool_use_id: None,
            },
            now,
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            AgentEvent::ProofOfLife { agent_id: a },
            now,
            Transport::Hook,
        );
        r.apply(
            &mut scene,
            AgentEvent::SessionEnd {
                agent_id: a,
                as_child: true,
            },
            now,
            Transport::Hook,
        );
        // An UNREGISTERED id (fresh each iter) stamps a hook end.
        r.apply(
            &mut scene,
            AgentEvent::SessionEnd {
                agent_id: AgentId::from_parts("claude-code", &format!("u{i}")),
                as_child: false,
            },
            now,
            Transport::Hook,
        );
        // An ORPHAN Task (no SessionStart, so no slot) leaves an tasks_in_flight
        // entry reaped ONLY by tick's slot-removal retain.
        r.apply(
            &mut scene,
            AgentEvent::ActivityStart {
                agent_id: AgentId::from_parts("claude-code", &format!("o{i}")),
                tool_use_id: Some("orphan".into()),
                detail: Some(ToolDetail::Task),
            },
            now,
            Transport::Hook,
        );

        // The interleaved tick is the ONLY reaper of the non-TTL orphan retains.
        r.tick(&mut scene, now);

        let lens = [
            r.corr.recent_hook_tool_uses.len(),
            r.corr.tasks_in_flight.len(),
            r.corr.recent_task_drains.len(),
            r.corr.lifecycles.len(),
            r.corr.recent_proof_of_life.len(),
            r.corr.action_requests.len(),
            r.corr
                .counted_calls
                .values()
                .map(std::collections::HashMap::len)
                .sum(),
        ];
        for (p, &l) in peak.iter_mut().zip(lens.iter()) {
            *p = (*p).max(l);
        }
        for (l, name) in lens.iter().zip(MAP_NAMES) {
            assert!(
                *l <= MAX_CORR_ENTRIES,
                "Correlation map `{name}` grew to {l} at iter {i} (> {MAX_CORR_ENTRIES}) — a prune site is missing"
            );
        }
    }

    for (p, name) in peak.iter().zip(MAP_NAMES) {
        assert!(
            *p > 0,
            "Correlation map `{name}` was never populated — its bound check was vacuous"
        );
    }
}

/// A session id and cwd arrive from another agent's process, so the reducer's
/// warn lines carry them as `?` fields — a `%` field would write their bytes raw.
#[test]
fn desks_full_warn_escapes_the_wire_session_id_and_cwd() {
    use crate::source::{AgentEvent, Transport};
    use crate::test_capture::capture_logs;
    use crate::{AgentId, Reducer, SceneState};
    use std::time::SystemTime;
    let mut scene = SceneState::uniform(1);
    let mut r = Reducer::new();
    let start = |sid: &str, cwd: &str| AgentEvent::SessionStart {
        agent_id: AgentId::from_parts("claude-code", sid),
        source: "claude-code".into(),
        session_id: sid.into(),
        cwd: cwd.into(),
        parent_id: None,
    };
    for i in 0..scene.total_capacity() {
        r.apply(
            &mut scene,
            start(&format!("s{i}"), "/p"),
            SystemTime::UNIX_EPOCH,
            Transport::Hook,
        );
    }
    let hostile = "\u{1b}]0;pwned\u{7}\u{202e}";
    let out = capture_logs(|| {
        r.apply(
            &mut scene,
            start(hostile, hostile),
            SystemTime::UNIX_EPOCH,
            Transport::Hook,
        );
    });
    assert!(out.contains("dropped SessionStart"), "got {out:?}");
    for raw in ['\u{1b}', '\u{7}', '\u{202e}'] {
        assert!(
            !out.contains(raw),
            "{raw:?} must not reach the log raw: {out:?}"
        );
    }
}

/// A label carries its cwd's basename, wire content, so the stale sweep logs it
/// as a `?` field.
#[test]
fn the_stale_sweep_logs_the_wire_label_escaped() {
    use super::STALE_IDLE_TIMEOUT;
    use crate::source::{AgentEvent, Transport};
    use crate::test_capture::capture_logs;
    use crate::{AgentId, Reducer, SceneState};
    use std::time::{Duration, SystemTime};
    let mut scene = SceneState::uniform(1);
    let mut r = Reducer::new();
    r.apply(
        &mut scene,
        AgentEvent::SessionStart {
            agent_id: AgentId::from_parts("claude-code", "s"),
            source: "claude-code".into(),
            session_id: "s".into(),
            cwd: "/w/re\u{1b}]0;pwned\u{7}po\u{202e}Z".into(),
            parent_id: None,
        },
        SystemTime::UNIX_EPOCH,
        Transport::Hook,
    );
    let out = capture_logs(|| {
        r.tick(
            &mut scene,
            SystemTime::UNIX_EPOCH + STALE_IDLE_TIMEOUT + Duration::from_secs(1),
        );
    });
    assert!(out.contains("stale agent"), "got {out:?}");
    for raw in ['\u{1b}', '\u{7}', '\u{202e}'] {
        assert!(
            !out.contains(raw),
            "{raw:?} must not reach the log raw: {out:?}"
        );
    }
    assert!(
        out.contains(r"re\u{1b}]0;pwned\u{7}po\u{202e}Z"),
        "escaped, not dropped: {out:?}"
    );
}
