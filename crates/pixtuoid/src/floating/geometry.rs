//! Pure window/monitor geometry for the floating desktop window — the pieces of
//! `window.rs`'s `winit` handler that don't need a live `ActiveEventLoop` /
//! cursor, so they can be unit-tested.

use std::sync::atomic::{AtomicUsize, Ordering};

use pixtuoid_core::sprite::format::Density;
use pixtuoid_core::state::MAX_FLOORS;
use pixtuoid_scene::cutaway::chrome_cell;
use pixtuoid_scene::layout::Size;
use pixtuoid_scene::render_scale::PixelFit;
use winit::dpi::{LogicalSize, PhysicalSize};

/// Does the saved window rect `(x, y, w, h)` overlap ANY currently-connected monitor?
///
/// Guards against restoring onto a now-disconnected monitor: frameless +
/// always-on-top + no taskbar means a fully off-screen window can never be dragged
/// back. `w`/`h` are the saved LOGICAL dims, used here only as an approximate
/// extent — a few px of HiDPI slop is irrelevant for an on/off-screen test. An
/// edge-touching window with a zero-area intersection does NOT count, and an EMPTY
/// monitor iterator returns `true` so we honor the saved position rather than
/// second-guessing the OS.
pub(crate) fn window_visible_on_monitors(
    win: (i32, i32, u32, u32),
    monitors: impl IntoIterator<Item = (i32, i32, u32, u32)>,
) -> bool {
    let (wx, wy, ww, wh) = win;
    let (win_l, win_t) = (i64::from(wx), i64::from(wy));
    let (win_r, win_b) = (win_l + i64::from(ww), win_t + i64::from(wh));
    let mut any_monitor = false;
    for (mx, my, mw, mh) in monitors {
        any_monitor = true;
        let (mon_l, mon_t) = (i64::from(mx), i64::from(my));
        let (mon_r, mon_b) = (mon_l + i64::from(mw), mon_t + i64::from(mh));
        if win_l < mon_r && win_r > mon_l && win_t < mon_b && win_b > mon_t {
            return true;
        }
    }
    !any_monitor
}

/// Is the cursor `(cx, cy)` within `corner_px` of the bottom-right corner of a `(w, h)`
/// window? A left-press there resizes the frameless window (SouthEast); elsewhere it drags.
pub(crate) fn near_resize_corner(cursor: (f64, f64), size: (u32, u32), corner_px: f64) -> bool {
    let (cx, cy) = cursor;
    let (w, h) = size;
    cx >= f64::from(w) - corner_px && cy >= f64::from(h) - corner_px
}

/// The window's natural real pixels per logical unit: what keeps the office
/// near `OFFICE_TARGET_H` units tall, so its art stays chunky and legible.
/// Min 1.
pub(crate) fn office_scale(win_h: u32) -> u32 {
    const OFFICE_TARGET_H: u32 = 180;
    (f64::from(win_h) / f64::from(OFFICE_TARGET_H))
        .round()
        .max(1.0) as u32
}

/// How far the window is zoomed from its automatic scale, in steps of the
/// pack's density: the `[floating]` `zoom` the window's keys move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Zoom(i8);

/// What a zoom key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoomKey {
    In,
    Out,
    Reset,
}

impl Zoom {
    pub fn new(steps: i8) -> Self {
        Self(steps)
    }

    pub(crate) fn steps(self) -> i8 {
        self.0
    }

    /// The zoom `key` leaves a `size` window at: one step from the zoom in
    /// effect, so a step back from past the largest that lays out always
    /// shows.
    #[must_use]
    pub(crate) fn stepped(self, key: ZoomKey, size: PhysicalSize<u32>, density: Density) -> Self {
        let step = match key {
            ZoomKey::In => 1,
            ZoomKey::Out => -1,
            ZoomKey::Reset => return Self::default(),
        };
        Self(self.in_effect(size, density).0.saturating_add(step)).in_effect(size, density)
    }

    /// The zoom a `size` window shows for this one: [`zoom_scale`]'s clamps
    /// read back as steps.
    fn in_effect(self, size: PhysicalSize<u32>, density: Density) -> Self {
        let d = i32::from(density.get());
        let moved =
            i32::from(zoom_scale(size, density, self)) - i32::from(auto_scale(size, density));
        Self(i8::try_from(moved / d).expect("a zoom reads back within its own i8 steps"))
    }
}

