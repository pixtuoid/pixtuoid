//! End-to-end headless harness: drives the real `TuiRenderer` (via ratatui
//! `TestBackend`) through the production render path.
use super::*;
use crate::panels::ui_state::RenderFrames;
use pixtuoid_core::AgentId;
use pixtuoid_core::state::{ActivityState, AgentSlot, GlobalDeskIndex, SceneState, ToolKind};
use pixtuoid_scene::anim::PAINT_FRAME_MS;
use pixtuoid_scene::display::PetHover;
use pixtuoid_scene::pet::PetKind;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub(super) fn slot(
    id: AgentId,
    floor_idx: usize,
    desk_index: usize,
    started: SystemTime,
) -> AgentSlot {
    AgentSlot {
        agent_id: id,
        source: Arc::from("cc"),
        session_id: Arc::from("s"),
        cwd: Arc::from(Path::new("/repo")),
        label: "a".into(),
        state: ActivityState::Idle,
        state_started_at: started,
        created_at: started,
        last_event_at: started,
        exiting_at: None,
        pending_idle_at: None,
        desk_index: GlobalDeskIndex(desk_index),
        floor_idx,
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

pub(super) fn render_until_settled<B: Backend<Error: Send + Sync + 'static>>(
    r: &mut TuiRenderer<B>,
    scene: &SceneState,
    now: &mut SystemTime,
    target_floor: usize,
) {
    for _ in 0..60 {
        *now += Duration::from_millis(PAINT_FRAME_MS);
        r.render(scene, *now).expect("render");
        if r.current_floor() == target_floor && r.transition().is_none() {
            return;
        }
    }
    panic!("floor transition to {target_floor} did not settle");
}

pub(super) use crate::test_flash::{pack, pack_arc};
pub(super) fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}
pub(super) fn normal_theme() -> &'static pixtuoid_scene::theme::Theme {
    pixtuoid_scene::theme::theme_by_name("normal").expect("normal theme")
}
pub(super) fn dark_theme() -> &'static pixtuoid_scene::theme::Theme {
    pixtuoid_scene::theme::theme_by_name("cyberpunk").expect("cyberpunk theme")
}
/// A terminal one row under the office minimum — the footer-only path, DERIVED
/// so a floor move can't leave these tests asserting against a size that has
/// since become perfectly renderable.
pub(super) fn too_small_terminal() -> (u16, u16) {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    (cols, rows - 1)
}

/// Build a renderer with the given pet KINDS, each using its default name.
pub(super) fn build(cols: u16, rows: u16, kinds: Vec<PetKind>) -> TuiRenderer<TestBackend> {
    build_pets(
        cols,
        rows,
        kinds
            .into_iter()
            .map(pixtuoid_scene::pet::Pet::defaulted)
            .collect(),
    )
}
pub(super) fn build_pets(
    cols: u16,
    rows: u16,
    pets: Vec<pixtuoid_scene::pet::Pet>,
) -> TuiRenderer<TestBackend> {
    TuiRenderer::new(
        Terminal::new(TestBackend::new(cols, rows)).expect("test backend"),
        normal_theme(),
        pets,
        pack_arc(),
    )
}
/// Idle agent on floor 0 at desk `desk`.
pub(super) fn idle(id: &str, desk: usize, started: SystemTime) -> AgentSlot {
    slot(AgentId::from_transcript_path(id), 0, desk, started)
}
pub(super) fn active(id: &str, desk: usize, detail: &str, started: SystemTime) -> AgentSlot {
    let mut s = idle(id, desk, started);
    s.state = ActivityState::Active {
        tool_use_id: Some(Arc::from("t")),
        detail: Some(Arc::from(detail)),
        kind: ToolKind::from_display(detail),
    };
    s.last_event_at = started;
    s
}
pub(super) fn scene_with(agents: Vec<AgentSlot>, cap: usize) -> SceneState {
    let mut s = SceneState::uniform(cap);
    for a in agents {
        s.agents.insert(a.agent_id, a);
    }
    s
}
/// Flatten the ratatui frame into one newline-joined string for substring
/// assertions on rendered text.
pub(super) fn frame_text(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}
pub(super) fn lum(c: pixtuoid_core::sprite::Rgb) -> f32 {
    0.299 * f32::from(c.r) + 0.587 * f32::from(c.g) + 0.114 * f32::from(c.b)
}
pub(super) fn avg_lum(buf: &RgbBuffer, x0: u16, y0: u16, w: u16, h: u16) -> f32 {
    let mut sum = 0.0;
    let mut n = 0u32;
    for y in y0..(y0 + h).min(buf.height()) {
        for x in x0..(x0 + w).min(buf.width()) {
            sum += lum(buf.get(x, y));
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}
pub(super) fn region_diff(a: &RgbBuffer, b: &RgbBuffer, x0: u16, y0: u16, w: u16, h: u16) -> u64 {
    let mut d = 0u64;
    for y in y0..(y0 + h).min(a.height()).min(b.height()) {
        for x in x0..(x0 + w).min(a.width()).min(b.width()) {
            let (p, q) = (a.get(x, y), b.get(x, y));
            d += u64::from((i32::from(p.r) - i32::from(q.r)).unsigned_abs())
                + u64::from((i32::from(p.g) - i32::from(q.g)).unsigned_abs())
                + u64::from((i32::from(p.b) - i32::from(q.b)).unsigned_abs());
        }
    }
    d
}

/// A backend that forwards every call to `inner`, its [`Tap`] seeing each
/// draw's cells, and answering the window's pixel size.
/// `append_lines` is forwarded too: ratatui defaults it to a no-op.
pub(super) struct Tapped<B, T> {
    pub(super) inner: B,
    pub(super) tap: T,
}

/// What a [`Tapped`] backend lets a test see or answer; each defaults to
/// forwarding.
pub(super) trait Tap<B: Backend> {
    /// Sees each draw's cells before `inner` does.
    fn on_draw(&mut self, _cells: &[(u16, u16, &ratatui::buffer::Cell)]) {}
    fn window_size(&mut self, inner: &mut B) -> Result<ratatui::backend::WindowSize, B::Error> {
        inner.window_size()
    }
}

impl<B: Backend, T: Tap<B>> Backend for Tapped<B, T> {
    type Error = B::Error;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        let cells: Vec<_> = content.collect();
        self.tap.on_draw(&cells);
        self.inner.draw(cells.into_iter())
    }
    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.inner.size()
    }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.tap.window_size(&mut self.inner)
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

impl<T> std::borrow::Borrow<TestBackend> for Tapped<TestBackend, T> {
    fn borrow(&self) -> &TestBackend {
        &self.inner
    }
}

/// A write that lands `by` late on the `screen` clock.
pub(super) struct Latency {
    pub(super) screen: pixtuoid_scene::screen::ManualClock,
    pub(super) by: Duration,
}

impl Latency {
    pub(super) fn land(&self) {
        self.screen.advance(self.by);
    }
}

/// The terminal's side of a [`FrameOut`](crate::tui::FrameOut); set `fail` to
/// make its writes fail as a terminal that left does, or `slow` to make a
/// flush land late.
#[derive(Clone, Default)]
pub(super) struct Wire {
    pub(super) bytes: Arc<std::sync::Mutex<Vec<u8>>>,
    pub(super) fail: Arc<std::sync::atomic::AtomicBool>,
    pub(super) slow: Arc<std::sync::Mutex<Option<Latency>>>,
}

impl std::io::Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        self.bytes.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(latency) = &*self.slow.lock().expect("lock") {
            latency.land();
        }
        Ok(())
    }
}

