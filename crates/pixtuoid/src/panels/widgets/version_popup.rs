use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{PanelGeometry, borderless_panel, to_color, truncate};

use pixtuoid_scene::hit::REPO_URL;

/// The scheme this popup's body row drops. A terminal that cannot click still
/// has to be able to COPY the destination, and the scheme is the one part of it
/// every browser infers.
const URL_SCHEME: &str = "https://";

/// The GitHub release for `version` — the only home of the changelog, since the
/// binary ships no notes of its own. `release.yml` publishes it from the
/// `v<version>` tag, so the two spellings meet on that tag name. A pre-release
/// tester build is the exception: `release.yml` strips the `-rc.N` suffix
/// before matching the manifest, so its `CARGO_PKG_VERSION` names a release
/// that exists only once the final tag lands.
pub(crate) fn release_url(version: &str) -> String {
    format!("{REPO_URL}/releases/tag/v{version}")
}

/// The body row: the destination itself, so the popup's one payload survives a
/// terminal with no mouse reporting (tmux's default, most SSH) where the link
/// below cannot be clicked. Truncated rather than wrapped — a `Paragraph` clips
/// a too-wide line silently, and `truncate`'s `…` says the row was cut.
fn body_row(version: &str, inner_w: usize) -> String {
    let url = release_url(version);
    truncate(
        url.strip_prefix(URL_SCHEME).unwrap_or(&url),
        inner_w.saturating_sub(INDENT.len()),
    )
}

/// The left indent the body and the link sit at, and the column the link's
/// click target starts at.
const INDENT: &str = "  ";

/// The clickable link's VISIBLE text — a compact label decouples display width
/// from the link, so it fits any usable terminal and every entrance-animation
/// frame where the raw URL hard-clipped.
const LINK_LABEL: &str = "\u{2197} Release notes";

/// Target content width. Sized so the indented body row holds the whole release
/// URL unclipped at this measure — `the_release_url_fits_the_measure_unclipped`
/// is what fails if either outgrows the other.
const VERSION_POPUP_W: u16 = 56;

/// The link is not clickable until the entrance animation is ≥70% scaled in —
/// below that the painted cell is smaller than the settled label.
const LINK_CLICKABLE_SCALE: f32 = 0.7;

/// The popup's rows: a leading blank, the body, a trailing blank, and the link.
const FRAME_ROWS: u16 = 4;

/// THE version-popup geometry authority. BOTH `paint_version_popup` and
/// `version_popup_url_rect` ride this with the same `(bounds, scale)`, so the
/// painted link and its click target can't drift apart.
fn version_geometry(bounds: Rect, scale: f32) -> Option<PanelGeometry> {
    // The title TEXT is irrelevant to geometry — only the reserved title row
    // (is_some) matters here; the painter draws the real title into it.
    PanelGeometry::compute(bounds, VERSION_POPUP_W, FRAME_ROWS, Some(""), scale)
}

/// The popup's title, sized to the panel's real inner width. The dismiss hint
/// outranks the prose: the title is the ONLY place the modal says how to close
/// itself, so a narrow panel drops the leading name ("Updated to" or
/// "pixtuoid") first rather than cutting mid-word and leaving a key-swallowing
/// overlay with no exit instruction.
fn version_title(version: &str, upgraded: bool, inner_w: usize) -> String {
    let named = if upgraded { "Updated to" } else { "pixtuoid" };
    let full = format!("{named} v{version} \u{2014} Enter to close");
    if full.chars().count() <= inner_w {
        return full;
    }
    truncate(&format!("v{version} \u{2014} Enter to close"), inner_w)
}

/// The popup for `version`.
pub(crate) fn paint_version_popup(
    f: &mut ratatui::Frame<'_>,
    version: &str,
    popup: &crate::panels::ui_state::VersionPopupFrame,
    bounds: Rect,
    theme: &pixtuoid_scene::theme::Theme,
    scale: f32,
) {
    let scale = scale.clamp(0.0, 1.0);
    let Some(geom) = version_geometry(bounds, scale) else {
        return;
    };
    let inner_w = geom.inner().width;

    let items = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("{INDENT}{}", body_row(version, inner_w as usize)),
            Style::default().fg(to_color(theme.ui.label_idle)),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw(INDENT),
            Span::styled(
                LINK_LABEL,
                Style::default()
                    .fg(to_color(theme.ui.neon_brand))
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]),
    ];

    let title = version_title(version, popup.upgraded, inner_w as usize);
    // `borderless_panel(outer)` returns the SAME rect `geom.inner()` and `cell_rect`
    // derive from, so paint and click agree.
    let painted = borderless_panel(f, geom.outer(), Some(&title), theme);
    f.render_widget(Paragraph::new(items), painted);
}

