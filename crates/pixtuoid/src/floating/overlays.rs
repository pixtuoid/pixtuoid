//! The window's chrome over the office: its footer, tooltip and panels, and the panel geometry a press maps through.

use pixtuoid_core::state::SceneState;
use pixtuoid_scene::cutaway::{Canvas, CellPx, Face, GridInk, paint_grid};
use pixtuoid_scene::display::cells::{CARD_SHADOW, CellGrid, CellRect};
use pixtuoid_scene::footer::FooterModel;
use pixtuoid_scene::pack::OfficeArt;
use pixtuoid_scene::render_scale::PixelFit;
use pixtuoid_scene::theme::Theme;

use super::compose::{Change, Layer, LayerId, LayerPixels, PxRect, RgbaLayer};
use super::geometry::{FOOTER_MARGIN_PX, footer_band};

/// Everything a presented frame shows beside the office: the window's size,
/// its footer, and the tooltip by the pointer.
#[derive(Debug, Clone)]
pub struct Overlays {
    pub window: (u32, u32),
    pub footer: FooterModel,
    pub tooltip: Option<(pixtuoid_scene::tooltip::Tooltip, (i32, i32))>,
    /// The open panels, as the TUI paints them: `panels_grid`.
    pub panels: Option<CellGrid>,
}

/// `frames`' open panels over a window `cols`×`rows` screen cells big, as
/// the TUI paints them over its terminal with the window's zoom keys in the
/// help, read back as a grid; `None` when none is open.
pub(crate) fn panels_grid(
    frames: &crate::panels::ui_state::RenderFrames,
    (cols, rows): (u16, u16),
    now: std::time::SystemTime,
    theme: &Theme,
) -> Option<CellGrid> {
    let overlays = frames.overlays(
        crate::panels::ui_state::popup_whole(frames.version_popup.open),
        super::input::ZOOM_SHORTCUTS,
    );
    if !overlays.any_open() {
        return None;
    }
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows)).ok()?;
    term.draw(|f| crate::panels::paint_overlays(f, &overlays, now, f.area(), theme))
        .ok()?;
    let buf = term.backend().buffer();
    Some(crate::panels::widgets::grid_of(buf, buf.area))
}

/// A window `window` physical px big, in screen cells of `cell`: the
/// surface the panels are laid out on.
pub(crate) fn panel_cells((w, h): (u32, u32), cell: CellPx) -> (u16, u16) {
    let fits = |px: u32, size: u16| u16::try_from(px / u32::from(size.max(1))).unwrap_or(u16::MAX);
    (fits(w, cell.w), fits(h, cell.h))
}

/// The screen cell of `cell` under window point `(x, y)`: the panels paint
/// from the window's top-left ([`paint_panels`]).
pub(crate) fn cell_at((x, y): (f64, f64), cell: CellPx) -> (u16, u16) {
    let at = |p: f64, size: u16| {
        u16::try_from(p.max(0.0) as u32 / u32::from(size.max(1))).unwrap_or(u16::MAX)
    };
    (at(x, cell.w), at(y, cell.h))
}

/// What the open panels make of a left press at window point `cursor` on a
/// `window`-px window showing `shown`: [`crate::panels::modal_mouse`] in its
/// panel cells, the popup drawn whole.
pub(crate) fn modal_press(
    ui: &mut crate::panels::ui_state::UiState,
    cursor: (f64, f64),
    window: (u32, u32),
    shown: Option<PixelFit>,
) -> crate::panels::ModalMouse {
    let cells = shown.map(|at| {
        let cell = Face::chrome(at);
        (cell_at(cursor, cell), panel_cells(window, cell))
    });
    let popup_scale = crate::panels::ui_state::popup_whole(ui.modal().version_popup_open);
    crate::panels::modal_mouse(ui, popup_scale, cells.map(|(at, _)| at), || {
        cells.map(|(_, screen)| screen)
    })
}

/// A tooltip's card where it opens: its top-left in window px, and the card.
#[derive(Debug)]
pub(crate) struct PlacedTip {
    at: (i32, i32),
    card: CellGrid,
}

