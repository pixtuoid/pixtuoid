//! The hero's scripted cast — a deterministic, LOOPED event timeline fed
//! through the REAL `Reducer`, so the web office can never drift from real
//! behavior. Just the same `(Transport, AgentEvent)` stream a live CLI produces.
//!
//! On loop wrap the same events replay: a `SessionStart` for a live slot is
//! a reducer no-op (backfill arm), the ended agent re-enters, so the office
//! stays coherent forever.

use std::path::PathBuf;

use pixtuoid_core::source::daemon::DaemonPresenceUpdate;
use pixtuoid_core::source::{
    antigravity, claude_code, codewhale, codex, copilot, cursor, grok, hermes, omp, opencode,
    reasonix,
};
use pixtuoid_core::{AgentEvent, AgentId, ToolDetail, Transport};

#[derive(Debug)]
pub(crate) struct Beat {
    pub at_ms: u64,
    pub transport: Transport,
    pub event: AgentEvent,
}

/// Long enough that the cycle doesn't read as a loop.
pub(crate) const LOOP_MS: u64 = 120_000;

/// A cast member: a source CLI + a repo-ish cwd (drives the label AND the
/// Team-Palette outfit, which keys on cwd). Sources MUST reference the modules'
/// `SOURCE_NAME` consts — a hand-typed string silently misses the registry and
/// the label falls back to the RAW string (`claude_code·api` not `cc·api`).
const CAST: &[(&str, &str, &str)] = &[
    // (source, session key, cwd)
    (claude_code::SOURCE_NAME, "hero-cc-api", "/work/api"),
    (codex::SOURCE_NAME, "hero-cx-web", "/work/webapp"),
    (antigravity::SOURCE_NAME, "hero-ag-infra", "/work/infra"),
    (codewhale::SOURCE_NAME, "hero-cw-data", "/work/data"),
    (opencode::SOURCE_NAME, "hero-oc-cli", "/work/cli"),
    (copilot::SOURCE_NAME, "hero-cp-docs", "/work/docs"),
    (cursor::SOURCE_NAME, "hero-cu-etl", "/work/etl"),
    (hermes::SOURCE_NAME, "hero-hm-tests", "/work/tests"),
    (grok::SOURCE_NAME, "hero-gk-ml", "/work/ml"),
    (reasonix::SOURCE_NAME, "hero-rx-research", "/work/research"),
    (omp::SOURCE_NAME, "hero-om-embedded", "/work/embedded"),
];

#[cfg(test)]
pub(crate) const CAST_LEN: usize = CAST.len();

pub(crate) fn cast_id(i: usize) -> AgentId {
    let (source, key, _) = CAST[i];
    AgentId::from_parts(source, key)
}

fn session_start(i: usize) -> AgentEvent {
    let (source, key, cwd) = CAST[i];
    AgentEvent::SessionStart {
        agent_id: cast_id(i),
        source: source.to_string(),
        session_id: key.to_string(),
        cwd: PathBuf::from(cwd),
        parent_id: None,
    }
}

/// One scripted call's length. Calls chain back to back: the reducer settles a
/// slot the moment its call ends, so any gap between them shows as Idle.
const BURST_MS: u64 = 1200;

fn tool(i: usize, at_ms: u64, tuid: &str, display: &str) -> [Beat; 2] {
    [
        Beat {
            at_ms,
            transport: Transport::Hook,
            event: AgentEvent::ToolUseStart {
                agent_id: cast_id(i),
                tool_use_id: Some(format!("hero-{i}-{tuid}")),
                detail: Some(ToolDetail::Generic {
                    display: display.to_string(),
                }),
            },
        },
        Beat {
            at_ms: at_ms + BURST_MS,
            transport: Transport::Hook,
            event: AgentEvent::ToolUseEnd {
                agent_id: cast_id(i),
                tool_use_id: Some(format!("hero-{i}-{tuid}")),
            },
        },
    ]
}

/// `n` chained bursts — continuously Active, then the agent settles Idle and
/// the engine's wander takes over until the next spell.
fn spell(beats: &mut Vec<Beat>, i: usize, at_ms: u64, n: u64, tools: &[&str]) {
    for (k, &display) in (0..n).zip(tools.iter().cycle()) {
        let t = format!("s{at_ms}-{k}");
        beats.extend(tool(i, at_ms + k * BURST_MS, &t, display));
    }
}

