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

/// The left indent the notices, the body and the link sit at, and the column
/// the link's click target starts at.
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

/// The popup's rows besides its notices: a leading blank, the body, a trailing
/// blank, and the link.
const FRAME_ROWS: u16 = 4;

/// What a notice's first row starts with, and the indent its wrapped rows hang
/// at, both after [`INDENT`].
const BULLET: &str = "\u{2022} ";
const HANG: &str = "  ";

/// The blank between the notices and the release, there only with a notice.
const NOTICE_GAP: u16 = 1;

/// What the last row of cut notices ends with; the space parts it from a
/// period the row may end on.
const CUT_MARK: &str = " \u{2026}";

/// The notices as painted: wrapped to the settled panel, and cut to the rows
/// its height leaves beside [`FRAME_ROWS`] and [`NOTICE_GAP`], so the link
/// always survives. Laid out at full scale, so the entrance animation
/// clips the rows rather than reflowing them.
fn notice_rows(bounds: Rect, notices: &[String]) -> Vec<String> {
    if notices.is_empty() {
        return Vec::new();
    }
    // The tallest inner the bounds allow: the content rows saturate to it.
    let Some(inner) = version_geometry(bounds, 1.0, usize::MAX).and_then(|g| g.inner()) else {
        return Vec::new();
    };
    let width = usize::from(inner.width).saturating_sub(INDENT.len());
    // Named, not defaulted: textwrap's defaults follow its features, which
    // feature unification can widen.
    let wrap = textwrap::Options::new(width)
        .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit)
        .word_separator(textwrap::WordSeparator::AsciiSpace)
        .initial_indent(BULLET)
        .subsequent_indent(HANG);
    let mut rows: Vec<String> = notices
        .iter()
        .flat_map(|n| textwrap::wrap(n, &wrap))
        .map(std::borrow::Cow::into_owned)
        .collect();
    let budget = usize::from(inner.height.saturating_sub(FRAME_ROWS + NOTICE_GAP));
    if rows.len() > budget {
        rows.truncate(budget);
        if let Some(last) = rows.last_mut() {
            *last = cut(last, width);
        }
    }
    rows
}

/// `row` ending in [`CUT_MARK`] within `width` columns, measured by display
/// width as the wrap is.
fn cut(row: &str, width: usize) -> String {
    let room = width.saturating_sub(textwrap::core::display_width(CUT_MARK));
    let mut used = 0;
    let kept: String = row
        .chars()
        .take_while(|c| {
            used += textwrap::core::display_width(c.encode_utf8(&mut [0; 4]));
            used <= room
        })
        .collect();
    format!("{}{CUT_MARK}", kept.trim_end())
}

/// The content rows with `notice_count` notice rows painted.
fn content_rows(notice_count: usize) -> u16 {
    if notice_count == 0 {
        return FRAME_ROWS;
    }
    let notices = u16::try_from(notice_count).unwrap_or(u16::MAX);
    FRAME_ROWS
        .saturating_add(notices)
        .saturating_add(NOTICE_GAP)
}

