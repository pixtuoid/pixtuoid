//! Smoke test that `TuiRenderer::render` drives a real half-block frame end to end, not
//! just an in-memory `SceneState` capture.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use pixtuoid::dev::{RenderFrames, TuiRenderer};
use pixtuoid_core::state::ActivityState;
use pixtuoid_core::{AgentId, AgentSlot, GlobalDeskIndex, SceneState};
use pixtuoid_scene::pack::OfficeArt;
use pixtuoid_scene::pack::load_bundled_pack;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn pack() -> Arc<OfficeArt> {
    static PACK: OnceLock<Arc<OfficeArt>> = OnceLock::new();
    Arc::clone(PACK.get_or_init(|| Arc::new(load_bundled_pack().expect("pack"))))
}

/// Build an `AgentSlot` for these render tests — fills the boilerplate fields so each
/// call site only varies what it cares about.
fn agent_slot(
    id: AgentId,
    session: &str,
    label: &str,
    desk: GlobalDeskIndex,
    floor: usize,
    state: ActivityState,
    now: SystemTime,
) -> AgentSlot {
    AgentSlot {
        agent_id: id,
        source: std::sync::Arc::from("claude-code"),
        session_id: std::sync::Arc::from(session),
        cwd: std::sync::Arc::from(PathBuf::from("/demo").as_path()),
        label: label.into(),
        state,
        state_started_at: now,
        created_at: now - Duration::from_secs(60),
        last_event_at: now - Duration::from_secs(60),
        exiting_at: None,
        pending_idle_at: None,
        desk_index: desk,
        floor_idx: floor,
        tool_call_count: 0,
        active_ms: 0,
        unknown_cwd: false,
        parent_id: None,
        pid: None,
        model: None,
        effort: None,
        tokens_used: 0,
        last_usage: None,
    }
}

#[test]
fn tui_renderer_render_paints_a_full_frame() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let mut scene = SceneState::uniform(8);
    let id = AgentId::from_transcript_path("/demo/a.jsonl");
    scene.agents.insert(
        id,
        agent_slot(
            id,
            "s-1",
            "demo",
            GlobalDeskIndex(0),
            0,
            ActivityState::Active {
                tool_use_id: Some(std::sync::Arc::from("t1")),
                detail: Some(std::sync::Arc::from("Write")),
                kind: pixtuoid_core::state::ToolKind::Edit,
            },
            now,
        ),
    );

    let backend = TestBackend::new(96, 36);
    let terminal = Terminal::new(backend).expect("terminal");
    let mut renderer = TuiRenderer::new(
        terminal,
        &pixtuoid_scene::theme::NORMAL,
        pixtuoid_scene::pet::PetKind::ALL
            .iter()
            .map(|&k| pixtuoid_scene::pet::Pet::defaulted(k))
            .collect(),
        pack(),
    );

    renderer.render(&scene, now).expect("render");

    // The 96×(36-1) scene area (one row reserved for the footer), doubled vertically
    // via half-block ⇒ 96 × 70.
    let buf = renderer.buf().expect("a frame");
    assert_eq!(buf.width(), 96);
    assert_eq!(buf.height(), 70);
}

/// The transition path must not hardcode `PetInputs::default()` or empty
/// coffee state — pets, cups and steam vanish during the slide if it does.
#[test]
fn tui_renderer_transition_paints_pets_and_coffee() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);

    let mut caps = [0usize; pixtuoid_core::state::MAX_FLOORS];
    caps[0] = 8;
    caps[1] = 8;
    let mut scene = SceneState::new(caps);
    for (i, name) in ["a", "b"].iter().enumerate() {
        let id = AgentId::from_transcript_path(&format!("/demo/{name}.jsonl"));
        scene.agents.insert(
            id,
            agent_slot(
                id,
                &format!("s-{i}"),
                name,
                GlobalDeskIndex(i * 8),
                i,
                ActivityState::Idle,
                now,
            ),
        );
    }

    let backend = TestBackend::new(96, 36);
    let terminal = Terminal::new(backend).expect("terminal");
    let mut renderer = TuiRenderer::new(
        terminal,
        &pixtuoid_scene::theme::NORMAL,
        pixtuoid_scene::pet::PetKind::ALL
            .iter()
            .map(|&k| pixtuoid_scene::pet::Pet::defaulted(k))
            .collect(),
        pack(),
    );

    // Initial render so the renderer grows its per-floor state to nf=2.
    renderer.render(&scene, now).expect("initial render");

    renderer.set_active_pet(Some(pixtuoid::dev::PetState {
        petted_at: now,
        kind: pixtuoid_scene::pet::PetKind::Cat,
        floor_idx: 0,
    }));

    renderer.navigate_floor(1, now);
    assert!(
        renderer.transition().is_some(),
        "navigate_floor should arm a transition"
    );

    let mid = now + Duration::from_millis(100);
    renderer.render(&scene, mid).expect("transition render");

    assert!(
        renderer.transition().is_some(),
        "transition should not have completed yet (was the path skipped?)"
    );

    let buf = renderer.buf().expect("a frame");
    let nonzero = buf
        .as_slice()
        .iter()
        .filter(|p| p.r != 0 || p.g != 0 || p.b != 0)
        .count();
    assert!(
        nonzero > 100,
        "transition buffer should have substantial paint (got {nonzero} non-black px)"
    );
}

