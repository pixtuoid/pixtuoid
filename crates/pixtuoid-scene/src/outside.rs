//! What the windows show: everything outside — the sky, the city and the
//! weather on the panes — drawn onto one window's [`WindowView`] at a time on
//! a painter's grid, which holds no cell on the window's joinery.

use std::ops::Range;
use std::sync::Arc;

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::Rgb;
use pixtuoid_core::sprite::format::Density;

use crate::atmosphere::Moment;
use crate::celestial::SkyView;
use crate::glass_weather::GlassWeather;
use crate::layout::{Bounds, Size, WindowBay, window_frame, window_rows, window_run};
use crate::skyline::CityStrip;
use crate::theme::Theme;

/// One cell of a window's glass on a painter's grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cell {
    /// Where it stands on the painter's grid, which a dither keys on.
    pub(crate) at: (u16, u16),
    /// Its offset from the glass's top-left ([`WindowBay::glass_box`]).
    pub(crate) glass_offset: (u16, u16),
}

/// One window on a grid `d` cells to the layout unit, and what its glass
/// shows: the only surface a sky effect draws on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WindowView {
    bay: WindowBay,
    /// The window's top row, in units.
    top: u16,
    /// The window's height in units, its frame included.
    h: u16,
    glass_box: Bounds,
    d: Density,
    /// The window's box row by row from its top-left, `None` on its joinery
    /// ([`window_frame`]).
    px: Vec<Option<Rgb>>,
}

impl WindowView {
    /// `bay`'s window over `rows`, on a grid `d` cells to the unit, each glass
    /// cell in `base`.
    pub(crate) fn new(
        bay: WindowBay,
        rows: Range<u16>,
        d: Density,
        mut base: impl FnMut(Cell) -> Rgb,
    ) -> Self {
        let h = rows.end.saturating_sub(rows.start);
        let mut view = Self {
            bay,
            top: rows.start,
            h,
            glass_box: bay.glass_box(rows),
            d,
            px: Vec::new(),
        };
        let size = Size { w: bay.w, h };
        let (d, origin) = (view.d.get(), view.origin());
        // A unit at a time, its d×d cells all glass or all joinery.
        let mut px = Vec::with_capacity(usize::from(view.cols()) * usize::from(view.rows()));
        for uy in 0..h {
            for sy in 0..d {
                let ay = uy * d + sy;
                for ux in 0..bay.w {
                    let joinery = window_frame(ux, uy, size);
                    for sx in 0..d {
                        let ax = ux * d + sx;
                        px.push((!joinery).then(|| base(origin.cell(ax, ay))));
                    }
                }
            }
        }
        view.px = px;
        view
    }

    /// Recolour every glass cell: `f` takes the cell and what it shows.
    pub(crate) fn paint(&mut self, mut f: impl FnMut(Cell, Rgb) -> Rgb) {
        let (cols, origin) = (usize::from(self.cols()).max(1), self.origin());
        for (ay, row) in (0u16..).zip(self.px.chunks_mut(cols)) {
            for (ax, px) in (0u16..).zip(row) {
                if let Some(c) = px {
                    *c = f(origin.cell(ax, ay), *c);
                }
            }
        }
    }

    /// Recolour the glass cell `glass_offset` cells from the glass's top-left, as
    /// [`paint`](Self::paint) does.
    pub(crate) fn paint_glass_at(
        &mut self,
        glass_offset: (u16, u16),
        f: impl FnOnce(Cell, Rgb) -> Rgb,
    ) {
        let (ix, iy) = self.origin().inset;
        let (ax, ay) = (
            glass_offset.0.saturating_add(ix),
            glass_offset.1.saturating_add(iy),
        );
        if ax >= self.cols() || ay >= self.rows() {
            return;
        }
        let cell = self.origin().cell(ax, ay);
        let i = usize::from(ay) * usize::from(self.cols()) + usize::from(ax);
        if let Some(Some(c)) = self.px.get_mut(i) {
            *c = f(cell, *c);
        }
    }

    /// Its window's [`WindowBay::idx`].
    pub(crate) fn idx(&self) -> u16 {
        self.bay.idx
    }

    /// Its [`WindowBay::glass_box`], in units.
    pub(crate) fn glass_size(&self) -> Size {
        Size {
            w: self.glass_box.width,
            h: self.glass_box.height,
        }
    }