impl PlacedTip {
    /// `tip`'s shared [`card`](pixtuoid_scene::tooltip::Tooltip::card) where
    /// [`place`](pixtuoid_scene::tooltip::place) opens it by the pointer at
    /// `cursor` (physical px) in a `window`-px window of screen cells `cell`,
    /// as the TUI places it in terminal cells.
    pub(crate) fn new(
        tip: &pixtuoid_scene::tooltip::Tooltip,
        cursor: (f64, f64),
        theme: &Theme,
        cell: CellPx,
        (w, h): (u32, u32),
    ) -> Self {
        let card = tip.card(theme);
        let cells =
            |px: u32, size: u16| u16::try_from(px / u32::from(size.max(1))).unwrap_or(u16::MAX);
        let area = CellRect {
            x: 0,
            y: 0,
            w: cells(w, cell.w),
            h: cells(h, cell.h),
        };
        let pointer = (
            cells(cursor.0.max(0.0) as u32, cell.w),
            cells(cursor.1.max(0.0) as u32, cell.h),
        );
        let placed = pixtuoid_scene::tooltip::place(card.rect(), pointer, area, tip.anchor);
        let at = (
            i32::from(placed.x) * i32::from(cell.w),
            i32::from(placed.y) * i32::from(cell.h),
        );
        Self { at, card }
    }

    /// What [`paint_tooltip`] draws over: the card and its shadow, a cell
    /// right and half a cell down ([`paint_grid`]), inside the window.
    pub(crate) fn rect(&self, cell: CellPx, window: (u32, u32)) -> PxRect {
        let (cw, ch) = (u32::from(cell.w), u32::from(cell.h));
        PxRect {
            x: self.at.0,
            y: self.at.1,
            w: u32::from(self.card.width()) * cw + cw,
            h: u32::from(self.card.height()) * ch + ch / 2,
        }
        .within(window)
        .unwrap_or(PxRect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        })
    }
}

/// Paint `placed` in screen cells of `cell`, with its drop shadow.
pub(crate) fn paint_tooltip(
    canvas: &mut impl Canvas,
    placed: &PlacedTip,
    (theme, pack): (&Theme, &OfficeArt),
    cell: CellPx,
) {
    let ink = GridInk {
        text: theme.ui.tooltip_text,
        halo: None,
        shadow: Some(CARD_SHADOW),
    };
    paint_grid(
        canvas,
        &placed.card,
        (placed.at, cell),
        (Face::Screen, pack),
        ink,
    );
}

/// The panel `name` (`help`, `dashboard`, `sources` or `theme`) open over
/// `scene` in a `window`-px window of screen cells `cell`, for a snapshot to
/// paint; `None` for a name it doesn't know.
#[doc(hidden)]
pub fn panel_preview(
    name: &str,
    scene: &SceneState,
    theme: &'static Theme,
    (window, cell): ((u32, u32), CellPx),
    now: std::time::SystemTime,
) -> Option<CellGrid> {
    let mut ui = crate::panels::ui_state::UiState::new(
        theme,
        crate::panels::welcome::WelcomeUi::from_detected(&[]),
        std::path::PathBuf::new(),
        None,
        crate::doctor::DriftSeen::default(),
    );
    match name {
        "help" => ui.toggle_help(),
        "dashboard" => ui.toggle_dashboard(scene),
        "sources" => ui.open_connection(crate::panels::connection::build_rows(
            &crate::runtime::ConnectedSources::default().snapshot(),
            &ui.read_conn_log(),
        )),
        "theme" => ui.open_theme_picker(),
        _ => return None,
    }
    panels_grid(
        &ui.build_frames(now, scene, &[]),
        panel_cells(window, cell),
        now,
        theme,
    )
}

