//! TRUE end-to-end: real wire bytes → the production decoder → a real
//! `Reducer` → a `SceneState` → the REAL `TuiRenderer` (ratatui `TestBackend`)
//! → an actual pixel buffer, asserting a sprite was PAINTED.
//!
//! Three transport classes are covered:
//!   - JSONL transcript  → `Transport::Jsonl` (every transcript-bearing registered source)
//!   - agent hook         → `Transport::Hook`  (every hook-only registered source)
//!   - daemon presence    → the sibling channel (`apply_presence`) (openclaw)

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use pixtuoid_core::SceneState;
use pixtuoid_core::harness::{DRIVEN_DESKS, Drive, Reach};
use pixtuoid_core::source::daemon::apply_presence;
use pixtuoid_core::source::registry;
use pixtuoid_scene::pack::OfficeArt;
use pixtuoid_scene::pack::load_bundled_pack;
use pixtuoid_scene::theme::NORMAL;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use pixtuoid::dev::TuiRenderer;

/// A fixed wall-clock so motion/animation is deterministic across runs.
fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800)
}

/// The core crate's source-test tree, so these tests decode the SAME captured
/// wire bytes that crate pins, never a hand-rolled re-encoding. It is the tree
/// and not `fixtures/` alone because a two-session capture cannot live under the
/// conformance harness's one-AgentId rule.
fn core_fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("pixtuoid-core")
        .join("tests")
        .join("sources")
}

fn read_nonblank_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

fn pack() -> Arc<OfficeArt> {
    static PACK: OnceLock<Arc<OfficeArt>> = OnceLock::new();
    Arc::clone(PACK.get_or_init(|| Arc::new(load_bundled_pack().expect("pack"))))
}

fn new_renderer(cols: u16, rows: u16) -> TuiRenderer<TestBackend> {
    let terminal = Terminal::new(TestBackend::new(cols, rows)).expect("test backend");
    TuiRenderer::new(terminal, &NORMAL, vec![], pack())
}

/// Render `scene` through the real renderer for a fixed settle window and return
/// the final frame's subpixels. The empty baseline and the occupied frame go
/// through the IDENTICAL sequence, so a later pixel-diff cancels the
/// (timezone-dependent) sky and isolates the agent's footprint.
fn settled_pixels(scene: &SceneState, cols: u16, rows: u16, now: SystemTime) -> Vec<(u8, u8, u8)> {
    // Bounded on BOTH sides: long enough for the entry walk to finish, and
    // SHORTER than VacancyDim's 5s EMPTY_DEBOUNCE_MS — past that the empty
    // office's floor dim would start easing while the occupied office holds its
    // lights, so the two backgrounds would stop being byte-identical and the diff
    // would no longer be purely the agent's paint. Don't raise past ~150 frames.
    const SETTLE_FRAMES: usize = 30;
    const FRAME_STEP: Duration = Duration::from_millis(pixtuoid_scene::anim::PAINT_FRAME_MS);
    let pack = pack();
    let mut r = new_renderer(cols, rows);
    let mut t = now;
    for _ in 0..SETTLE_FRAMES {
        r.render(scene, &pack, t).expect("render office");
        t += FRAME_STEP;
    }
    r.buf()
        .expect("a frame")
        .as_slice()
        .iter()
        .map(|px| (px.r, px.g, px.b))
        .collect()
}

/// The empty→occupied pixel-diff floor: well above the deterministic render's
/// zero-diff noise floor, and comfortably below a settled character's real
/// footprint so sprite-art tweaks don't trip it. IMMUNE to the time-of-day sky
/// because the diff cancels the (identical) background.
const MIN_SPRITE_PIXELS: usize = 40;

/// WHICH of a source's two wires this case drives — and, for a transcript,
/// whether the driver stands in for the watcher's first-sight registration.
enum Wire {
    /// The fixture is a transcript, driven through the source's registry
    /// `LineDecoder` on `Transport::Jsonl`.
    Transcript {
        /// Seed the `SessionStart` the watcher's first-sight would emit. Needed
        /// for a transcript that carries only activity: a JSONL event for an
        /// unknown id is a documented no-op, so without it the wire drives
        /// nothing. False where the fixture's own head line registers the session.
        seeded: bool,
    },
    /// The fixture is a `hook-payloads.jsonl`, driven through the shared
    /// `decode_hook_payload` on `Transport::Hook`. Never seeded: a hook event
    /// for an unknown session id REGISTERS it (hooks are proof of life).
    Hooks,
}

