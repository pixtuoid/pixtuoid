//! The cutaway through the production render path: what reaches the
//! terminal as escapes, and what the cells show.
use super::*;
use crate::graphics::kitty::PLACEHOLDER;
use crate::graphics::{CellSize, ImageProtocol, cutaway_fit};
use crate::tui::cutaway::TileCutaway;
use pixtuoid_core::sprite::format::Density;
use std::sync::atomic::{AtomicBool, Ordering};

/// A cell whose natural scale is the bundled art's density, so the image is
/// the density render itself, unscaled.
const CELL: CellSize = CellSize { w: 4, h: 8 };
const TRANSMIT: &str = "\x1b_Ga=T,";
const SIXEL: &str = "\x1bP9;1q";
const ITERM2: &str = "\x1b]1337;File=";

impl Wire {
    /// What arrived since the last take.
    fn take(&self) -> String {
        let bytes = std::mem::take(&mut *self.bytes.lock().expect("lock"));
        String::from_utf8(bytes).expect("escapes are ascii")
    }
}

/// A terminal whose window reports each cell at its `tap` [`CellSize`], as a
/// real one's does.
type Window = Tapped<TestBackend, CellSize>;

impl Window {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            inner: TestBackend::new(cols, rows),
            tap: CELL,
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.inner.resize(cols, rows);
    }

    /// Zoom the font to `cell`, the window keeping its cells: only the pixels
    /// change.
    fn zoom(&mut self, cell: CellSize) {
        self.tap = cell;
    }
}

impl Tap<TestBackend> for CellSize {
    fn window_size(
        &mut self,
        inner: &mut TestBackend,
    ) -> Result<ratatui::backend::WindowSize, <TestBackend as ratatui::backend::Backend>::Error>
    {
        let columns_rows = ratatui::backend::Backend::size(inner)?;
        Ok(ratatui::backend::WindowSize {
            columns_rows,
            pixels: ratatui::layout::Size::new(
                columns_rows.width * self.w,
                columns_rows.height * self.h,
            ),
        })
    }
}

/// [`painter`], its flashes held on a screen clock the test moves.
fn on_screen(
    cols: u16,
    rows: u16,
    protocol: ImageProtocol,
) -> (
    TuiRenderer<Window>,
    Wire,
    pixtuoid_scene::screen::ManualClock,
) {
    let (mut r, wire) = painter(cols, rows, protocol);
    let screen = pixtuoid_scene::screen::ManualClock::default();
    r.screen = pixtuoid_scene::screen::Screen::on(screen.clock());
    (r, wire, screen)
}

/// The plan's fit over a `cols`×`rows` terminal's scene.
fn fit(cols: u16, rows: u16) -> pixtuoid_scene::render_scale::PixelFit {
    let area = crate::tui::renderer::scene_rect(Rect::new(0, 0, cols, rows)).as_size();
    let fit = cutaway_fit(CELL, area, pack_arc().max_density_variant()).expect("fits");
    assert_eq!(fit.upscale(), 1);
    assert_eq!(fit.density(), Density::new(4).expect("nonzero"));
    fit
}

/// A renderer painting the cutaway over `protocol` into a `cols`×`rows`
/// terminal.
fn painter(cols: u16, rows: u16, protocol: ImageProtocol) -> (TuiRenderer<Window>, Wire) {
    let (r, wire, _) = armed(cols, rows, protocol, vec![]);
    (r, wire)
}

/// [`painter`], and the flag it sets where the process's would tell the
/// unwind that grid pixels were drawn: its own, so no test leaks into
/// another.
fn armed(
    cols: u16,
    rows: u16,
    protocol: ImageProtocol,
    pets: Vec<PetKind>,
) -> (TuiRenderer<Window>, Wire, &'static AtomicBool) {
    let mut r = TuiRenderer::new(
        Terminal::new(Window::new(cols, rows)).expect("terminal"),
        normal_theme(),
        pets.into_iter()
            .map(pixtuoid_scene::pet::Pet::defaulted)
            .collect(),
        pack_arc(),
    );
    let (wire, in_grid) = (Wire::default(), Box::leak(Box::new(AtomicBool::new(false))));
    // As the TUI does: the transmits land with their frame.
    let out = crate::tui::FrameOut::new(wire.clone(), false);
    r.present_through(out.clone());
    r.set_cutaway(
        TileCutaway::new(
            fit(cols, rows),
            CELL,
            crate::graphics::Route::direct(protocol, false),
            Box::new(out),
        )
        .arming(in_grid),
    );
    (r, wire, in_grid)
}

fn kitty(cols: u16, rows: u16) -> (TuiRenderer<Window>, Wire) {
    painter(cols, rows, ImageProtocol::Kitty)
}

fn office() -> SceneState {
    scene_with(vec![idle("/k/0.jsonl", 0, t0())], 16)
}

fn placeholders_in_row(r: &TuiRenderer<Window>, y: u16) -> usize {
    let buf = r.frame_buffer();
    (0..buf.area.width)
        .filter(|&x| buf[(x, y)].symbol().starts_with(PLACEHOLDER))
        .count()
}

/// The last frame's world text alone, painted where the renderer painted it
/// over the image: the only text of the scene's.
fn world_text(r: &TuiRenderer<Window>) -> ratatui::buffer::Buffer {
    let area = r.frame_buffer().area;
    let picture = r.session.picture();
    let text = match &picture {
        Some(pixtuoid_scene::look::Picture::Floor(floor)) => floor.text,
        _ => None,
    }
    .expect("the host sets the world's text");
    let map = r.screen.shown().expect("a frame drawn").geometry.map();
    let mut term = Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
    term.draw(|f| {
        let scene = crate::tui::renderer::scene_rect(f.area());
        crate::panels::widgets::paint_host_text(f, text, (scene, map), None);
    })
    .expect("draw");
    term.backend().buffer().clone()
}

/// The scene's cells of `buf` that hold text: any ratatui wrote.
fn text_cells(buf: &ratatui::buffer::Buffer) -> std::collections::BTreeSet<(u16, u16)> {
    let scene = crate::tui::renderer::scene_rect(buf.area);
    scene
        .positions()
        .filter(|&p| buf[p] != ratatui::buffer::Cell::default())
        .map(|p| (p.x, p.y))
        .collect()
}

/// The cells the SIXEL images on `wire` draw, each read back from its cursor
/// move and its raster size in [`CELL`]s.
fn sixel_cells(wire: &str) -> std::collections::BTreeSet<(u16, u16)> {
    let mut cells = std::collections::BTreeSet::new();
    for (at, _) in wire.match_indices(SIXEL) {
        let head = &wire[..at];
        let open = head.rfind("\x1b[").expect("a cursor move");
        let (row, col) = head[open + 2..]
            .strip_suffix('H')
            .and_then(|at| at.split_once(';'))
            .expect("row;col");
        let (row, col): (u16, u16) = (row.parse().expect("row"), col.parse().expect("col"));
        let raster = wire[at + SIXEL.len()..]
            .strip_prefix("\"1;1;")
            .expect("raster attributes");
        let mut px = raster
            .split(|c: char| !c.is_ascii_digit())
            .map(|n| n.parse::<u16>().expect("px"));
        let (w, h) = (px.next().expect("w"), px.next().expect("h"));
        for y in 0..h / CELL.h {
            for x in 0..w / CELL.w {
                cells.insert((col - 1 + x, row - 1 + y));
            }
        }
    }
    cells
}

#[test]
fn the_first_frame_transmits_and_an_identical_one_sends_nothing() {
    let (mut r, wire) = kitty(120, 40);
    let scene = office();
    r.render(&scene, t0()).expect("render");
    assert!(wire.take().contains(TRANSMIT));
    r.render(&scene, t0()).expect("render");
    assert_eq!(wire.take(), "");
}