/// `size`'s two axes in the layout's pixels, a too-wide one held at the most
/// it counts.
fn px_size(size: PhysicalSize<u32>) -> Size {
    let px = |p: u32| u16::try_from(p).unwrap_or(u16::MAX);
    Size {
        w: px(size.width),
        h: px(size.height),
    }
}

/// The office at `scale` real px per unit over a `px` window, above its
/// `footer_band`.
fn fit_at(scale: u16, density: Density, px: Size) -> PixelFit {
    let fit = PixelFit::at_least_density(scale, density, px);
    // The band's cell is the fit's, and `over` keeps the fit's scale, so the
    // band measured here is the band painted.
    fit.over(Size {
        w: px.w,
        h: px.h.saturating_sub(footer_band(fit)),
    })
}

/// Whether the office at `scale` over a `size` window lays out.
fn lays_out(scale: u16, density: Density, size: PhysicalSize<u32>) -> bool {
    let min = pixtuoid_scene::layout::min_layout_size();
    let office = fit_at(scale, density, px_size(size)).logical();
    office.w >= min.w && office.h >= min.h
}

/// `office_scale` fitted to the pack's `density` and never below it, so the
/// window never falls back to the classic, then stepped down to the largest
/// scale whose office lays out: `office_scale` reads the height alone, so a
/// narrow window's would not fit across.
fn auto_scale(size: PhysicalSize<u32>, density: Density) -> u16 {
    let natural = u16::try_from(office_scale(size.height)).unwrap_or(u16::MAX);
    let natural = PixelFit::at_least_density(natural, density, px_size(size))
        .scale()
        .get();
    let d = density.get();
    std::iter::successors(Some(natural), |&s| s.checked_sub(d).filter(|&s| s >= d))
        .find(|&s| lays_out(s, density, size))
        .unwrap_or(natural)
}

/// The scale `zoom` draws a `size` window at: the automatic scale moved that
/// many density steps, never below the density, and never so far in that the
/// office stops laying out, unless the automatic scale already doesn't.
fn zoom_scale(size: PhysicalSize<u32>, density: Density, zoom: Zoom) -> u16 {
    let d = density.get();
    let auto = auto_scale(size, density);
    let want = (i32::from(auto) + i32::from(zoom.0) * i32::from(d)).max(i32::from(d));
    let want = u16::try_from(want).unwrap_or(u16::MAX);
    if want <= auto {
        return want;
    }
    std::iter::successors(Some(want), |&s| s.checked_sub(d).filter(|&s| s > auto))
        .find(|&s| lays_out(s, density, size))
        .unwrap_or(auto)
}

/// How a PHYSICAL-px window draws its office at `zoom`: the cutaway at the
/// pack's `density`, at [`Zoom`]'s scale, over the window above its
/// `footer_band`. The ONE place this geometry lives, so the desk capacity
/// derived from it can't drift from the office drawn.
///
/// Takes winit's `PhysicalSize` rather than two bare `u32`s so the UNIT is carried by
/// the type: the `[floating]` config size is LOGICAL, and handing it here is a compile
/// error instead of a silent HiDPI mis-seed (#803).
pub fn window_geometry(size: PhysicalSize<u32>, density: Density, zoom: Zoom) -> PixelFit {
    fit_at(zoom_scale(size, density, zoom), density, px_size(size))
}

/// The footer's own row at the window's foot, in physical px: one screen
/// cell of `fit`'s chrome and a margin over and under it, as the terminal
/// gives its footer a row of its own.
pub(crate) fn footer_band(fit: PixelFit) -> u16 {
    chrome_cell(fit).h + 2 * FOOTER_MARGIN_PX
}