struct WireCase {
    name: &'static str,
    source: &'static str,
    /// Path to the fixture file, relative to `core_fixtures_root()`.
    fixture: &'static str,
    wire: Wire,
    /// The state classes this fixture's committed wire must drive the slot
    /// through — judged per-source against each decoder's documented semantics,
    /// so the wire is proven to reach the RIGHT lifecycle state, not merely a slot.
    must_reach: &'static [Reach],
}

fn assert_renders_a_sprite(case: &WireCase) {
    let path = core_fixtures_root().join(case.fixture);
    let lines = read_nonblank_lines(&path);

    let driven = match case.wire {
        Wire::Transcript { seeded } => {
            let drive = Drive::transcript_at(case.source, &path).unwrap_or_else(|| {
                panic!(
                    "{}: source {:?} has no line_decoder",
                    case.name, case.source
                )
            });
            let drive = if seeded { drive.seeded() } else { drive };
            drive.at(t0()).lines(lines)
        }
        Wire::Hooks => Drive::hooks().at(t0()).lines(lines),
    };

    driven.assert_clean(case.name);
    assert!(
        driven.events.len() >= 2,
        "{}: fixture should decode to a registration + activity, got {:?}",
        case.name,
        driven.events
    );
    assert!(
        !driven.scene.agents.is_empty(),
        "{}: the wire bytes must register at least one agent slot (got 0)",
        case.name
    );
    for want in case.must_reach {
        assert!(
            driven.reached.contains(want),
            "{}: the wire must drive the slot through {want:?}; reached only {:?}",
            case.name,
            driven.reached
        );
    }

    // Both skies are byte-identical (the agent touches neither weather nor
    // stars), so the diff is exactly the character's footprint.
    let (cols, rows) = (120, 44);
    // The empty baseline must be shaped like the driven scene (same desk
    // capacity), or the diff would carry a layout difference as well as the agent.
    let empty = settled_pixels(&SceneState::uniform(DRIVEN_DESKS), cols, rows, t0());
    let occupied = settled_pixels(&driven.scene, cols, rows, t0());
    let changed = empty.iter().zip(&occupied).filter(|(a, b)| a != b).count();
    assert!(
        changed >= MIN_SPRITE_PIXELS,
        "{}: an occupied office (from real {} wire bytes) must repaint materially more \
         pixels than the empty office at the same instant (changed={changed}, \
         floor={MIN_SPRITE_PIXELS})",
        case.name,
        case.source
    );
}

