use super::*;
use pixtuoid_core::source::{CopyId, claude_code, codex, copilot, hermes, openclaw, opencode};
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
    fn new(cues: impl IntoIterator<Item = Cue>, started: SystemTime) -> Self {
        Self {
            playhead: Playhead::new(Script::new(PERIOD, cues), started),
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
fn fresh(cues: impl IntoIterator<Item = Cue>, after: Duration) -> SceneState {
    let mut host = Host::new(cues, t0());
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
    let mut cues = vec![lead().start(ms(0)), cx.start(ms(0))];
    cues.extend(lead().spell(ms(1_000), 3, &["Bash: cargo test", "Read: src/lib.rs"]));
    cues.extend(cx.spell(ms(1_000), 1, &["exec_command"]));
    let mid = |k: u32| ms(1_000) + CALL * k + CALL / 2;
    for (k, want) in [ToolKind::Bash, ToolKind::Read, ToolKind::Bash]
        .into_iter()
        .enumerate()
    {
        let k = u32::try_from(k).expect("three calls");
        assert_eq!(
            kind(&fresh(cues.clone(), mid(k)), &lead()),
            Some(want),
            "call {k}"
        );
    }
    assert_eq!(
        kind(&fresh(cues.clone(), mid(0)), &cx),
        Some(ToolKind::Other)
    );
    let after = ms(1_000) + CALL * 3 + ms(1);
    assert_eq!(state(&fresh(cues, after), &lead()), ActivityState::Idle);
}

#[test]
fn a_wait_requires_action_until_its_call_ends() {
    for (source, tool) in [
        (hermes::SOURCE_NAME, "terminal: rm -rf dist"),
        (copilot::SOURCE_NAME, "bash: rm -rf dist"),
    ] {
        let asker = Actor::new(source, "asker", "/work/tests");
        let mut cues = vec![asker.start(ms(0))];
        cues.extend(asker.wait(ms(1_000), ms(5_000), tool));
        assert!(
            matches!(
                state(&fresh(cues.clone(), ms(1_000)), &asker),
                ActivityState::RequiresAction { reason: Some(reason) } if &*reason == "permission"
            ),
            "{source}"
        );
        assert!(
            matches!(
                state(&fresh(cues.clone(), ms(5_999)), &asker),
                ActivityState::RequiresAction { .. }
            ),
            "{source}"
        );
        assert_eq!(
            state(&fresh(cues, ms(6_000)), &asker),
            ActivityState::Idle,
            "{source}"
        );
    }
}

#[test]
fn a_subagent_works_under_its_delegating_lead_and_ends_as_a_child() {
    let sub = lead().subagent("lead-sub");
    let mut cues = vec![
        lead().start(ms(0)),
        sub.start(ms(2_000)),
        sub.end(ms(8_000)),
    ];
    cues.extend(lead().delegate(ms(1_000), ms(9_000)));
    cues.extend(sub.spell(ms(2_000), 4, &["Read: src/lib.rs"]));
    let working = fresh(cues.clone(), ms(3_000));
    assert_eq!(working.agents[&sub.id()].parent_id, Some(lead().id()));
    assert_eq!(kind(&working, &lead()), Some(ToolKind::Task));
    assert_eq!(kind(&working, &sub), Some(ToolKind::Read));
    let ended = fresh(cues, ms(8_001));
    assert!(ended.agents[&sub.id()].exiting_at.is_some());
    assert_eq!(kind(&ended, &lead()), Some(ToolKind::Task));
    let as_child = |actor: &Actor| match actor.end(ms(0)) {
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
    let cues = [
        lead().start(ms(0)),
        lead().caught_up(ms(0)),
        lead().model(ms(1_000), "claude-fable-5"),
        lead().usage(ms(2_000), 1_000),
    ];
    let scene = fresh(cues, ms(2_000));
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

fn presence(at: Duration, update: DaemonPresenceUpdate) -> Cue {
    Cue::Presence {
        at,
        key: gateway(),
        update,
    }
}

#[test]
fn a_presence_cue_lands_on_the_daemon_roster_and_registers_no_agent() {
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
    let cues = [
        presence(ms(0), GatewayUp { pid: None }),
        presence(ms(1_000), GatewayDown),
        presence(ms(30_000), SessionEnded),
    ];
    let mut stepped = Host::new(cues.clone(), t0());
    let mut after = Duration::ZERO;
    while after <= ms(30_000) {
        stepped.at(t0() + after);
        after += ms(250);
    }
    let caught_up = fresh(cues, ms(30_000));
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
fn a_script_orders_its_cues_by_phase_ties_in_authored_order() {
    let past_the_wrap = lead().end(PERIOD + ms(500));
    let [start, ask, end] = lead().wait(ms(1_000), ms(1_000), "Bash: rm -rf dist");
    let script = Script::new(
        PERIOD,
        [
            end.clone(),
            start.clone(),
            ask.clone(),
            past_the_wrap.clone(),
        ],
    );
    assert_eq!(script.cues, [past_the_wrap, start, ask, end]);
}

/// A loop's calls and named readings count again every lap, on the agent and
/// on the day's tally.
#[test]
fn a_replayed_loop_counts_its_work_again() {
    let mut cues = vec![
        lead().start(ms(0)),
        lead().caught_up(ms(0)),
        Cue::Event {
            at: ms(500),
            transport: Transport::Jsonl,
            event: AgentEvent::usage(lead().id(), 100, Some("reading".to_owned()))
                .with_copy_id(CopyId::new("copy")),
        },
    ];
    cues.extend(lead().spell(ms(1_000), 3, &["Bash: cargo test"]));
    let mut host = Host {
        playhead: Playhead::new(Script::new(PERIOD, cues), t0()),
        reducer: Reducer::new().counting(|_| chrono::NaiveDate::MIN),
        scene: SceneState::uniform(8),
    };
    for lap in 1..=3u32 {
        let scene = host.at(t0() + PERIOD * lap - ms(1));
        let slot = &scene.agents[&lead().id()];
        assert_eq!(slot.tool_call_count, 3 * lap, "lap {lap}");
        assert_eq!(slot.tokens_used, 100 * u64::from(lap), "lap {lap}");
        assert_eq!(
            scene.today.map(|today| (today.tool_calls, today.tokens)),
            Some((u64::from(3 * lap), 100 * u64::from(lap))),
            "lap {lap}"
        );
    }
}

#[test]
#[should_panic(expected = "a looped script needs a period")]
fn a_script_without_a_period_is_refused() {
    let _ = Script::new(Duration::ZERO, [lead().start(ms(0))]);
}

/// Usage accumulates once per landed reading: the stepping can't double or
/// drop one.
#[test]
fn each_cue_lands_once_however_the_clock_steps() {
    let cues = [
        lead().start(ms(0)),
        lead().caught_up(ms(0)),
        lead().usage(ms(10_000), 1),
        lead().usage(ms(40_000), 10),
    ];
    let span = PERIOD * 2 + ms(15_000);
    for step in [ms(33), ms(700), ms(13_000)] {
        let mut host = Host::new(cues.clone(), t0());
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

/// A sample cast's coming and going across the wrap, as the hero's.
fn cast() -> (Vec<Actor>, Vec<Cue>) {
    let cx = Actor::new(codex::SOURCE_NAME, "cx", "/work/web");
    let hm = Actor::new(hermes::SOURCE_NAME, "hm", "/work/tests");
    let visitor = Actor::new(claude_code::SOURCE_NAME, "visitor", "/work/docs");
    let mut cues = vec![
        lead().start(ms(0)),
        cx.start(ms(400)),
        hm.start(ms(800)),
        visitor.start(ms(30_000)),
        visitor.end(ms(55_000)),
    ];
    cues.extend(lead().spell(ms(1_000), 8, &["Edit: src/main.rs", "Bash: cargo test"]));
    cues.extend(cx.spell(ms(20_000), 6, &["apply_patch", "exec_command"]));
    cues.extend(hm.wait(ms(40_000), ms(8_000), "terminal: rm -rf dist"));
    cues.extend(lead().spell(PERIOD - ms(600), 1, &["Read: src/lib.rs"]));
    (vec![lead(), cx, hm, visitor], cues)
}

#[test]
fn the_office_stays_seated_and_never_empties_across_the_wrap() {
    let (actors, cues) = cast();
    let mut host = Host::new(cues, t0());
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
fn a_call_across_the_wrap_runs_from_its_start_through_it() {
    let (_, cues) = cast();
    let mut host = Host::new(cues, t0());
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

/// The first loop's wrapped End belongs to a loop before `started`, so it
/// never lands: landed inside the hook-wins window before its own loop's
/// Start, it would drop that Start as an already-ended call.
#[test]
fn the_first_loop_never_lands_a_wrapped_end_and_still_runs_its_start() {
    use pixtuoid_core::state::reducer::HOOK_WINS_WINDOW;
    let start = ms(2_000);
    let early = HOOK_WINS_WINDOW / 2;
    let mut cues = vec![lead().start(ms(0))];
    cues.extend(lead().delegate(start, PERIOD - early));
    assert_eq!(
        state(&fresh(cues.clone(), start - early), &lead()),
        ActivityState::Idle
    );
    assert_eq!(kind(&fresh(cues, start), &lead()), Some(ToolKind::Task));
}

#[test]
fn a_playhead_started_a_loop_ago_seats_everyone_in_the_past() {
    let (actors, cues) = cast();
    let now = t0() + PERIOD;
    let mut host = Host::new(cues, t0());
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
fn a_long_gap_jumps_whole_loops_in_phase() {
    let (_, mut cues) = cast();
    cues.extend([lead().caught_up(ms(0)), lead().usage(ms(10_000), 1)]);
    let mut host = Host::new(cues, t0());
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

/// A call open when the clock jumps still ends: the jump plays on in the lap
/// it interrupts, so the End names the Start that landed.
#[test]
fn a_call_open_across_a_long_gap_still_ends() {
    let mut cues = vec![lead().start(ms(0))];
    cues.extend(lead().spell(ms(1_000), 1, &["Bash: cargo test"]));
    cues.extend(lead().spell(ms(20_000), 1, &["Read: src/lib.rs"]));
    let mut host = Host::new(cues, t0());
    let open = host.at(t0() + ms(1_500)).clone();
    assert_eq!(kind(&open, &lead()), Some(ToolKind::Bash), "premise");
    let after = host.at(t0() + PERIOD * 10 + ms(30_000));
    assert_eq!(state(after, &lead()), ActivityState::Idle);
}

#[test]
fn a_fresh_playhead_is_the_office_a_stepped_one_reaches() {
    let (_, cues) = cast();
    let mut stepped = Host::new(cues.clone(), t0());
    let mut after = Duration::ZERO;
    while after < PERIOD * 2 - ms(1_000) {
        stepped.at(t0() + after);
        after += ms(33);
    }
    let now = t0() + after;
    assert_eq!(stepped.at(now), &fresh(cues.clone(), after));
    assert_eq!(fresh(cues.clone(), after), fresh(cues, after));
}

#[test]
#[should_panic(expected = "names no registered source")]
fn an_actor_of_no_registered_source_is_refused() {
    let _ = Actor::new("claude_code", "typo", "/work/api");
}

/// A visitor whose end lands just before the wrap and whose start opens the
/// loop comes back every loop, over hooks or over a transcript alone.
#[test]
fn a_visitor_ending_at_the_wrap_returns_every_loop() {
    for source in [claude_code::SOURCE_NAME, copilot::SOURCE_NAME] {
        let visitor = Actor::new(source, "visitor", "/work/docs");
        let cues = [visitor.start(ms(0)), visitor.end(PERIOD - ms(2_000))];
        let mut host = Host::new(cues, t0());
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

/// Every id the reducer counts once and every turn an event names takes the
/// lap, whichever field carries it.
#[test]
fn a_lap_names_every_counted_id_and_turn_apart() {
    let lap = Lap {
        n: 3,
        seq_stride: 10,
    };
    let agent_id = lead().id();
    let id = || "x".to_owned();
    let turn_id = || TurnKey::Id("t".into());
    let request = || AgentEvent::UserActionRequest {
        agent_id,
        reason: None,
        tool_use_id: Some(id()),
        tool_name: None,
        turn: Some(turn_id()),
    };
    let pending = || AgentEvent::ToolUsePending {
        agent_id,
        tool_use_id: id(),
        tool_name: None,
        turn: Some(turn_id()),
    };
    for mut event in [
        AgentEvent::tool_use_start(agent_id, Some(id()), None),
        AgentEvent::ToolUseEnd {
            agent_id,
            tool_use_id: Some(id()),
        },
        request(),
        pending(),
        AgentEvent::UserActionResolved {
            agent_id,
            by: Resolution::Call(id()),
        },
        AgentEvent::usage(agent_id, 1, Some(id())),
    ] {
        lap.name(&mut event);
        assert_eq!(
            counted_id(&mut event).cloned(),
            Some("x#3".to_owned()),
            "{event:?}"
        );
    }
    for (mut event, want) in [
        (
            AgentEvent::TurnStart {
                agent_id,
                turn: turn_id(),
            },
            TurnKey::Id("t#3".into()),
        ),
        (
            AgentEvent::TurnEnd {
                agent_id,
                turn: Some(turn_id()),
            },
            TurnKey::Id("t#3".into()),
        ),
        (request(), TurnKey::Id("t#3".into())),
        (pending(), TurnKey::Id("t#3".into())),
        (
            AgentEvent::UserActionResolved {
                agent_id,
                by: Resolution::Turn(turn_id()),
            },
            TurnKey::Id("t#3".into()),
        ),
        (
            AgentEvent::TurnStart {
                agent_id,
                turn: TurnKey::Seq(2),
            },
            TurnKey::Seq(2 + 10 * 3),
        ),
    ] {
        lap.name(&mut event);
        assert_eq!(turn(&mut event).cloned(), Some(want), "{event:?}");
    }
}

/// Each turn a loop brackets opens again every lap, by an opaque id or by its
/// position on the stream, which the reducer would refuse as a replay. Two
/// `Seq` turns from 0 pin the stride: one past the highest, never zero.
#[test]
fn a_replayed_turn_opens_again_every_lap() {
    let brackets = [(ms(1_000), ms(6_000)), (ms(10_000), ms(15_000))];
    for (source, keys) in [
        (
            codex::SOURCE_NAME,
            [TurnKey::Id("a".into()), TurnKey::Id("b".into())],
        ),
        (opencode::SOURCE_NAME, [TurnKey::Seq(0), TurnKey::Seq(1)]),
    ] {
        let turner = Actor::new(source, "turner", "/work/cli");
        let bracket = |at, event| Cue::Event {
            at,
            transport: turner.lifecycle,
            event,
        };
        let mut cues = vec![turner.start(ms(0))];
        for ((opens, closes), turn) in brackets.into_iter().zip(&keys) {
            cues.push(bracket(
                opens,
                AgentEvent::TurnStart {
                    agent_id: turner.id(),
                    turn: turn.clone(),
                },
            ));
            cues.push(bracket(
                closes,
                AgentEvent::TurnEnd {
                    agent_id: turner.id(),
                    turn: Some(turn.clone()),
                },
            ));
        }
        let mut host = Host::new(cues, t0());
        for lap in 0..3u32 {
            for (opens, closes) in brackets {
                let open = host.at(t0() + PERIOD * lap + opens + ms(500));
                assert!(
                    matches!(state(open, &turner), ActivityState::Running { .. }),
                    "{source} lap {lap} at {opens:?}: {:?}",
                    state(open, &turner)
                );
                let closed = host.at(t0() + PERIOD * lap + closes + ms(500));
                assert_eq!(
                    state(closed, &turner),
                    ActivityState::Idle,
                    "{source} lap {lap} at {closes:?}"
                );
            }
        }
    }
}