/// THE version-popup geometry authority. BOTH `paint_version_popup` and
/// `version_popup_url_rect` ride this with the same `(bounds, scale)` and the
/// same [`notice_rows`], so the painted link and its click target can't drift
/// apart. The URL row is always one (truncated) row, so only the notice rows
/// vary the height.
fn version_geometry(bounds: Rect, scale: f32, notice_count: usize) -> Option<PanelGeometry> {
    // The title TEXT is irrelevant to geometry — only the reserved title row
    // (is_some) matters here; the painter draws the real title into it.
    let geom = PanelGeometry::compute(
        bounds,
        VERSION_POPUP_W,
        content_rows(notice_count),
        Some(""),
        scale,
    );
    geom.inner()?;
    Some(geom)
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

/// The popup for `version`, with `popup`'s notices (each one paragraph) above
/// its release.
pub(crate) fn paint_version_popup(
    f: &mut ratatui::Frame<'_>,
    version: &str,
    popup: &crate::panels::ui_state::VersionPopupFrame,
    bounds: Rect,
    theme: &pixtuoid_scene::theme::Theme,
    scale: f32,
) {
    let scale = scale.clamp(0.0, 1.0);
    let rows = notice_rows(bounds, &popup.notices);
    let Some(geom) = version_geometry(bounds, scale, rows.len()) else {
        return;
    };
    let outer = geom
        .outer()
        .expect("version_geometry guarantees a rendered geom");
    let inner_w = geom
        .inner()
        .expect("version_geometry guarantees a rendered geom")
        .width;

    let notice = Style::default().fg(to_color(theme.ui.tooltip_text));
    let mut items = vec![Line::from("")];
    if !rows.is_empty() {
        items.extend(
            rows.into_iter()
                .map(|row| Line::from(Span::styled(format!("{INDENT}{row}"), notice))),
        );
        items.push(Line::from(""));
    }
    items.extend([
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
    ]);

    let title = version_title(version, popup.upgraded, inner_w as usize);
    // `borderless_panel(outer)` returns the SAME rect `geom.inner()` and
    // `cell_rect` derive from, so paint and click agree.
    let inner = borderless_panel(f, outer, Some(&title), theme);
    f.render_widget(Paragraph::new(items), inner);
}

/// The screen rect of the clickable link, or `None` when it isn't
/// rendered/clickable. Derived from the SAME `version_geometry` and
/// `notice_rows` the painter uses, so a click can never land where the link
/// isn't painted.
pub(crate) fn version_popup_url_rect(bounds: Rect, notices: &[String], scale: f32) -> Option<Rect> {
    let scale = scale.clamp(0.0, 1.0);
    if scale < LINK_CLICKABLE_SCALE {
        return None;
    }
    let rows = notice_rows(bounds, notices).len();
    version_geometry(bounds, scale, rows)?.cell_rect(
        content_rows(rows) - 1,
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
        version_geometry(bounds, 1.0, 0)
            .and_then(|g| g.inner())
            .expect("renders")
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

    #[test]
    fn link_click_rect_is_the_painted_link_cell() {
        let geom = version_geometry(wide(), 1.0, 0).expect("renders");
        let inner = geom.inner().expect("rendered ⇒ inner Some");
        let expected = Rect::new(
            inner.x + INDENT.len() as u16,
            inner.y + FRAME_ROWS - 1,
            LINK_LABEL.chars().count() as u16,
            1,
        );
        assert_eq!(version_popup_url_rect(wide(), &[], 1.0), Some(expected));
    }

    #[test]
    fn link_fits_where_the_old_url_clipped() {
        // 50 cols is where the old ~46-char raw URL hard-clipped.
        let rect = version_popup_url_rect(Rect::new(0, 0, 50, 30), &[], 1.0).expect("fits");
        assert_eq!(rect.width, LINK_LABEL.chars().count() as u16);
    }

    /// A panel narrower than the measure: the body must fit the CLAMPED inner
    /// width, and the link — the CTA, not filler — must survive the height
    /// clamp. Below the office's own floor on purpose, where the geometry does
    /// the most clamping.
    #[test]
    fn the_popup_renders_whole_on_a_narrow_terminal() {
        let bounds = Rect::new(0, 0, 32, 31);
        let geom = version_geometry(bounds, 1.0, 0).expect("renders");
        let inner = geom.inner().expect("rendered ⇒ inner Some");
        let row = body_row("1.2.3", inner.width as usize);
        assert!(
            row.chars().count() + INDENT.len() <= inner.width as usize,
            "a painted row wider than {} cols clips silently: {row:?}",
            inner.width
        );
        assert!(FRAME_ROWS <= inner.height);
        assert!(version_popup_url_rect(bounds, &[], 1.0).is_some());
    }

    #[test]
    fn url_rect_none_below_clickable_scale_and_tiny_bounds() {
        assert!(version_popup_url_rect(wide(), &[], 0.5).is_none());
        assert!(version_popup_url_rect(wide(), &[], 0.0).is_none());
        assert!(version_popup_url_rect(Rect::new(0, 0, 3, 60), &[], 1.0).is_none());
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

    fn notices() -> Vec<String> {
        vec![
            "pixtuoid now connects to Claude Code, Codex and Reasonix as a plugin: \
             reconnect the ones you use in Sources (s)."
                .to_string(),
            "This terminal can draw the cutaway office: try \
             `pixtuoid run --graphics auto`, or set graphics = \"auto\" in the config."
                .to_string(),
        ]
    }

    /// The popup painted whole, one string per screen row.
    fn painted(bounds: Rect, notices: &[String]) -> Vec<String> {
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
                    notices: notices.into(),
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

    /// Each notice wraps inside the panel with a bullet and a hanging indent,
    /// losing no word.
    #[test]
    fn a_notice_wraps_inside_the_panel_and_keeps_every_word() {
        let inner_w = inner_width(wide());
        let rows = notice_rows(wide(), &notices());
        assert!(rows.len() > notices().len(), "the notices wrap: {rows:?}");
        for row in &rows {
            assert!(
                textwrap::core::display_width(row) + INDENT.len() <= inner_w,
                "{row:?} overruns {inner_w} cols"
            );
            assert!(row.starts_with(BULLET) || row.starts_with(HANG), "{row:?}");
        }
        assert_eq!(
            rows.iter().filter(|r| r.starts_with(BULLET)).count(),
            notices().len()
        );
        let words = |s: &str| {
            s.split_whitespace()
                .filter(|w| *w != "\u{2022}")
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(words(&rows.join(" ")), words(&notices().join(" ")));
    }

    /// The release build's wrap, first-fit at ASCII spaces, in every build:
    /// optimal fit would move `bb` down, and Unicode breaking would split the
    /// ideographs to fill the row after `x`.
    #[test]
    fn a_notice_wraps_first_fit_at_spaces_in_any_build() {
        let room = inner_width(wide()) - INDENT.len() - textwrap::core::display_width(BULLET);
        let long = "a".repeat(room - 3);
        let ideographs = "字".repeat(room / 2);
        let notices = [format!("{long} bb cc {long}zz"), format!("x {ideographs}")];
        assert_eq!(
            notice_rows(wide(), &notices),
            [
                format!("{BULLET}{long} bb"),
                format!("{HANG}cc"),
                format!("{HANG}{long}zz"),
                format!("{BULLET}x"),
                format!("{HANG}{ideographs}"),
            ]
        );
    }

    /// The notices sit above the release, and the link's click rect is the
    /// cell it is painted on, however many rows they took.
    #[test]
    fn the_link_is_clicked_where_it_paints_below_the_notices() {
        let bounds = Rect::new(0, 0, 80, 30);
        let screen = painted(bounds, &notices());
        let row_of = |needle: &str| {
            screen
                .iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} is painted: {screen:#?}"))
        };
        let link = row_of(LINK_LABEL);
        assert!(row_of("pixtuoid now connects to") < row_of("releases/tag/v1.2.3"));
        let rect = version_popup_url_rect(bounds, &notices(), 1.0).expect("clickable");
        assert_eq!(usize::from(rect.y), link);
        let col = screen[link]
            .chars()
            .position(|c| c == '\u{2197}')
            .expect("the label");
        assert_eq!(usize::from(rect.x), col);
    }

    /// A terminal too short for every notice cuts them with `…` and keeps the
    /// link, the popup's call to action.
    #[test]
    fn a_short_terminal_cuts_the_notices_and_keeps_the_link() {
        let bounds = Rect::new(0, 0, 60, 12);
        let rows = notice_rows(bounds, &notices());
        let whole = notice_rows(wide(), &notices()).len();
        assert!(!rows.is_empty() && rows.len() < whole, "{rows:?}");
        assert!(
            rows.last().is_some_and(|r| r.ends_with(CUT_MARK)),
            "{rows:?}"
        );
        let rect = version_popup_url_rect(bounds, &notices(), 1.0).expect("the link survives");
        assert!(painted(bounds, &notices())[usize::from(rect.y)].contains(LINK_LABEL));
    }

    /// Both sides of the budget, found through the geometry `notice_rows`
    /// reads: at the shortest panel that holds every row they come back whole
    /// and unmarked, and one row shorter they lose a row and gain the mark.
    #[test]
    fn notices_are_cut_exactly_past_the_rows_the_panel_holds() {
        let whole = notice_rows(wide(), &notices());
        assert!(!whole.iter().any(|r| r.ends_with(CUT_MARK)), "{whole:?}");
        let at = |h: u16| notice_rows(Rect::new(0, 0, 80, h), &notices());
        let fits = (1..60)
            .find(|&h| at(h) == whole)
            .expect("some height holds them");
        let cut = at(fits - 1);
        assert_eq!(cut.len(), whole.len() - 1, "{cut:?}");
        assert!(cut.last().is_some_and(|r| r.ends_with(CUT_MARK)), "{cut:?}");
    }

    /// The cut row is measured as the wrap is, by display width, so a row of
    /// wide glyphs keeps its mark inside the panel instead of losing it to the
    /// clip.
    #[test]
    fn a_cut_row_of_wide_glyphs_keeps_its_mark_inside_the_panel() {
        let wide_glyphs = vec!["升级后请在来源面板中重新连接你使用的每一个代理工具".repeat(4)];
        for (w, h) in [(20, 12), (33, 12), (60, 10)] {
            let bounds = Rect::new(0, 0, w, h);
            let rows = notice_rows(bounds, &wide_glyphs);
            let last = rows.last().expect("some rows fit");
            assert!(last.ends_with(CUT_MARK), "{rows:?}");
            let width = inner_width(bounds) - INDENT.len();
            assert!(
                textwrap::core::display_width(last) <= width,
                "{last:?} overruns {width} cols at {w}x{h}"
            );
        }
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
