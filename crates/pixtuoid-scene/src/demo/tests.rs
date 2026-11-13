use super::*;
use pixtuoid_core::source::{claude_code, codex, hermes, openclaw};
use pixtuoid_core::state::{ActivityState, DaemonInstanceId, ToolKind};

const PERIOD: Duration = Duration::from_secs(60);

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
}

/// A host: what a front end owns beside its playhead.
struct Host {
    playhead: Playhead,
    reducer: Reducer,
    scene: SceneState,
}

impl Host {
    fn new(beats: impl IntoIterator<Item = Beat>, started: SystemTime) -> Self {
        Self {
            playhead: Playhead::new(Script::new(PERIOD, beats), started),
            reducer: Reducer::new(),
            scene: SceneState::uniform(8),
        }
    }

    fn at(&mut self, now: SystemTime) -> &SceneState {
        self.playhead
            .advance(now, &mut self.reducer, &mut self.scene);
        &self.scene
    }
}

/// The office a fresh playhead started at [`t0`] shows `after` it.
fn fresh(beats: impl IntoIterator<Item = Beat>, after: Duration) -> SceneState {
    let mut host = Host::new(beats, t0());
    host.at(t0() + after).clone()
}

fn lead() -> Actor {
    Actor::new(claude_code::SOURCE_NAME, "lead", "/work/api")
}

fn state(scene: &SceneState, actor: &Actor) -> ActivityState {
    scene.agents[&actor.id()].state.clone()
}

fn kind(scene: &SceneState, actor: &Actor) -> Option<ToolKind> {
    match state(scene, actor) {
        ActivityState::Running { item } => item.map(|item| item.kind),
        _ => None,
    }
}

#[test]
fn a_spell_runs_its_calls_back_to_back_each_kind_from_its_real_tool_name() {
    let cx = Actor::new(codex::SOURCE_NAME, "cx", "/work/web");
    let mut beats = vec![lead().start(ms(0)), cx.start(ms(0))];
    beats.extend(lead().spell(ms(1_000), 3, &["Bash: cargo test", "Read: src/lib.rs"]));
    beats.extend(cx.spell(ms(1_000), 1, &["exec_command"]));
    let mid = |k: u32| ms(1_000) + CALL * k + CALL / 2;
    for (k, want) in [ToolKind::Bash, ToolKind::Read, ToolKind::Bash]
        .into_iter()
        .enumerate()
    {
        let k = u32::try_from(k).expect("three calls");
        assert_eq!(
            kind(&fresh(beats.clone(), mid(k)), &lead()),
            Some(want),
            "call {k}"
        );
    }
    assert_eq!(
        kind(&fresh(beats.clone(), mid(0)), &cx),
        Some(ToolKind::Other)
    );
    let after = ms(1_000) + CALL * 3 + ms(1);
    assert_eq!(state(&fresh(beats, after), &lead()), ActivityState::Idle);
}

#[test]
fn a_wait_requires_action_until_its_call_ends() {
    let hm = Actor::new(hermes::SOURCE_NAME, "hm", "/work/tests");
    let mut beats = vec![hm.start(ms(0))];
    beats.extend(hm.wait(ms(1_000), ms(5_000), "terminal: rm -rf dist"));
    assert!(matches!(
        state(&fresh(beats.clone(), ms(1_000)), &hm),
        ActivityState::RequiresAction { reason: Some(reason) } if &*reason == "permission"
    ));
    assert!(matches!(
        state(&fresh(beats.clone(), ms(5_999)), &hm),
        ActivityState::RequiresAction { .. }
    ));
    assert_eq!(state(&fresh(beats, ms(6_000)), &hm), ActivityState::Idle);
}

#[test]
fn a_subagent_works_under_its_delegating_lead_and_ends_as_a_child() {
    let sub = lead().subagent("lead-sub");
    let mut beats = vec![
        lead().start(ms(0)),
        sub.start(ms(2_000)),
        sub.end(ms(8_000)),
    ];
    beats.extend(lead().delegate(ms(1_000), ms(9_000)));
    beats.extend(sub.spell(ms(2_000), 4, &["Read: src/lib.rs"]));
    let working = fresh(beats.clone(), ms(3_000));
    assert_eq!(working.agents[&sub.id()].parent_id, Some(lead().id()));
    assert_eq!(kind(&working, &lead()), Some(ToolKind::Task));
    assert_eq!(kind(&working, &sub), Some(ToolKind::Read));
    let ended = fresh(beats, ms(8_001));
    assert!(ended.agents[&sub.id()].exiting_at.is_some());
    assert_eq!(kind(&ended, &lead()), Some(ToolKind::Task));
    let as_child = |actor: &Actor| match actor.end(ms(0)).cue {
        Cue::Event {
            event: AgentEvent::SessionEnd { as_child, .. },
            ..
        } => as_child,
        other => panic!("{other:?}"),
    };
    assert!(as_child(&sub) && !as_child(&lead()));
}