/// The smallest window, in logical px, whose office lays out:
/// [`min_layout_size`](pixtuoid_scene::layout::min_layout_size) at the pack's
/// `density`, which [`window_geometry`] never draws below, and its
/// `footer_band`, on a display that gives a logical px one physical px.
pub(crate) fn min_window(density: Density) -> LogicalSize<u32> {
    let min = pixtuoid_scene::layout::min_layout_size();
    let px = |units: u16| units.saturating_mul(density.get());
    let office = Size {
        w: px(min.w),
        h: px(min.h),
    };
    let band = footer_band(PixelFit::at_least_density(density.get(), density, office));
    LogicalSize::new(u32::from(office.w), u32::from(office.h) + u32::from(band))
}

/// Per-floor desk capacities for an office buffer of `buf_w`×`buf_h`. THE one
/// derivation: the boot seed and every redraw's [`sync_floor_caps`] both call it, so
/// their agreement is structural rather than two loops that happen to agree.
pub(crate) fn floor_caps_for_buffer(buf_w: u16, buf_h: u16) -> [usize; MAX_FLOORS] {
    std::array::from_fn(|i| {
        pixtuoid_scene::floor::floor_capacity(buf_w, buf_h, pixtuoid_scene::floor::floor_seed(i))
    })
}

/// Per-floor boot desk-capacities for the FLOATING window, from the REAL
/// `window.inner_size()`. Do NOT reuse the TUI's `runtime::boot_capacities_for` — it
/// subtracts a footer row AND ignores the window upscale, so it OVER-seeds: in the
/// sub-frame boot race before the first redraw, a `SessionStart` could land at a
/// `desk_index` the smaller real layout lacks (invisible-but-alive until a resize).
///
/// There is deliberately NO `cap == 0 → FALLBACK_DESKS` clause: `sync_floor_caps`
/// `store`s the honest 0 for a window too small to lay out, and a fallback points the
/// WRONG way, admitting `FALLBACK_DESKS` agents onto desks that do not exist.
pub(crate) fn boot_capacities_for_window(
    size: PhysicalSize<u32>,
    density: Density,
    zoom: Zoom,
) -> [usize; MAX_FLOORS] {
    let office = window_geometry(size, density, zoom).logical();
    floor_caps_for_buffer(office.w, office.h)
}

/// Publish [`floor_caps_for_buffer`]'s answer into the reducer's per-floor capacity
/// atomics, keeping admission in lockstep with the office actually rendered at
/// `buf_w`×`buf_h`. Returns whether it recomputed — `false` means `last` already held
/// this buffer size and the publish was skipped.
///
/// `store`, NOT the TUI's monotone `fetch_max`: the floating window's pixel size is
/// exact and authoritative on every redraw, so a shrink genuinely LOWERS capacity and
/// the reducer must stop admitting agents onto desks that no longer exist. Don't
/// "harmonize" the two — the direction is deliberate, and pinned by
/// `a_shrink_lowers_the_published_capacity_it_is_store_not_fetch_max`.
///
/// The resize DETECTION rides along with the publish because `floor_capacity` runs a
/// full layout compute per floor, so this must not run per frame. Both live here rather
/// than at the `window::redraw` call site because `window.rs` is excluded from BOTH
/// codecov and cargo-mutants, so a guard there is measured by nothing.
pub(crate) fn sync_floor_caps(
    last: &mut Option<(u16, u16)>,
    floor_caps: &[AtomicUsize; MAX_FLOORS],
    buf_w: u16,
    buf_h: u16,
) -> bool {
    if *last == Some((buf_w, buf_h)) {
        return false;
    }
    *last = Some((buf_w, buf_h));
    for (cap, capacity) in floor_caps.iter().zip(floor_caps_for_buffer(buf_w, buf_h)) {
        cap.store(capacity, Ordering::Relaxed);
    }
    true
}

/// Breathing room around the footer's text in its band — the band's height,
/// its paint and the [`footer_budget`](super::overlays::footer_budget) column math all read it, so they can't
/// drift.
pub(super) const FOOTER_MARGIN_PX: u16 = 6;

#[cfg(test)]
mod tests {
    use super::super::fixtures::density;
    use super::super::offscreen::unit_in;
    use super::*;

    const HD: (i32, i32, u32, u32) = (0, 0, 1920, 1080);

    #[test]
    fn overlapping_window_is_visible() {
        assert!(window_visible_on_monitors((100, 100, 800, 600), [HD]));
    }