/// The onboarding's backdrop dims the cutaway: the composed frame darkens
/// under it, its tiles are sent again though nothing else in the office
/// moved, and the world text the terminal sets over it darkens too.
#[test]
fn the_onboarding_dims_the_cutaway_and_sends_its_tiles() {
    use crate::panels::welcome::OnboardingFrame;
    let (mut r, wire) = kitty(120, 40);
    let scene = office();
    let lum = |r: &TuiRenderer<Window>| {
        let picture = r.session.picture().expect("a frame");
        let frame = picture.pixels();
        avg_lum(frame, 0, 0, frame.width(), frame.height())
    };
    r.render(&scene, t0()).expect("render");
    let bright = lum(&r);
    let bright_cells = r.frame_buffer().clone();
    wire.take();
    r.set_frames(
        RenderFrames {
            onboarding: OnboardingFrame {
                open: true,
                dim: pixtuoid_scene::look::Dim::from_brightness(0.4),
                ..OnboardingFrame::default()
            },
            ..RenderFrames::default()
        },
        t0(),
    );
    r.render(&scene, t0()).expect("render");
    let dim = lum(&r);
    assert!(dim < bright * 0.6, "dim={dim} vs bright={bright}");
    assert!(wire.take().contains(TRANSMIT), "the dimmed tiles are sent");
    assert_world_text_dims(
        &bright_cells,
        r.frame_buffer(),
        pixtuoid_scene::look::Dim::from_brightness(0.4),
    );
}

/// A frame encoded across four threads, on any machine, still reaches the
/// wire as one transmit per tile, in the grid's order.
#[test]
fn a_whole_frame_transmits_every_tile_once_in_order() {
    let (mut r, wire) = kitty(120, 40);
    r.cutaway.as_mut().expect("a cutaway").split_across(4);
    r.render(&office(), t0()).expect("render");
    let ids: Vec<u32> = wire
        .take()
        .split(TRANSMIT)
        .skip(1)
        .filter_map(|keys| {
            keys.split(',')
                .find_map(|k| k.strip_prefix("i=")?.parse().ok())
        })
        .collect();
    let first = *ids.first().expect("a transmit");
    assert!(
        ids.len() >= 4 * crate::tui::cutaway::TILES_PER_THREAD,
        "a share for each of the four threads"
    );
    assert_eq!(ids, (first..first + ids.len() as u32).collect::<Vec<_>>());
}

/// Warmed at boot, the first frame shown draws no cloud: every mass of its
/// sky is already cached.
#[test]
fn a_warmed_first_frame_draws_no_cloud() {
    let (mut r, _wire) = kitty(120, 40);
    r.set_weather(pixtuoid_scene::sky::WeatherPolicy::Forced(
        pixtuoid_scene::sky::Weather::Overcast,
    ));
    let scene = office();
    assert!(
        cloud_draws(|| r.warm(&scene, t0())) > 0,
        "warming an overcast sky draws its masses"
    );
    let first = cloud_draws(|| r.render(&scene, t0()).expect("render"));
    assert_eq!(first, 0, "the first frame drew a cloud");
}

/// The cloud rasters `f` draws: the `clouds.draw` spans it opens.
fn cloud_draws(f: impl FnOnce()) -> usize {
    use tracing_subscriber::layer::SubscriberExt;
    #[derive(Clone, Default)]
    struct Count(Arc<std::sync::atomic::AtomicUsize>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
        fn on_new_span(
            &self,
            span: &tracing::span::Attributes<'_>,
            _: &tracing::span::Id,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if span.metadata().name() == "clouds.draw" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let count = Count::default();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(count.clone()), f);
    count.0.load(Ordering::Relaxed)
}

/// A frame with no room for the image reports no transmits, not the last
/// cutaway frame's.
#[test]
fn a_frame_without_room_reports_no_transmits() {
    let (mut r, _wire) = kitty(120, 40);
    r.render(&office(), t0()).expect("render");
    let cutaway = |r: &TuiRenderer<Window>| r.cutaway.as_ref().expect("a cutaway").last_send();
    assert_ne!(cutaway(&r).sent, 0, "the first frame sent its tiles");
    let (cols, rows) = too_small_terminal();
    r.terminal.backend_mut().resize(cols, rows);
    r.render(&office(), t0()).expect("render");
    assert_ne!(cutaway(&r).dirty, crate::jank::Painted::All);
    assert_eq!(cutaway(&r).sent, 0);
}

/// A whole send for a view the terminal had not shown says so, though the
/// scene repainted nothing: a first frame, a floor, each slide frame.
#[test]
fn a_new_view_is_reported_fresh() {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, _wire) = kitty(cols, rows);
    let fresh = |r: &TuiRenderer<Window>| r.cutaway.as_ref().expect("a cutaway").last_send().fresh;
    let scene = two_floor_scene();
    let now = t0();
    r.render(&scene, now).expect("render");
    assert!(fresh(&r), "the first frame");
    r.render(&scene, now).expect("render");
    assert!(!fresh(&r), "the same floor again");
    r.navigate_floor(1, now);
    r.render(&scene, now + Duration::from_millis(1))
        .expect("render");
    assert!(r.transition().is_some() && fresh(&r), "a slide frame");
}

/// A frame the flash hold keeps back reports no transmits, not the last
/// frame's, on a floor and sliding.
#[test]
fn a_held_frame_reports_no_transmits() {
    use crate::test_flash::{held_frames, storm_strike};
    let strike = storm_strike();
    let [dark, late, held, _] = held_frames(&strike);
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let cutaway = |r: &TuiRenderer<Window>| r.cutaway.as_ref().expect("a cutaway").last_send();
    for slide in [false, true] {
        let (mut r, _wire, screen) = on_screen(cols, rows, ImageProtocol::Kitty);
        r.set_weather(strike.weather);
        r.set_motion(pixtuoid_scene::anim::Motion::Full);
        let scene = two_floor_scene();
        if slide {
            r.navigate_floor(1, dark);
        }
        for at in [dark, late] {
            screen.at(at);
            r.render(&scene, at).expect("render");
        }
        assert_ne!(cutaway(&r).sent, 0, "the late phase sent, slide {slide}");
        screen.at(held);
        r.render(&scene, held).expect("render");
        assert_eq!(
            cutaway(&r),
            crate::jank::FrameSend::default(),
            "slide {slide}"
        );
        assert_eq!(r.transition().is_some(), slide);
    }
}

/// Text and image never share a cell: placeholders fill the scene but the
/// world's text, and never the footer row.
#[test]
fn placeholders_fill_the_scene_and_never_the_footer() {
    let (cols, rows) = (120, 40);
    let (mut r, _wire) = kitty(cols, rows);
    r.render(&office(), t0()).expect("render");
    let text = text_cells(&world_text(&r));
    assert!(
        !text.is_empty(),
        "premise: the office has a board and a badge"
    );
    let buf = r.frame_buffer();
    for p in crate::tui::renderer::scene_rect(buf.area).positions() {
        let placeholder = buf[p].symbol().starts_with(PLACEHOLDER);
        assert_eq!(placeholder, !text.contains(&(p.x, p.y)), "{p:?}");
    }
    assert_eq!(placeholders_in_row(&r, rows - 1), 0);
}