/// The wire→pixels matrix: every supported AGENT source, transcript and hook
/// classes both.
fn agent_cases() -> Vec<WireCase> {
    vec![
        WireCase {
            name: "claude_code",
            source: "claude-code",
            fixture: "fixtures/claude-code/permission-recorded/947851e2-a15a-4de8-8375-46e9cddb5c8f.jsonl",
            wire: Wire::Transcript { seeded: true },
            must_reach: &[Reach::Active],
        },
        WireCase {
            name: "codex",
            source: "codex",
            fixture: "fixtures/codex/permission-recorded/rollout-2026-08-15T06-28-11-01a0059b-cb9d-7a92-8821-85ebb7604464.jsonl",
            wire: Wire::Transcript { seeded: true },
            must_reach: &[Reach::Active],
        },
        WireCase {
            name: "antigravity",
            source: "antigravity",
            fixture: "fixtures/antigravity/tool-run-recorded/transcript.jsonl",
            wire: Wire::Transcript { seeded: true },
            must_reach: &[Reach::Active],
        },
        WireCase {
            name: "copilot",
            source: "copilot",
            fixture: "fixtures/copilot/tool-run/65f8cef9-7dd8-46fa-9f6a-78cc95f68ab3/events.jsonl",
            wire: Wire::Transcript { seeded: false },
            must_reach: &[Reach::Active],
        },
        WireCase {
            name: "omp",
            source: "omp",
            fixture: "fixtures/omp/tool-run-recorded/2026-09-19T18-16-13-370Z_01a0bae2-133a-769f-851a-927926fbca45.jsonl",
            wire: Wire::Transcript { seeded: false },
            must_reach: &[Reach::Active],
        },
        WireCase {
            name: "omp-bridge",
            source: "omp",
            // The bridge's approval round: the Waiting the transcript can
            // never carry must survive all the way to pixels (#951).
            fixture: "omp/fixtures/approval-recorded/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Waiting, Reach::Active],
        },
        WireCase {
            name: "grok",
            source: "grok",
            fixture: "fixtures/grok/permission-recorded/01a0059d-ece9-7931-8e07-c91d77ba5943/updates.jsonl",
            wire: Wire::Transcript { seeded: true },
            must_reach: &[Reach::Active],
        },
        WireCase {
            name: "reasonix",
            source: "reasonix",
            fixture: "reasonix/fixtures/subagent-hermetic/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active, Reach::Delegating],
        },
        WireCase {
            name: "codewhale",
            source: "codewhale",
            fixture: "fixtures/codewhale/tool-run/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active],
        },
        WireCase {
            name: "opencode",
            source: "opencode",
            fixture: "fixtures/opencode/session-recorded/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active, Reach::Waiting],
        },
        WireCase {
            name: "cursor",
            source: "cursor",
            // The DELEGATING capture, not the tool-run one: `Delegating` needs a
            // Task dispatch, and a real cursor turn only delegates when asked.
            // Two sessions, so it cannot live under `fixtures/`.
            fixture: "cursor/fixtures/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active, Reach::Delegating],
        },
        WireCase {
            name: "hermes",
            source: "hermes",
            fixture: "fixtures/hermes/tool-run/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active],
        },
        WireCase {
            // The gate fires TWICE for one command, so this also drives the
            // reducer path where a re-notified Waiting must not clear the gate
            // the first one set.
            name: "hermes_approval",
            source: "hermes",
            fixture: "fixtures/hermes/approval-recorded/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Waiting],
        },
        WireCase {
            // The recorded tool run pins Active on real wire.
            name: "dsh",
            source: "dsh",
            fixture: "fixtures/dsh/tool-run-recorded/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active],
        },
        WireCase {
            // The delegation capture: two sessions (so it lives outside
            // `fixtures/`, like cursor's), pinning the parent's Delegating
            // through the `subagent` dispatch.
            name: "dsh-delegation",
            source: "dsh",
            fixture: "delegation/fixtures/dsh/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active, Reach::Delegating],
        },
        WireCase {
            name: "kimi",
            source: "kimi",
            // The permission fixture, because a HEADLESS kimi run cannot reach
            // Waiting — it auto-approves even `rm -rf`, so `PermissionRequest`
            // only fires for a human at a real TUI. Both fixtures are captures;
            // this is the one whose turn covers both classes.
            fixture: "fixtures/kimi/permission-flow/hook-payloads.jsonl",
            wire: Wire::Hooks,
            must_reach: &[Reach::Active, Reach::Waiting],
        },
    ]
}

/// Look up one matrix case by its registered source name — never a positional
/// index into `agent_cases()`, where an insertion would silently shift every
/// downstream case onto the wrong fixture.
fn agent_case(name: &str) -> WireCase {
    // Keyed on `name`, not `source`: a source may have more than one case (hermes
    // has a tool run and an approval gate) and `name` is the unique one.
    agent_cases()
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no wire-to-pixels case named {name:?}"))
}