    #[test]
    fn fully_offscreen_after_a_monitor_disconnect_is_not_visible() {
        assert!(!window_visible_on_monitors((3000, 0, 800, 600), [HD]));
    }

    #[test]
    fn partial_overlap_counts_as_visible() {
        assert!(window_visible_on_monitors((1800, 100, 400, 300), [HD]));
    }

    #[test]
    fn edge_touching_is_not_overlap() {
        assert!(!window_visible_on_monitors((1920, 0, 100, 100), [HD]));
    }

    #[test]
    fn lands_on_a_negative_origin_second_monitor() {
        assert!(window_visible_on_monitors(
            (-1500, 100, 400, 300),
            [HD, (-1920, 0, 1920, 1080)],
        ));
    }

    #[test]
    fn empty_monitor_list_honors_the_saved_position() {
        let none: [(i32, i32, u32, u32); 0] = [];
        assert!(window_visible_on_monitors((100, 100, 800, 600), none));
    }

    #[test]
    fn near_resize_corner_only_in_the_bottom_right() {
        let size = (800, 600);
        assert!(near_resize_corner((795.0, 595.0), size, 18.0));
        assert!(!near_resize_corner((400.0, 300.0), size, 18.0));
        assert!(!near_resize_corner((795.0, 100.0), size, 18.0));
        assert!(!near_resize_corner((100.0, 595.0), size, 18.0));
    }

    #[test]
    fn office_scale_keeps_the_office_chunky_and_never_zero() {
        assert_eq!(office_scale(180), 1);
        assert_eq!(office_scale(360), 2);
        assert_eq!(office_scale(720), 4);
        // Never 0 — redraw divides by it.
        assert_eq!(office_scale(90), 1);
        assert_eq!(office_scale(0), 1);
    }

    /// The footer's band holds no office: the fit stops above it, a pointer
    /// in it finds no unit, and the capacity is the office's.
    #[test]
    fn the_footer_band_is_below_the_office_not_over_it() {
        for (w, h) in [(480u32, 320u32), (960, 640), (1280, 720)] {
            let at = window_geometry(PhysicalSize::new(w, h), density(), Zoom::default());
            let band = u32::from(footer_band(at));
            let office_px = u32::from(at.logical().h) * u32::from(at.scale().get());
            assert!(office_px <= h - band, "{w}x{h}: office reaches the band");
            let in_band = (f64::from(w) / 2.0, f64::from(h - band / 2));
            assert_eq!(unit_in(in_band, at), None, "{w}x{h}: the band hits a unit");
            let above = (f64::from(w) / 2.0, f64::from(office_px - 1));
            assert!(
                unit_in(above, at).is_some(),
                "{w}x{h}: the office's last row"
            );
            let office = at.logical();
            assert_eq!(
                boot_capacities_for_window(PhysicalSize::new(w, h), density(), Zoom::default()),
                floor_caps_for_buffer(office.w, office.h),
            );
        }
    }

    /// A saved size below the pack's minimum opens, and is placed, at the
    /// minimum: an office that seats every floor.
    #[test]
    fn a_saved_size_below_the_minimum_opens_where_every_floor_seats() {
        let min = min_window(density());
        let cfg: crate::config::AppConfig =
            toml::from_str("[floating]\nwidth = 1\nheight = 1\n").expect("parses");
        let f = crate::config::resolve_floating(&cfg).at_least(min.width, min.height);
        assert_eq!((f.width, f.height), (min.width, min.height));
        let caps = boot_capacities_for_window(
            PhysicalSize::new(f.width, f.height),
            density(),
            Zoom::default(),
        );
        assert!(caps.iter().all(|&c| c > 0), "{caps:?}");
    }

    #[test]
    fn the_smallest_window_seats_every_floor_at_any_scale_factor() {
        let min = min_window(density());
        for factor in 1..=3 {
            let size = PhysicalSize::new(min.width * factor, min.height * factor);
            let caps = boot_capacities_for_window(size, density(), Zoom::default());
            assert!(caps.iter().all(|&c| c > 0), "{factor}x: {caps:?}");
        }
    }