/// The board's star is a link in both looks: the pointer finds it on the
/// cells each writes it in, which are the same.
#[test]
fn the_star_is_clickable_in_both_looks() {
    use crate::tui::hit_test::SceneHit;
    let (mut cutaway, _wire) = kitty(120, 40);
    cutaway.render(&office(), t0()).expect("render");
    let mut classic = build(120, 40, vec![]);
    classic.render(&office(), t0()).expect("render");
    let star_cells = |r: &dyn Fn(u16, u16) -> bool| {
        (0..40u16)
            .flat_map(|row| (0..120u16).map(move |col| (col, row)))
            .filter(|&(col, row)| r(col, row))
            .collect::<Vec<_>>()
    };
    let on_classic = star_cells(&|c, r| matches!(classic.scene_hit_at(c, r), Some(SceneHit::Star)));
    let on_cutaway = star_cells(&|c, r| matches!(cutaway.scene_hit_at(c, r), Some(SceneHit::Star)));
    let buf = classic.terminal.backend().buffer();
    let star: Vec<char> = "\u{2605} Star".chars().collect();
    let width = star.len() as u16;
    let written: Vec<(u16, u16)> = (0..40u16)
        .flat_map(|row| (0..=120 - width).map(move |col| (col, row)))
        .find(|&(col, row)| {
            (col..)
                .zip(&star)
                .all(|(x, c)| buf[(x, row)].symbol() == &*c.encode_utf8(&mut [0; 4]))
        })
        .map(|(col, row)| (col..col + width).map(|x| (x, row)).collect())
        .expect("the classic writes the star");
    assert_eq!(
        on_classic, written,
        "the classic's star is a link on its cells"
    );
    assert_eq!(
        on_cutaway, on_classic,
        "the cutaway's star is the same link"
    );
}

/// A refused frame sends no tiles and leaves no hit targets behind.
#[test]
fn shrinking_under_the_minimum_refuses_the_cutaway_frame() {
    let (cols, rows) = (120, 40);
    let (mut r, wire) = kitty(cols, rows);
    let scene = office();
    r.render(&scene, t0()).expect("render");
    assert!(r.scene_area_at(cols / 2, rows / 2).is_some());
    wire.take();
    let (small_cols, small_rows) = too_small_terminal();
    r.terminal.backend_mut().resize(small_cols, small_rows);
    r.render(&scene, t0()).expect("render");
    assert!(!wire.take().contains(TRANSMIT));
    assert_eq!(r.scene_area_at(cols / 2, rows / 2), None);
    assert!(r.drawn_pet().is_none());
}

/// A resize clears the screen ratatui redraws, SIXEL and iTerm2 pixels with
/// it: a terminal shrunk under the minimum and restored gets every tile again.
/// Both refusal arms: the layout's, and the scene minimum's.
#[test]
fn a_terminal_restored_from_too_small_gets_every_tile_again() {
    use crate::tui::renderer::{FOOTER_ROWS, MIN_SCENE_HEIGHT, min_terminal_size};
    let (cols, rows) = min_terminal_size();
    let smalls = [
        too_small_terminal(),
        (cols, MIN_SCENE_HEIGHT + FOOTER_ROWS - 1),
    ];
    for (protocol, intro) in [
        (ImageProtocol::Sixel, SIXEL),
        (ImageProtocol::Iterm2, ITERM2),
    ] {
        let scene = office();
        let cadence = protocol.cadence();
        // The images the last of three frames sends.
        let sent = |between: (u16, u16)| {
            let (mut r, wire) = painter(cols, rows, protocol);
            r.render(&scene, t0()).expect("render");
            wire.take();
            r.terminal.backend_mut().resize(between.0, between.1);
            r.render(&scene, t0() + cadence).expect("render");
            r.terminal.backend_mut().resize(cols, rows);
            r.render(&scene, t0() + cadence * 2).expect("render");
            wire.take().matches(intro).count()
        };
        // Every tile, as a fresh terminal gets that last frame: the text it
        // carves moves with the board's flap.
        let all = {
            let (mut r, wire) = painter(cols, rows, protocol);
            r.render(&scene, t0() + cadence * 2).expect("render");
            wire.take().matches(intro).count()
        };
        let steady = sent((cols, rows));
        assert!(
            steady < all,
            "{protocol:?}: {steady} of {all} change anyway"
        );
        for (small_cols, small_rows) in smalls {
            assert_eq!(
                sent((small_cols, small_rows)),
                all,
                "{protocol:?} from {small_cols}x{small_rows}"
            );
        }
    }
}

/// A whole SIXEL tile, no text carved out of it, for a `cell`-sized cell.
fn whole_tile(cell: CellSize) -> String {
    let shape = ImageProtocol::Sixel.tile();
    format!(
        "{SIXEL}\"1;1;{};{}",
        shape.cols * cell.w,
        shape.rows * cell.h
    )
}

/// The plan's cell is the terminal's own answer, which outranks the window's
/// where they differ, so `doctor`, the boot seed and the painted office all
/// read it until a font zoom moves the window's cell.
#[test]
fn the_plans_cell_holds_until_the_windows_moves() {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let padded = CellSize {
        w: CELL.w + 1,
        h: CELL.h + 2,
    };
    let (mut r, wire) = painter(cols, rows, ImageProtocol::Sixel);
    r.terminal.backend_mut().zoom(padded);
    let scene = office();
    let cadence = ImageProtocol::Sixel.cadence();
    r.render(&scene, t0()).expect("render");
    assert!(wire.take().contains(&whole_tile(CELL)), "the plan's cell");
    let plan = crate::graphics::Plan::Cutaway {
        fit: fit(cols, rows),
        route: crate::graphics::Route::direct(ImageProtocol::Sixel, false),
        cell: CELL,
        chosen: crate::graphics::Chosen::Answer,
    };
    let office = plan.office_extent(ratatui::layout::Size::new(cols, rows));
    assert_eq!(
        r.office_extent(),
        (office.w, office.h),
        "the boot seed's office"
    );

    let zoomed = CellSize {
        w: CELL.w * 2,
        h: CELL.h * 2,
    };
    r.terminal.backend_mut().zoom(zoomed);
    r.render(&scene, t0() + cadence).expect("render");
    assert!(wire.take().contains(&whole_tile(zoomed)), "zoomed in");
    r.terminal.backend_mut().zoom(padded);
    r.render(&scene, t0() + cadence * 2).expect("render");
    assert!(wire.take().contains(&whole_tile(CELL)), "zoomed back");
}

/// A window that reports 0 px on the first frame has read nothing, so the
/// plan's cell holds when the real cell arrives: it is no font zoom.
#[test]
fn a_first_frame_with_no_pixels_does_not_pose_as_the_windows_baseline() {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let padded = CellSize {
        w: CELL.w + 1,
        h: CELL.h + 2,
    };
    let (mut r, wire) = painter(cols, rows, ImageProtocol::Sixel);
    r.terminal.backend_mut().zoom(CellSize { w: 0, h: 0 });
    let scene = office();
    r.render(&scene, t0()).expect("render");
    assert!(wire.take().contains(&whole_tile(CELL)), "the plan's cell");
    r.terminal.backend_mut().zoom(padded);
    r.render(&scene, t0() + ImageProtocol::Sixel.cadence())
        .expect("render");
    assert_eq!(wire.take(), "", "the plan's cell holds, nothing re-cut");
    let plan = crate::graphics::Plan::Cutaway {
        fit: fit(cols, rows),
        route: crate::graphics::Route::direct(ImageProtocol::Sixel, false),
        cell: CELL,
        chosen: crate::graphics::Chosen::Answer,
    };
    let office = plan.office_extent(ratatui::layout::Size::new(cols, rows));
    assert_eq!(
        r.office_extent(),
        (office.w, office.h),
        "the boot seed's office"
    );
}