/// A token-usage reading: the desk paper tower's wire. HONESTY RULE — only
/// cast members whose REAL CLI carries a per-turn usage wire (cc, cx) get
/// these; the hero must never show a tower the product can't produce for that
/// CLI. Jsonl transport, matching production (usage is JSONL-only).
fn usage(i: usize, at_ms: u64, fresh_tokens: u64) -> Beat {
    Beat {
        at_ms,
        transport: Transport::Jsonl,
        event: AgentEvent::Usage {
            agent_id: cast_id(i),
            fresh_tokens,
        },
    }
}

pub(crate) const HIRE_STAY_MS: u64 = 70_000;

/// One visitor-hired coworker's lifecycle, as offsets from the hire instant.
/// Chains its calls back to back on the cast's `BURST_MS`. The cwd is the
/// hire's own ("/work/yours") so Team Palette gives hires a distinct outfit
/// family.
pub(crate) fn hire_beats(id: AgentId, session: String) -> Vec<(u64, AgentEvent)> {
    let mut out: Vec<(u64, AgentEvent)> = Vec::new();
    out.push((
        0,
        AgentEvent::SessionStart {
            agent_id: id,
            source: claude_code::SOURCE_NAME.to_string(),
            session_id: session,
            cwd: PathBuf::from("/work/yours"),
            parent_id: None,
        },
    ));
    // Spaced so the hire also idles/wanders like everyone else.
    for (k, start) in [8_000u64, 28_000, 50_000].into_iter().enumerate() {
        for b in 0..4u64 {
            let at = start + b * BURST_MS;
            let tuid = format!("hire-{k}-{b}");
            out.push((
                at,
                AgentEvent::ToolUseStart {
                    agent_id: id,
                    tool_use_id: Some(tuid.clone()),
                    detail: Some(ToolDetail::Generic {
                        display: "Edit".to_string(),
                    }),
                },
            ));
            out.push((
                at + BURST_MS,
                AgentEvent::ToolUseEnd {
                    agent_id: id,
                    tool_use_id: Some(tuid),
                },
            ));
        }
    }
    out.push((
        HIRE_STAY_MS,
        AgentEvent::SessionEnd {
            agent_id: id,
            as_child: false,
        },
    ));
    out
}

/// One scripted presence beat for the OpenClaw gateway mascot. Presence is
/// deliberately NOT a [`Beat`]/`AgentEvent` (invariant #2: the one event
/// channel is `AgentId`-pure) — these ride their own lane and land through the
/// REAL `source::daemon::apply_presence` state machine.
#[derive(Debug)]
pub(crate) struct PresenceBeat {
    pub at_ms: u64,
    pub update: DaemonPresenceUpdate,
}

/// The ONE gateway the hero scripts, so the authored timeline lands on exactly
/// one lobster.
pub(crate) fn hero_gateway() -> pixtuoid_core::source::daemon::DaemonInstanceKey {
    pixtuoid_core::source::daemon::DaemonInstanceKey::new(
        pixtuoid_core::source::openclaw::SOURCE_NAME,
        pixtuoid_core::state::DaemonInstanceId::from_port(HERO_GATEWAY_PORT),
    )
}

/// OpenClaw's documented default gateway port — the hero's single instance id.
const HERO_GATEWAY_PORT: std::num::NonZeroU16 = std::num::NonZeroU16::new(18789).expect("nonzero");

/// The lobster's loop: the OpenClaw mascot scuttles in mid-loop, shuttles
/// through two busy turns, and walks out before the wrap — so every loop
/// replays a clean enter animation (GatewayUp after Down re-anchors
/// `entered_at`).
pub(crate) fn lobster_beats() -> Vec<PresenceBeat> {
    use DaemonPresenceUpdate::*;
    [
        (25_000, GatewayUp { pid: None }),
        (
            40_000,
            TurnStarted {
                turn: pixtuoid_core::state::TurnKey::Id("hero-turn-1".into()),
            },
        ),
        (
            62_000,
            TurnEnded {
                turn: pixtuoid_core::state::TurnKey::Id("hero-turn-1".into()),
            },
        ),
        (
            78_000,
            TurnStarted {
                turn: pixtuoid_core::state::TurnKey::Id("hero-turn-2".into()),
            },
        ),
        (
            96_000,
            TurnEnded {
                turn: pixtuoid_core::state::TurnKey::Id("hero-turn-2".into()),
            },
        ),
        (112_000, GatewayDown),
    ]
    .into_iter()
    .map(|(at_ms, update)| PresenceBeat { at_ms, update })
    .collect()
}