#[test]
fn only_a_popup_edge_restarts_its_animation() {
    use std::time::{Duration, SystemTime};

    let backend = TestBackend::new(96, 36);
    let terminal = Terminal::new(backend).expect("terminal");
    let mut renderer = TuiRenderer::new(
        terminal,
        &pixtuoid_scene::theme::NORMAL,
        pixtuoid_scene::pet::PetKind::ALL
            .iter()
            .map(|&k| pixtuoid_scene::pet::Pet::defaulted(k))
            .collect(),
        pack(),
    );

    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let t1 = t0 + Duration::from_millis(50);
    let tick = Duration::from_millis(1);
    let popup = |open| RenderFrames {
        version_popup: pixtuoid::dev::VersionPopupFrame {
            open,
            ..Default::default()
        },
        ..Default::default()
    };

    assert_eq!(
        renderer.version_popup_scale(t0),
        0.0,
        "no edge yet: the popup is hidden"
    );

    renderer.set_frames(popup(false), t0);
    assert_eq!(
        renderer.version_popup_scale(t1),
        0.0,
        "false → false is no edge"
    );

    // 0.0 at t0 means the entrance clock starts no earlier than t0; growing a
    // tick later means it starts no later.
    renderer.set_frames(popup(true), t0);
    assert_eq!(
        renderer.version_popup_scale(t0),
        0.0,
        "the entrance starts at its edge"
    );
    assert!(
        renderer.version_popup_scale(t0 + tick) > 0.0,
        "the entrance starts at its edge"
    );

    let later = renderer.version_popup_scale(t1 + tick);
    renderer.set_frames(popup(true), t1);
    assert_eq!(
        renderer.version_popup_scale(t1 + tick),
        later,
        "true → true is no edge: the entrance runs on"
    );

    // The dismissal starts at t1 from the scale it interrupted: a stale t0 clock
    // would already have shrunk it by then.
    let interrupted = renderer.version_popup_scale(t1);
    renderer.set_frames(popup(false), t1);
    assert_eq!(
        renderer.version_popup_scale(t1),
        interrupted,
        "the dismissal starts at its edge, from where the entrance stood"
    );
}

#[test]
fn version_popup_animation_starts_small_then_grows() {
    use std::time::{Duration, SystemTime};

    let backend = TestBackend::new(96, 36);
    let terminal = Terminal::new(backend).expect("terminal");
    let mut renderer = TuiRenderer::new(
        terminal,
        &pixtuoid_scene::theme::NORMAL,
        pixtuoid_scene::pet::PetKind::ALL
            .iter()
            .map(|&k| pixtuoid_scene::pet::Pet::defaulted(k))
            .collect(),
        pack(),
    );

    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    renderer.set_frames(
        RenderFrames {
            version_popup: pixtuoid::dev::VersionPopupFrame {
                open: true,
                ..Default::default()
            },
            ..Default::default()
        },
        t0,
    );

    let scale_start = renderer.version_popup_scale(t0);
    assert!(
        scale_start < 0.1,
        "expected entrance start scale < 0.1; got {scale_start}"
    );

    let scale_mid = renderer.version_popup_scale(t0 + Duration::from_millis(100));
    assert!(
        scale_mid > 0.8,
        "expected scale > 0.8 at mid-entrance; got {scale_mid}"
    );

    let scale_end = renderer.version_popup_scale(t0 + Duration::from_millis(200));
    assert!(
        (scale_end - 1.0).abs() < 1e-3,
        "expected scale 1.0 at end of entrance; got {scale_end}"
    );

    let t1 = t0 + Duration::from_millis(200);
    renderer.set_frames(RenderFrames::default(), t1);

    let scale_dismiss_mid = renderer.version_popup_scale(t1 + Duration::from_millis(60));
    assert!(
        scale_dismiss_mid > 0.0 && scale_dismiss_mid < 1.0,
        "expected mid-dismissal scale between 0 and 1; got {scale_dismiss_mid}"
    );

    let scale_dismiss_end = renderer.version_popup_scale(t1 + Duration::from_millis(120));
    assert!(
        scale_dismiss_end < 0.01,
        "expected dismissal end scale ~0; got {scale_dismiss_end}"
    );
}