/// A font zoom changes the cell's pixels: the window's cell cuts the tiles,
/// classic paints while the cell is too small for the art, and the cutaway
/// returns once it fits.
#[test]
fn a_font_zoom_refits_the_cutaway_to_the_windows_cell() {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, wire) = painter(cols, rows, ImageProtocol::Sixel);
    let scene = office();
    let cadence = ImageProtocol::Sixel.cadence();
    r.render(&scene, t0()).expect("render");
    assert!(wire.take().contains(&whole_tile(CELL)));

    let zoomed = CellSize {
        w: CELL.w * 2,
        h: CELL.h * 2,
    };
    r.terminal.backend_mut().zoom(zoomed);
    r.render(&scene, t0() + cadence).expect("render");
    assert!(wire.take().contains(&whole_tile(zoomed)), "zoomed in");

    let tiny = CellSize {
        w: CELL.w / 2,
        h: CELL.h / 2,
    };
    let area = crate::tui::renderer::scene_rect(Rect::new(0, 0, cols, rows)).as_size();
    assert!(cutaway_fit(tiny, area, pack_arc().max_density_variant()).is_none());
    r.terminal.backend_mut().zoom(tiny);
    r.render(&scene, t0() + cadence * 2).expect("render");
    assert!(!wire.take().contains(SIXEL), "too small for the art");
    assert!(
        frame_text(r.frame_buffer()).contains('\u{2580}'),
        "classic paints meanwhile"
    );

    r.terminal.backend_mut().zoom(zoomed);
    r.render(&scene, t0() + cadence * 3).expect("render");
    assert!(wire.take().contains(&whole_tile(zoomed)), "zoomed back in");
}

/// While classic paints in the cutaway's place, a click hit-tests classic's
/// frame, never the canvas's last: an agent gone since is gone.
#[test]
fn a_cell_too_small_hit_tests_the_classic_frame() {
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, _wire) = kitty(cols, rows);
    let id = AgentId::from_transcript_path("/k/0.jsonl");
    r.render(&office(), t0()).expect("render");
    hover_agent(&mut r, id);
    let (col, row) = r.mouse_pos.expect("hovered");
    r.set_mouse_pos(None);
    r.terminal.backend_mut().zoom(CellSize {
        w: CELL.w / 2,
        h: CELL.h / 2,
    });
    r.render(&scene_with(vec![], 16), t0()).expect("render");
    assert_eq!(r.hit_test_agent_at(col, row), None);
}

/// A slide the terminal shrinks under mid-way is cancelled, as classic's is:
/// it lands on the destination floor, with nothing left to hit-test.
#[test]
fn shrinking_mid_slide_lands_on_the_destination() {
    let (cols, rows) = (120, 40);
    let (mut r, _wire) = kitty(cols, rows);
    let scene = two_floor_scene();
    r.render(&scene, t0()).expect("render");
    r.navigate_floor(1, t0());
    let (small_cols, small_rows) = too_small_terminal();
    r.terminal.backend_mut().resize(small_cols, small_rows);
    r.render(&scene, t0() + Duration::from_millis(100))
        .expect("render");
    assert!(r.transition().is_none());
    assert_eq!(r.current_floor(), 1);
    assert!(r.cached_layout().is_none());
    assert_eq!(r.scene_area_at(small_cols / 2, small_rows / 2), None);
}

#[test]
fn a_modal_over_the_image_shows_its_text() {
    let (mut r, _wire) = kitty(120, 40);
    r.set_frames(
        RenderFrames {
            help_open: true,
            ..Default::default()
        },
        t0(),
    );
    r.render(&office(), t0()).expect("render");
    assert!(frame_text(r.frame_buffer()).contains("? Keyboard"));
}

/// A modal's drop shadow over kitty's placeholders leaves each naming the
/// image it named without the modal: its foreground colour is the image id.
#[test]
fn a_modal_shadow_leaves_kitty_placeholders_naming_their_images() {
    let (mut r, _wire) = kitty(120, 40);
    let scene = office();
    r.render(&scene, t0()).expect("render");
    let bare = r.frame_buffer().clone();
    r.set_frames(
        RenderFrames {
            help_open: true,
            ..Default::default()
        },
        t0(),
    );
    r.render(&scene, t0()).expect("render");
    let modal = r.frame_buffer();
    assert!(frame_text(modal).contains("? Keyboard"));
    let renamed: Vec<_> = modal
        .area
        .positions()
        .filter(|&p| modal[p].symbol().starts_with(PLACEHOLDER) && modal[p].fg != bare[p].fg)
        .collect();
    assert!(renamed.is_empty(), "placeholders renamed: {renamed:?}");
}

#[test]
fn a_redraw_re_sends_every_tile() {
    let (mut r, wire) = kitty(120, 40);
    let scene = office();
    r.render(&scene, t0()).expect("render");
    let tiles = wire.take().matches(TRANSMIT).count();
    r.redraw().expect("redraw");
    r.render(&scene, t0()).expect("render");
    assert_eq!(wire.take().matches(TRANSMIT).count(), tiles);
}

#[test]
fn an_agent_under_the_cutaway_is_hit_tested_on_the_canvas() {
    let (mut r, _wire) = kitty(120, 40);
    let scene = scene_with(vec![running("/k/0.jsonl", 0, "Bash", t0())], 16);
    r.render(&scene, t0()).expect("render");
    assert!(!frame_text(r.frame_buffer()).contains("Running"));
    hover_agent(&mut r, AgentId::from_transcript_path("/k/0.jsonl"));
    r.render(&scene, t0()).expect("render");
    assert!(
        frame_text(r.frame_buffer()).contains("Running"),
        "the hovered agent's tooltip"
    );
}

/// The pet and the gateways answer the pointer under the cutaway too: what
/// its tooltip names is what a click acts on, a hovered agent's tooltip
/// carrying the name the image's badge shows.
#[test]
fn what_the_tooltip_names_is_what_a_click_acts_on_under_the_cutaway() {
    let (mut r, _wire, _) = armed(140, 48, ImageProtocol::Kitty, vec![PetKind::Cat]);
    super::hit_test::the_tooltip_names_what_a_click_acts_on(&mut r, |a| a.label.to_string());
}

/// A click on an agent focuses it and a click on the pet pets it under the
/// cutaway too, through the mouse handler itself.
#[test]
fn a_click_focuses_an_agent_and_pets_the_pet_under_the_cutaway() {
    let (mut r, _wire, _) = armed(140, 48, ImageProtocol::Kitty, vec![PetKind::Cat]);
    super::hit_test::a_click_acts_on_what_it_hits(&mut r);
}

#[test]
fn sixel_and_iterm2_draw_changed_tiles_and_nothing_on_an_identical_frame() {
    for (protocol, intro) in [
        (ImageProtocol::Sixel, SIXEL),
        (ImageProtocol::Iterm2, ITERM2),
    ] {
        let (mut r, wire) = painter(120, 40, protocol);
        let scene = office();
        r.render(&scene, t0()).expect("render");
        assert!(wire.take().contains(intro), "{protocol:?}");
        r.render(&scene, t0()).expect("render");
        assert_eq!(wire.take(), "", "{protocol:?}");
    }
}

/// Ratatui writes the scene's text and none of the image's cells.
#[test]
fn the_image_cells_are_never_written_by_ratatui() {
    let (cols, rows) = (120, 40);
    let (mut r, _wire) = painter(cols, rows, ImageProtocol::Sixel);
    r.render(&office(), t0()).expect("render");
    let world = world_text(&r);
    assert!(!text_cells(&world).is_empty(), "premise: there is text");
    assert_eq!(text_cells(r.frame_buffer()), text_cells(&world));
    let text = frame_text(r.frame_buffer());
    let lines: Vec<&str> = text.lines().collect();
    assert_ne!(
        lines[usize::from(rows - 1)].trim(),
        "",
        "the footer is text"
    );
}

