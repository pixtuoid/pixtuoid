use super::*;
use pixtuoid_core::source::{claude_code, codex, copilot, hermes, openclaw};
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
    fn new(moments: impl IntoIterator<Item = Moment>, started: SystemTime) -> Self {
        Self {
            playhead: Playhead::new(Script::new(PERIOD, moments), started),
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
fn fresh(moments: impl IntoIterator<Item = Moment>, after: Duration) -> SceneState {
    let mut host = Host::new(moments, t0());
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
    let mut moments = vec![lead().start(ms(0)), cx.start(ms(0))];
    moments.extend(lead().spell(ms(1_000), 3, &["Bash: cargo test", "Read: src/lib.rs"]));
    moments.extend(cx.spell(ms(1_000), 1, &["exec_command"]));
    let mid = |k: u32| ms(1_000) + CALL * k + CALL / 2;
    for (k, want) in [ToolKind::Bash, ToolKind::Read, ToolKind::Bash]
        .into_iter()
        .enumerate()
    {
        let k = u32::try_from(k).expect("three calls");
        assert_eq!(
            kind(&fresh(moments.clone(), mid(k)), &lead()),
            Some(want),
            "call {k}"
        );
    }
    assert_eq!(
        kind(&fresh(moments.clone(), mid(0)), &cx),
        Some(ToolKind::Other)
    );
    let after = ms(1_000) + CALL * 3 + ms(1);
    assert_eq!(state(&fresh(moments, after), &lead()), ActivityState::Idle);
}

#[test]
fn a_wait_requires_action_until_its_call_ends() {
    for (source, tool) in [
        (hermes::SOURCE_NAME, "terminal: rm -rf dist"),
        (copilot::SOURCE_NAME, "bash: rm -rf dist"),
    ] {
        let asker = Actor::new(source, "asker", "/work/tests");
        let mut moments = vec![asker.start(ms(0))];
        moments.extend(asker.wait(ms(1_000), ms(5_000), tool));
        assert!(
            matches!(
                state(&fresh(moments.clone(), ms(1_000)), &asker),
                ActivityState::RequiresAction { reason: Some(reason) } if &*reason == "permission"
            ),
            "{source}"
        );
        assert!(
            matches!(
                state(&fresh(moments.clone(), ms(5_999)), &asker),
                ActivityState::RequiresAction { .. }
            ),
            "{source}"
        );
        assert_eq!(
            state(&fresh(moments, ms(6_000)), &asker),
            ActivityState::Idle,
            "{source}"
        );
    }
}

#[test]
fn a_subagent_works_under_its_delegating_lead_and_ends_as_a_child() {
    let sub = lead().subagent("lead-sub");
    let mut moments = vec![
        lead().start(ms(0)),
        sub.start(ms(2_000)),
        sub.end(ms(8_000)),
    ];
    moments.extend(lead().delegate(ms(1_000), ms(9_000)));
    moments.extend(sub.spell(ms(2_000), 4, &["Read: src/lib.rs"]));
    let working = fresh(moments.clone(), ms(3_000));
    assert_eq!(working.agents[&sub.id()].parent_id, Some(lead().id()));
    assert_eq!(kind(&working, &lead()), Some(ToolKind::Task));
    assert_eq!(kind(&working, &sub), Some(ToolKind::Read));
    let ended = fresh(moments, ms(8_001));
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
    let moments = [
        lead().start(ms(0)),
        lead().caught_up(ms(0)),
        lead().model(ms(1_000), "claude-fable-5"),
        lead().usage(ms(2_000), 1_000),
    ];
    let scene = fresh(moments, ms(2_000));
    let slot = &scene.agents[&lead().id()];
    assert_eq!(slot.model.as_deref(), Some("claude-fable-5"));
    assert_eq!(slot.tokens_used, 1_000);
}

fn gateway() -> DaemonInstanceKey {
    DaemonInstanceKey::new(
        openclaw::SOURCE_NAME,
        DaemonInstanceId::from_port(std::num::NonZeroU16::MIN),
    )
}

fn presence(at: Duration, update: DaemonPresenceUpdate) -> Moment {
    Moment {
        at,
        cue: Cue::Presence {
            key: gateway(),
            update,
        },
    }
}

#[test]
fn a_presence_moment_lands_on_the_daemon_roster_and_registers_no_agent() {
    let key = gateway();
    let scene = fresh(
        [presence(
            ms(1_000),
            DaemonPresenceUpdate::GatewayUp { pid: None },
        )],
        ms(1_000),
    );
    assert!(scene.daemon(key.source(), key.instance()).is_some());
    assert!(scene.agents.is_empty());
}

/// A `SessionEnded` keeps a Down gateway Down but recreates a removed one
/// Up, so a catch-up must remove the Down entry before landing it, as a
/// stepped playhead's sweeps did.
#[test]
fn a_catch_up_sweeps_presence_before_each_delta_as_stepping_does() {
    use DaemonPresenceUpdate::*;
    let moments = [
        presence(ms(0), GatewayUp { pid: None }),
        presence(ms(1_000), GatewayDown),
        presence(ms(30_000), SessionEnded),
    ];
    let mut stepped = Host::new(moments.clone(), t0());
    let mut after = Duration::ZERO;
    while after <= ms(30_000) {
        stepped.at(t0() + after);
        after += ms(250);
    }
    let caught_up = fresh(moments, ms(30_000));
    let key = gateway();
    let shown = |scene: &SceneState| {
        scene
            .daemon(key.source(), key.instance())
            .map(|p| p.display_state())
    };
    assert_eq!(shown(&caught_up), shown(&stepped.scene));
    assert_eq!(caught_up, stepped.scene);
}

#[test]
fn a_script_lands_each_moment_at_its_phase_ties_in_authored_order() {
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
    let wrapped = Moment {
        at: ms(5_000),
        ..past_the_seam
    };
    assert_eq!(script.moments, [start, ask, end, wrapped]);
}

#[test]
#[should_panic(expected = "a looped script needs a period")]
fn a_script_without_a_period_is_refused() {
    let _ = Script::new(Duration::ZERO, [lead().start(ms(0))]);
}

/// Usage accumulates once per landed reading: the stepping can't double or
/// drop one.
#[test]
fn each_moment_lands_once_however_the_clock_steps() {
    let moments = [
        lead().start(ms(0)),
        lead().caught_up(ms(0)),
        lead().usage(ms(10_000), 1),
        lead().usage(ms(40_000), 10),
    ];
    let span = PERIOD * 2 + ms(15_000);
    for step in [ms(33), ms(700), ms(13_000)] {
        let mut host = Host::new(moments.clone(), t0());
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
fn cast() -> (Vec<Actor>, Vec<Moment>) {
    let cx = Actor::new(codex::SOURCE_NAME, "cx", "/work/web");
    let hm = Actor::new(hermes::SOURCE_NAME, "hm", "/work/tests");
    let visitor = Actor::new(claude_code::SOURCE_NAME, "visitor", "/work/docs");
    let mut moments = vec![
        lead().start(ms(0)),
        cx.start(ms(400)),
        hm.start(ms(800)),
        visitor.start(ms(30_000)),
        visitor.end(ms(55_000)),
    ];
    moments.extend(lead().spell(ms(1_000), 8, &["Edit: src/main.rs", "Bash: cargo test"]));
    moments.extend(cx.spell(ms(20_000), 6, &["apply_patch", "exec_command"]));
    moments.extend(hm.wait(ms(40_000), ms(8_000), "terminal: rm -rf dist"));
    moments.extend(lead().spell(PERIOD - ms(600), 1, &["Read: src/lib.rs"]));
    (vec![lead(), cx, hm, visitor], moments)
}

#[test]
fn the_office_stays_seated_and_never_empties_across_the_wrap() {
    let (actors, moments) = cast();
    let mut host = Host::new(moments, t0());
    let mut after = Duration::ZERO;
    while after <= PERIOD * 3 {
        let scene = host.at(t0() + after);
        if after >= ms(800) {
            assert!(scene.agents.len() >= 3, "{after:?}: {}", scene.agents.len());
        }
        assert!(scene.agents.len() <= actors.len(), "{after:?}");
        for slot in scene.agents.values() {
            assert!(
                slot.desk.is_some() || slot.exiting_at.is_some(),
                "{after:?}: {} unseated",
                slot.label
            );
        }
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
    let (_, moments) = cast();
    let mut host = Host::new(moments, t0());
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
    let (_, moments) = cast();
    assert_eq!(
        state(&fresh(moments.clone(), ms(600)), &lead()),
        ActivityState::Idle
    );
    let caught_up = fresh(moments, PERIOD - ms(300));
    assert_eq!(kind(&caught_up, &lead()), Some(ToolKind::Read));
}

#[test]
fn a_playhead_started_a_loop_ago_seats_everyone_in_the_past() {
    let (actors, moments) = cast();
    let now = t0() + PERIOD;
    let mut host = Host::new(moments, t0());
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
    let (_, mut moments) = cast();
    moments.extend([lead().caught_up(ms(0)), lead().usage(ms(10_000), 1)]);
    let mut host = Host::new(moments, t0());
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
    let (_, moments) = cast();
    let mut stepped = Host::new(moments.clone(), t0());
    let mut after = Duration::ZERO;
    while after < PERIOD * 2 - ms(1_000) {
        stepped.at(t0() + after);
        after += ms(33);
    }
    let now = t0() + after;
    assert_eq!(stepped.at(now), &fresh(moments.clone(), after));
    assert_eq!(fresh(moments.clone(), after), fresh(moments, after));
}

#[test]
#[should_panic(expected = "names no registered source")]
fn an_actor_of_no_registered_source_is_refused() {
    let _ = Actor::new("claude_code", "typo", "/work/api");
}

/// A visitor whose end lands just before the seam and whose start opens the
/// loop comes back every loop, over hooks or over a transcript alone.
#[test]
fn a_visitor_ending_at_the_seam_returns_every_loop() {
    for source in [claude_code::SOURCE_NAME, copilot::SOURCE_NAME] {
        let visitor = Actor::new(source, "visitor", "/work/docs");
        let moments = [visitor.start(ms(0)), visitor.end(PERIOD - ms(2_000))];
        let mut host = Host::new(moments, t0());
        let mut after = Duration::ZERO;
        while after < PERIOD * 3 + ms(1_000) {
            host.at(t0() + after);
            after += ms(500);
        }
        let slot = host.scene.agents.get(&visitor.id());
        assert!(
            slot.is_some_and(|slot| slot.exiting_at.is_none()),
            "{source}: {slot:?}"
        );
    }
}