#[test]
fn dismiss_mid_entrance_does_not_snap_to_full() {
    let backend = TestBackend::new(96, 36);
    let terminal = Terminal::new(backend).expect("terminal");
    let mut renderer = TuiRenderer::new(
        terminal,
        &pixtuoid_scene::theme::NORMAL,
        pixtuoid_scene::pet::PetKind::ALL
            .iter()
            .map(|&k| pixtuoid_scene::pet::Pet::defaulted(k))
            .collect(),
        pack(),
    );
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    // 100ms is mid-entrance: EaseOutCubic(0.5) ≈ 0.875.
    renderer.set_frames(
        RenderFrames {
            version_popup: pixtuoid::dev::VersionPopupFrame {
                open: true,
                ..Default::default()
            },
            ..Default::default()
        },
        t0,
    );
    let mid_entrance = t0 + Duration::from_millis(100);
    let scale_at_mid = renderer.version_popup_scale(mid_entrance);
    assert!(
        scale_at_mid > 0.7 && scale_at_mid < 1.0,
        "expected mid-entrance scale 0.7..1.0; got {scale_at_mid}"
    );

    renderer.set_frames(RenderFrames::default(), mid_entrance);

    let just_after = mid_entrance + Duration::from_millis(1);
    let scale_after = renderer.version_popup_scale(just_after);
    assert!(
        scale_after < scale_at_mid + 0.05,
        "scale should NOT snap up after dismiss; got {scale_after} (was {scale_at_mid})"
    );
}

/// `cancel_transition` must land the user on `to_floor` — leaving it at `from_floor`
/// silently reverts a user-initiated navigation with no UI signal.
#[test]
fn cancel_transition_lands_on_destination_floor() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);

    let mut caps = [0usize; pixtuoid_core::state::MAX_FLOORS];
    caps[0] = 8;
    caps[1] = 8;
    let mut scene = SceneState::new(caps);
    for (i, name) in ["a", "b"].iter().enumerate() {
        let id = AgentId::from_transcript_path(&format!("/demo/{name}.jsonl"));
        scene.agents.insert(
            id,
            agent_slot(
                id,
                &format!("s-{i}"),
                name,
                GlobalDeskIndex(i * 8),
                i,
                ActivityState::Idle,
                now,
            ),
        );
    }

    let backend = TestBackend::new(96, 36);
    let terminal = Terminal::new(backend).expect("terminal");
    let mut renderer = TuiRenderer::new(
        terminal,
        &pixtuoid_scene::theme::NORMAL,
        pixtuoid_scene::pet::PetKind::ALL
            .iter()
            .map(|&k| pixtuoid_scene::pet::Pet::defaulted(k))
            .collect(),
        pack(),
    );

    renderer.render(&scene, now).expect("initial render");
    assert_eq!(renderer.current_floor(), 0);

    renderer.navigate_floor(1, now);
    assert!(renderer.transition().is_some());
    assert_eq!(
        renderer.current_floor(),
        0,
        "current_floor stays at source until transition completes or cancels"
    );

    renderer.cancel_transition();
    assert!(renderer.transition().is_none());
    assert_eq!(
        renderer.current_floor(),
        1,
        "cancel_transition should snap to the destination floor"
    );
}

fn make_renderer() -> TuiRenderer<TestBackend> {
    let backend = TestBackend::new(96, 36);
    let terminal = Terminal::new(backend).expect("terminal");
    TuiRenderer::new(
        terminal,
        &pixtuoid_scene::theme::NORMAL,
        pixtuoid_scene::pet::PetKind::ALL
            .iter()
            .map(|&k| pixtuoid_scene::pet::Pet::defaulted(k))
            .collect(),
        pack(),
    )
}

#[test]
fn the_help_paints_only_while_open() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let scene = SceneState::uniform(8);
    let mut r = make_renderer();
    let help_painted = |r: &TuiRenderer<TestBackend>| {
        let text: String = r
            .terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        text.contains("Keyboard")
    };
    let help = |open| RenderFrames {
        help_open: open,
        ..Default::default()
    };

    r.render(&scene, now).expect("render");
    assert!(!help_painted(&r));
    r.set_frames(help(true), now);
    r.render(&scene, now).expect("render");
    assert!(help_painted(&r));
    r.set_frames(help(false), now);
    r.render(&scene, now).expect("render");
    assert!(!help_painted(&r));
}