pub(crate) fn hero_script() -> Vec<Beat> {
    let mut b: Vec<Beat> = Vec::new();

    // Walk-ins — the morning rush: the cast is through the door within ~2.5s
    // of reveal, so scroll-0 never reads as an empty looping video.
    for (i, delay) in [0u64, 350, 750, 1_150, 1_600, 2_050, 2_500]
        .iter()
        .enumerate()
    {
        b.push(Beat {
            at_ms: *delay,
            transport: Transport::Jsonl,
            event: session_start(i),
        });
    }
    // The rush's tail: gk and rx trail the first seven, keeping the door busy
    // without delaying the ≥4-monitors-by-3s pin.
    b.push(Beat {
        at_ms: 2_900,
        transport: Transport::Jsonl,
        event: session_start(8),
    });
    b.push(Beat {
        at_ms: 3_300,
        transport: Transport::Jsonl,
        event: session_start(9),
    });
    b.push(Beat {
        at_ms: 30_000,
        transport: Transport::Jsonl,
        event: session_start(10),
    });

    // Opening spells: ≥4 monitors on by ~3s; offsets interleave so
    // wander/coffee/meetings emerge.
    spell(
        &mut b,
        0,
        1_000,
        8,
        &["Bash: cargo test", "Edit main.rs", "Read lib.rs"],
    );
    spell(&mut b, 1, 1_600, 6, &["Edit App.tsx", "Bash: pnpm build"]);
    spell(
        &mut b,
        2,
        2_200,
        10,
        &["Bash: terraform plan", "Read modules.tf"],
    );
    spell(&mut b, 3, 2_800, 6, &["Bash: dbt run", "Edit models.sql"]);
    spell(&mut b, 4, 3_400, 8, &["Edit cmd.rs", "Bash: cargo clippy"]);
    spell(&mut b, 5, 4_000, 6, &["Read index.ts", "Edit routes.ts"]);
    spell(&mut b, 8, 4_600, 8, &["Bash: pytest -q", "Edit agent.py"]);
    spell(&mut b, 9, 5_200, 6, &["Read planner.md", "Edit reason.rs"]);
    spell(
        &mut b,
        1,
        16_000,
        6,
        &["Bash: pnpm test", "Edit styles.css"],
    );
    spell(&mut b, 3, 24_000, 6, &["Read schema.sql", "Edit etl.py"]);
    spell(
        &mut b,
        0,
        42_000,
        10,
        &["Write api.rs", "Bash: cargo check"],
    );
    spell(
        &mut b,
        2,
        55_000,
        8,
        &["Bash: kubectl apply", "Read deploy.yml"],
    );
    spell(
        &mut b,
        1,
        62_000,
        8,
        &["Edit styles.css", "Bash: pnpm test"],
    );
    spell(&mut b, 6, 40_000, 6, &["Edit README.md", "Read guide.md"]);
    spell(
        &mut b,
        4,
        70_000,
        8,
        &["Bash: cargo build", "Edit parse.rs"],
    );
    spell(&mut b, 3, 80_000, 6, &["Read schema.sql", "Edit etl.py"]);
    spell(&mut b, 5, 90_000, 8, &["Bash: vitest run", "Edit hooks.ts"]);
    spell(&mut b, 8, 38_000, 8, &["Edit train.py", "Bash: pytest -q"]);
    spell(&mut b, 9, 48_000, 6, &["Edit reason.rs", "Read notes.md"]);
    spell(
        &mut b,
        10,
        32_000,
        8,
        &["Bash: make flash", "Edit firmware.c"],
    );
    spell(&mut b, 10, 76_000, 6, &["Read boot.c", "Edit firmware.c"]);
    spell(&mut b, 8, 86_000, 6, &["Bash: pytest -q", "Edit eval.py"]);
    spell(&mut b, 9, 96_000, 6, &["Read paper.md", "Edit reason.rs"]);
    spell(
        &mut b,
        0,
        100_000,
        6,
        &["Edit tests.rs", "Bash: cargo test"],
    );

    // Token-meter readings — cc (0) and cx (1) only, the two cast CLIs whose
    // real wires carry per-turn usage. cc opens with a big restore-sized
    // reading so its first ream lands within the first second of the reveal.
    b.push(usage(0, 500, 1_200_000));
    b.push(usage(0, 11_000, 180_000));
    b.push(usage(0, 54_000, 320_000));
    b.push(usage(0, 108_000, 200_000));
    b.push(usage(1, 12_000, 150_000));
    b.push(usage(1, 25_000, 90_000));
    b.push(usage(1, 70_000, 160_000));

    // A permission park: agent 6 raises a request mid-loop, answered by its
    // member call's completion.
    b.push(Beat {
        at_ms: 58_000,
        transport: Transport::Hook,
        event: AgentEvent::ToolUseStart {
            agent_id: cast_id(6),
            tool_use_id: Some("hero-6-gated".to_string()),
            detail: Some(ToolDetail::Generic {
                display: "Bash: rm -rf dist".to_string(),
            }),
        },
    });
    b.push(Beat {
        at_ms: 58_400,
        transport: Transport::Hook,
        event: AgentEvent::UserActionRequest {
            agent_id: cast_id(6),
            reason: "permission".to_string(),
            tool_use_id: None,
        },
    });
    b.push(Beat {
        at_ms: 70_500,
        transport: Transport::Hook,
        event: AgentEvent::ToolUseEnd {
            agent_id: cast_id(6),
            tool_use_id: Some("hero-6-gated".to_string()),
        },
    });

    b.push(Beat {
        at_ms: 104_000,
        transport: Transport::Hook,
        event: AgentEvent::SessionEnd {
            agent_id: cast_id(5),
            as_child: false,
        },
    });
    b.push(Beat {
        at_ms: 108_000,
        transport: Transport::Jsonl,
        event: session_start(7),
    });
    spell(&mut b, 7, 110_000, 6, &["Read main.rs", "Bash: just test"]);
    // 7 leaves near the wrap: its replayed start lands AFTER this end, so the
    // pair nets out to a periodic visitor.
    b.push(Beat {
        at_ms: LOOP_MS - 2_000,
        transport: Transport::Hook,
        event: AgentEvent::SessionEnd {
            agent_id: cast_id(7),
            as_child: false,
        },
    });

    b.sort_by_key(|beat| beat.at_ms);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixtuoid_core::state::SceneState;
    use pixtuoid_core::state::reducer::Reducer;
    use std::time::{Duration, SystemTime};

    fn run_script_through_reducer(loops: u32) -> SceneState {
        let mut scene = SceneState::uniform(16);
        let mut reducer = Reducer::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000);
        let script = hero_script();
        for l in 0..loops {
            for beat in &script {
                let now = t0 + Duration::from_millis(u64::from(l) * LOOP_MS + beat.at_ms);
                reducer.apply(&mut scene, beat.event.clone(), now, beat.transport);
                reducer.tick(&mut scene, now);
            }
        }
        scene
    }

    #[test]
    fn script_is_sorted_and_fits_one_loop() {
        let s = hero_script();
        assert!(s.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
        assert!(s.last().unwrap().at_ms < LOOP_MS);
    }

    #[test]
    fn one_loop_populates_a_working_office() {
        let scene = run_script_through_reducer(1);
        assert!(
            scene.agents.len() >= 9,
            "expected a populated office, got {}",
            scene.agents.len()
        );
        let desks: std::collections::HashSet<_> =
            scene.agents.values().map(|a| a.desk_index.0).collect();
        assert_eq!(
            desks.len(),
            scene.agents.len(),
            "each agent has its own desk"
        );
        for a in scene.agents.values() {
            let prefix = a.label.split('·').next().unwrap();
            assert!(
                [
                    "cc", "cx", "ag", "cw", "oc", "cp", "cu", "hm", "gk", "rx", "om"
                ]
                .contains(&prefix),
                "label {:?} must carry a registered source prefix",
                a.label
            );
        }
    }

    #[test]
    fn usage_beats_grow_only_wire_bearing_towers() {
        let scene = run_script_through_reducer(1);
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
        for (i, (source, _, _)) in CAST.iter().enumerate().skip(2) {
            assert_eq!(
                tokens_of(i),
                0,
                "cast {i} ({source}) has no per-turn usage wire — its desk must stay bare"
            );
        }
        let scene3 = run_script_through_reducer(3);
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
        let mut scene = SceneState::uniform(16);
        let mut reducer = Reducer::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000);
        const RUSH_MS: u64 = 3_000;
        for beat in hero_script().iter().filter(|b| b.at_ms <= RUSH_MS) {
            let now = t0 + Duration::from_millis(beat.at_ms);
            reducer.apply(&mut scene, beat.event.clone(), now, beat.transport);
        }
        reducer.tick(&mut scene, t0 + Duration::from_millis(RUSH_MS));
        assert!(
            scene.agents.len() >= 6,
            "morning rush: expected >=6 walk-ins by 3s, got {}",
            scene.agents.len()
        );
        let active = scene
            .agents
            .values()
            .filter(|a| matches!(a.state, pixtuoid_core::state::ActivityState::Active { .. }))
            .count();
        // The schedule lands `active` at exactly this floor with zero headroom:
        // a spell-offset tweak can drop it to 3 without the morning-rush spec
        // actually regressing, so re-check the rendered office before assuming
        // a real regression.
        assert!(
            active >= 4,
            "morning rush: expected >=4 monitors on by 3s, got {active}"
        );
    }
}