/// The screen rect of the clickable link, or `None` when it isn't
/// rendered/clickable. Derived from the SAME `version_geometry` the painter
/// uses, so a click can never land where the link isn't painted.
pub(crate) fn version_popup_url_rect(bounds: Rect, scale: f32) -> Option<Rect> {
    let scale = scale.clamp(0.0, 1.0);
    if scale < LINK_CLICKABLE_SCALE {
        return None;
    }
    version_geometry(bounds, scale)?.cell_rect(
        FRAME_ROWS - 1,
        INDENT.len() as u16,
        LINK_LABEL.chars().count() as u16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide() -> Rect {
        Rect::new(0, 0, 200, 60)
    }

    /// The width the PAINTER lays out against, so a test never measures against
    /// a second derivation of it.
    fn inner_width(bounds: Rect) -> usize {
        version_geometry(bounds, 1.0)
            .expect("renders")
            .inner()
            .width as usize
    }

    #[test]
    fn release_url_is_the_tag_release_page() {
        assert_eq!(
            release_url("1.2.3"),
            format!("{REPO_URL}/releases/tag/v1.2.3")
        );
    }

    /// The body row is the only copyable destination a mouse-less terminal gets,
    /// so it must reach the panel WHOLE at the popup's own measure. Fails from
    /// either side: a longer repo path, or a narrower `VERSION_POPUP_W`.
    #[test]
    fn the_release_url_fits_the_measure_unclipped() {
        let inner_w = inner_width(wide());
        let row = body_row("0.100.100", inner_w);
        assert!(
            !row.contains('\u{2026}'),
            "the release URL is clipped at the popup's own measure ({inner_w} cols): {row:?}"
        );
        assert!(
            row.chars().count() + INDENT.len() <= inner_w,
            "the painted row must fit {inner_w} cols: {row:?}"
        );
    }

    #[test]
    fn the_body_row_drops_only_the_scheme() {
        let host_path = REPO_URL
            .strip_prefix("https://")
            .expect("an https repository");
        assert_eq!(
            body_row("1.2.3", 80),
            format!("{host_path}/releases/tag/v1.2.3")
        );
    }

    /// A row too narrow for the URL says so with `…` rather than handing the
    /// reader a silently shortened address.
    #[test]
    fn a_too_narrow_row_marks_the_cut() {
        let row = body_row("1.2.3", 20);
        assert!(row.ends_with('\u{2026}'), "{row:?}");
        assert_eq!(row.chars().count() + INDENT.len(), 20);
    }

    /// The popup painted whole, one string per screen row.
    fn painted(bounds: Rect) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(bounds.width, bounds.height)).unwrap();
        term.draw(|f| {
            paint_version_popup(
                f,
                "1.2.3",
                &crate::panels::ui_state::VersionPopupFrame {
                    open: true,
                    upgraded: true,
                },
                bounds,
                &pixtuoid_scene::theme::NORMAL,
                1.0,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..bounds.height)
            .map(|y| (0..bounds.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn link_click_rect_is_the_painted_link_cell() {
        let bounds = Rect::new(0, 0, 80, 30);
        let screen = painted(bounds);
        let row = screen
            .iter()
            .position(|r| r.contains(LINK_LABEL))
            .unwrap_or_else(|| panic!("the link is painted: {screen:#?}"));
        let col = screen[row]
            .chars()
            .position(|c| c == '\u{2197}')
            .expect("the label");
        let rect = version_popup_url_rect(bounds, 1.0).expect("clickable");
        assert_eq!((usize::from(rect.y), usize::from(rect.x)), (row, col));
        assert_eq!(usize::from(rect.width), LINK_LABEL.chars().count());
    }

    /// A panel narrower than the measure: the body must fit the CLAMPED inner
    /// width, and the link — the CTA, not filler — must survive the height
    /// clamp. Below the office's own floor on purpose, where the geometry does
    /// the most clamping.
    #[test]
    fn the_popup_renders_whole_on_a_narrow_terminal() {
        let bounds = Rect::new(0, 0, 32, 31);
        let inner = version_geometry(bounds, 1.0).expect("renders").inner();
        let row = body_row("1.2.3", inner.width as usize);
        assert!(
            row.chars().count() + INDENT.len() <= inner.width as usize,
            "a painted row wider than {} cols clips silently: {row:?}",
            inner.width
        );
        assert!(FRAME_ROWS <= inner.height);
        assert!(version_popup_url_rect(bounds, 1.0).is_some());
    }

    #[test]
    fn url_rect_none_below_clickable_scale_and_tiny_bounds() {
        assert!(version_popup_url_rect(wide(), 0.5).is_none());
        assert!(version_popup_url_rect(wide(), 0.0).is_none());
        assert!(version_popup_url_rect(Rect::new(0, 0, 3, 60), 1.0).is_none());
    }

    #[test]
    fn a_narrow_title_keeps_the_dismiss_hint() {
        let inner_w = inner_width(Rect::new(0, 0, 32, 31));
        for upgraded in [true, false] {
            let title = version_title("0.16.0", upgraded, inner_w);
            assert!(
                title.chars().count() <= inner_w,
                "the title must fit its row: {title:?} in {inner_w}"
            );
            assert!(
                title.contains("Enter to close"),
                "the dismiss hint outranks the prose: {title:?}"
            );
        }
        let wide_title = version_title("0.16.0", true, 60);
        assert_eq!(wide_title, "Updated to v0.16.0 \u{2014} Enter to close");
    }

    /// Reopened on a run no upgrade booted, the popup claims no update.
    #[test]
    fn a_popup_no_upgrade_opened_names_the_version_alone() {
        assert_eq!(
            version_title("0.16.0", false, 60),
            "pixtuoid v0.16.0 \u{2014} Enter to close"
        );
    }

    #[test]
    fn version_popup_skips_render_when_fully_dismissed() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            paint_version_popup(
                f,
                "1.2.3",
                &crate::panels::ui_state::VersionPopupFrame::default(),
                Rect::new(0, 0, 80, 30),
                &pixtuoid_scene::theme::NORMAL,
                0.0,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert!(
            !buf.content().iter().any(|c| !c.symbol().trim().is_empty()),
            "dismissed popup must paint nothing"
        );
    }
}