#[test]
fn the_protocols_cadence_gates_its_transmits() {
    let (mut r, wire) = painter(120, 40, ImageProtocol::Sixel);
    let scene = office();
    r.render(&scene, t0()).expect("render");
    wire.take();
    r.redraw().expect("redraw");
    let cadence = ImageProtocol::Sixel.cadence();
    r.render(&scene, t0() + cadence / 2).expect("render");
    assert_eq!(wire.take(), "", "inside the cadence, the tiles stay owed");
    r.render(&scene, t0() + cadence).expect("render");
    assert!(wire.take().contains(SIXEL));
}

/// How `protocol`'s transmit of one tile opens.
fn intro(protocol: ImageProtocol) -> &'static str {
    match protocol {
        ImageProtocol::Kitty => TRANSMIT,
        ImageProtocol::Sixel => SIXEL,
        ImageProtocol::Iterm2 => ITERM2,
    }
}

/// Each phase of a strike stays on screen at least the photosensitive floor
/// on every protocol, at its cadence, at each frame grid: from the send that
/// first shows it to the one that replaces it. A strike lifts the whole room,
/// so a send of most of the tiles is a change of phase.
#[test]
fn each_strike_phase_holds_the_floor_on_screen_at_every_protocols_cadence() {
    use crate::test_flash::{assert_each_phase_holds_the_floor, frame_grid, lead, storm_strike};
    let strike = storm_strike();
    let tick = crate::tui::frame_tick();
    for protocol in [
        ImageProtocol::Kitty,
        ImageProtocol::Sixel,
        ImageProtocol::Iterm2,
    ] {
        for (frame, offset) in frame_grid(tick) {
            let (cols, rows) = crate::tui::renderer::min_terminal_size();
            let (mut r, wire, screen) = on_screen(cols, rows, protocol);
            r.set_weather(strike.weather);
            r.set_motion(pixtuoid_scene::anim::Motion::Full);
            let scene = office();
            let now = strike.start - lead(frame) + offset;
            screen.at(now);
            r.render(&scene, now).expect("render");
            let tiles = wire.take().matches(intro(protocol)).count();
            let mut changed = Vec::new();
            for now in crate::test_flash::frames_after(now, frame, strike.end + lead(frame)) {
                screen.at(now);
                r.render(&scene, now).expect("render");
                if 2 * wire.take().matches(intro(protocol)).count() > tiles {
                    changed.push(now);
                }
            }
            let at = format!("{protocol:?}, a frame each {frame:?} from +{offset:?}");
            assert_each_phase_holds_the_floor(&changed, strike.changes.len(), &at);
        }
    }
}

/// A sliding frame whose strike phase would replace one shown under the floor
/// sends nothing; the frame the floor later does.
#[test]
fn a_held_slide_frame_sends_nothing() {
    use crate::test_flash::{held_frames, storm_strike};
    let strike = storm_strike();
    let [dark, late, held, shown] = held_frames(&strike);
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, wire, screen) = on_screen(cols, rows, ImageProtocol::Kitty);
    r.set_weather(strike.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let scene = two_floor_scene();
    r.navigate_floor(1, dark);
    for at in [dark, late] {
        screen.at(at);
        r.render(&scene, at).expect("render");
    }
    wire.take();
    screen.at(held);
    r.render(&scene, held).expect("render");
    assert_eq!(wire.take(), "", "held");
    screen.at(shown);
    r.render(&scene, shown).expect("render");
    assert!(wire.take().contains(TRANSMIT), "shown");
    assert!(r.transition().is_some(), "still sliding");
}

/// A held cutaway frame writes nothing, its text and chrome as well as its
/// tiles, and a pointer hits the frame the terminal shows: agents the held
/// frame no longer draws are still hit where their tiles show them.
#[test]
fn a_held_cutaway_frame_leaves_the_pointer_hitting_the_one_on_screen() {
    use crate::test_flash::{held_frames, storm_strike};
    let strike = storm_strike();
    let [dark, late, held, _] = held_frames(&strike);
    let (cols, rows) = (140, 60);
    let (mut r, wire, screen) = on_screen(cols, rows, ImageProtocol::Kitty);
    r.set_weather(strike.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let scene = office();
    for at in [dark, late] {
        screen.at(at);
        r.render(&scene, at).expect("render");
    }
    let (col, row, id) = (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (col, row)))
        .find_map(|(col, row)| Some((col, row, r.hit_test_agent_at(col, row)?)))
        .expect("the cutaway shows an agent");
    wire.take();
    let before = r.frame_buffer().clone();
    screen.at(held);
    r.render(&scene_with(vec![], 16), held).expect("render");
    assert!(!wire.take().contains(TRANSMIT), "premise: held");
    assert_eq!(
        *r.frame_buffer(),
        before,
        "nothing drawn over the held tiles"
    );
    assert_eq!(r.hit_test_agent_at(col, row), Some(id));
}