#[test]
fn a_model_and_a_usage_reading_land_on_the_slot() {
    let beats = [
        lead().start(ms(0)),
        lead().caught_up(ms(0)),
        lead().model(ms(1_000), "claude-fable-5"),
        lead().usage(ms(2_000), 1_000),
    ];
    let scene = fresh(beats, ms(2_000));
    let slot = &scene.agents[&lead().id()];
    assert_eq!(slot.model.as_deref(), Some("claude-fable-5"));
    assert_eq!(slot.tokens_used, 1_000);
}

#[test]
fn a_presence_beat_lands_on_the_daemon_roster_and_registers_no_agent() {
    let key = DaemonInstanceKey::new(
        openclaw::SOURCE_NAME,
        DaemonInstanceId::from_port(std::num::NonZeroU16::MIN),
    );
    let beat = Beat {
        at: ms(1_000),
        cue: Cue::Presence {
            key: key.clone(),
            update: DaemonPresenceUpdate::GatewayUp { pid: None },
        },
    };
    let scene = fresh([beat], ms(1_000));
    assert!(scene.daemon(key.source(), key.instance()).is_some());
    assert!(scene.agents.is_empty());
}

#[test]
fn a_script_lands_each_beat_at_its_phase_ties_in_authored_order() {
    let past_the_seam = lead().end(PERIOD + ms(5_000));
    let [start, ask, end] = lead().wait(ms(1_000), ms(1_000), "Bash: rm -rf dist");
    let script = Script::new(
        PERIOD,
        [
            past_the_seam.clone(),
            end.clone(),
            start.clone(),
            ask.clone(),
        ],
    );
    let wrapped = Beat {
        at: ms(5_000),
        ..past_the_seam
    };
    assert_eq!(script.beats, [start, ask, end, wrapped]);
}

#[test]
#[should_panic(expected = "a looped script needs a period")]
fn a_script_without_a_period_is_refused() {
    let _ = Script::new(Duration::ZERO, [lead().start(ms(0))]);
}

/// Usage accumulates once per landed reading: the stepping can't double or
/// drop one.
#[test]
fn each_beat_lands_once_however_the_clock_steps() {
    let beats = [
        lead().start(ms(0)),
        lead().caught_up(ms(0)),
        lead().usage(ms(10_000), 1),
        lead().usage(ms(40_000), 10),
    ];
    let span = PERIOD * 2 + ms(15_000);
    for step in [ms(33), ms(700), ms(13_000)] {
        let mut host = Host::new(beats.clone(), t0());
        let mut after = Duration::ZERO;
        while after <= span {
            host.at(t0() + after);
            after += step;
        }
        assert_eq!(
            host.scene.agents[&lead().id()].tokens_used,
            1 + 10 + 1 + 10 + 1,
            "stepped by {step:?}"
        );
    }
}

/// A sample cast's coming and going across the seam, as the hero's.
fn cast() -> (Vec<Actor>, Vec<Beat>) {
    let cx = Actor::new(codex::SOURCE_NAME, "cx", "/work/web");
    let hm = Actor::new(hermes::SOURCE_NAME, "hm", "/work/tests");
    let visitor = Actor::new(claude_code::SOURCE_NAME, "visitor", "/work/docs");
    let mut beats = vec![
        lead().start(ms(0)),
        cx.start(ms(400)),
        hm.start(ms(800)),
        visitor.start(ms(30_000)),
        visitor.end(ms(55_000)),
    ];
    beats.extend(lead().spell(ms(1_000), 8, &["Edit: src/main.rs", "Bash: cargo test"]));
    beats.extend(cx.spell(ms(20_000), 6, &["apply_patch", "exec_command"]));
    beats.extend(hm.wait(ms(40_000), ms(8_000), "terminal: rm -rf dist"));
    beats.extend(lead().spell(PERIOD - ms(600), 1, &["Read: src/lib.rs"]));
    (vec![lead(), cx, hm, visitor], beats)
}