/// What open `panels` draw over: the cells they ink, and one cell more right
/// and down, which a wide glyph, a halo or a bold strike reaches into
/// ([`paint_grid`]); `None` when they ink none.
pub(crate) fn panels_rect(panels: &CellGrid, cell: CellPx, window: (u32, u32)) -> Option<PxRect> {
    let inked = |x, y| {
        panels
            .get(x, y)
            .is_some_and(|c| c.bg.is_some() || !c.symbol.trim().is_empty())
    };
    let (x0, y0, x1, y1) = (0..panels.height())
        .flat_map(|y| (0..panels.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| inked(x, y))
        .fold(None::<(u16, u16, u16, u16)>, |span, (x, y)| {
            Some(match span {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            })
        })?;
    let (cw, ch) = (u32::from(cell.w), u32::from(cell.h));
    PxRect {
        x: i32::try_from(u32::from(x0) * cw).unwrap_or(i32::MAX),
        y: i32::try_from(u32::from(y0) * ch).unwrap_or(i32::MAX),
        w: (u32::from(x1 - x0) + 2) * cw,
        h: (u32::from(y1 - y0) + 2) * ch,
    }
    .within(window)
}

/// Paint `panels` over the window from its top-left, in screen cells of
/// `cell`: the TUI's panels, cell for cell.
pub(crate) fn paint_panels(
    canvas: &mut impl Canvas,
    panels: &CellGrid,
    (theme, pack): (&Theme, &OfficeArt),
    cell: CellPx,
) {
    let ink = GridInk {
        text: theme.ui.tooltip_text,
        halo: None,
        shadow: None,
    };
    paint_grid(canvas, panels, ((0, 0), cell), (Face::Screen, pack), ink);
}

/// How many screen cells of `cell` fit across a `win_w`-pixel window
/// between the footer's margins: its column budget.
pub fn footer_budget(win_w: usize, cell: CellPx) -> u16 {
    let room = win_w.saturating_sub(2 * usize::from(FOOTER_MARGIN_PX));
    u16::try_from(room / usize::from(cell.w.max(1))).unwrap_or(u16::MAX)
}

/// The footer's band: a `window`'s full width at its foot, `at`'s
/// [`footer_band`] tall.
pub(crate) fn footer_rect(at: PixelFit, (w, h): (u32, u32)) -> PxRect {
    let band = u32::from(footer_band(at)).min(h);
    PxRect {
        x: 0,
        y: i32::try_from(h - band).unwrap_or(i32::MAX),
        w,
        h: band,
    }
}

/// Paint the shared status footer in `at`'s `footer_band` at the foot of a
/// `window`-px window: the theme's ground, the TUI footer row's terminal
/// background, and the line in `at`'s screen cells over it — the window's
/// twin of the TUI's status row, from the same
/// [`build_footer`](pixtuoid_scene::footer::build_footer) model.
pub(crate) fn paint_footer(
    canvas: &mut impl Canvas,
    model: &FooterModel,
    (theme, pack): (&Theme, &OfficeArt),
    at: PixelFit,
    window: (u32, u32),
) {
    let cell = Face::chrome(at);
    let band = footer_rect(at, window);
    let ground = theme.surface.bg_fallback;
    for y in band.y..band.y.saturating_add_unsigned(band.h) {
        for x in 0..i32::try_from(band.w).unwrap_or(i32::MAX) {
            canvas.set(x, y, ground);
        }
    }
    let margin = i32::from(FOOTER_MARGIN_PX);
    let ink = GridInk {
        text: theme.ui.label_idle,
        halo: None,
        shadow: None,
    };
    paint_grid(
        canvas,
        &model.line(theme),
        ((margin, band.y + margin), cell),
        (Face::Screen, pack),
        ink,
    );
}

/// What an overlay's pixels depend on beside its model. The theme is
/// compared by identity: every theme is a `&'static` table.
#[derive(Debug, Clone, Copy)]
struct Look {
    theme: &'static Theme,
    at: PixelFit,
    window: (u32, u32),
}

impl PartialEq for Look {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.theme, other.theme) && self.at == other.at && self.window == other.window
    }
}

#[derive(Debug)]
struct Painted<K> {
    key: K,
    layer: RgbaLayer,
    /// Whether the last [`OverlayLayers::update`] painted it.
    fresh: bool,
}