    #[test]
    fn boot_capacities_for_window_match_the_first_redraw_geometry_not_the_tui_overseed() {
        let (w, h) = (1280u32, 720u32);
        let office = window_geometry(PhysicalSize::new(w, h), density(), Zoom::default()).logical();
        let boot = boot_capacities_for_window(PhysicalSize::new(w, h), density(), Zoom::default());
        for (i, &got) in boot.iter().enumerate() {
            let want = pixtuoid_scene::floor::floor_capacity(
                office.w,
                office.h,
                pixtuoid_scene::floor::floor_seed(i),
            );
            assert_eq!(
                got, want,
                "floor {i} boot cap must match the rendered geometry"
            );
        }
        let overseed = crate::runtime::boot_capacities_for(
            crate::graphics::Plan::Classic {
                reason: crate::graphics::ClassicReason::Disabled,
            },
            ratatui::layout::Size::new(w as u16, (h / 2) as u16),
        );
        assert!(
            overseed[0] >= boot[0],
            "TUI helper over-seeds ({} vs {})",
            overseed[0],
            boot[0]
        );
    }

    /// Crosses the logical/physical boundary using winit's own conversion, at scale
    /// factors where the two disagree — the test above feeds the SAME numbers to both
    /// sides, so it can never see a UNITS mismatch (#803).
    #[test]
    fn the_boot_seed_tracks_the_physical_window_not_the_logical_config() {
        let logical = LogicalSize::new(
            f64::from(crate::config::FLOATING_DEFAULT_W),
            f64::from(crate::config::FLOATING_DEFAULT_H),
        );
        // The logical size read as physical — the defect.
        let as_if_physical = boot_capacities_for_window(
            PhysicalSize::new(logical.width as u32, logical.height as u32),
            density(),
            Zoom::default(),
        );

        // MEASURED offices for the default 480×320 logical window, above its
        // footer band. `office_scale` ROUNDS before the density fit, so the
        // office is NOT monotone in sf — no logical-side seed is sound.
        let measured = [
            (1.00_f64, (120u32, 73u32), 6usize),
            (1.25, (150, 93), 12),
            (1.50, (180, 113), 20),
            (1.75, (210, 133), 24),
            (2.00, (240, 153), 30),
            (3.00, (360, 233), 80),
        ];
        for (sf, want_buf, want_floor0) in measured {
            let physical: PhysicalSize<u32> = logical.to_physical(sf);
            let office = window_geometry(physical, density(), Zoom::default()).logical();
            assert_eq!(
                (u32::from(office.w), u32::from(office.h)),
                want_buf,
                "office at {sf}× of {logical:?}"
            );
            assert_eq!(
                boot_capacities_for_window(physical, density(), Zoom::default())[0],
                want_floor0,
                "floor-0 seed at {sf}×"
            );
        }
        let at_2x =
            boot_capacities_for_window(logical.to_physical(2.0), density(), Zoom::default());
        assert!(
            at_2x[0] > as_if_physical[0],
            "logical-as-physical under-seeds at 2×: {} vs the real {}",
            as_if_physical[0],
            at_2x[0],
        );
    }

    #[test]
    fn an_unlayoutable_window_seeds_zero_not_a_fallback() {
        // DERIVED, not a pinned 64x48: the floor dropped below it and the premise
        // assert below went red. Derive so the next move can't reach it.
        let min = pixtuoid_scene::layout::min_layout_size();
        let tiny = PhysicalSize::new(u32::from(min.w), u32::from(min.h - 1));
        let office = window_geometry(tiny, density(), Zoom::default()).logical();
        assert_eq!(
            pixtuoid_scene::floor::floor_capacity(
                office.w,
                office.h,
                pixtuoid_scene::floor::floor_seed(0)
            ),
            0,
            "fixture must actually be unlayoutable, else this asserts nothing"
        );
        assert_eq!(
            boot_capacities_for_window(tiny, density(), Zoom::default())[0],
            0,
            "the seed must agree with what the redraw stores, not invent desks"
        );
    }