/// The tiles and the half-blocks are one screen under one hold: a strike
/// phase the cutaway showed holds the floor across a font zoom that falls
/// back to the half-blocks, whose first frame shows the next phase. Under a
/// hold per look, that frame went out at once.
#[test]
fn a_phase_holds_the_floor_across_a_fall_back_to_the_half_blocks() {
    use crate::test_flash::{held_frames, storm_strike};
    let strike = storm_strike();
    let [dark, late, held, shown] = held_frames(&strike);
    let (mut r, _wire, screen) = on_screen(140, 60, ImageProtocol::Kitty);
    r.set_weather(strike.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let scene = office();
    for at in [dark, late] {
        screen.at(at);
        r.render(&scene, at).expect("render");
    }
    assert!(!r.offers_walkable_debug(), "premise: the cutaway shows");
    let before = r.frame_buffer().clone();
    r.terminal.backend_mut().zoom(CellSize {
        w: CELL.w / 2,
        h: CELL.h / 2,
    });
    screen.at(held);
    r.render(&scene, held).expect("render");
    assert!(
        r.offers_walkable_debug(),
        "premise: the half-blocks drew it"
    );
    assert_eq!(*r.frame_buffer(), before, "held");
    screen.at(shown);
    r.render(&scene, shown).expect("render");
    assert_ne!(*r.frame_buffer(), before, "shown");
}

/// A terminal resized under a hold gets its frame at once, alone or sliding:
/// a screen of a new shape shows nothing to hold.
#[test]
fn a_resized_terminal_is_never_held() {
    use crate::test_flash::{held_frames, storm_strike};
    let strike = storm_strike();
    let [dark, late, held, _] = held_frames(&strike);
    for sliding in [false, true] {
        let (cols, rows) = crate::tui::renderer::min_terminal_size();
        let (mut r, wire, screen) = on_screen(cols, rows, ImageProtocol::Kitty);
        r.set_weather(strike.weather);
        r.set_motion(pixtuoid_scene::anim::Motion::Full);
        let scene = two_floor_scene();
        if sliding {
            r.navigate_floor(1, dark);
        }
        for at in [dark, late] {
            screen.at(at);
            r.render(&scene, at).expect("render");
        }
        wire.take();
        r.terminal.backend_mut().inner.resize(cols + 8, rows + 4);
        screen.at(held);
        r.render(&scene, held).expect("render");
        assert!(wire.take().contains(TRANSMIT), "sliding {sliding}: sent");
    }
}

/// A phase holds the floor from when its write lands, not from when its frame
/// began: after a slow write, the next phase waits for the floor to pass on
/// the screen clock, though its frame's own clock says it has.
#[test]
fn a_slow_writes_phase_holds_the_floor_from_when_it_lands() {
    use crate::test_flash::{SLOW, storm_strike};
    let floor = Duration::from_millis(pixtuoid_scene::anim::PHOTOSENSITIVE_PHASE_MIN_MS);
    let strike = storm_strike();
    let [first, second] = [strike.changes[0], strike.changes[1]];
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, wire, screen) = on_screen(cols, rows, ImageProtocol::Kitty);
    r.set_weather(strike.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let scene = office();
    screen.at(first - 2 * floor);
    r.render(&scene, first - 2 * floor).expect("render");
    *wire.slow.lock().expect("lock") = Some(Latency {
        screen: screen.clone(),
        by: SLOW,
    });
    screen.at(first);
    r.render(&scene, first).expect("render");
    let landed = screen.now();
    *wire.slow.lock().expect("lock") = None;
    wire.take();
    assert!(
        second.duration_since(first).expect("in order") >= floor,
        "the frame clock says the floor has passed"
    );
    screen.at(second);
    r.render(&scene, second).expect("render");
    assert_eq!(
        wire.take(),
        "",
        "held until the slow write's phase shows the floor"
    );
    let shown = std::time::UNIX_EPOCH + landed + floor;
    screen.at(shown);
    r.render(&scene, shown).expect("render");
    assert!(wire.take().contains(TRANSMIT), "shown once it has");
}

/// A pause freezes the frame clock inside a hold; the screen clock runs on,
/// so the hold runs out and the paused frame, with a theme change, is sent.
#[test]
fn a_pause_inside_a_hold_never_wedges_the_repaint() {
    use crate::test_flash::{held_frames, storm_strike};
    let floor = Duration::from_millis(pixtuoid_scene::anim::PHOTOSENSITIVE_PHASE_MIN_MS);
    let strike = storm_strike();
    let [dark, late, paused, _] = held_frames(&strike);
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let (mut r, wire, screen) = on_screen(cols, rows, ImageProtocol::Kitty);
    r.set_weather(strike.weather);
    r.set_motion(pixtuoid_scene::anim::Motion::Full);
    let scene = office();
    for at in [dark, late] {
        screen.at(at);
        r.render(&scene, at).expect("render");
    }
    wire.take();
    screen.at(paused);
    r.render(&scene, paused).expect("render");
    assert_eq!(wire.take(), "", "held");
    r.set_theme(dark_theme());
    screen.at(late);
    screen.advance(floor);
    r.render(&scene, paused).expect("render");
    assert!(
        wire.take().contains(TRANSMIT),
        "the paused frame went out once the hold ran out"
    );
}

/// When a modal reaches every tile, a strike shows in the cells it leaves
/// free, and each phase there holds the floor too.
#[test]
fn each_strike_phase_holds_the_floor_in_the_cells_a_full_modal_leaves() {
    use crate::test_flash::{assert_each_phase_holds_the_floor, frame_grid, lead, storm_strike};
    let strike = storm_strike();
    let tick = crate::tui::frame_tick();
    for (frame, offset) in frame_grid(tick) {
        let (cols, rows) = crate::tui::renderer::min_terminal_size();
        let (mut r, wire, screen) = on_screen(cols, rows, ImageProtocol::Sixel);
        r.set_weather(strike.weather);
        r.set_motion(pixtuoid_scene::anim::Motion::Full);
        r.set_frames(
            RenderFrames {
                help_open: true,
                ..Default::default()
            },
            t0(),
        );
        let scene = office();
        let now = strike.start - lead(frame) + offset;
        screen.at(now);
        r.render(&scene, now).expect("render");
        let free = sixel_cells(&wire.take());
        assert!(!free.is_empty(), "the modal leaves cells free");
        let mut changed = Vec::new();
        for now in crate::test_flash::frames_after(now, frame, strike.end + lead(frame)) {
            screen.at(now);
            r.render(&scene, now).expect("render");
            if 2 * sixel_cells(&wire.take()).len() > free.len() {
                changed.push(now);
            }
        }
        let at = format!("a frame each {frame:?} from +{offset:?}");
        assert_each_phase_holds_the_floor(&changed, strike.changes.len(), &at);
    }
}

/// A starved neon's every catch, and every dark between, stays on screen at
/// least the photosensitive floor on every protocol, at its cadence, at each
/// frame grid: from the send of the tube's tiles that shows it to the one
/// that replaces it.
#[test]
fn each_stutter_phase_holds_the_floor_on_screen_at_every_protocols_cadence() {
    use crate::test_flash::{
        assert_each_phase_holds_the_floor, frame_grid, lead, neon_tube, starved_stutter,
    };
    let stutter = starved_stutter();
    let tick = crate::tui::frame_tick();
    let scene = scene_with(vec![], 16);
    let (cols, rows) = crate::tui::renderer::min_terminal_size();
    let area = crate::tui::renderer::scene_rect(Rect::new(0, 0, cols, rows));
    for protocol in [
        ImageProtocol::Kitty,
        ImageProtocol::Sixel,
        ImageProtocol::Iterm2,
    ] {
        for (frame, offset) in frame_grid(tick) {
            let (mut r, wire, screen) = on_screen(cols, rows, protocol);
            r.set_weather(stutter.weather);
            r.set_motion(pixtuoid_scene::anim::Motion::Full);
            for at in stutter.setup {
                screen.at(at);
                r.render(&scene, at).expect("render");
            }
            let shape = r.cutaway.as_ref().expect("a cutaway").tile_shape();
            let across = u32::from(area.width.div_ceil(shape.cols));
            let tiles: std::collections::BTreeSet<(u16, u16)> = area
                .positions()
                .filter(|p| neon_tube(p.x, 2 * p.y))
                .map(|p| (p.x / shape.cols, p.y / shape.rows))
                .collect();
            let tube_sent = |wire: &str| match protocol {
                ImageProtocol::Kitty => kitty_images(wire).iter().any(|&(id, _)| {
                    tiles.iter().any(|&(tx, ty)| {
                        id == crate::graphics::kitty::process_base()
                            + u32::from(ty) * across
                            + u32::from(tx)
                    })
                }),
                ImageProtocol::Sixel | ImageProtocol::Iterm2 => tiles.iter().any(|&(tx, ty)| {
                    let at = (ty * shape.rows + 1, tx * shape.cols + 1);
                    wire.contains(&format!("\x1b[{};{}H{}", at.0, at.1, intro(protocol)))
                }),
            };
            let now = stutter.start - lead(frame) + offset;
            screen.at(now);
            r.render(&scene, now).expect("render");
            wire.take();
            let mut changed = Vec::new();
            for now in crate::test_flash::frames_after(now, frame, stutter.end + lead(frame)) {
                screen.at(now);
                r.render(&scene, now).expect("render");
                if tube_sent(&wire.take()) {
                    changed.push(now);
                }
            }
            let at = format!("{protocol:?}, a frame each {frame:?} from +{offset:?}");
            assert_each_phase_holds_the_floor(&changed, stutter.changes.len(), &at);
        }
    }
}

/// Each kitty image the wire carried, in order: its id and its pixels,
/// inflated, or read from shared memory as a terminal reads it (`t=s`, `S`).
fn kitty_images(wire: &str) -> Vec<(u32, Vec<u8>)> {
    let mut images = Vec::new();
    let mut open: Option<(u32, Option<usize>, String)> = None;
    for escape in wire
        .split("\x1b\\")
        .filter_map(|e| e.split_once("\x1b_G").map(|(_, e)| e))
    {
        let (keys, payload) = escape.split_once(';').expect("a payload");
        if let Some(id) = keys.split(',').find_map(|k| k.strip_prefix("i=")) {
            let shared = keys
                .contains("t=s")
                .then(|| keys.split(',').find_map(|k| k.strip_prefix("S=")))
                .flatten()
                .map(|len| len.parse().expect("a length"));
            open = Some((id.parse().expect("an id"), shared, String::new()));
        }
        let (_, _, data) = open.as_mut().expect("an image under way");
        data.push_str(payload);
        if keys.contains("m=0") {
            let (id, shared, data) = open.take().expect("an image under way");
            let bytes = base64_simd::STANDARD
                .decode_to_vec(data.as_bytes())
                .expect("base64");
            let rgb = match shared {
                #[cfg(unix)]
                Some(len) => crate::graphics::shm::read_and_unlink(
                    std::str::from_utf8(&bytes).expect("a name"),
                    len,
                )
                .expect("the object holds the tile"),
                #[cfg(not(unix))]
                Some(_) => unreachable!("shared memory is Unix-only"),
                None => miniz_oxide::inflate::decompress_to_vec_zlib(&bytes).expect("zlib"),
            };
            images.push((id, rgb));
        }
    }
    images
}

/// Through shared memory the terminal receives every tile the escapes would
/// carry, pixel for pixel: the medium changes how, never what.
#[cfg(unix)]
#[test]
fn shared_memory_carries_the_tiles_the_escapes_would() {
    let run = |shm| {
        let (cols, rows) = (120, 40);
        let mut r = TuiRenderer::new(
            Terminal::new(Window::new(cols, rows)).expect("terminal"),
            normal_theme(),
            Vec::new(),
            pack_arc(),
        );
        let wire = Wire::default();
        let out = crate::tui::FrameOut::new(wire.clone(), false);
        r.present_through(out.clone());
        r.set_cutaway(TileCutaway::new(
            fit(cols, rows),
            CELL,
            crate::graphics::Route::of(ImageProtocol::Kitty, false, shm),
            Box::new(out),
        ));
        r.render(&office(), t0()).expect("render");
        kitty_images(&wire.take())
    };
    let direct = run(false);
    assert!(!direct.is_empty(), "the first frame sends its tiles");
    assert_eq!(run(true), direct);
}

/// A floor switch slides the cutaway the way classic slides its half-blocks:
/// mid-slide, the middle tile shows neither floor and nothing is
/// hit-tested; then it settles on the destination.
#[test]
fn a_floor_switch_slides_the_cutaway_then_settles() {
    let (mut r, wire) = kitty(120, 40);
    let scene = two_floor_scene();
    let mut now = t0();
    r.render(&scene, now).expect("render");
    let shape = r.cutaway.as_ref().expect("a cutaway").tile_shape();
    let across = 120u32.div_ceil(u32::from(shape.cols));
    let down = 40u32.div_ceil(u32::from(shape.rows));
    let middle = crate::graphics::kitty::process_base() + across * (down / 2) + across / 2;
    let tile = |sent: &str| {
        kitty_images(sent)
            .into_iter()
            .rev()
            .find(|(id, _)| *id == middle)
            .map(|(_, rgb)| rgb)
    };
    let before = tile(&wire.take()).expect("the first frame sends every tile");

    r.navigate_floor(1, now);
    let half = Duration::from_millis(r.transition().expect("sliding").duration_ms / 2);
    r.render(&scene, now + half).expect("render");
    // What the terminal shows for the tile: its last transmit, or the one
    // before when none came.
    let mid = tile(&wire.take()).unwrap_or_else(|| before.clone());
    assert!(r.transition().is_some());
    assert!(r.cached_layout().is_none());
    let area = r.frame_buffer().area;
    assert!(
        area.positions()
            .all(|p| r.hit_test_agent_at(p.x, p.y).is_none())
    );

    now += half;
    render_until_settled(&mut r, &scene, &mut now, 1);
    let after = tile(&wire.take()).unwrap_or_else(|| mid.clone());
    assert!(mid != before && mid != after);
    assert_eq!(r.current_floor(), 1);
    hover_agent(&mut r, AgentId::from_transcript_path("/n/1.jsonl"));
}

/// A slide is no resize: the extent a resize changes holds through it, so the
/// slide runs its course rather than landing after its first frame; a resize
/// lands it on the destination.
#[test]
fn a_cutaway_slide_runs_until_a_resize_lands_it() {
    let (cols, rows) = (120, 40);
    let (mut r, _wire) = kitty(cols, rows);
    let scene = two_floor_scene();
    let start = t0();
    r.render(&scene, start).expect("render");
    let extent = r.office_extent();
    r.navigate_floor(1, start);
    let slide = Duration::from_millis(r.transition().expect("sliding").duration_ms);
    let frames = 4;
    let at = |frame| start + slide * frame / frames;
    for frame in 1..frames - 1 {
        r.render(&scene, at(frame)).expect("render");
        assert_eq!(r.office_extent(), extent, "frame {frame}");
        assert!(r.transition().is_some(), "landed at frame {frame}");
    }
    r.terminal.backend_mut().resize(cols - 1, rows);
    r.render(&scene, at(frames - 1)).expect("render");
    assert!(r.transition().is_none());
    assert_eq!(r.current_floor(), 1);
}

/// A modal's tiles are sent but for its cells, and once it closes, the
/// cells it held go back to the image: all of them but the world's text.
#[test]
fn the_cells_a_modal_held_go_back_to_the_image_when_it_closes() {
    let (cols, rows) = (120, 40);
    let (mut r, wire) = painter(cols, rows, ImageProtocol::Sixel);
    let scene = office();
    let cadence = ImageProtocol::Sixel.cadence();
    r.render(&scene, t0()).expect("render");
    wire.take();
    let frames = RenderFrames {
        help_open: true,
        ..Default::default()
    };
    r.set_frames(frames, t0());
    r.render(&scene, t0() + cadence).expect("render");
    let held = text_cells(r.frame_buffer());
    let open = sixel_cells(&wire.take());
    assert!(!open.is_empty(), "the modal's tiles are sent");
    assert!(open.is_disjoint(&held), "never over its text");

    r.set_frames(RenderFrames::default(), t0());
    r.render(&scene, t0() + cadence * 2).expect("render");
    let sent = sixel_cells(&wire.take());
    let text = text_cells(&world_text(&r));
    let lost: Vec<_> = held
        .iter()
        .filter(|c| !text.contains(c) && !sent.contains(c))
        .collect();
    assert!(lost.is_empty(), "left to the modal's text: {lost:?}");
}

#[test]
fn a_grid_image_arms_the_unwinds_erase() {
    let erases = |drew: &AtomicBool| {
        crate::graphics::grid_unwind(drew.load(Ordering::Relaxed))
            .windows(4)
            .any(|w| w == b"\x1b[2J")
    };
    let (mut r, _wire, in_grid) = armed(120, 40, ImageProtocol::Sixel, vec![]);
    assert!(!erases(in_grid), "nothing drawn yet");
    r.render(&office(), t0()).expect("render");
    assert!(erases(in_grid));
}

/// While a modal stays open, no image is drawn over its text, tick after
/// tick, though every tile is owed.
#[test]
fn no_image_is_drawn_over_text() {
    let (mut r, wire) = painter(120, 40, ImageProtocol::Sixel);
    let scene = office();
    let cadence = ImageProtocol::Sixel.cadence();
    r.render(&scene, t0()).expect("render");
    wire.take();
    r.set_frames(
        RenderFrames {
            help_open: true,
            ..Default::default()
        },
        t0(),
    );
    r.redraw().expect("redraw");
    // The backend keeps a cell's last text under an image ratatui skips, so
    // a frame's text is the modal's, as the cleared screen first got it, and
    // the world's of that frame.
    let mut modal = None;
    for tick in 1..4 {
        r.render(&scene, t0() + cadence * tick).expect("render");
        let sent = sixel_cells(&wire.take());
        let world = text_cells(&world_text(&r));
        let modal = modal.get_or_insert_with(|| {
            assert!(!sent.is_empty(), "the cells the modal leaves free are sent");
            &text_cells(r.frame_buffer()) - &world
        });
        assert!(sent.is_disjoint(modal), "tick {tick}: over the modal");
        assert!(
            sent.is_disjoint(&world),
            "tick {tick}: over the world's text"
        );
    }
}

/// A backend that marks on its `tap` wire where each flush of cells lands
/// among the transmits.
type Logged = Tapped<Window, Wire>;

const FLUSH: &str = "<flush>";

impl Tap<Window> for Wire {
    fn on_draw(&mut self, cells: &[(u16, u16, &ratatui::buffer::Cell)]) {
        if !cells.is_empty() {
            self.bytes
                .lock()
                .expect("lock")
                .extend_from_slice(FLUSH.as_bytes());
        }
    }
}

/// kitty's images go out before the placeholders that show them; SIXEL's
/// after the cells they must avoid.
#[test]
fn kitty_transmits_before_the_flush_and_sixel_after() {
    for (protocol, intro, first) in [
        (ImageProtocol::Kitty, TRANSMIT, true),
        (ImageProtocol::Sixel, SIXEL, false),
    ] {
        let wire = Wire::default();
        let backend = Logged {
            inner: Window::new(120, 40),
            tap: wire.clone(),
        };
        let mut r = TuiRenderer::new(
            Terminal::new(backend).expect("terminal"),
            normal_theme(),
            vec![],
            pack_arc(),
        );
        r.set_cutaway(
            TileCutaway::new(
                fit(120, 40),
                CELL,
                crate::graphics::Route::direct(protocol, false),
                Box::new(wire.clone()),
            )
            .arming(Box::leak(Box::new(AtomicBool::new(false)))),
        );
        r.render(&office(), t0()).expect("render");
        let sent = wire.take();
        let (image, flush) = (
            sent.find(intro).expect("an image"),
            sent.find(FLUSH).expect("a flush"),
        );
        assert_eq!(image < flush, first, "{protocol:?}");
    }
}

/// The cutaway's twin of the classic's refused-frame clamp: the door closes
/// on time while the frame is refused.
#[test]
fn a_refused_cutaway_frame_keeps_the_doors_clamp_on_time() {
    let (mut r, _wire) = kitty(120, 40);
    let scene = office();
    r.render(&scene, t0()).expect("render");
    assert!(
        r.session.floor(0).expect("a floor").ctx.door_anim_max_ms > 0,
        "the entry walk holds the door"
    );
    let (small_cols, small_rows) = too_small_terminal();
    r.terminal.backend_mut().resize(small_cols, small_rows);
    r.render(&scene, t0() + Duration::from_secs(600))
        .expect("render");
    assert_eq!(
        r.session.floor(0).expect("a floor").ctx.door_anim_max_ms,
        0,
        "the walk long arrived"
    );
}

/// A slide cancelled on its first frame never paints, so nothing marks the
/// screen as the slide's; the destination floor drawn before still repaints
/// whole once the terminal is back, not as a diff of its own last frame.
#[test]
fn a_slide_cancelled_on_its_first_frame_repaints_the_destination_whole() {
    let (cols, rows) = (120, 40);
    let (mut r, wire) = painter(cols, rows, ImageProtocol::Sixel);
    let scene = two_floor_scene();
    let mut now = t0();
    r.render(&scene, now).expect("render");
    let every = wire.take().matches(SIXEL).count();
    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);
    r.navigate_floor(0, now);
    render_until_settled(&mut r, &scene, &mut now, 0);
    r.navigate_floor(1, now);
    let (small_cols, small_rows) = too_small_terminal();
    r.terminal.backend_mut().resize(small_cols, small_rows);
    now += ImageProtocol::Sixel.cadence();
    r.render(&scene, now).expect("render");
    assert_eq!(r.current_floor(), 1);
    r.terminal.backend_mut().resize(cols, rows);
    wire.take();
    now += ImageProtocol::Sixel.cadence();
    r.render(&scene, now).expect("render");
    assert_eq!(wire.take().matches(SIXEL).count(), every);
}