/// A half-block renderer over a `cols`×`rows` terminal, presenting each
/// frame through a [`Wire`] as the TUI does through its terminal, its
/// flashes held on a screen clock the test moves.
pub(super) fn half_blocks_on_screen(
    cols: u16,
    rows: u16,
) -> (
    TuiRenderer<TestBackend>,
    pixtuoid_scene::screen::ManualClock,
    Wire,
) {
    let screen = pixtuoid_scene::screen::ManualClock::default();
    let mut r = TuiRenderer::new(
        Terminal::new(TestBackend::new(cols, rows)).expect("test backend"),
        normal_theme(),
        vec![],
        pack_arc(),
    );
    let wire = Wire::default();
    r.present_through(crate::tui::FrameOut::new(wire.clone(), true));
    r.screen = pixtuoid_scene::screen::Screen::on(screen.clock());
    (r, screen, wire)
}

/// Every cell of world text `bright` sets over its office that `dimmed` sets
/// too shows its colors under `dim` there.
pub(super) fn assert_world_text_dims(
    bright: &ratatui::buffer::Buffer,
    dimmed: &ratatui::buffer::Buffer,
    dim: pixtuoid_scene::look::Dim,
) {
    use ratatui::style::Color;
    let under = |c: Color| match c {
        Color::Rgb(r, g, b) => {
            let pixtuoid_core::sprite::Rgb { r, g, b } =
                dim.of(pixtuoid_core::sprite::Rgb { r, g, b });
            Color::Rgb(r, g, b)
        }
        other => other,
    };
    let text: Vec<_> = crate::tui::renderer::scene_rect(bright.area)
        .positions()
        .filter(|&p| {
            bright[p].symbol().chars().any(char::is_alphanumeric)
                && bright[p].symbol() == dimmed[p].symbol()
        })
        .collect();
    assert!(!text.is_empty(), "the office sets no world text");
    for p in text {
        assert_eq!(
            (dimmed[p].fg, dimmed[p].bg),
            (under(bright[p].fg), under(bright[p].bg)),
            "{p:?}"
        );
    }
}

pub(super) fn two_floor_scene() -> SceneState {
    let cap = 16;
    scene_with(
        vec![
            idle("/n/0.jsonl", 0, t0() - Duration::from_secs(120)),
            slot(AgentId::from_transcript_path("/n/1.jsonl"), 1, cap, t0()),
        ],
        cap,
    )
}

mod audio;
mod coffee_pets;
mod cutaway;
mod dashboard;
mod edge_cases;
mod floors;
mod hit_test;
mod mascot;
mod overlays;
mod render_text;
mod theme_lighting;

/// Drive the PRODUCTION hover path programmatically: scan the last rendered
/// frame for a cell whose agent hit-test resolves to `id` and park the mouse
/// there. Panics when the agent isn't hit-testable — a test wiring error, not a
/// case.
pub(super) fn hover_agent<B>(r: &mut TuiRenderer<B>, id: pixtuoid_core::AgentId)
where
    B: Backend<Error: Send + Sync + 'static> + std::borrow::Borrow<TestBackend>,
{
    let cell = r
        .frame_buffer()
        .area()
        .positions()
        .find(|p| r.hit_test_agent_at(p.x, p.y) == Some(id))
        .unwrap_or_else(|| panic!("agent {id:?} is not hit-testable in this layout"));
    r.set_mouse_pos(Some((cell.x, cell.y)));
}