    #[test]
    fn a_shrink_lowers_the_published_capacity_it_is_store_not_fetch_max() {
        let caps: [AtomicUsize; MAX_FLOORS] = std::array::from_fn(|_| AtomicUsize::new(0));
        let (big, small) = ((360u16, 240u16), (240u16, 160u16));
        let want_big = floor_caps_for_buffer(big.0, big.1);
        let want_small = floor_caps_for_buffer(small.0, small.1);
        assert!(
            want_small[0] < want_big[0] && want_small[0] > 0,
            "fixture must shrink floor 0 to a smaller NON-zero capacity: {} → {}",
            want_big[0],
            want_small[0]
        );

        let mut last = None;
        sync_floor_caps(&mut last, &caps, big.0, big.1);
        for (floor, want) in want_big.iter().enumerate() {
            assert_eq!(
                caps[floor].load(Ordering::Relaxed),
                *want,
                "floor {floor} must publish the layout's own capacity at {big:?}"
            );
        }

        sync_floor_caps(&mut last, &caps, small.0, small.1);
        for (floor, want) in want_small.iter().enumerate() {
            assert_eq!(
                caps[floor].load(Ordering::Relaxed),
                *want,
                "floor {floor} must FALL to the smaller window's capacity — a `fetch_max` \
                 publish would strand it at the larger one"
            );
        }
    }

    /// One fixture per divergence class: the derived sub-floor window is the only one that
    /// can catch a re-introduced `cap == 0 → FALLBACK_DESKS` fallback (the other two have
    /// capacity on every floor), and 853×480 (`office_scale` 3) is the one
    /// whose capacity moves under a few px of one-sided buffer drift — the other two
    /// absorb it.
    #[test]
    fn the_first_redraws_publish_agrees_with_the_boot_seed() {
        // DERIVED as in the sibling above, which reds by name if this stops seeding zero.
        let min = pixtuoid_scene::layout::min_layout_size();
        let unlayoutable = PhysicalSize::new(u32::from(min.w), u32::from(min.h - 1));
        let windows = [
            PhysicalSize::new(1280u32, 720u32),
            PhysicalSize::new(853, 480),
            unlayoutable,
        ];
        let zooms = [Zoom::default(), Zoom::new(1), Zoom::new(-1)];
        for (window, zoom) in windows.into_iter().flat_map(|w| zooms.map(|z| (w, z))) {
            let seed = boot_capacities_for_window(window, density(), zoom);
            let caps: [AtomicUsize; MAX_FLOORS] = std::array::from_fn(|_| AtomicUsize::new(0));
            let office = window_geometry(window, density(), zoom).logical();
            sync_floor_caps(&mut None, &caps, office.w, office.h);
            let published: [usize; MAX_FLOORS] =
                std::array::from_fn(|i| caps[i].load(Ordering::Relaxed));
            assert_eq!(
                published, seed,
                "the first redraw's publish must store what {window:?} at {zoom:?} seeded"
            );
        }
    }

    #[test]
    fn the_resize_memo_publishes_on_a_change_and_skips_a_repeat() {
        let caps: [AtomicUsize; MAX_FLOORS] = std::array::from_fn(|_| AtomicUsize::new(0));
        let mut last = None;
        assert!(
            sync_floor_caps(&mut last, &caps, 360, 240),
            "the FIRST call has no previous size, so it must publish"
        );
        assert_eq!(
            last,
            Some((360, 240)),
            "the memo must record what it published"
        );
        assert!(
            !sync_floor_caps(&mut last, &caps, 360, 240),
            "an unchanged buffer size must skip the per-floor layout compute"
        );
        assert!(
            sync_floor_caps(&mut last, &caps, 240, 160),
            "a resize must republish"
        );
        caps[0].store(999, Ordering::Relaxed);
        assert!(!sync_floor_caps(&mut last, &caps, 240, 160));
        assert_eq!(
            caps[0].load(Ordering::Relaxed),
            999,
            "a skipped publish must not touch the atomics"
        );
    }

    /// No zoom is the automatic scale.
    #[test]
    fn no_zoom_is_the_automatic_scale() {
        let d = density();
        for (w, h) in [(3840u32, 2160u32), (960, 640), (1280, 720)] {
            let auto = PixelFit::at_least_density(
                u16::try_from(office_scale(h)).expect("small"),
                d,
                Size { w: 1, h: 1 },
            );
            let at = window_geometry(PhysicalSize::new(w, h), d, Zoom::default());
            assert_eq!(at.scale(), auto.scale(), "{w}x{h}");
        }
    }

