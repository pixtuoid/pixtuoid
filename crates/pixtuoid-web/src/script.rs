//! The hero's scripted cast: a looped [`Script`] the shared
//! [`Playhead`](pixtuoid_scene::demo::Playhead) plays through the REAL
//! `Reducer`, so the web office can never drift from real behavior.
//!
//! On loop wrap the same moments replay: a `SessionStart` for a live slot is
//! a reducer no-op (backfill arm), the ended agent re-enters, so the office
//! stays coherent forever.

use std::path::PathBuf;
use std::time::Duration;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::daemon::{DaemonInstanceKey, DaemonPresenceUpdate};
use pixtuoid_core::source::{
    antigravity, claude_code, codewhale, codex, copilot, cursor, grok, hermes, omp, openclaw,
    opencode, reasonix,
};
use pixtuoid_core::state::{DaemonInstanceId, TurnKey};
use pixtuoid_scene::DEMO_REPOS;
use pixtuoid_scene::demo::{Actor, Cue, Moment, Script};

/// Long enough that the cycle doesn't read as a loop.
pub(crate) const LOOP: Duration = Duration::from_secs(120);

/// A cast member: a source CLI + its session key; the `i`th works in
/// [`DEMO_REPOS`]`[i]`, the cwd its label AND its Team-Palette outfit key on.
const CAST: &[(&str, &str)] = &[
    // (source, session key)
    (claude_code::SOURCE_NAME, "hero-cc-api"),
    (codex::SOURCE_NAME, "hero-cx-web"),
    (antigravity::SOURCE_NAME, "hero-ag-infra"),
    (codewhale::SOURCE_NAME, "hero-cw-data"),
    (opencode::SOURCE_NAME, "hero-oc-cli"),
    (copilot::SOURCE_NAME, "hero-cp-docs"),
    (cursor::SOURCE_NAME, "hero-cu-etl"),
    (hermes::SOURCE_NAME, "hero-hm-tests"),
    (grok::SOURCE_NAME, "hero-gk-ml"),
    (reasonix::SOURCE_NAME, "hero-rx-research"),
    (omp::SOURCE_NAME, "hero-om-embedded"),
];
const _: () = assert!(
    CAST.len() <= DEMO_REPOS.len(),
    "a cast member without a repo"
);

#[cfg(test)]
pub(crate) const CAST_LEN: usize = CAST.len();

fn actor(i: usize) -> Actor {
    let (source, key) = CAST[i];
    Actor::new(source, key, PathBuf::from("/work").join(DEMO_REPOS[i]))
}

#[cfg(test)]
pub(crate) fn cast_id(i: usize) -> AgentId {
    actor(i).id()
}

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn spell(moments: &mut Vec<Moment>, i: usize, at: u64, calls: u32, tools: &[&str]) {
    moments.extend(actor(i).spell(ms(at), calls, tools));
}

pub(crate) const HIRE_STAY: Duration = Duration::from_secs(70);

/// Visitor hire `n`'s agent and lifecycle, from the hire instant. The cwd is
/// the hire's own ("/work/yours") so Team Palette gives hires a distinct
/// outfit family.
pub(crate) fn hire(n: u32) -> (AgentId, Vec<Moment>) {
    let hire = Actor::new(claude_code::SOURCE_NAME, format!("hire-{n}"), "/work/yours");
    let mut moments = vec![hire.start(Duration::ZERO), hire.end(HIRE_STAY)];
    // Spaced so the hire also idles/wanders like everyone else.
    for at in [8_000, 28_000, 50_000] {
        moments.extend(hire.spell(ms(at), 4, &["Edit: notes.md"]));
    }
    (hire.id(), moments)
}

/// The ONE gateway the hero scripts, so the authored timeline lands on exactly
/// one lobster.
pub(crate) fn hero_gateway() -> DaemonInstanceKey {
    DaemonInstanceKey::new(
        openclaw::SOURCE_NAME,
        DaemonInstanceId::from_port(HERO_GATEWAY_PORT),
    )
}

/// OpenClaw's documented default gateway port — the hero's single instance id.
const HERO_GATEWAY_PORT: std::num::NonZeroU16 = std::num::NonZeroU16::new(18789).expect("nonzero");

/// The lobster's loop: the OpenClaw mascot scuttles in mid-loop, shuttles
/// through two busy turns, and walks out before the wrap — so every loop
/// replays a clean enter animation (GatewayUp after Down re-anchors
/// `entered_at`).
fn lobster() -> impl Iterator<Item = Moment> {
    use DaemonPresenceUpdate::*;
    let turn = |id: &str| TurnKey::Id(id.into());
    [
        (25_000, GatewayUp { pid: None }),
        (
            40_000,
            TurnStarted {
                turn: turn("hero-turn-1"),
            },
        ),
        (
            62_000,
            TurnEnded {
                turn: turn("hero-turn-1"),
            },
        ),
        (
            78_000,
            TurnStarted {
                turn: turn("hero-turn-2"),
            },
        ),
        (
            96_000,
            TurnEnded {
                turn: turn("hero-turn-2"),
            },
        ),
        (112_000, GatewayDown),
    ]
    .into_iter()
    .map(|(at, update)| Moment {
        at: ms(at),
        cue: Cue::Presence {
            key: hero_gateway(),
            update,
        },
    })
}