    pub(crate) fn d(&self) -> Density {
        self.d
    }

    /// Whether its glass shows at `at` on the grid.
    pub(crate) fn shows(&self, at: (u16, u16)) -> bool {
        let d = self.d.get();
        let (Some(ax), Some(ay)) = (
            at.0.checked_sub(self.bay.x.saturating_mul(d)),
            at.1.checked_sub(self.top.saturating_mul(d)),
        ) else {
            return false;
        };
        ax < self.cols()
            && ay < self.rows()
            && self.px[usize::from(ay) * usize::from(self.cols()) + usize::from(ax)].is_some()
    }

    /// Each glass cell's place on the grid, and what it shows.
    #[cfg(test)]
    pub(crate) fn cells(&self) -> impl Iterator<Item = ((u16, u16), Rgb)> + '_ {
        self.grid()
            .filter_map(|(cell, px)| px.map(|c| (cell.at, c)))
    }

    /// Each cell's place on the grid, and what its glass shows: `None` on the
    /// joinery.
    pub(crate) fn every(&self) -> impl Iterator<Item = ((u16, u16), Option<Rgb>)> + '_ {
        self.grid().map(|(cell, px)| (cell.at, px))
    }

    /// Each joinery cell's place on the grid.
    #[cfg(test)]
    pub(crate) fn joinery(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.grid()
            .filter(|(_, px)| px.is_none())
            .map(|(cell, _)| cell.at)
    }

    /// Each cell of [`px`](Self::px), row by row, with what its glass shows.
    fn grid(&self) -> impl Iterator<Item = (Cell, Option<Rgb>)> + '_ {
        let (cols, origin) = (usize::from(self.cols()).max(1), self.origin());
        (0u16..)
            .zip(self.px.chunks(cols))
            .flat_map(move |(ay, row)| {
                (0u16..)
                    .zip(row)
                    .map(move |(ax, &px)| (origin.cell(ax, ay), px))
            })
    }

    fn cols(&self) -> u16 {
        self.bay.w.saturating_mul(self.d.get())
    }

    fn rows(&self) -> u16 {
        self.h.saturating_mul(self.d.get())
    }

    fn origin(&self) -> Origin {
        let d = self.d.get();
        Origin {
            at: (self.bay.x.saturating_mul(d), self.top.saturating_mul(d)),
            inset: (
                (self.glass_box.x - self.bay.x).saturating_mul(d),
                (self.glass_box.y - self.top).saturating_mul(d),
            ),
        }
    }

    /// The glass's top-left cell on the painter's grid.
    pub(crate) fn glass_origin(&self) -> (u16, u16) {
        (
            self.glass_box.x.saturating_mul(self.d.get()),
            self.glass_box.y.saturating_mul(self.d.get()),
        )
    }
}

/// Where a window's cells stand on a painter's grid: the window's top-left,
/// and its glass's top-left in cells from there. `Copy`, so
/// [`WindowView::paint`] reads it while it borrows the view's cells.
#[derive(Debug, Clone, Copy)]
struct Origin {
    at: (u16, u16),
    inset: (u16, u16),
}

impl Origin {
    /// The cell `(ax, ay)` cells from the window's top-left.
    fn cell(self, ax: u16, ay: u16) -> Cell {
        Cell {
            at: (self.at.0.saturating_add(ax), self.at.1.saturating_add(ay)),
            glass_offset: (
                ax.saturating_sub(self.inset.0),
                ay.saturating_sub(self.inset.1),
            ),
        }
    }
}

/// A wall's windows: the band they're cut in, `buf_w` by `band_h`, and the
/// bays its painter draws: a strike lands on their panes, and
/// [`Outside::views`] paints exactly these.
#[derive(Debug, PartialEq)]
pub(crate) struct Wall {
    pub(crate) size: (u16, u16),
    pub(crate) bays: Vec<WindowBay>,
}

impl Wall {
    /// The run of glass its windows cut, in units, and the glass's height:
    /// the span the clouds are drawn across.
    pub(crate) fn glass(&self) -> (Range<u16>, u16) {
        let rows = window_rows(self.size.1);
        (
            window_run(self.size.0),
            crate::layout::glass_rows(rows.end - rows.start),
        )
    }
}