#[test]
fn the_office_stays_seated_and_never_empties_across_the_wrap() {
    let (actors, beats) = cast();
    let mut host = Host::new(beats, t0());
    let mut after = Duration::ZERO;
    while after <= PERIOD * 3 {
        let scene = host.at(t0() + after);
        if after >= ms(800) {
            assert!(scene.agents.len() >= 3, "{after:?}: {}", scene.agents.len());
        }
        assert!(scene.agents.len() <= actors.len(), "{after:?}");
        let desks: std::collections::BTreeSet<_> =
            scene.agents.values().filter_map(|slot| slot.desk).collect();
        let seated = scene
            .agents
            .values()
            .filter(|slot| slot.desk.is_some())
            .count();
        assert_eq!(desks.len(), seated, "{after:?}: two agents share a desk");
        after += ms(250);
    }
}

#[test]
fn a_call_across_the_seam_runs_from_its_start_through_the_wrap() {
    let (_, beats) = cast();
    let mut host = Host::new(beats, t0());
    for loop_start in [PERIOD, PERIOD * 2] {
        let running = host.at(t0() + loop_start - ms(300)).clone();
        assert_eq!(
            kind(&running, &lead()),
            Some(ToolKind::Read),
            "{loop_start:?}"
        );
        let through = host.at(t0() + loop_start + ms(300)).clone();
        assert_eq!(
            kind(&through, &lead()),
            Some(ToolKind::Read),
            "{loop_start:?}"
        );
        let ended = host.at(t0() + loop_start + ms(600)).clone();
        assert_eq!(
            state(&ended, &lead()),
            ActivityState::Idle,
            "{loop_start:?}"
        );
    }
}

/// The first loop's wrapped End has no Start before it: the reducer ignores
/// it, and it doesn't eat that loop's own Start of the call.
#[test]
fn the_first_loop_ignores_a_wrapped_end_and_still_runs_its_start() {
    let (_, beats) = cast();
    assert_eq!(
        state(&fresh(beats.clone(), ms(600)), &lead()),
        ActivityState::Idle
    );
    let caught_up = fresh(beats, PERIOD - ms(300));
    assert_eq!(kind(&caught_up, &lead()), Some(ToolKind::Read));
}

#[test]
fn a_playhead_started_a_loop_ago_seats_everyone_in_the_past() {
    let (actors, beats) = cast();
    let now = t0() + PERIOD;
    let mut host = Host::new(beats, t0());
    let scene = host.at(now);
    let starts = [ms(0), ms(400), ms(800)];
    for (actor, start) in actors.iter().zip(starts) {
        assert_eq!(
            scene.agents[&actor.id()].arrived_at,
            Some(t0() + start),
            "{actor:?} walks in at its first loop's start, not now"
        );
    }
}

#[test]
fn nothing_lands_before_the_script_starts() {
    let mut host = Host::new(cast().1, t0() + ms(10_000));
    assert!(host.at(t0()).agents.is_empty());
}

#[test]
fn a_long_gap_replays_the_last_whole_loop_in_phase() {
    let (_, mut beats) = cast();
    beats.extend([lead().caught_up(ms(0)), lead().usage(ms(10_000), 1)]);
    let mut host = Host::new(beats, t0());
    host.at(t0());
    let scene = host.at(t0() + PERIOD * 10 + ms(7_000)).clone();
    let advanced = host.playhead.origin.duration_since(t0()).expect("forward");
    assert_eq!(advanced, PERIOD * 10, "the current loop, in phase");
    assert_eq!(
        scene.agents[&lead().id()].tokens_used,
        1,
        "one loop replayed"
    );
    assert!(scene.agents.len() >= 3, "{}", scene.agents.len());
}

#[test]
fn a_fresh_playhead_is_the_office_a_stepped_one_reaches() {
    let (_, beats) = cast();
    let mut stepped = Host::new(beats.clone(), t0());
    let mut after = Duration::ZERO;
    while after < PERIOD * 2 - ms(1_000) {
        stepped.at(t0() + after);
        after += ms(33);
    }
    let now = t0() + after;
    assert_eq!(stepped.at(now), &fresh(beats.clone(), after));
    assert_eq!(fresh(beats.clone(), after), fresh(beats, after));
}