pub(crate) fn hero() -> Script {
    let mut b: Vec<Moment> = Vec::new();

    // Walk-ins — the morning rush: the cast is through the door within ~2.5s
    // of reveal, so scroll-0 never reads as an empty looping video.
    for (i, at) in [0, 350, 750, 1_150, 1_600, 2_050, 2_500]
        .into_iter()
        .enumerate()
    {
        b.extend([actor(i).start(ms(at)), actor(i).caught_up(ms(at))]);
    }
    // The rush's tail: gk and rx trail the first seven, keeping the door busy
    // without delaying the ≥4-monitors-by-3s pin.
    b.push(actor(8).start(ms(2_900)));
    b.push(actor(9).start(ms(3_300)));
    b.push(actor(10).start(ms(30_000)));

    // Opening spells: ≥4 monitors on by ~3s; offsets interleave so
    // wander/coffee/meetings emerge. Each member calls its own CLI's tools, by
    // the names its recorded fixtures carry, so its monitor glows as the
    // product's would for that CLI.
    spell(
        &mut b,
        0,
        1_000,
        8,
        &["Bash: cargo test", "Edit: src/main.rs", "Read: src/lib.rs"],
    );
    spell(&mut b, 1, 1_600, 6, &["apply_patch", "exec_command"]);
    spell(
        &mut b,
        2,
        2_200,
        10,
        &["read_file: main.tf", "read_file: modules.tf"],
    );
    spell(&mut b, 3, 2_800, 6, &["bash: dbt run", "read: models.sql"]);
    spell(&mut b, 4, 3_400, 8, &["read", "shell"]);
    spell(
        &mut b,
        5,
        4_000,
        6,
        &["view: index.ts", "glob: src/**/*.ts"],
    );
    spell(
        &mut b,
        8,
        4_600,
        8,
        &["run_terminal_command: pytest -q", "read_file: agent.py"],
    );
    spell(
        &mut b,
        9,
        5_200,
        6,
        &["read_file: planner.md", "bash: cargo check"],
    );
    spell(&mut b, 1, 16_000, 6, &["exec_command", "apply_patch"]);
    spell(
        &mut b,
        3,
        24_000,
        6,
        &["read: schema.sql", "bash: python etl.py"],
    );
    spell(
        &mut b,
        0,
        42_000,
        10,
        &["Write: src/api.rs", "Bash: cargo check"],
    );
    spell(
        &mut b,
        2,
        55_000,
        8,
        &["read_file: deploy.yml", "read_file: values.yml"],
    );
    spell(&mut b, 1, 62_000, 8, &["apply_patch", "exec_command"]);
    spell(
        &mut b,
        6,
        40_000,
        6,
        &["Read: etl/README.md", "Shell: make etl"],
    );
    spell(&mut b, 4, 70_000, 8, &["shell", "read"]);
    spell(
        &mut b,
        3,
        80_000,
        6,
        &["read: schema.sql", "bash: python etl.py"],
    );
    spell(
        &mut b,
        5,
        90_000,
        8,
        &["bash: vitest run", "view: hooks.ts"],
    );
    spell(
        &mut b,
        8,
        38_000,
        8,
        &["read_file: train.py", "run_terminal_command: pytest -q"],
    );
    spell(
        &mut b,
        9,
        48_000,
        6,
        &["bash: cargo test", "read_file: notes.md"],
    );
    spell(
        &mut b,
        10,
        32_000,
        8,
        &["bash: make flash", "read: firmware.c"],
    );
    spell(&mut b, 10, 76_000, 6, &["read: boot.c", "bash: make test"]);
    spell(
        &mut b,
        8,
        86_000,
        6,
        &["run_terminal_command: pytest -q", "read_file: eval.py"],
    );
    spell(
        &mut b,
        9,
        96_000,
        6,
        &["read_file: paper.md", "bash: cargo check"],
    );
    spell(
        &mut b,
        0,
        100_000,
        6,
        &["Edit: tests/api.rs", "Bash: cargo test"],
    );

    // Token-meter readings — cc (0) and cx (1) only, the two cast CLIs whose
    // real wires carry per-turn usage: the hero must never show a tower the
    // product can't produce for that CLI. cc opens with a big restore-sized
    // reading so its first ream lands within the first second of the reveal.
    b.extend([
        actor(0).usage(ms(500), 1_200_000),
        actor(0).usage(ms(11_000), 180_000),
        actor(0).usage(ms(54_000), 320_000),
        actor(0).usage(ms(108_000), 200_000),
        actor(1).usage(ms(12_000), 150_000),
        actor(1).usage(ms(25_000), 90_000),
        actor(1).usage(ms(70_000), 160_000),
    ]);

    // A permission park: cp, whose CLI asks, waits on a prompt mid-loop until
    // its call completes.
    b.extend(actor(5).wait(ms(58_000), ms(12_500), "bash: rm -rf dist"));

    b.push(actor(5).end(ms(104_000)));
    b.push(actor(7).start(ms(108_000)));
    spell(
        &mut b,
        7,
        110_000,
        6,
        &["read_file: src/main.rs", "terminal: just test"],
    );
    // 7 leaves near the wrap: its replayed start lands AFTER this end, so the
    // pair nets out to a periodic visitor.
    b.push(actor(7).end(LOOP - ms(2_000)));

    Script::new(LOOP, b.into_iter().chain(lobster()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::state::SceneState;
    use pixtuoid_core::state::reducer::Reducer;
    use pixtuoid_scene::demo::Playhead;
    use std::time::SystemTime;

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000)
    }

    /// The office `after` the hero starts, its clock stepped by `step`.
    fn played(after: Duration, step: Duration) -> SceneState {
        let mut scene = SceneState::uniform(16);
        let mut reducer = Reducer::new();
        let mut playhead = Playhead::new(hero(), t0());
        let mut at = Duration::ZERO;
        while at < after {
            playhead.advance(t0() + at, &mut reducer, &mut scene);
            at += step;
        }
        playhead.advance(t0() + after, &mut reducer, &mut scene);
        scene
    }

    fn loops(n: u32) -> SceneState {
        played(LOOP * n - ms(1), Duration::from_secs(1))
    }

    #[test]
    fn one_loop_populates_a_running_office() {
        let scene = loops(1);
        assert!(
            scene.agents.len() >= 9,
            "expected a populated office, got {}",
            scene.agents.len()
        );
        let desks: std::collections::HashSet<_> =
            scene.agents.values().filter_map(|a| a.desk).collect();
        assert_eq!(
            desks.len(),
            scene.agents.len(),
            "each agent has its own desk"
        );
        for a in scene.agents.values() {
            assert!(
                pixtuoid_core::source::registry::descriptor_for(&a.source).is_some(),
                "{} works for an unregistered source, {:?}",
                a.label,
                a.source
            );
        }
    }

    #[test]
    fn usage_beats_grow_only_wire_bearing_towers() {
        let scene = loops(1);
        let tokens_of = |i: usize| scene.agents.get(&cast_id(i)).map_or(0, |a| a.tokens_used);
        assert!(
            pixtuoid_scene::token_meter::token_tier(tokens_of(0)) >= 1,
            "cc must carry a tower after one loop, got {} tokens",
            tokens_of(0)
        );
        assert!(
            pixtuoid_scene::token_meter::token_tier(tokens_of(1)) >= 1,
            "cx must carry a tower after one loop, got {} tokens",
            tokens_of(1)
        );
        for (i, (source, _)) in CAST.iter().enumerate().skip(2) {
            assert_eq!(
                tokens_of(i),
                0,
                "cast {i} ({source}) has no per-turn usage wire — its desk must stay bare"
            );
        }
        let scene3 = loops(3);
        let cc3 = scene3.agents.get(&cast_id(0)).map_or(0, |a| a.tokens_used);
        assert!(
            pixtuoid_scene::token_meter::token_tier(cc3) >= 2,
            "cc must have grown to T2 by loop 3, got {cc3}"
        );
        assert!(
            (9..=11).contains(&scene3.agents.len()),
            "cast must stay bounded across loops, got {}",
            scene3.agents.len()
        );
    }

    #[test]
    fn morning_rush_populates_within_three_seconds() {
        const RUSH: Duration = Duration::from_secs(3);
        let scene = played(RUSH, RUSH);
        assert!(
            scene.agents.len() >= 6,
            "morning rush: expected >=6 walk-ins by 3s, got {}",
            scene.agents.len()
        );
        let running = scene
            .agents
            .values()
            .filter(|a| matches!(a.state, pixtuoid_core::state::ActivityState::Running { .. }))
            .count();
        // The schedule lands `running` at exactly this floor with zero headroom:
        // a spell-offset tweak can drop it to 3 without the morning-rush spec
        // actually regressing, so re-check the rendered office before assuming
        // a real regression.
        assert!(
            running >= 4,
            "morning rush: expected >=4 monitors on by 3s, got {running}"
        );
    }
}
