//! Test fixtures the floating modules' tests share.

use pixtuoid_core::sprite::format::Density;
use pixtuoid_core::state::SceneState;
use pixtuoid_scene::layout::Size;
use pixtuoid_scene::render_scale::PixelFit;

/// The bundled pack's densest art, which the window draws at.
pub(super) fn density() -> Density {
    crate::test_flash::pack().max_density_variant()
}

/// The window's geometry for an office `size` units big, at the test
/// pack's densest art and no upscale.
pub(super) fn cutaway(size: Size) -> PixelFit {
    let density = density();
    let px = |units: u16| units * density.get();
    PixelFit::at_least_density(
        density.get(),
        density,
        Size {
            w: px(size.w),
            h: px(size.h),
        },
    )
}

/// Local twin of the TUI harness's `running_on` — `tui` and `floating` are sibling
/// painters that don't share code, test helpers included.
pub(super) fn running_on(
    path: &str,
    floor_idx: usize,
    desk: usize,
) -> pixtuoid_core::state::AgentSlot {
    use pixtuoid_core::state::{ActivityState, AgentSlot, GlobalDeskIndex, ToolKind, ToolUse};
    use std::sync::Arc;
    let started = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    AgentSlot {
        agent_id: pixtuoid_core::AgentId::from_transcript_path(path),
        source: Arc::from("cc"),
        session_id: Arc::from("s"),
        cwd: Arc::from(std::path::Path::new("/repo")),
        label: "a".into(),
        state: ActivityState::Running {
            item: Some(ToolUse {
                tool_use_id: Some(Arc::from("t")),
                detail: Some(Arc::from("Edit")),
                kind: ToolKind::from_display("Edit"),
            }),
        },
        state_started_at: started,
        created_at: started,
        last_event_at: started,
        exiting_at: None,
        desk_index: GlobalDeskIndex(desk),
        floor_idx,
        tool_call_count: 0,
        running_ms: 0,
        unknown_cwd: false,
        parent_id: None,
        pid: None,
        model: None,
        effort: None,
        tokens_used: 0,
        last_usage: None,
    }
}

pub(super) fn scene_with(agents: Vec<pixtuoid_core::state::AgentSlot>, cap: usize) -> SceneState {
    let mut s = SceneState::uniform(cap);
    for a in agents {
        s.agents.insert(a.agent_id, a);
    }
    s
}

/// One floor's frame of `scene` for the window at `now`, on the ground
/// floor with no pet.
pub(super) fn frame<'a>(
    scene: &'a SceneState,
    theme: &'static pixtuoid_scene::theme::Theme,
    now: std::time::SystemTime,
) -> super::offscreen::WindowFrame<'a> {
    super::offscreen::WindowFrame {
        world: pixtuoid_scene::floor::FloorInputs {
            scene,
            now,
            floor: pixtuoid_scene::floor::FloorMeta::ground(),
            pets: pixtuoid_scene::floor::PetInputs::default(),
        },
        theme,
        place: pixtuoid_scene::look::Place::default(),
        dim: pixtuoid_scene::look::Dim::NONE,
    }
}