/// The outside's state across frames, for one pack in one theme (the
/// [`OfficeRaster`](crate::look::OfficeRaster) that holds it empties it for
/// another): the clouds' masses, and the last frame's views beside what
/// `Outside::of` drew them from and what that resolved to, which a frame
/// given the same, or resolving to the same, reuses
/// ([Blink's display-item cache](https://chromium.googlesource.com/chromium/src/+/HEAD/third_party/blink/renderer/core/paint/README.md#display-item-caching)).
#[derive(Debug, Default)]
pub struct OutsideCache {
    pub(crate) clouds: crate::clouds::CloudCache,
    last: Option<Last>,
}

#[derive(Debug)]
struct Last {
    key: OutsideKey,
    /// All the views' painting reads: a sky whose key moves below every step
    /// the outside is drawn in resolves to an equal one.
    painted: Outside,
    views: Views,
}

/// Each of a wall's bays, and what its glass shows.
pub(crate) type Views = Vec<(WindowBay, Arc<WindowView>)>;

/// All [`Outside::of`] reads but the pack and the theme, which are the
/// cache's own: it takes this and nothing else, so nothing it reads can
/// miss the key.
#[derive(Debug, PartialEq)]
pub(crate) struct OutsideKey {
    sky: crate::sky::Sky,
    altitude: f32,
    beat: crate::anim::Beat,
    wall: Wall,
    density: Density,
    weather: GlassWeather,
}

impl OutsideKey {
    /// The outside `wall` shows at `moment` under `weather`, at `density`.
    pub(crate) fn of(moment: &Moment, wall: Wall, density: Density, weather: GlassWeather) -> Self {
        Self {
            sky: moment.sky,
            altitude: moment.altitude,
            beat: moment.timing.beat,
            wall,
            density,
            weather,
        }
    }
}

impl OutsideCache {
    /// Each of `wall`'s bays and what its glass shows, as
    /// [`Outside::of`]`(..).views()` gives them.
    pub(crate) fn views(
        &mut self,
        moment: &Moment,
        pack: &OfficeArt,
        theme: &Theme,
        wall: Wall,
        density: Density,
        weather: GlassWeather,
    ) -> Views {
        let drawn = OutsideKey::of(moment, wall, density, weather);
        let (run, glass_h) = drawn.wall.glass();
        let glass = (run.end - run.start, glass_h);
        if let Some(last) = &self.last
            && last.key == drawn
        {
            crate::clouds::Clouds::draw_ahead(moment, glass, density, &mut self.clouds);
            return last.views.clone();
        }
        let outside = Outside::of(&drawn, pack, theme, &mut self.clouds);
        crate::clouds::Clouds::draw_ahead(moment, glass, density, &mut self.clouds);
        if let Some(last) = &mut self.last
            && last.painted == outside
        {
            last.key = drawn;
            return last.views.clone();
        }
        let views: Views = outside
            .views()
            .into_iter()
            .map(|(bay, view)| (bay, Arc::new(view)))
            .collect();
        self.last = Some(Last {
            key: drawn,
            painted: outside,
            views: views.clone(),
        });
        views
    }
}

/// Everything one frame's windows look out on, the same through every one,
/// on one painter's grid.
#[derive(PartialEq)]
pub(crate) struct Outside {
    sky: SkyView,
    clouds: crate::clouds::Clouds,
    city: CityStrip,
    /// The column, in units, the city's west end stands at.
    run_x0: u16,
    weather: GlassWeather,
    rows: Range<u16>,
    d: Density,
    bays: Vec<WindowBay>,
}

impl std::fmt::Debug for Outside {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outside").finish_non_exhaustive()
    }
}

impl Outside {
    /// The outside `key` names, its sky in `theme`, a strike landing only on
    /// the glass of its bays; `clouds` keeps the clouds' masses across
    /// frames, and an empty one draws them afresh.
    pub(crate) fn of(
        key: &OutsideKey,
        pack: &OfficeArt,
        theme: &Theme,
        clouds: &mut crate::clouds::CloudCache,
    ) -> Self {
        let &OutsideKey {
            sky,
            altitude,
            beat,
            ref wall,
            density,
            weather,
        } = key;
        let outlook = crate::atmosphere::Outlook::resolve(sky, theme, altitude, beat);
        let (run, glass_h) = wall.glass();
        let (buf_w, band_h) = wall.size;
        let bays = wall.bays.clone();
        let rows = window_rows(band_h);
        let panes: Vec<Range<u16>> = bays
            .iter()
            .copied()
            .flat_map(WindowBay::panes)
            .map(|p| p.start - run.start..p.end - run.start)
            .collect();
        Self {
            sky: SkyView::of(&outlook, buf_w, band_h, theme),
            clouds: crate::clouds::Clouds::of(
                &outlook,
                (run.end - run.start, glass_h),
                density,
                &panes,
                clouds,
            ),
            city: CityStrip::draw(
                pack,
                (run.end - run.start, glass_h),
                &outlook,
                theme,
                density,
            ),
            run_x0: run.start,
            weather,
            rows,
            d: density,
            bays,
        }
    }