/// A tooltip and the window point it opens by, as [`Overlays`] holds it.
type Tip = (pixtuoid_scene::tooltip::Tooltip, (i32, i32));

/// The overlays as layers, each repainted only when what it shows changed.
#[derive(Debug, Default)]
pub struct OverlayLayers {
    footer: Option<Painted<(FooterModel, Look)>>,
    tooltip: Option<Painted<(Tip, Look)>>,
    panels: Option<Painted<(CellGrid, Look)>>,
}

/// Keep `slot` if it already shows `want`, else repaint it, or drop it when
/// nothing is wanted or `paint` draws nothing.
fn refresh<K: PartialEq>(
    slot: &mut Option<Painted<K>>,
    want: Option<K>,
    paint: impl FnOnce(&K) -> Option<RgbaLayer>,
) {
    match (slot.as_mut(), want) {
        (Some(painted), Some(key)) if painted.key == key => painted.fresh = false,
        (_, Some(key)) => {
            *slot = paint(&key).map(|layer| Painted {
                key,
                layer,
                fresh: true,
            });
        }
        (_, None) => *slot = None,
    }
}

impl OverlayLayers {
    /// Bring the layers to `next` over a frame drawn at `at`.
    pub fn update(
        &mut self,
        next: Overlays,
        at: PixelFit,
        (theme, pack): (&'static Theme, &OfficeArt),
    ) {
        let Overlays {
            window,
            footer,
            tooltip,
            panels,
        } = next;
        let look = Look { theme, at, window };
        let cell = Face::chrome(at);
        refresh(&mut self.footer, Some((footer, look)), |(model, _)| {
            let mut layer = RgbaLayer::new(footer_rect(at, window));
            paint_footer(&mut layer, model, (theme, pack), at, window);
            Some(layer)
        });
        refresh(
            &mut self.tooltip,
            tooltip.map(|tip| (tip, look)),
            |((tip, cursor), _)| {
                let cursor = (f64::from(cursor.0), f64::from(cursor.1));
                let placed = PlacedTip::new(tip, cursor, theme, cell, window);
                let mut layer = RgbaLayer::new(placed.rect(cell, window));
                paint_tooltip(&mut layer, &placed, (theme, pack), cell);
                Some(layer)
            },
        );
        refresh(
            &mut self.panels,
            panels.map(|panels| (panels, look)),
            |(panels, _)| {
                let mut layer = RgbaLayer::new(panels_rect(panels, cell, window)?);
                paint_panels(&mut layer, panels, (theme, pack), cell);
                Some(layer)
            },
        );
    }

    /// The layers shown, in paint order: each [`Change::All`] in the update
    /// that painted it, else [`Change::Unchanged`].
    pub(crate) fn layers(&self) -> impl Iterator<Item = Layer<'_>> {
        fn one<K>(id: LayerId, p: &Option<Painted<K>>) -> Option<Layer<'_>> {
            p.as_ref().map(|p| Layer {
                id,
                pixels: LayerPixels::Rgba(&p.layer),
                origin: p.layer.origin(),
                scale: 1,
                extent: p.layer.size(),
                change: if p.fresh {
                    Change::All
                } else {
                    Change::Unchanged
                },
            })
        }
        [
            one(LayerId::Footer, &self.footer),
            one(LayerId::Tooltip, &self.tooltip),
            one(LayerId::Panels, &self.panels),
        ]
        .into_iter()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::super::compose::{XrgbSurface, pack_xrgb};
    use super::super::fixtures::{active_on, density, scene_with};
    use super::super::geometry::{Zoom, window_geometry};
    use super::*;
    use pixtuoid_scene::footer::{FooterInputs, build_footer};
    use winit::dpi::PhysicalSize;