/// A floor can leave the screen without a slide: the top floor's last agent
/// ends, the floor count drops, and the view clamps to the floor below. That
/// floor's raster diffs against its own last frame, not the vanished floor
/// on screen, so the terminal ends up holding exactly what a fresh painter of
/// that floor sends.
#[test]
fn a_floor_clamped_into_view_repaints_whole() {
    let held = |wire: &str, tiles: &mut std::collections::BTreeMap<u32, Vec<u8>>| {
        tiles.extend(kitty_images(wire));
    };
    let (cols, rows) = (120, 40);
    let (mut r, wire) = kitty(cols, rows);
    let mut scene = two_floor_scene();
    let mut now = t0();
    let mut terminal = std::collections::BTreeMap::new();
    r.render(&scene, now).expect("render");
    r.navigate_floor(1, now);
    render_until_settled(&mut r, &scene, &mut now, 1);
    scene
        .agents
        .remove(&AgentId::from_transcript_path("/n/1.jsonl"));
    now += ImageProtocol::Kitty.cadence();
    r.render(&scene, now).expect("render");
    assert_eq!(r.current_floor(), 0, "the view clamped");
    held(&wire.take(), &mut terminal);

    let (mut fresh, fresh_wire) = kitty(cols, rows);
    fresh.render(&scene, now).expect("render");
    let mut want = std::collections::BTreeMap::new();
    held(&fresh_wire.take(), &mut want);
    assert!(
        terminal == want,
        "the terminal still shows the vanished floor"
    );
}