    /// Each of the wall's bays, and what its glass shows, the bays drawn
    /// across the cores ([`crate::par`]).
    pub(crate) fn views(&self) -> Vec<(WindowBay, WindowView)> {
        crate::par::map(&self.bays, crate::par::cores(), |&bay| {
            (bay, self.through(bay))
        })
    }

    /// What `bay`'s glass shows: each part of the outside, back to front, in
    /// the one order every painter draws it in.
    fn through(&self, bay: WindowBay) -> WindowView {
        let front = self.city.front(self.run_x0, self.d);
        let mut view = self.sky.window(bay, self.rows.clone(), self.d, &front);
        self.clouds.paint(&mut view, self.run_x0, &front);
        self.weather.paint(&mut view);
        view
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::display::pen::test_density;
    use crate::sky::{Sky, Weather};
    use pixtuoid_core::sprite::RgbBuffer;

    /// The windows a wall `buf_w` wide has.
    fn slots(buf_w: u16) -> Vec<WindowBay> {
        crate::layout::window_slots(buf_w).collect()
    }

    /// One sky a painter's frame is drawn under, beside the no-weather sky of
    /// its instant, which its windows show in the frame it is held against.
    pub(crate) struct Weathered {
        pub(crate) name: String,
        pub(crate) now: std::time::SystemTime,
        pub(crate) sky: Sky,
        pub(crate) bare: Sky,
        pub(crate) changes_glass: bool,
    }

    /// Every weather by day, at sunset and by night, a storm mid-strike too.
    pub(crate) fn every_sky() -> Vec<Weathered> {
        let mut skies = Vec::new();
        for hour in [12, 19, 23] {
            let now = crate::localclock::at_hour(hour);
            for w in Weather::ALL {
                let strikes = if w == Weather::Storm {
                    [None, Some(crate::sky::StrikePhase::Primary)].as_slice()
                } else {
                    &[None]
                };
                for &strike in strikes {
                    skies.push(Weathered {
                        name: format!("{w:?} at {hour}h, strike {strike:?}"),
                        now,
                        sky: Sky::at_with(now, w).with_strike(strike),
                        bare: Sky::at_with(now, Weather::Clear),
                        changes_glass: w != Weather::Clear,
                    });
                }
            }
        }
        assert!(
            skies.iter().any(|s| {
                crate::atmosphere::SkyTones::resolve(&s.sky, &crate::theme::NORMAL).golden_hour
                    > 0.1
            }),
            "the sweep reaches a sunset's blaze"
        );
        skies
    }

    /// What a test buffer holds where nothing painted.
    pub(crate) const UNPAINTED: Rgb = Rgb { r: 1, g: 2, b: 3 };

    /// Which pixels of a `w`×`h` buffer some of `pieces` writes, each painted
    /// alone by `paint` and looked for in the rows and columns `within` gives it.
    pub(crate) fn painted_alone<P>(
        (w, h): (u16, u16),
        pieces: impl IntoIterator<Item = P>,
        within: impl Fn(&P) -> (Range<u16>, Range<u16>),
        mut paint: impl FnMut(&P, &mut RgbBuffer),
    ) -> Vec<bool> {
        let mut over = RgbBuffer::filled(w, h, UNPAINTED);
        let mut painted = vec![false; usize::from(w) * usize::from(h)];
        for piece in pieces {
            let epoch = over.begin_writes();
            paint(&piece, &mut over);
            let (xs, ys) = within(&piece);
            for y in ys.start..ys.end.min(h) {
                for x in xs.start..xs.end.min(w) {
                    if over.written_in(x, y, epoch) {
                        painted[usize::from(y) * usize::from(w) + usize::from(x)] = true;
                    }
                }
            }
        }
        over.end_writes();
        painted
    }

    /// Under each of [`every_sky`], a painter's frame matches the one whose
    /// windows show its instant's no-weather sky — the same room, lit alike —
    /// everywhere but on glass nothing else paints, and a sky other than a
    /// clear one changes some glass. `frames` paints a sky's frame and that
    /// twin, `covered` the pixels something else paints at its instant
    /// ([`painted_alone`]). Returns the glass pixels something hangs over, for
    /// the painter's population guard.
    pub(crate) fn assert_the_outside_reaches_only_the_glass(
        painter: &str,
        (w, h): (u16, u16),
        on_glass: impl Fn(u16, u16) -> bool,
        mut frames: impl FnMut(&Weathered) -> [RgbBuffer; 2],
        mut covered: impl FnMut(&Weathered) -> Vec<bool>,
    ) -> std::collections::HashSet<(u16, u16)> {
        let mut hung_over_glass = std::collections::HashSet::new();
        let mut at_instant: Option<(std::time::SystemTime, Vec<bool>)> = None;
        for weathered in every_sky() {
            let name = format!("{painter}, {}", weathered.name);
            if at_instant
                .as_ref()
                .is_none_or(|(now, _)| *now != weathered.now)
            {
                at_instant = Some((weathered.now, covered(&weathered)));
            }
            let covered = &at_instant.as_ref().expect("filled above").1;
            let [shown, bare] = frames(&weathered);
            let mut glass = 0;
            for y in 0..h {
                for x in 0..w {
                    let hung = covered[usize::from(y) * usize::from(w) + usize::from(x)];
                    if hung && on_glass(x, y) {
                        hung_over_glass.insert((x, y));
                    }
                    if shown.get(x, y) == bare.get(x, y) {
                        continue;
                    }
                    assert!(
                        on_glass(x, y) && !hung,
                        "{name}: the outside reached ({x}, {y}), off the glass"
                    );
                    glass += 1;
                }
            }
            assert_eq!(
                glass > 0,
                weathered.changes_glass,
                "{name}: {glass} glass pixels"
            );
        }
        hung_over_glass
    }

    fn bay() -> WindowBay {
        WindowBay {
            x: 5,
            w: crate::layout::WINDOW_W,
            idx: 0,
        }
    }

    const ROWS: Range<u16> = 1..21;

    /// The view's cells are the glass [`window_frame`] leaves and nothing
    /// else, at every grid density, so its mask is the window geometry's.
    #[test]
    fn a_view_holds_the_glass_and_none_of_the_joinery() {
        let h = ROWS.end - ROWS.start;
        for d in [1, 2, 4] {
            let view = WindowView::new(bay(), ROWS, test_density(d), |_| Rgb { r: 1, g: 2, b: 3 });
            let glass: std::collections::HashSet<_> = view.cells().map(|(at, _)| at).collect();
            let joinery: std::collections::HashSet<_> = view.joinery().collect();
            for ay in ROWS.start * d..ROWS.end * d {
                for ax in bay().x * d..bay().span().end * d {
                    let frame = window_frame(
                        ax / d - bay().x,
                        ay / d - ROWS.start,
                        Size { w: bay().w, h },
                    );
                    assert_eq!(glass.contains(&(ax, ay)), !frame, "d={d} ({ax}, {ay})");
                    assert_eq!(joinery.contains(&(ax, ay)), frame, "d={d} ({ax}, {ay})");
                    assert_eq!(view.shows((ax, ay)), !frame, "d={d} ({ax}, {ay})");
                }
            }
        }
    }

    #[test]
    fn no_write_reaches_the_joinery() {
        const INK: Rgb = Rgb { r: 9, g: 9, b: 9 };
        for d in [1, 4] {
            let mut view =
                WindowView::new(bay(), ROWS, test_density(d), |_| Rgb { r: 1, g: 2, b: 3 });
            let joinery: Vec<_> = view.joinery().collect();
            view.paint(|_, _| INK);
            let glass = view.glass_size();
            for gy in 0..(glass.h + 2) * d {
                for gx in 0..(glass.w + 2) * d {
                    view.paint_glass_at((gx, gy), |_, _| INK);
                }
            }
            assert_eq!(view.joinery().collect::<Vec<_>>(), joinery, "d={d}");
            assert!(view.cells().all(|(_, c)| c == INK), "d={d}");
        }
    }

    #[test]
    fn a_glass_offset_counts_from_the_glass() {
        let d = 4;
        let mut view = WindowView::new(bay(), ROWS, test_density(d), |_| Rgb { r: 0, g: 0, b: 0 });
        let mut seen = None;
        view.paint_glass_at((0, 0), |cell, c| {
            seen = Some(cell);
            c
        });
        let glass = bay().glass_box(ROWS);
        assert_eq!(
            seen,
            Some(Cell {
                at: (glass.x * d, glass.y * d),
                glass_offset: (0, 0),
            })
        );
    }

    /// A frame drawn through the office's outside cache is the frame drawn
    /// without it, byte for byte: as the masses drift and the light steps
    /// through an evening, in every weather and across transitions, at both
    /// densities on one cache; after a frame that differs from it in any one
    /// of [`Outside::of`]'s arguments alone but the cache's own pack and
    /// theme; and a drift alone draws no mass anew.
    #[test]
    fn the_outside_cache_draws_what_a_fresh_frame_draws() {
        use crate::sky::WeatherMix;
        let pack = crate::pack::test_office();
        let wall = (crate::layout::WINDOW_W * 3, 32);
        struct Frame {
            moment: Moment,
            weather: GlassWeather,
            wall: (u16, u16),
            d: Density,
        }
        let pixels = |f: &Frame, cache: &mut OutsideCache| {
            cache
                .views(
                    &f.moment,
                    &pack,
                    &crate::theme::NORMAL,
                    Wall {
                        size: f.wall,
                        bays: slots(f.wall.0),
                    },
                    f.d,
                    f.weather,
                )
                .into_iter()
                .map(|(_, view)| view.cells().collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };
        let moment = |weather: WeatherMix, now: std::time::SystemTime, altitude| {
            Moment::resolve(
                Sky::at_with(now, Weather::Clear).with_weather(weather),
                &crate::theme::NORMAL,
                altitude,
                crate::anim::Motion::Full.timing(now),
            )
        };
        let frame = |moment: Moment, wall, d| Frame {
            weather: GlassWeather::of(&moment),
            moment,
            wall,
            d,
        };
        let transitions = [
            (Weather::Clear, Weather::Rain),
            (Weather::Overcast, Weather::Storm),
            (Weather::Storm, Weather::Clear),
        ];
        let skies =
            Weather::ALL
                .map(WeatherMix::pure)
                .into_iter()
                .chain(transitions.into_iter().flat_map(|(from, to)| {
                    [0.2, 0.4, 0.6, 0.8].map(|p| WeatherMix::toward(from, to, p))
                }));
        let mut cache = OutsideCache::default();
        let evening = crate::localclock::at_hour(17);
        for weather in skies {
            for minutes in [0, 1, 120, 300] {
                let now = evening + std::time::Duration::from_secs(minutes * 60);
                for d in [Density::ONE, pack.max_density_variant()] {
                    let f = frame(moment(weather, now, 0.0), wall, d);
                    assert_eq!(
                        pixels(&f, &mut cache),
                        pixels(&f, &mut OutsideCache::default()),
                        "{weather:?} at 17h+{minutes}m, {d:?}"
                    );
                }
            }
        }

        // Each pair differs in one argument and draws a different outside: a
        // key that drops it serves the first frame's to the second.
        let noon = crate::localclock::at_hour(12);
        let at = |mix, secs: u64| moment(mix, noon + std::time::Duration::from_secs(secs), 0.0);
        // a grid of its own whatever variants the pack draws
        let (one, dense) = (Density::ONE, test_density(4));
        let overcast = WeatherMix::pure(Weather::Overcast);
        let fog = WeatherMix::pure(Weather::Fog);
        let raining = Moment::resolve(
            Sky::at_with(noon, Weather::Rain),
            &crate::theme::NORMAL,
            0.0,
            crate::anim::Motion::Full.timing(noon),
        );
        // one heaviness bucket all the way across
        let fogging = |p| WeatherMix::toward(Weather::Overcast, Weather::Fog, p);
        let wide = (crate::layout::WINDOW_W * 5, 32);
        let tall = (crate::layout::WINDOW_W * 3, 40);
        let base = || frame(at(overcast, 0), wall, one);
        let pairs = [
            ("span", base(), frame(at(overcast, 0), wide, one)),
            ("glass", base(), frame(at(overcast, 0), tall, one)),
            ("density", base(), frame(at(overcast, 0), wall, dense)),
            // one deck kind, one light bucket
            ("weather", base(), frame(at(fog, 0), wall, one)),
            (
                "share",
                frame(at(fogging(0.3), 0), wall, one),
                frame(at(fogging(0.6), 0), wall, one),
            ),
            ("light", base(), frame(at(overcast, 7 * 3600), wall, one)),
            // noon's sky and glass, the clouds a minute on
            (
                "beat",
                base(),
                Frame {
                    moment: Moment::resolve(
                        at(overcast, 0).sky,
                        &crate::theme::NORMAL,
                        0.0,
                        crate::anim::Motion::Full.timing(noon + std::time::Duration::from_secs(60)),
                    ),
                    ..base()
                },
            ),
            // dusk's sky on noon's beat and glass
            (
                "sky",
                base(),
                Frame {
                    moment: Moment::resolve(
                        at(overcast, 7 * 3600).sky,
                        &crate::theme::NORMAL,
                        0.0,
                        crate::anim::Motion::Full.timing(noon),
                    ),
                    ..base()
                },
            ),
            (
                "altitude",
                base(),
                frame(moment(overcast, noon, 1.0), wall, one),
            ),
            (
                "glass weather",
                base(),
                Frame {
                    weather: GlassWeather::of(&raining),
                    ..base()
                },
            ),
        ];
        for (input, a, b) in &pairs {
            assert_ne!(
                pixels(a, &mut OutsideCache::default()),
                pixels(b, &mut OutsideCache::default()),
                "the pair apart in its {input} draws one outside"
            );
            let mut cache = OutsideCache::default();
            let _ = pixels(a, &mut cache);
            assert_eq!(
                pixels(b, &mut cache),
                pixels(b, &mut OutsideCache::default()),
                "a frame apart in its {input} drew the other's outside"
            );
        }

        // A sky apart below every step the outside is drawn in: the key
        // differs, the outside resolves alike, and its views are reused.
        let views = |f: &Frame, cache: &mut OutsideCache| {
            cache.views(
                &f.moment,
                &pack,
                &crate::theme::NORMAL,
                Wall {
                    size: f.wall,
                    bays: slots(f.wall.0),
                },
                f.d,
                f.weather,
            )
        };
        let (near, nearer) = (
            frame(at(fogging(0.4), 0), wall, one),
            frame(at(fogging(0.4001), 0), wall, one),
        );
        let mut cache = OutsideCache::default();
        let first = views(&near, &mut cache);
        let second = views(&nearer, &mut cache);
        assert!(
            Arc::ptr_eq(&first[0].1, &second[0].1),
            "a sky resolving alike painted afresh"
        );
        assert_eq!(
            pixels(&nearer, &mut cache),
            pixels(&nearer, &mut OutsideCache::default())
        );

        // the policy's own overcast: the frames ahead see what this one does
        let forced = |now| {
            let timing = crate::anim::Motion::Full.timing(now);
            let moment = Moment::resolve(
                Sky::at_with(now, Weather::Overcast),
                &crate::theme::NORMAL,
                0.0,
                timing,
            );
            frame(moment, wall, Density::ONE)
        };
        let mut cache = OutsideCache::default();
        let _ = pixels(&forced(noon), &mut cache);
        let drawn = cache.clouds.len();
        assert!(drawn > 0, "an overcast deck draws its masses");
        let later = noon + std::time::Duration::from_secs(60);
        let _ = pixels(&forced(later), &mut cache);
        assert_eq!(cache.clouds.len(), drawn, "a drift redrew a mass");
    }

    #[test]
    fn a_window_shows_the_city_strip_from_its_own_column() {
        // Overcast noon: no stars and no disc, which key on the screen column,
        // not the city's; the sky's dither does too, so the far pane sits a
        // whole number of its periods east.
        let now = crate::localclock::on_day(15, 12);
        let theme = &crate::theme::NORMAL;
        let moment = Moment::resolve(
            Sky::at_with(now, Weather::Overcast),
            theme,
            0.0,
            crate::anim::Motion::Full.timing(now),
        );
        let dx = 7;
        let far = (crate::layout::WINDOW_W + dx).next_multiple_of(crate::dither::PERIOD);
        let mut outside = Outside::of(
            &OutsideKey::of(
                &moment,
                Wall {
                    size: (crate::layout::WINDOW_W * 3, 32),
                    bays: slots(crate::layout::WINDOW_W * 3),
                },
                Density::ONE,
                GlassWeather::of(&moment),
            ),
            &crate::pack::test_office(),
            theme,
            &mut crate::clouds::CloudCache::default(),
        );
        let mut pane = |x: u16, run_x0: u16| {
            outside.run_x0 = run_x0;
            let bay = WindowBay {
                x,
                w: crate::layout::WINDOW_W,
                idx: 0,
            };
            outside
                .through(bay)
                .cells()
                .map(|(_, c)| c)
                .collect::<Vec<_>>()
        };
        let (west, east) = (pane(0, 0), pane(far, far));
        assert_eq!(west, east, "a pane shows the strip from the run's west end");
        assert_ne!(
            pane(dx, 0),
            west,
            "a pane {dx} columns east shows a different stretch of city"
        );
    }

    /// A storm's bolt lands on the glass where its trunk starts, the run's
    /// west end counted in, at both densities, and never over the city.
    #[test]
    fn a_bolt_lands_under_its_trunk_and_behind_the_city() {
        let theme = &crate::theme::NORMAL;
        let pack = crate::pack::test_office();
        let wall = (crate::layout::WINDOW_W * 3, 32);
        let bays = slots(wall.0);
        for d in [Density::ONE, pack.max_density_variant()] {
            let mut bolts = 0;
            for k in 0..60u64 {
                let then = crate::localclock::at_hour(12) + std::time::Duration::from_secs(k * 15);
                let beat = crate::anim::Motion::Full.timing(then).beat;
                let start = crate::sky::strike_start_ms(beat) - beat.ms();
                let now = then + std::time::Duration::from_millis(start);
                let at = |weather| {
                    let sky = Sky::at_with(now, weather)
                        .with_strike(Some(crate::sky::StrikePhase::Primary));
                    Moment::resolve(sky, theme, 0.0, crate::anim::Motion::Full.timing(now))
                };
                let mut outside = Outside::of(
                    &OutsideKey::of(
                        &at(Weather::Storm),
                        Wall {
                            size: wall,
                            bays: bays.clone(),
                        },
                        d, // a clear pane's glass: no veil or rain over the bolt
                        GlassWeather::of(&at(Weather::Clear)),
                    ),
                    &pack,
                    theme,
                    &mut crate::clouds::CloudCache::default(),
                );
                assert!(outside.run_x0 > 0, "a run off the wall's west edge");
                let Some((x, y)) = outside.clouds.bolt_top() else {
                    continue;
                };
                let col = x as u16 + outside.run_x0;
                let Some(&bay) = bays.iter().find(|b| (b.x..b.x + b.w).contains(&col)) else {
                    continue;
                };
                // what the bolt alone changes, its flash lifting nothing
                outside.clouds.only_bolt(true);
                let struck = outside.through(bay);
                outside.clouds.only_bolt(false);
                let bare = outside.through(bay);
                let lit: Vec<Cell> = struck
                    .grid()
                    .zip(bare.grid())
                    .filter(|((_, a), (_, b))| a != b)
                    .map(|((cell, _), _)| cell)
                    .collect();
                let front = outside.city.front(outside.run_x0, outside.d);
                for &cell in &lit {
                    assert!(
                        front(cell).is_none(),
                        "d {d:?}, strike {k}: a bolt over the city"
                    );
                }
                let Some(top) = lit.iter().map(|c| c.glass_offset.1).min() else {
                    continue;
                };
                let df = f32::from(d.get());
                let want = (
                    ((x + f32::from(outside.run_x0)) * df) as u16,
                    (y * df) as u16,
                );
                let starts: Vec<u16> = lit
                    .iter()
                    .filter(|c| c.glass_offset.1 == top)
                    .map(|c| c.at.0)
                    .collect();
                assert_eq!(
                    top, want.1,
                    "d {d:?}, strike {k}: the bolt starts at its trunk's row"
                );
                assert!(
                    starts.contains(&want.0),
                    "d {d:?}, strike {k}: the bolt starts at {starts:?}, its trunk at {}",
                    want.0
                );
                bolts += 1;
            }
            assert!(
                bolts > 0,
                "d {d:?}: the sample must strike a bolt on the glass"
            );
        }
    }
}