    /// A tooltip paints its box in the theme's tooltip background, inside the
    /// window wherever the pointer is: a label near the top flips below it,
    /// and a card at the right edge shifts left.
    #[test]
    fn a_tooltip_stays_in_the_window_and_flips_off_the_edges() {
        let theme = pixtuoid_scene::theme::theme_by_name("normal").expect("normal theme exists");
        let bg = pack_xrgb(theme.ui.tooltip_bg);
        let (w, h) = (320usize, 200usize);
        let pack = pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack loads");
        let painted = |tip: &pixtuoid_scene::tooltip::Tooltip, cursor: (f64, f64)| {
            let mut px = vec![0u32; w * h];
            let mut sb = XrgbSurface::new(&mut px, w, h).expect("sized");
            let look = (theme, &pack);
            let cell = Face::Screen.cell(1);
            let placed = PlacedTip::new(tip, cursor, theme, cell, (w as u32, h as u32));
            paint_tooltip(&mut sb, &placed, look, cell);
            let rows: Vec<usize> = (0..h)
                .filter(|&y| px[y * w..(y + 1) * w].contains(&bg))
                .collect();
            let cols: Vec<usize> = (0..w)
                .filter(|&x| (0..h).any(|y| px[y * w + x] == bg))
                .collect();
            (rows, cols)
        };
        let label = pixtuoid_scene::tooltip::coffee();
        let (rows, _) = painted(&label, (100.0, 4.0));
        assert!(!rows.is_empty(), "the label painted nothing");
        assert!(
            rows[0] > 4,
            "a label at the top flips below the pointer: {rows:?}"
        );
        let (rows, _) = painted(&label, (100.0, 150.0));
        assert!(
            *rows.last().expect("painted") < 150,
            "a label opens above the pointer"
        );
        let (_, cols) = painted(&label, (318.0, 100.0));
        assert!(
            *cols.last().expect("painted") < w,
            "the label shifted inside the right edge"
        );
        assert!(
            cols[0] < 318,
            "the label shifted left of a right-edge pointer"
        );
        // An agent's card opens below the pointer, flips above at the bottom
        // edge, and shifts left at the right one.
        let agent = active_on("/p/a.jsonl", 0, 0);
        let id = agent.agent_id;
        let scene = scene_with(vec![agent], 16);
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let card = pixtuoid_scene::tooltip::agent(&scene, id, now).expect("the agent's card");
        let (rows, _) = painted(&card, (100.0, 20.0));
        assert!(rows[0] > 20, "a card opens below the pointer: {rows:?}");
        let (rows, _) = painted(&card, (100.0, 195.0));
        assert!(
            *rows.last().expect("painted") < 195 && rows[0] > 0,
            "a card at the bottom flips above, inside the window: {rows:?}"
        );
        let (_, cols) = painted(&card, (318.0, 20.0));
        assert!(
            cols[0] < 318 && *cols.last().expect("painted") < w,
            "a card at the right edge shifts left, inside the window"
        );
    }