/// The matrix is truth-complete: a newly registered source with no wire case
/// FAILS here instead of silently shipping untested.
#[test]
fn wire_matrix_covers_every_registered_source() {
    use std::collections::BTreeSet;

    // Uniqueness is on NAME, which is what `agent_case` looks up — a source may
    // legitimately have two cases (hermes: a tool run and an approval gate).
    let names: BTreeSet<&str> = agent_cases().iter().map(|c| c.name).collect();
    assert_eq!(
        names.len(),
        agent_cases().len(),
        "two agent_cases rows share a `name`, so one is unreachable"
    );
    let agents: BTreeSet<&str> = agent_cases().iter().map(|c| c.source).collect();
    for source in &agents {
        let d = registry::descriptor_for(source)
            .unwrap_or_else(|| panic!("matrix source {source:?} is not in the registry"));
        assert!(
            !d.is_daemon(),
            "{source}: a daemon belongs in the presence-test list, not the agent matrix"
        );
    }

    // A 2nd daemon must gain a presence test AND a row here.
    let daemons_covered = [pixtuoid_core::source::openclaw::SOURCE_NAME];
    for source in daemons_covered {
        let d = registry::descriptor_for(source)
            .unwrap_or_else(|| panic!("presence-test source {source:?} is not in the registry"));
        assert!(d.is_daemon(), "{source}: presence tests are for daemons");
    }

    let covered: BTreeSet<&str> = agents.iter().copied().chain(daemons_covered).collect();
    let registered: BTreeSet<&str> =
        pixtuoid_core::source::registry::registered_source_names().collect();
    assert_eq!(
        covered,
        registered,
        "the wire→pixels suite must cover EVERY registered source.\n  \
         registered but untested (add a WireCase / presence test): {:?}\n  \
         tested but not registered (stale row): {:?}",
        registered.difference(&covered).collect::<Vec<_>>(),
        covered.difference(&registered).collect::<Vec<_>>(),
    );
}

#[test]
fn every_agent_source_renders_a_painted_sprite_from_real_wire() {
    for case in agent_cases() {
        assert_renders_a_sprite(&case);
    }
}

// Per-source tests too, so a single source can be run in isolation and a
// regression localizes to its own `#[test]`.

#[test]
fn claude_code_transcript_line_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("claude_code"));
}

#[test]
fn codex_transcript_line_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("codex"));
}

#[test]
fn antigravity_transcript_line_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("antigravity"));
}

#[test]
fn copilot_transcript_line_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("copilot"));
}

#[test]
fn omp_transcript_line_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("omp"));
}

#[test]
fn omp_bridge_approval_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("omp-bridge"));
}

#[test]
fn grok_transcript_line_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("grok"));
}

#[test]
fn reasonix_hook_envelope_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("reasonix"));
}

#[test]
fn codewhale_hook_envelope_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("codewhale"));
}

#[test]
fn opencode_hook_envelope_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("opencode"));
}

#[test]
fn cursor_hook_envelope_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("cursor"));
}

#[test]
fn hermes_hook_envelope_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("hermes"));
}

#[test]
fn hermes_approval_gate_renders_a_waiting_sprite() {
    assert_renders_a_sprite(&agent_case("hermes_approval"));
}

#[test]
fn kimi_hook_envelope_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("kimi"));
}

#[test]
fn dsh_plugin_lifecycle_renders_a_painted_sprite() {
    assert_renders_a_sprite(&agent_case("dsh"));
}

/// Cells the gateway presence ADDS to an otherwise identical office.
///
/// A differential, not a colour probe: the foreground wash recolours every
/// drawable, so matching the authored carapace RGB would have to predict the
/// wash — coupling a presence assertion to the lighting model and expecting a
/// value the code under test computed.
fn lobster_px(
    r: &mut TuiRenderer<TestBackend>,
    scene: &SceneState,
    pack: &pixtuoid_scene::pack::OfficeArt,
    times: &[SystemTime],
) -> usize {
    let baseline = SceneState::uniform(16);
    let settle = |r: &mut TuiRenderer<TestBackend>, s: &SceneState| {
        for &t in times {
            r.render(s, pack, t).expect("render office");
        }
    };
    settle(r, &baseline);
    let base = r.buf().expect("a frame").clone();
    settle(r, scene);
    let buf = r.buf().expect("a frame");
    (0..buf.height())
        .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.get(x, y) != base.get(x, y))
        .count()
}