    /// A zoom step is one density step of scale, and zooming out stops at
    /// the density itself, the art's own pixels.
    #[test]
    fn a_zoom_step_is_one_density_step() {
        let d = density().get();
        let size = PhysicalSize::new(3840, 2160);
        let scale = |steps| {
            window_geometry(size, density(), Zoom::new(steps))
                .scale()
                .get()
        };
        let auto = scale(0);
        assert_eq!(scale(1), auto + d);
        assert_eq!(scale(-1), auto - d);
        assert_eq!(scale(i8::MIN), d, "never below the density");
        for steps in i8::MIN..=i8::MAX {
            let s = zoom_scale(size, density(), Zoom::new(steps));
            assert!(s >= d && s.is_multiple_of(d), "zoom {steps} draws {s}");
        }
    }

    /// Zooming in stops at the largest scale whose office still lays out.
    #[test]
    fn zooming_in_never_drops_the_office_below_its_layout() {
        let min = pixtuoid_scene::layout::min_layout_size();
        let d = density().get();
        // Height-limited, then width-limited.
        for (w, h) in [(3840u16, 2160u16), (1000, 2160)] {
            let size = PhysicalSize::new(u32::from(w), u32::from(h));
            let at = window_geometry(size, density(), Zoom::new(i8::MAX));
            assert!(
                at.logical().w >= min.w && at.logical().h >= min.h,
                "{w}x{h}: {:?}",
                at.logical()
            );
            let one_more =
                PixelFit::at_least_density(at.scale().get() + d, density(), Size { w, h });
            let one_more = one_more.over(Size {
                w,
                h: h - footer_band(one_more),
            });
            assert!(
                one_more.logical().w < min.w || one_more.logical().h < min.h,
                "{w}x{h}: a larger scale would still lay out: {:?}",
                one_more.logical()
            );
        }
    }

    /// A window too narrow for any scale to lay out keeps the scale its
    /// height asks for, as it did before the step down existed.
    #[test]
    fn a_window_nothing_lays_out_in_keeps_its_natural_scale() {
        let d = density();
        let size = PhysicalSize::new(min_window(d).width - 1, 1200);
        let natural = PixelFit::at_least_density(
            u16::try_from(office_scale(size.height)).expect("small"),
            d,
            Size { w: 1, h: 1 },
        );
        assert!(natural.scale().get() > d.get(), "a step down was possible");
        assert_eq!(
            window_geometry(size, d, Zoom::default()).scale(),
            natural.scale()
        );
    }

    /// A window the office lays out in lays it out with no zoom, however
    /// tall: the automatic scale follows the height alone, so a narrow one
    /// takes the largest scale whose office still fits across.
    #[test]
    fn a_window_that_can_lay_out_does_with_no_zoom() {
        let min = pixtuoid_scene::layout::min_layout_size();
        let d = density();
        let narrowest = min_window(d).width;
        for h in (min_window(d).height..4000).step_by(37) {
            let size = PhysicalSize::new(narrowest, h);
            let office = window_geometry(size, d, Zoom::default()).logical();
            assert!(
                office.w >= min.w && office.h >= min.h,
                "{narrowest}x{h}: {office:?}"
            );
        }
    }

    /// [`Zoom::stepped`] from past the largest zoom that lays out, and each
    /// key's one step or reset.
    #[test]
    fn a_step_starts_from_the_zoom_in_effect() {
        let size = PhysicalSize::new(3840, 2160);
        let scale = |zoom: Zoom| window_geometry(size, density(), zoom).scale().get();
        let max = Zoom::new(i8::MAX);
        let out = max.stepped(ZoomKey::Out, size, density());
        assert_eq!(
            scale(out) + density().get(),
            scale(max),
            "one step under the largest"
        );
        assert_eq!(
            Zoom::default().stepped(ZoomKey::In, size, density()),
            Zoom::new(1)
        );
        assert_eq!(
            Zoom::new(3).stepped(ZoomKey::Reset, size, density()),
            Zoom::default()
        );
        let floor = Zoom::new(i8::MIN).stepped(ZoomKey::Out, size, density());
        assert_eq!(
            scale(floor),
            density().get(),
            "out from the floor stays there"
        );
    }
}