    /// The open panels read back as the TUI paints them, and nothing when
    /// none is open.
    #[test]
    fn the_window_shows_the_tuis_panels() {
        let theme = pixtuoid_scene::theme::theme_by_name("normal").expect("normal theme exists");
        let scene = SceneState::new([8; pixtuoid_core::state::MAX_FLOORS]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ui = crate::panels::ui_state::UiState::new(
            theme,
            crate::panels::welcome::WelcomeUi::from_detected(&[]),
            tmp.path().join("sock"),
            None,
            crate::doctor::DriftSeen::default(),
        );
        let now = std::time::SystemTime::UNIX_EPOCH;
        let size = (100, 40);
        let shut = ui.build_frames(now, &scene, &[]);
        assert_eq!(panels_grid(&shut, size, now, theme), None);
        ui.toggle_help();
        let help = ui.build_frames(now, &scene, &[]);
        let grid = panels_grid(&help, size, now, theme).expect("help is open");
        let text: String = (0..grid.height())
            .flat_map(|y| (0..grid.width()).map(move |x| (x, y)))
            .filter_map(|(x, y)| grid.get(x, y))
            .map(|c| c.symbol.as_str())
            .collect();
        assert!(text.contains("switch floor"), "{text}");
        assert!(text.contains("zoom"), "the window's own keys: {text}");
    }

    #[test]
    fn paint_footer_blits_into_the_bottom_band_and_tones_via_the_shared_authority() {
        use pixtuoid_scene::footer::{FooterTone, RungKind};
        let theme = pixtuoid_scene::theme::theme_by_name("normal").expect("normal theme exists");
        let mut scene = SceneState::new([8; pixtuoid_core::state::MAX_FLOORS]);
        let slot = active_on("/p/a.jsonl", 0, 0);
        scene.agents.insert(slot.agent_id, slot);
        let inputs = FooterInputs::new(
            &scene,
            crate::panels::widgets::footer_context(&scene, None, true, None, None),
        );
        let (w, h) = (640usize, 400usize);
        let at = window_geometry(
            PhysicalSize::new(w as u32, h as u32),
            density(),
            Zoom::default(),
        );
        let model = build_footer(&inputs, footer_budget(w, Face::chrome(at)));
        let pack = pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack loads");
        // The office as the window draws it, then the footer over it.
        const OFFICE: u32 = 0x0012_3456;
        let mut sb = vec![OFFICE; w * h];
        paint_footer(
            &mut XrgbSurface::new(&mut sb, w, h).expect("sized"),
            &model,
            (theme, &pack),
            at,
            (w as u32, h as u32),
        );
        let top = h - usize::from(footer_band(at));
        assert!(sb[..top * w].iter().all(|&p| p == OFFICE), "above the band");
        assert!(
            sb[top * w..].iter().all(|&p| p != OFFICE),
            "no office pixel under the band"
        );
        assert!(
            sb[top * w..].contains(&pack_xrgb(theme.surface.bg_fallback)),
            "the band's ground"
        );
        assert!(
            sb.contains(&pack_xrgb(FooterTone::Rung(RungKind::Active).rgb(theme))),
            "the ●A rung paints the shared label_active hue"
        );
    }

    /// A window point reads as the panel cell painted under it.
    #[test]
    fn a_window_point_reads_as_its_panel_cell() {
        let cell = CellPx { w: 8, h: 16 };
        assert_eq!(panel_cells((80, 160), cell), (10, 10));
        assert_eq!(cell_at((0.0, 0.0), cell), (0, 0));
        assert_eq!(cell_at((15.9, 16.0), cell), (1, 1));
        assert_eq!(cell_at((-3.0, 40.0), cell), (0, 2));
    }

    /// A press reaches the office only with no panel open; the help closes
    /// on it, and the popup, drawn whole, swallows a press off its link.
    #[test]
    fn a_press_meets_the_open_panels_first() {
        use crate::panels::ModalMouse;
        let window = (960, 640);
        let shown = Some(window_geometry(
            PhysicalSize::new(window.0, window.1),
            density(),
            Zoom::default(),
        ));
        let boot = |popup: bool| {
            let mut ui = crate::panels::ui_state::UiState::new(
                pixtuoid_scene::theme::ALL_THEMES[0],
                crate::panels::welcome::WelcomeUi::from_detected(&[]),
                std::path::PathBuf::new(),
                None,
                crate::doctor::DriftSeen::default(),
            );
            if popup {
                ui.open_version_popup(&std::collections::HashSet::new(), &[]);
            }
            ui
        };
        let corner = (1.0, 1.0);
        let mut ui = boot(false);
        assert_eq!(
            modal_press(&mut ui, corner, window, shown),
            ModalMouse::Office
        );
        ui.toggle_help();
        assert_eq!(
            modal_press(&mut ui, corner, window, shown),
            ModalMouse::Took
        );
        assert!(!ui.help_open());
        ui.open_theme_picker();
        assert_eq!(
            modal_press(&mut ui, corner, window, shown),
            ModalMouse::Inert
        );
        let mut ui = boot(true);
        assert_eq!(
            modal_press(&mut ui, corner, window, shown),
            ModalMouse::Inert
        );
    }

    /// Each overlay painted into its own layer is what the same painter
    /// draws over the whole window, inside the layer's rect, and nothing
    /// outside it.
    #[test]
    fn an_overlay_layer_holds_what_the_whole_window_paint_draws() {
        let theme = pixtuoid_scene::theme::theme_by_name("normal").expect("normal theme exists");
        let pack = pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack loads");
        let window = (640u32, 400u32);
        let at = window_geometry(
            PhysicalSize::new(window.0, window.1),
            density(),
            Zoom::default(),
        );
        let cell = Face::chrome(at);
        let tip = pixtuoid_scene::tooltip::coffee();
        for cursor in [(100.0, 4.0), (630.0, 390.0), (0.0, 200.0)] {
            let placed = PlacedTip::new(&tip, cursor, theme, cell, window);
            let rect = placed.rect(cell, window);
            let mut layer = RgbaLayer::new(rect);
            paint_tooltip(&mut layer, &placed, (theme, &pack), cell);
            // The whole-window paint over a known ground: every pixel it
            // changed is inside the rect and set in the layer.
            const GROUND: u32 = 0x0012_3456;
            let (w, h) = (window.0 as usize, window.1 as usize);
            let mut px = vec![GROUND; w * h];
            let mut flat = XrgbSurface::new(&mut px, w, h).expect("sized");
            paint_tooltip(&mut flat, &placed, (theme, &pack), cell);
            let mut changed = 0;
            for (i, &p) in px.iter().enumerate() {
                let (x, y) = ((i % w) as i32, (i / w) as i32);
                let inside = x >= rect.x
                    && y >= rect.y
                    && x < rect.x + rect.w as i32
                    && y < rect.y + rect.h as i32;
                if p != GROUND {
                    changed += 1;
                    assert!(inside, "{cursor:?}: ({x},{y}) painted outside {rect:?}");
                    let [_, _, _, a] = layer.texel((x - rect.x) as u32, (y - rect.y) as u32);
                    assert_ne!(a, 0, "{cursor:?}: ({x},{y}) missing from the layer");
                }
            }
            assert!(changed > 0, "{cursor:?}: the tooltip painted nothing");
        }
    }

    /// A theme change repaints an overlay whose model did not change, and an
    /// unchanged frame repaints nothing.
    #[test]
    fn a_theme_change_repaints_an_unchanged_overlay() {
        let normal = pixtuoid_scene::theme::theme_by_name("normal").expect("normal");
        let other = pixtuoid_scene::theme::ALL_THEMES
            .iter()
            .copied()
            .find(|t| !std::ptr::eq(*t, normal))
            .expect("a second theme");
        let pack = pixtuoid_scene::pack::load_bundled_pack().expect("bundled pack loads");
        let window = (640u32, 400u32);
        let at = window_geometry(
            PhysicalSize::new(window.0, window.1),
            density(),
            Zoom::default(),
        );
        let next = Overlays {
            window,
            footer: FooterModel {
                segments: vec![pixtuoid_scene::footer::FooterSegment {
                    text: "a".into(),
                    tone: pixtuoid_scene::footer::FooterTone::Neutral,
                }],
            },
            tooltip: None,
            panels: None,
        };
        let mut layers = OverlayLayers::default();
        let fresh = |l: &OverlayLayers| {
            l.layers()
                .map(|l| l.change == Change::All)
                .collect::<Vec<_>>()
        };
        layers.update(next.clone(), at, (normal, &pack));
        assert_eq!(fresh(&layers), [true], "the first frame paints the footer");
        layers.update(next.clone(), at, (normal, &pack));
        assert_eq!(fresh(&layers), [false], "nothing changed");
        layers.update(next, at, (other, &pack));
        assert_eq!(fresh(&layers), [true], "the theme changed");
    }

    /// Open panels' layer spans the cells they ink, plus the one-cell spill a
    /// glyph may reach into, and is absent when no cell is inked.
    #[test]
    fn the_panels_layer_spans_the_inked_cells() {
        let cell = CellPx { w: 4, h: 8 };
        let mut grid = CellGrid::new(10, 5);
        assert_eq!(panels_rect(&grid, cell, (40, 40)), None);
        grid.put((3, 1), "x", None, false);
        assert_eq!(
            panels_rect(&grid, cell, (40, 40)),
            Some(PxRect {
                x: 12,
                y: 8,
                w: 8,
                h: 16
            })
        );
    }
}