/// Drives one openclaw capture through the presence seam and hands back what the
/// office would paint. `stop_at` ends the stream before a type, as the lobster
/// test does to stay inside the live window.
fn openclaw_state(scenario: &str, stop_at: &[&str]) -> pixtuoid_core::state::DaemonState {
    let hooks =
        core_fixtures_root().join(format!("fixtures/openclaw/{scenario}/hook-payloads.jsonl"));
    let mut scene = SceneState::uniform(16);
    let now = t0();
    for line in &read_nonblank_lines(&hooks) {
        let v: serde_json::Value = serde_json::from_str(line).expect("openclaw hook json");
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if stop_at.contains(&ty) {
            break;
        }
        let decoded = pixtuoid_core::source::openclaw::decode_openclaw_hook_payload(&v)
            .expect("decode_openclaw_hook_payload");
        let key = pixtuoid_core::source::daemon::DaemonInstanceKey::new(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            decoded.instance.clone(),
        );
        for update in decoded.updates {
            apply_presence(&mut scene, &key, update, now);
        }
    }
    let (_, _, presence) = scene
        .daemons()
        .find(|(source, _, _)| *source == pixtuoid_core::source::openclaw::SOURCE_NAME)
        .expect("openclaw presence must be populated");
    presence.display_state()
}

/// pixtuoid usually starts AFTER the gateway, so `gateway_start` is the one
/// envelope a real attach never sees; the capture here opens on `session_start`.
#[test]
fn a_gateway_we_never_saw_start_still_materializes_from_its_session_wire() {
    use pixtuoid_core::state::DaemonState;
    assert_eq!(
        openclaw_state("mid-attach-recorded", &["agent_end"]),
        DaemonState::Busy,
        "a mid-attach stream carries no gateway_start, so the session envelopes must \
         materialize the instance themselves and put the run in flight on it"
    );
    assert_eq!(
        openclaw_state("mid-attach-recorded", &[]),
        DaemonState::Idle,
        "and agent_end must retire that run"
    );
}

/// A provider that refuses every connection, from real bytes: each retried
/// attempt's `agent_end` carries no `error`, and only the plugin's `errored`
/// tells the failure apart.
#[test]
fn a_failed_provider_degrades_the_gateway_rather_than_downing_it() {
    use pixtuoid_core::state::DaemonState;
    assert_eq!(
        openclaw_state("provider-failed-run-recorded", &["gateway_stop"]),
        DaemonState::Degraded
    );
}

/// The daemon/presence class: `apply_presence` is the sibling-channel seam
/// `runtime/driver.rs` uses — NEVER `Reducer::apply`, which is `AgentId`-pure.
#[test]
fn openclaw_presence_envelope_renders_a_lobster() {
    let hooks = core_fixtures_root()
        .join("fixtures/openclaw/gateway-lifecycle-recorded/hook-payloads.jsonl");

    let lines = read_nonblank_lines(&hooks);
    let mut scene = SceneState::uniform(16);
    let now = t0();
    let mut applied = 0;
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("openclaw hook json");
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "session_end" || ty == "gateway_stop" {
            break;
        }
        let decoded = pixtuoid_core::source::openclaw::decode_openclaw_hook_payload(&v)
            .expect("decode_openclaw_hook_payload");
        let key = pixtuoid_core::source::daemon::DaemonInstanceKey::new(
            pixtuoid_core::source::openclaw::SOURCE_NAME,
            decoded.instance.clone(),
        );
        for update in decoded.updates {
            apply_presence(&mut scene, &key, update, now);
            applied += 1;
        }
    }
    assert!(
        applied > 0,
        "the openclaw fixture must decode to presence deltas (got 0)"
    );

    let (_, _, presence) = scene
        .daemons()
        .find(|(source, _, _)| *source == pixtuoid_core::source::openclaw::SOURCE_NAME)
        .expect("openclaw presence must be populated");
    assert_ne!(
        presence.liveness,
        pixtuoid_core::state::DaemonLiveness::Down,
        "the in-session presence stream must leave the gateway UP"
    );

    // `entered_at` == `now` means the lobster is mid walk-in; settle at a LATER
    // wall-clock so it's scuttling the floor, well past the elevator.
    let pack = pack();
    let mut r = new_renderer(160, 80);
    let times: Vec<_> = (0..10)
        .map(|k| now + Duration::from_secs(20) + Duration::from_millis(k * 130))
        .collect();
    assert!(
        lobster_px(&mut r, &scene, &pack, &times) > 10,
        "a live OpenClaw gateway (from real presence wire bytes) must paint the lobster mascot"
    );
}