/// `w` follows the look the last frame drew: not listed or read under the
/// cutaway, which never draws the walkable layer, and listed, read and drawn
/// while a cell too small for the art falls back to the half-blocks.
#[cfg(debug_assertions)]
#[test]
fn w_follows_the_look_the_frame_drew() {
    use crossterm::event::KeyCode;
    let (mut r, _wire) = kitty(140, 60);
    let scene = office();
    let cadence = ImageProtocol::Kitty.cadence();
    let w = |r: &mut TuiRenderer<Window>| {
        crate::tui::toggle_walkable_debug_on_w(
            r,
            KeyCode::Char('w'),
            &crate::panels::ModalState::closed(),
        )
    };
    // The help the frame draws: its text, read off the composed cells.
    let help = |r: &mut TuiRenderer<Window>, at| {
        r.set_frames(
            RenderFrames {
                help_open: true,
                ..Default::default()
            },
            t0(),
        );
        r.render(&scene, at).expect("render");
        let text = frame_text(r.frame_buffer());
        r.set_frames(RenderFrames::default(), t0());
        text
    };
    r.render(&scene, t0()).expect("render");
    assert!(!r.offers_walkable_debug(), "the cutaway");
    let text = help(&mut r, t0() + cadence);
    assert!(text.contains("Keyboard"), "premise: the help is drawn");
    assert!(!text.contains("walkable"), "the cutaway's help: {text}");
    assert!(!w(&mut r) && !r.debug_walkable());

    r.terminal.backend_mut().zoom(CellSize {
        w: CELL.w / 2,
        h: CELL.h / 2,
    });
    r.render(&scene, t0() + cadence * 2).expect("render");
    assert!(r.offers_walkable_debug(), "the fallback");
    let text = help(&mut r, t0() + cadence * 3);
    assert!(text.contains("walkable"), "the fallback's help: {text}");
    r.render(&scene, t0() + cadence * 4).expect("render");
    let before = r.buf().expect("a frame").clone();
    assert!(w(&mut r));
    r.render(&scene, t0() + cadence * 5).expect("render");
    let after = r.buf().expect("a frame");
    assert!(
        region_diff(&before, after, 0, 0, before.width(), before.height()) > 1_000,
        "the fallback's half-blocks show the layer"
    );

    r.terminal.backend_mut().zoom(CELL);
    r.render(&scene, t0() + cadence * 6).expect("render");
    assert!(!r.offers_walkable_debug(), "the cutaway is back");
}
