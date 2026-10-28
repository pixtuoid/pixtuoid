//! The cutaway painted into a buffer kept across frames: a frame whose display
//! list matches the last one's is not painted at all, and one that differs is
//! painted whole but reports where it can differ, so a painter re-encodes only
//! there.

use std::collections::HashMap;
use std::sync::Arc;

use crate::pack::OfficeArt;
use pixtuoid_core::sprite::RgbBuffer;

use crate::cutaway::paint::paint;
use crate::display::light::Ambient;
use crate::display::{Backdrop, Hovers, Office, Recolours, Showing, Span, compose};
use crate::floor::SteppedFloor;
use crate::layout::Bounds;
use crate::render_scale::RenderScale;

/// A cutaway painter's frame buffer and what it shows, for one pack.
#[derive(Debug)]
pub struct CutawayCanvas {
    // Held, so no other pack can take its place unnoticed.
    pack: Arc<OfficeArt>,
    buf: RgbBuffer,
    /// `None` until the first frame.
    shown: Option<Shown>,
    lights: crate::display::compose::LightCache,
}

/// One frame from [`CutawayCanvas::frame`].
#[derive(Debug)]
pub struct CanvasFrame {
    /// Where it may differ from the canvas's previous frame.
    pub dirty: Dirty,
    /// What of it flashes.
    pub(crate) flash: crate::screen::FlashPhase,
    /// Why it painted whole, when it did.
    pub(crate) repaint: Option<Repaint>,
}

/// Why a frame painted whole: the first, or which inputs every pixel is
/// painted under changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Repaint {
    /// Nothing was shown before it.
    pub(crate) first: bool,
    /// The office's walls, windows or floor coverings.
    pub(crate) backdrop: bool,
    /// The per-agent palettes.
    pub(crate) recolours: bool,
    /// The render scale.
    pub(crate) scale: bool,
    /// The room's light.
    pub(crate) ambient: bool,
    /// The carpet's weather tint.
    pub(crate) carpet: bool,
    /// A strike's lift of the room.
    pub(crate) flash: bool,
    /// The host's modal dim.
    pub(crate) dim: bool,
}

impl Repaint {
    /// Why a frame under `now` paints whole after one under `was`; `None`
    /// when it need not.
    fn between(was: Option<&Epoch>, now: &Epoch) -> Option<Self> {
        let Some(was) = was else {
            return Some(Self {
                first: true,
                ..Self::default()
            });
        };
        (was != now).then(|| Self {
            first: false,
            backdrop: was.backdrop != now.backdrop,
            recolours: was.recolours != now.recolours,
            scale: was.scale != now.scale,
            ambient: was.ambient != now.ambient,
            carpet: was.carpet != now.carpet,
            flash: was.flash != now.flash,
            dim: was.dim != now.dim,
        })
    }
}

/// Where a frame's pixels may differ from the frame before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dirty {
    /// Anywhere.
    All,
    /// Only inside these, in buffer pixels on whole layout cells.
    Rects(Rects),
    /// Nowhere: the frame was not painted.
    Unchanged,
}

/// The rects a frame may differ inside: never none, which is
/// [`Dirty::Unchanged`], so a consumer that skips on it skips every unchanged
/// frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rects(Vec<Bounds>);

impl Rects {
    /// Each rect, in buffer pixels on whole layout cells.
    pub fn as_slice(&self) -> &[Bounds] {
        &self.0
    }
}

/// The most passes one frame's rects are painted in: a pass has a fixed cost,
/// so many small ones outrun the whole repaint, and one bounding box over
/// rects far apart paints most of the frame.
const MAX_PASSES: usize = 8;

/// `rects` merged into at most `max` disjoint boxes that cover them all, each
/// merge the pair whose box adds the least area: Mozilla's
/// `nsRegion::SimplifyOutward`, "at most aMaxRects by adding area to it ...
/// a superset of the original region" (gecko-dev `gfx/src/nsRegion.h`).
/// Overlapping boxes merge first whatever the cap, so no pixel is painted, or
/// lit, twice. The pairs wait in a heap whose stale ones are skipped as they
/// surface: O(n² log n) in the rects, not a scan of every pair a merge.
fn passes(rects: &[Bounds], max: usize) -> Vec<Bounds> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let area = |b: &Bounds| i64::from(b.width) * i64::from(b.height);
    // Overlapping pairs first, then by the area a merge adds.
    let key = |a: &Bounds, b: &Bounds| (!a.overlaps(*b), area(&a.union(*b)) - area(a) - area(b));
    let mut boxes: Vec<Option<Bounds>> = rects.iter().copied().map(Some).collect();
    let mut heap = BinaryHeap::new();
    for i in 0..boxes.len() {
        for j in i + 1..boxes.len() {
            if let (Some(a), Some(b)) = (&boxes[i], &boxes[j]) {
                heap.push(Reverse((key(a, b), i, j)));
            }
        }
    }
    let mut alive = boxes.len();
    while let Some(Reverse(((apart, _), i, j))) = heap.pop() {
        let (Some(a), Some(b)) = (boxes[i], boxes[j]) else {
            continue;
        };
        if apart && alive <= max.max(1) {
            break;
        }
        let merged = a.union(b);
        boxes[i] = None;
        boxes[j] = None;
        let k = boxes.len();
        for (o, other) in boxes.iter().enumerate() {
            if let Some(other) = other {
                heap.push(Reverse((key(other, &merged), o, k)));
            }
        }
        boxes.push(Some(merged));
        alive -= 1;
    }
    boxes.into_iter().flatten().collect()
}

impl Dirty {
    /// Only inside `rects`: [`Self::Unchanged`] when there are none.
    pub fn within(rects: Vec<Bounds>) -> Self {
        if rects.is_empty() {
            Self::Unchanged
        } else {
            Self::Rects(Rects(rects))
        }
    }
}

/// What every pixel of a frame is painted under, beyond its pieces and the
/// canvas's pack.
#[derive(Debug, PartialEq)]
struct Epoch {
    backdrop: Backdrop,
    recolours: Recolours,
    scale: RenderScale,
    ambient: Ambient,
    carpet: crate::dither::Dithered<crate::atmosphere::Carpet>,
    flash: crate::display::light::Flash,
    dim: crate::look::Dim,
}

#[derive(Debug)]
struct Shown {
    epoch: Epoch,
    /// Every piece's reach and every light's span, each with its fingerprint.
    footprints: Vec<(Span, u64)>,
    hovers: Arc<Hovers>,
    /// The cells of the star it drew, if it drew one.
    star: Option<Bounds>,
    /// The world text it left to the host, if it baked none.
    host_text: Option<crate::display::text::HostRuns>,
}

impl CutawayCanvas {
    /// A canvas that draws with `pack`.
    pub fn new(pack: Arc<OfficeArt>) -> Self {
        Self {
            pack,
            buf: RgbBuffer::filled(0, 0, pixtuoid_core::sprite::Rgb { r: 0, g: 0, b: 0 }),
            shown: None,
            lights: crate::display::compose::LightCache::default(),
        }
    }

    /// Paint `stepped` as [`render_cutaway`](crate::cutaway::paint::render_cutaway)
    /// does, its world text as `text` says, unless the frame would be the one
    /// already shown.
    pub fn frame(
        &mut self,
        stepped: &SteppedFloor,
        theme: &crate::theme::Theme,
        scale: RenderScale,
        showing: Showing<'_>,
        text: crate::look::WorldText,
        (cache, outside): (
            &mut crate::cutaway::paint::CutawayCache,
            &mut crate::outside::OutsideCache,
        ),
    ) -> CanvasFrame {
        let layout = &stepped.layout;
        let office = Office {
            layout,
            pack: &self.pack,
            theme,
            scale,
        };
        let mut list = tracing::trace_span!("canvas.compose")
            .in_scope(|| compose(&stepped.frame, office, showing, (&mut self.lights, outside)));
        let host_text = (text == crate::look::WorldText::Host).then(|| list.host_text());
        let epoch = Epoch {
            backdrop: list.backdrop().clone(),
            recolours: list.recolours().clone(),
            scale,
            ambient: list.ambient(),
            carpet: list.carpet(),
            flash: list.flash(),
            dim: list.dim(),
        };
        let footprints: Vec<(Span, u64)> = list
            .pieces()
            .iter()
            .map(|p| (p.reach(), p.fingerprint))
            .chain(list.lights().iter().map(|l| (l.span, l.fingerprint)))
            .collect();
        let size = (scale.to_buffer(layout.buf_w), scale.to_buffer(layout.buf_h));
        let repaint = Repaint::between(self.shown.as_ref().map(|s| &s.epoch), &epoch);
        if let Some(r) = repaint {
            tracing::trace!(
                first = r.first,
                backdrop = r.backdrop,
                recolours = r.recolours,
                scale = r.scale,
                ambient = r.ambient,
                carpet = r.carpet,
                flash = r.flash,
                dim = r.dim,
                ambient_now = ?epoch.ambient,
                flash_now = ?epoch.flash,
                "canvas.epoch"
            );
        }
        let dirty = match self.shown.take() {
            Some(shown) if shown.epoch == epoch => Dirty::within(
                changed(&shown.footprints, &footprints)
                    .into_iter()
                    .filter_map(|s| on_buffer(s, scale, size))
                    .collect(),
            ),
            _ => Dirty::All,
        };
        tracing::trace_span!("canvas.paint").in_scope(|| match &dirty {
            Dirty::All => {
                if (self.buf.width(), self.buf.height()) != size {
                    self.buf = RgbBuffer::filled(size.0, size.1, list.backdrop().tones.bg);
                }
                paint(&list, cache, &mut self.buf);
            }
            Dirty::Rects(rects) => {
                // a new size changes the epoch's backdrop, so it paints whole
                debug_assert_eq!((self.buf.width(), self.buf.height()), size);
                for pass in passes(rects.as_slice(), MAX_PASSES) {
                    let clip = (pass.x..pass.x + pass.width, pass.y..pass.y + pass.height);
                    self.buf.with_clip(clip, |buf| paint(&list, cache, buf));
                }
            }
            Dirty::Unchanged => {}
        });
        self.shown = Some(Shown {
            epoch,
            footprints,
            hovers: Arc::new(list.hovers().clone()),
            star: list
                .texts()
                .find(|run| run.role == crate::display::TextRole::Star)
                .map(|run| {
                    crate::display::compose::run_box(
                        run,
                        crate::display::pen::Pen::for_pack(scale, &self.pack),
                    )
                }),
            host_text,
        });
        CanvasFrame {
            dirty,
            flash: list.flash_phase(),
            repaint,
        }
    }

    /// Forget what was shown, so the next frame paints whole.
    pub(crate) fn forget(&mut self) {
        self.shown = None;
    }

    /// The last frame painted, empty before the first.
    pub(crate) fn buf(&self) -> &RgbBuffer {
        &self.buf
    }

    /// What the last frame answers a pointer with; `None` before the first.
    pub(crate) fn hovers(&self) -> Option<&Arc<Hovers>> {
        self.shown.as_ref().map(|shown| &shown.hovers)
    }

    /// The cells of the star the last frame drew; `None` before the first or
    /// when it drew none.
    pub(crate) fn star(&self) -> Option<Bounds> {
        self.shown.as_ref()?.star
    }

    /// The world text the last frame left to the host; `None` before the
    /// first or when it baked its own.
    pub(crate) fn host_text(&self) -> Option<crate::display::HostText<'_>> {
        let shown = self.shown.as_ref()?;
        shown.host_text.as_ref().map(|w| w.view(shown.epoch.dim))
    }
}

/// The spans of the footprints in `was` or `now` but not both, counting
/// repeats; each once, in order.
fn changed(was: &[(Span, u64)], now: &[(Span, u64)]) -> Vec<Span> {
    let mut count: HashMap<(Span, u64), isize> = HashMap::new();
    for &f in was {
        *count.entry(f).or_default() += 1;
    }
    for &f in now {
        *count.entry(f).or_default() -= 1;
    }
    was.iter()
        .chain(now)
        .filter_map(|f| {
            let n = count.get_mut(f)?;
            (std::mem::take(n) != 0).then_some(f.0)
        })
        .collect()
}

/// `span`'s cells in buffer pixels at `scale`, clipped to a `w`×`h` buffer;
/// `None` where none of it is left.
fn on_buffer(span: Span, scale: RenderScale, (w, h): (u16, u16)) -> Option<Bounds> {
    let (x, y) = (
        scale.to_buffer(span.x0).min(w),
        scale.to_buffer(span.y0).min(h),
    );
    let x1 = scale.to_buffer(span.x1.saturating_add(1)).min(w);
    let y1 = scale.to_buffer(span.y1.saturating_add(1)).min(h);
    (x1 > x && y1 > y).then_some(Bounds {
        x,
        y,
        width: x1 - x,
        height: y1 - y,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use pixtuoid_core::AgentId;

    use super::*;
    use crate::anim::Motion;
    use crate::cutaway::paint::render_cutaway;
    use crate::display::compose::tests::{empty_frame, lively_office, sit_down};
    use crate::display::{HoverTarget, PieceKind};
    use crate::floor::FloorMeta;
    use crate::layout::SceneLayout;
    use crate::pack::test_office;
    use crate::sim::SimFrame;
    use crate::theme::Theme;

    /// The ground floor under a clear sky: rain or snow on the glass moves
    /// every tick, so the clock's weather would decide what a tick repaints.
    fn clear_ground() -> FloorMeta {
        FloorMeta::ground().with_weather(crate::sky::WeatherPolicy::Forced(
            crate::sky::Weather::Clear,
        ))
    }

    /// How a run of frames through a canvas was reported.
    #[derive(Debug, Default)]
    struct Run {
        skipped: usize,
        partial: usize,
        whole: usize,
        /// Pixels that changed outside what diffing by span, not reach, reports.
        missed_by_span: usize,
    }

    fn normal() -> &'static Theme {
        crate::theme::theme_by_name("normal").expect("theme")
    }

    /// `layout`'s full render of `frame` as `showing` says, under the normal
    /// theme.
    fn full_render(
        layout: &SceneLayout,
        pack: &OfficeArt,
        scale: RenderScale,
        frame: &SimFrame,
        showing: Showing<'_>,
    ) -> RgbBuffer {
        let theme = normal();
        let mut buf = RgbBuffer::filled(
            scale.to_buffer(layout.buf_w),
            scale.to_buffer(layout.buf_h),
            theme.surface.bg_fallback,
        );
        let office = Office {
            layout,
            pack,
            theme,
            scale,
        };
        let mut cache = crate::cutaway::paint::CutawayCache::default();
        render_cutaway(
            frame,
            office,
            showing,
            (&mut cache, &mut crate::outside::OutsideCache::default()),
            &mut buf,
        );
        buf
    }

    /// Drive `steps` through one canvas beside a full render of each, at the
    /// pack's densest scale: every frame it shows, painted or skipped, is the
    /// full render, and every pixel two full renders in a row differ in lies in
    /// what it reported.
    fn run(layout: SceneLayout, pack: OfficeArt, steps: &[(SimFrame, SystemTime)]) -> Run {
        run_under(
            layout,
            pack,
            steps,
            (clear_ground(), crate::look::Dim::NONE),
        )
    }

    /// [`run`] on `floor`, under its weather and `dim`.
    fn run_under(
        layout: SceneLayout,
        pack: OfficeArt,
        steps: &[(SimFrame, SystemTime)],
        (floor, dim): (FloorMeta, crate::look::Dim),
    ) -> Run {
        let (layout, pack) = (Arc::new(layout), Arc::new(pack));
        let theme = normal();
        let scale = RenderScale::from(pack.max_density_variant());
        let office = Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale,
        };
        let w = scale.to_buffer(layout.buf_w);
        let inside = |rects: &[Bounds], i: usize| {
            let (x, y) = ((i % usize::from(w)) as u16, (i / usize::from(w)) as u16);
            rects
                .iter()
                .any(|r| (r.x..r.x + r.width).contains(&x) && (r.y..r.y + r.height).contains(&y))
        };
        let mut canvas = CutawayCanvas::new(Arc::clone(&pack));
        let (mut cache, mut outside) = (
            crate::cutaway::paint::CutawayCache::default(),
            crate::outside::OutsideCache::default(),
        );
        let mut tally = Run::default();
        let mut last: Option<(RgbBuffer, Vec<(Span, u64)>)> = None;
        for (k, (frame, now)) in steps.iter().enumerate() {
            let mut full = full_render(
                &layout,
                &pack,
                scale,
                frame,
                crate::display::compose::tests::showing(floor, *now),
            );
            // Dimmed apart from `paint`, which the canvas shares.
            for px in full.as_mut_slice() {
                *px = dim.of(*px);
            }
            let list = compose(
                frame,
                office,
                crate::display::compose::tests::showing(floor, *now),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            );
            let spans: Vec<(Span, u64)> = list
                .pieces()
                .iter()
                .map(|p| (p.span, p.fingerprint))
                .chain(list.lights().iter().map(|l| (l.span, l.fingerprint)))
                .collect();
            let stepped = SteppedFloor {
                layout: Arc::clone(&layout),
                frame: frame.clone(),
            };
            let shown = canvas.frame(
                &stepped,
                theme,
                scale,
                Showing {
                    dim,
                    ..crate::display::compose::tests::showing(floor, *now)
                },
                crate::look::WorldText::Baked,
                (&mut cache, &mut outside),
            );
            assert!(
                canvas.buf().as_slice() == full.as_slice(),
                "step {k}: the canvas shows other than the full render ({:?})",
                shown.dirty
            );
            if let Some((before, was_spans)) = &last {
                let differ: Vec<usize> = (0..full.as_slice().len())
                    .filter(|&i| before.as_slice()[i] != full.as_slice()[i])
                    .collect();
                let rects = match &shown.dirty {
                    Dirty::All => None,
                    Dirty::Rects(rects) => Some(rects.as_slice()),
                    Dirty::Unchanged => Some(&[][..]),
                };
                match rects {
                    None => tally.whole += 1,
                    Some(rects) => {
                        if rects.is_empty() {
                            tally.skipped += 1;
                        } else {
                            tally.partial += 1;
                        }
                        assert_eq!(
                            differ.iter().find(|&&i| !inside(rects, i)),
                            None,
                            "step {k}: a pixel changed outside {rects:?}"
                        );
                        let by_span: Vec<Bounds> = changed(was_spans, &spans)
                            .into_iter()
                            .filter_map(|s| on_buffer(s, scale, (w, scale.to_buffer(layout.buf_h))))
                            .collect();
                        tally.missed_by_span +=
                            differ.iter().filter(|&&i| !inside(&by_span, i)).count();
                    }
                }
            }
            last = Some((full, spans));
        }
        tally
    }

    /// `n` instants a frame tick apart from `start`.
    fn ticks(start: SystemTime, n: u64) -> impl Iterator<Item = SystemTime> {
        (0..n).map(move |i| start + Duration::from_millis(100 * i))
    }

    /// A walk to a desk and the sit, at noon when shadows are deepest. Diffing
    /// by span instead of reach misses where the walker's shadow moved, so the
    /// walk proves the reach is needed.
    #[test]
    fn a_walk_repaints_only_where_it_moves() {
        let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 4);
        let steps: Vec<_> = frames
            .into_iter()
            .zip(ticks(crate::localclock::at_hour(12), u64::MAX))
            .collect();
        let tally = run(layout, pack, &steps);
        assert!(tally.partial > 0, "{tally:?}");
        assert!(
            tally.missed_by_span > 0,
            "diffing by span missed no shadow, so the reach goes untested: {tally:?}"
        );
    }

    /// Under a host's dim, every frame the canvas shows is the full render
    /// dimmed, those it repaints only in part included; and a change of the
    /// dim repaints the whole frame.
    #[test]
    fn a_dim_darkens_every_frame_and_its_change_repaints_whole() {
        let (layout, pack, frames, _) = sit_down(crate::layout::Facing::South, 4);
        let steps: Vec<_> = frames
            .into_iter()
            .zip(ticks(crate::localclock::at_hour(12), u64::MAX))
            .collect();
        let (seated, now) = steps.last().cloned().expect("a seated frame");
        let (layout, pack) = (Arc::new(layout), Arc::new(pack));
        let tally = run_under(
            SceneLayout::clone(&layout),
            OfficeArt::clone(&pack),
            &steps,
            (clear_ground(), crate::look::Dim::from_brightness(0.4)),
        );
        assert!(tally.partial > 0, "{tally:?}");
        let (mut cache, mut outside) = (
            crate::cutaway::paint::CutawayCache::default(),
            crate::outside::OutsideCache::default(),
        );
        let mut canvas = CutawayCanvas::new(Arc::clone(&pack));
        let stepped = SteppedFloor {
            layout,
            frame: seated,
        };
        let mut show = |dim| {
            canvas.frame(
                &stepped,
                normal(),
                RenderScale::from(pack.max_density_variant()),
                Showing {
                    dim,
                    ..crate::display::compose::tests::showing(clear_ground(), now)
                },
                crate::look::WorldText::Baked,
                (&mut cache, &mut outside),
            )
        };
        show(crate::look::Dim::NONE);
        let dimmed = show(crate::look::Dim::from_brightness(0.4));
        assert_eq!(dimmed.dirty, Dirty::All);
        assert!(
            dimmed.repaint.is_some_and(|r| r.dim),
            "{:?}",
            dimmed.repaint
        );
    }

    /// From afternoon into night, ten minutes a step, the room's tone moves
    /// with the hour and the weather, repainting the whole frame; the steps
    /// between move only the sky and the lights.
    #[test]
    fn each_change_of_the_rooms_tone_repaints_the_whole_frame() {
        let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
        let seated = frames.last().expect("a seated frame");
        let steps: Vec<_> = (0..36)
            .map(|i| {
                (
                    seated.clone(),
                    crate::localclock::at_hour_min(16 + i / 6, (i % 6) * 10),
                )
            })
            .collect();
        let tally = run(layout, pack, &steps);
        assert!(tally.whole > 0 && tally.partial > 0, "{tally:?}");
    }

    /// A storm's strike, a tick apart through its phases: each change of the
    /// flash repaints the whole frame, and the canvas shows the full render.
    #[test]
    fn each_phase_of_a_strike_repaints_the_whole_frame() {
        let (layout, pack, frames, _) = sit_down(crate::layout::Facing::North, 2);
        let seated = frames.last().expect("a seated frame");
        let storm = crate::sky::WeatherPolicy::Forced(crate::sky::Weather::Storm);
        let strike = (0..60_000u64)
            .step_by(10)
            .map(|ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms))
            .find(|&t| crate::sky::Sky::at(Motion::Full.timing(t), storm).flash() > 0.0)
            .expect("a strike in the first minute");
        let steps: Vec<_> = ticks(strike - Duration::from_millis(200), 8)
            .map(|t| (seated.clone(), t))
            .collect();
        let tally = run_under(
            layout,
            pack,
            &steps,
            (
                FloorMeta::ground().with_weather(storm),
                crate::look::Dim::NONE,
            ),
        );
        assert!(tally.whole >= 2, "{tally:?}");
    }

    /// The elevator opening, the sign fading up to alert and both appliances
    /// busy, a tick apart.
    #[test]
    fn what_plays_repaints_only_where_it_plays() {
        let layout = lively_office();
        let quiet = empty_frame(&layout);
        let steps: Vec<_> = ticks(crate::localclock::at_hour(21), 30)
            .enumerate()
            .map(|(i, now)| {
                let frame = SimFrame {
                    door_frame: i.min(5) / 2,
                    neon: crate::floor::NeonLevels {
                        alert: (i as f32 / 20.0).min(1.0),
                        ..crate::floor::NeonLevels::CALM
                    },
                    occupied_waypoints: (0..layout.waypoints.len()).collect(),
                    ..quiet.clone()
                };
                (frame, now)
            })
            .collect();
        let tally = run(layout, test_office(), &steps);
        assert!(tally.partial > 0, "{tally:?}");
    }

    /// An office with nobody in it skips the ticks on which nothing it shows
    /// moves, and reports only where the sign breathes or a loop plays on the
    /// rest.
    #[test]
    fn an_idle_office_skips_the_ticks_that_change_nothing() {
        let layout = lively_office();
        let quiet = empty_frame(&layout);
        let steps: Vec<_> = ticks(crate::localclock::at_hour(12), 30)
            .map(|now| (quiet.clone(), now))
            .collect();
        let tally = run(layout, test_office(), &steps);
        assert!(tally.skipped > 0 && tally.partial > 0, "{tally:?}");
        assert_eq!(tally.whole, 0, "{tally:?}");
    }

    /// A change of anything the list does not fingerprint repaints the whole
    /// frame: the theme, the scale.
    #[test]
    fn a_new_theme_or_scale_repaints_everything() {
        let layout = Arc::new(lively_office());
        let stepped = SteppedFloor {
            frame: empty_frame(&layout),
            layout,
        };
        let now = crate::localclock::at_hour(12);
        let (mut cache, mut outside) = (
            crate::cutaway::paint::CutawayCache::default(),
            crate::outside::OutsideCache::default(),
        );
        let mut canvas = CutawayCanvas::new(Arc::new(test_office()));
        let other = crate::theme::ALL_THEMES
            .iter()
            .copied()
            .find(|t| !std::ptr::eq(*t, normal()))
            .expect("a second theme");
        let mut dirty = |theme, s| {
            let scale = RenderScale::new(s).expect("nonzero");
            canvas
                .frame(
                    &stepped,
                    theme,
                    scale,
                    crate::display::compose::tests::showing(clear_ground(), now),
                    crate::look::WorldText::Baked,
                    (&mut cache, &mut outside),
                )
                .dirty
        };
        assert_eq!(dirty(normal(), 2), Dirty::All, "the first frame");
        assert_eq!(dirty(normal(), 2), Dirty::Unchanged, "the same frame");
        assert_eq!(dirty(other, 2), Dirty::All, "a new theme");
        assert_eq!(dirty(other, 3), Dirty::All, "a new scale");
    }

    /// A walk to a desk, shown at scale 2.
    struct Hovering {
        layout: Arc<SceneLayout>,
        pack: Arc<OfficeArt>,
        frames: Vec<SimFrame>,
        scale: RenderScale,
    }

    impl Hovering {
        /// To a north-facing desk.
        fn new() -> Self {
            Self::facing(crate::layout::Facing::North)
        }

        fn facing(facing: crate::layout::Facing) -> Self {
            let (layout, pack, frames, _) = sit_down(facing, 2);
            Self {
                layout: Arc::new(layout),
                pack: Arc::new(pack),
                frames,
                scale: RenderScale::new(2).expect("nonzero"),
            }
        }

        fn now() -> SystemTime {
            crate::localclock::at_hour(12)
        }

        /// `frame`'s pieces bar the badges as `(is a desk, box, agent)`, back
        /// to front: a character's box is their body.
        fn boxes(&self, frame: &SimFrame) -> Vec<(bool, Span, Option<AgentId>)> {
            let office = Office {
                layout: &self.layout,
                pack: &self.pack,
                theme: normal(),
                scale: self.scale,
            };
            let list = compose(
                frame,
                office,
                crate::display::compose::tests::showing(clear_ground(), Self::now()),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            );
            list.pieces()
                .iter()
                .filter_map(|p| match &p.kind {
                    PieceKind::Text { .. } => None,
                    PieceKind::Character { figure, body, .. } => {
                        Some((false, *body, Some(figure.key.frame.agent_id)))
                    }
                    kind => Some((matches!(kind, PieceKind::Desk { .. }), p.span, None)),
                })
                .collect()
        }

        /// `canvas` after showing `frame`, and the frame's hovers.
        fn show(&self, canvas: &mut CutawayCanvas, frame: &SimFrame) -> Hovers {
            let stepped = SteppedFloor {
                layout: Arc::clone(&self.layout),
                frame: frame.clone(),
            };
            let (mut cache, mut outside) = (
                crate::cutaway::paint::CutawayCache::default(),
                crate::outside::OutsideCache::default(),
            );
            canvas.frame(
                &stepped,
                normal(),
                self.scale,
                crate::display::compose::tests::showing(clear_ground(), Self::now()),
                crate::look::WorldText::Baked,
                (&mut cache, &mut outside),
            );
            Hovers::clone(canvas.hovers().expect("a frame"))
        }
    }

    /// What one half-block terminal cell shows, from logical `(x, y)` down.
    fn cell((x, y): (u16, u16)) -> Bounds {
        Bounds {
            x,
            y,
            width: 1,
            height: 2,
        }
    }

    /// Where a cell shows the walker over a desk drawn before them, hovering
    /// names them: the topmost box wins.
    #[test]
    fn hovering_a_walker_in_front_of_a_desk_names_the_walker() {
        let h = Hovering::new();
        let (frame, area, id) = h
            .frames
            .iter()
            .find_map(|frame| {
                let boxes = h.boxes(frame);
                boxes.iter().enumerate().find_map(|(i, &(desk, d, _))| {
                    if !desk {
                        return None;
                    }
                    boxes[i + 1..].iter().find_map(|&(_, b, agent)| {
                        let area = cell((d.x0.max(b.x0), d.y0.max(b.y0)));
                        (d.meets(area) && b.meets(area)).then_some((frame, area, agent?))
                    })
                })
            })
            .expect("the walk passes in front of a desk");
        let mut canvas = CutawayCanvas::new(Arc::clone(&h.pack));
        assert_eq!(
            h.show(&mut canvas, frame).at(area),
            Some(&HoverTarget::Agent(id))
        );
    }

    /// A desk painted over its sitter's legs leaves them to the pointer: only
    /// a later figure covers one.
    #[test]
    fn a_desk_over_a_sitter_names_the_sitter() {
        let h = Hovering::facing(crate::layout::Facing::South);
        let seated = h.frames.last().expect("a seated frame");
        let boxes = h.boxes(seated);
        let (area, id) = boxes
            .iter()
            .enumerate()
            .find_map(|(i, &(_, b, agent))| {
                let id = agent?;
                boxes[i + 1..].iter().find_map(|&(desk, d, _)| {
                    let area = cell((d.x0.max(b.x0), d.y0.max(b.y0)));
                    (desk && d.meets(area) && b.meets(area)).then_some((area, id))
                })
            })
            .expect("the desk lies over its sitter");
        let mut canvas = CutawayCanvas::new(Arc::clone(&h.pack));
        assert_eq!(
            h.show(&mut canvas, seated).at(area),
            Some(&HoverTarget::Agent(id))
        );
    }

    /// A cell showing floor no piece stands on hovers nothing.
    #[test]
    fn hovering_bare_floor_names_nobody() {
        let h = Hovering::new();
        let frame = h.frames.last().expect("a frame");
        let boxes = h.boxes(frame);
        let area = (h.layout.top_margin..h.layout.buf_h - 1)
            .rev()
            .flat_map(|y| (0..h.layout.buf_w).map(move |x| cell((x, y))))
            .find(|&a| boxes.iter().all(|&(_, s, _)| !s.meets(a)))
            .expect("bare floor");
        let mut canvas = CutawayCanvas::new(Arc::clone(&h.pack));
        assert_eq!(h.show(&mut canvas, frame).at(area), None);
    }

    /// Hovering answers from the last frame: where the walker stood before
    /// they walked to their desk no longer names them.
    #[test]
    fn hovering_where_a_walker_left_names_them_no_more() {
        let h = Hovering::new();
        let (first, last) = (&h.frames[0], h.frames.last().expect("a frame"));
        let (body, id) = h
            .boxes(first)
            .into_iter()
            .find_map(|(_, s, agent)| Some((s, agent?)))
            .expect("the walker");
        let area = cell((
            u16::midpoint(body.x0, body.x1),
            u16::midpoint(body.y0, body.y1),
        ));
        assert!(
            h.boxes(last)
                .iter()
                .all(|&(_, s, agent)| agent != Some(id) || !s.meets(area)),
            "the walker never left {area:?}"
        );
        let mut canvas = CutawayCanvas::new(Arc::clone(&h.pack));
        let id = HoverTarget::Agent(id);
        assert_eq!(h.show(&mut canvas, first).at(area), Some(&id));
        assert_ne!(h.show(&mut canvas, last).at(area), Some(&id));
    }

    /// A neighbour sitting just south has their badge plate over the sitter's
    /// body; hovering the body there still names the sitter, as in the classic,
    /// where a badge is no hover target.
    #[test]
    fn hovering_a_sitter_under_a_neighbours_badge_names_the_sitter() {
        const NEIGHBOUR: &str = "cc\u{b7}neighbour";
        let h = Hovering::new();
        let seated = h.frames.last().expect("a seated frame");
        let a = seated.characters[0].clone();
        let a_id = seated.agents[a.agent_idx].agent_id;
        let body = h
            .boxes(seated)
            .into_iter()
            .find_map(|(_, s, agent)| (agent == Some(a_id)).then_some(s))
            .expect("the sitter's body");
        let mut tried = 0;
        for dy in 1..body.y1 - body.y0 + 16 {
            let mut both = seated.clone();
            let mut b = both.agents[a.agent_idx].clone();
            b.agent_id = AgentId::from_transcript_path("/neighbour.jsonl");
            b.label = NEIGHBOUR.into();
            both.agents.push(b.clone());
            both.characters.push(crate::sim::CharacterPlacement {
                agent_idx: both.agents.len() - 1,
                top_left: crate::layout::Point {
                    x: a.top_left.x + 2,
                    y: a.top_left.y + dy,
                },
                sort_row: a.sort_row + dy,
                seat_desk: None,
                seated: false,
                ..a.clone()
            });
            let plate = {
                let office = Office {
                    layout: &h.layout,
                    pack: &h.pack,
                    theme: normal(),
                    scale: h.scale,
                };
                let list = compose(
                    &both,
                    office,
                    crate::display::compose::tests::showing(clear_ground(), Hovering::now()),
                    (
                        &mut crate::display::compose::LightCache::default(),
                        &mut crate::outside::OutsideCache::default(),
                    ),
                );
                list.pieces().iter().find(|p| {
                    matches!(&p.kind, crate::display::PieceKind::Text { run } if run.role == crate::display::TextRole::Badge(b.agent_id))
                })
                    .map(|p| p.span)
                    .expect("the neighbour's plate")
            };
            let b_body = h
                .boxes(&both)
                .into_iter()
                .find_map(|(_, s, agent)| (agent == Some(b.agent_id)).then_some(s))
                .expect("the neighbour's body");
            let Some(at) = (body.y0..=body.y1)
                .flat_map(|y| (body.x0..=body.x1).map(move |x| cell((x, y))))
                .find(|&c| plate.meets(c) && !b_body.meets(c))
            else {
                continue;
            };
            tried += 1;
            let mut canvas = CutawayCanvas::new(Arc::clone(&h.pack));
            assert_eq!(
                h.show(&mut canvas, &both).at(at),
                Some(&HoverTarget::Agent(a_id)),
                "at {at:?}, {dy} rows south"
            );
        }
        assert!(
            tried > 0,
            "no neighbour's plate ever lay over the sitter's body"
        );
    }

    /// A badge is a piece of its own: a sitter whose name or state changes
    /// repaints their badge, and a renamed one nothing else.
    #[test]
    fn a_changed_badge_repaints_only_itself() {
        let h = Hovering::new();
        let seated = h.frames.last().expect("a seated frame");
        let plate = |frame: &SimFrame| {
            let office = Office {
                layout: &h.layout,
                pack: &h.pack,
                theme: normal(),
                scale: h.scale,
            };
            let list = compose(
                frame,
                office,
                crate::display::compose::tests::showing(clear_ground(), Hovering::now()),
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            );
            list.pieces()
                .iter()
                .find(|p| matches!(p.kind, crate::display::PieceKind::Text { .. }))
                .map(|p| (p.span, p.fingerprint))
                .expect("a badge")
        };
        let mut renamed = seated.clone();
        renamed.agents[0].label = "cc\u{b7}renamed".into();
        let mut canvas = CutawayCanvas::new(Arc::clone(&h.pack));
        h.show(&mut canvas, seated);
        let stepped = SteppedFloor {
            layout: Arc::clone(&h.layout),
            frame: renamed.clone(),
        };
        let (mut cache, mut outside) = (
            crate::cutaway::paint::CutawayCache::default(),
            crate::outside::OutsideCache::default(),
        );
        let size = (
            h.scale.to_buffer(h.layout.buf_w),
            h.scale.to_buffer(h.layout.buf_h),
        );
        let dirty = canvas
            .frame(
                &stepped,
                normal(),
                h.scale,
                crate::display::compose::tests::showing(clear_ground(), Hovering::now()),
                crate::look::WorldText::Baked,
                (&mut cache, &mut outside),
            )
            .dirty;
        let spans = [plate(seated).0, plate(&renamed).0];
        let want: Vec<Bounds> = spans
            .iter()
            .filter_map(|&s| on_buffer(s, h.scale, size))
            .collect();
        assert_eq!(dirty, Dirty::within(want));
        let mut requires_action = seated.clone();
        requires_action.agents[0].state = pixtuoid_core::state::ActivityState::RequiresAction {
            reason: Some("permission?".into()),
        };
        let (was, now) = (plate(seated), plate(&requires_action));
        assert_eq!(was.0, now.0, "the plate stays put");
        assert_ne!(was.1, now.1, "its tone is in its fingerprint");
    }

    /// The star a pointer opens the repo on is the cells the canvas drew it in:
    /// its box on the pack's grid where the pack's density divides the scale,
    /// and nothing on the base art's grid, where it yields to the brand.
    #[test]
    fn the_star_link_is_the_drawn_star() {
        use crate::display::TextRole;
        let pack = Arc::new(test_office());
        let now = crate::localclock::at_hour(12);
        let layout = SceneLayout::compute_with_seed(160, 96, None, 0).expect("lays out");
        let stepped = SteppedFloor {
            frame: empty_frame(&layout),
            layout: Arc::new(layout),
        };
        let d = pack.max_density_variant().get();
        for s in [1, d] {
            let scale = RenderScale::new(s).expect("nonzero");
            let mut canvas = CutawayCanvas::new(Arc::clone(&pack));
            let showing = crate::display::compose::tests::showing(clear_ground(), now);
            canvas.frame(
                &stepped,
                normal(),
                scale,
                showing,
                crate::look::WorldText::Baked,
                (
                    &mut crate::cutaway::paint::CutawayCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            );
            let office = Office {
                layout: &stepped.layout,
                pack: &pack,
                theme: normal(),
                scale,
            };
            let drawn = compose(
                &stepped.frame,
                office,
                showing,
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            )
            .texts()
            .find(|run| run.role == TextRole::Star)
            .map(|run| {
                crate::display::compose::run_box(
                    run,
                    crate::display::pen::Pen::for_pack(scale, &pack),
                )
            });
            assert_eq!(drawn.is_some(), s == d, "at scale {s} the star is drawn");
            assert_eq!(canvas.star(), drawn, "at scale {s}");
        }
    }

    /// The board's lines are pieces of their own: a new tally repaints exactly
    /// the board lines it changed, the old line's cells and the new one's.
    #[test]
    fn a_new_tally_repaints_only_the_board() {
        let h = Hovering::new();
        let seated = h.frames.last().expect("a seated frame");
        let quiet = crate::display::compose::tests::showing(clear_ground(), Hovering::now());
        let counts = crate::tally::StateCounts {
            running: 3,
            total: 3,
            ..crate::tally::StateCounts::default()
        };
        let busy =
            crate::neon_sign::build_board(counts, 60, None, None, Motion::Full, Hovering::now());
        let busy = Showing {
            board: &busy,
            ..quiet
        };
        let board = |showing| -> Vec<(Span, u64)> {
            use crate::display::TextRole;
            let office = Office {
                layout: &h.layout,
                pack: &h.pack,
                theme: normal(),
                scale: h.scale,
            };
            compose(
                seated,
                office,
                showing,
                (
                    &mut crate::display::compose::LightCache::default(),
                    &mut crate::outside::OutsideCache::default(),
                ),
            )
            .pieces()
            .iter()
            .filter(|p| {
                matches!(&p.kind, PieceKind::Text { run }
                        if matches!(run.role, TextRole::Brand | TextRole::Star | TextRole::Board))
            })
            .map(|p| (p.span, p.fingerprint))
            .collect()
        };
        let mut canvas = CutawayCanvas::new(Arc::clone(&h.pack));
        h.show(&mut canvas, seated);
        let stepped = SteppedFloor {
            layout: Arc::clone(&h.layout),
            frame: seated.clone(),
        };
        let (mut cache, mut outside) = (
            crate::cutaway::paint::CutawayCache::default(),
            crate::outside::OutsideCache::default(),
        );
        let dirty = canvas
            .frame(
                &stepped,
                normal(),
                h.scale,
                busy,
                crate::look::WorldText::Baked,
                (&mut cache, &mut outside),
            )
            .dirty;
        let size = (
            h.scale.to_buffer(h.layout.buf_w),
            h.scale.to_buffer(h.layout.buf_h),
        );
        let (was, now) = (board(quiet), board(busy));
        let only = |a: &[(Span, u64)], b: &[(Span, u64)]| -> Vec<Span> {
            a.iter()
                .filter(|line| !b.contains(line))
                .map(|&(span, _)| span)
                .collect()
        };
        let want: Vec<Bounds> = only(&was, &now)
            .into_iter()
            .chain(only(&now, &was))
            .filter_map(|s| on_buffer(s, h.scale, size))
            .collect();
        assert!(
            !want.is_empty(),
            "the tally changes a board line: {was:?} {now:?} {dirty:?}"
        );
        assert_eq!(dirty, Dirty::within(want));
    }

    /// A cutaway leaving its text to the host keeps no text piece to paint,
    /// and hands over each one it dropped; the host may set more, as the star
    /// at a scale the image draws none at.
    #[test]
    fn leaving_text_to_the_host_keeps_no_text_piece() {
        let h = Hovering::new();
        let seated = h.frames.last().expect("a seated frame");
        let office = Office {
            layout: &h.layout,
            pack: &h.pack,
            theme: normal(),
            scale: h.scale,
        };
        let mut list = compose(
            seated,
            office,
            crate::display::compose::tests::showing(clear_ground(), Hovering::now()),
            (
                &mut crate::display::compose::LightCache::default(),
                &mut crate::outside::OutsideCache::default(),
            ),
        );
        let baked: Vec<crate::display::TextRun> = list.texts().cloned().collect();
        assert!(!baked.is_empty(), "premise: the frame has text");
        let handed = list.host_text();
        assert_eq!(list.texts().count(), 0, "a text piece stays to be painted");
        for run in &baked {
            let handed = match run.role {
                crate::display::TextRole::Badge(id) => handed.badges.iter().any(|b| b.agent == id),
                _ => handed.bubbles.contains(run) || handed.signs.contains(run),
            };
            assert!(handed, "{:?} was dropped, not handed over", run.role);
        }
    }

    /// A frame's rects merge into at most the cap's boxes that cover every
    /// one of them, the nearest merging first: rects far apart stay apart,
    /// and overlapping ones become one.
    #[test]
    fn rects_merge_into_few_boxes_that_cover_them_all() {
        let b = |x, y, width, height| Bounds {
            x,
            y,
            width,
            height,
        };
        let covers = |boxes: &[Bounds], r: &Bounds| {
            boxes.iter().any(|o| {
                o.x <= r.x
                    && o.y <= r.y
                    && r.x + r.width <= o.x + o.width
                    && r.y + r.height <= o.y + o.height
            })
        };
        // a row of windows across the top, a light far below, an overlap
        let rects: Vec<Bounds> = (0..7)
            .map(|i| b(i * 40, 0, 20, 10))
            .chain([b(150, 200, 30, 30), b(160, 210, 30, 30)])
            .collect();
        for max in [1, 2, 3, 8, 64] {
            let boxes = passes(&rects, max);
            assert!(boxes.len() <= max.max(1), "{max}: {boxes:?}");
            assert!(rects.iter().all(|r| covers(&boxes, r)), "{max}: {boxes:?}");
        }
        let two = passes(&rects, 2);
        assert!(
            two.iter().all(|o| o.y >= 200 || o.y + o.height <= 10),
            "the windows' row and the light stay apart: {two:?}"
        );
        assert_eq!(
            passes(&rects, 8).len(),
            8,
            "the overlapping pair is one box"
        );
        // Boxes never overlap, whatever the cap: a scatter that merging
        // forces into overlap, as many rects as a transition frame's.
        let scatter: Vec<Bounds> = (0..96u16)
            .map(|i| b((i * 37) % 600, (i * 53) % 400, 10 + i % 30, 8 + i % 20))
            .collect();
        for max in [1, 4, 8, 64, 200] {
            let boxes = passes(&scatter, max);
            assert!(scatter.iter().all(|r| covers(&boxes, r)), "{max}");
            for (i, a) in boxes.iter().enumerate() {
                for b in &boxes[i + 1..] {
                    let meet = a.x < b.x + b.width
                        && b.x < a.x + a.width
                        && a.y < b.y + b.height
                        && b.y < a.y + a.height;
                    assert!(!meet, "{max}: {a:?} overlaps {b:?}");
                }
            }
        }
    }

    /// No list of rects is empty: "nowhere" is only ever
    /// [`Dirty::Unchanged`], so a consumer that skips on it skips every
    /// unchanged frame.
    #[test]
    fn an_empty_list_of_rects_is_unchanged() {
        let b = Bounds {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert_eq!(Dirty::within(Vec::new()), Dirty::Unchanged);
        let Dirty::Rects(rects) = Dirty::within(vec![b]) else {
            panic!("a rect is somewhere");
        };
        assert_eq!(rects.as_slice(), [b]);
    }

    /// A new layout of the same size shows whole, and the same one laid out
    /// again shows nothing new: the epoch compares the backdrop by value, so
    /// no address the allocator hands back decides either.
    #[test]
    fn a_new_layout_repaints_everything() {
        let pack = Arc::new(test_office());
        let scale = RenderScale::new(2).expect("nonzero");
        let now = crate::localclock::at_hour(12);
        let (mut cache, mut outside) = (
            crate::cutaway::paint::CutawayCache::default(),
            crate::outside::OutsideCache::default(),
        );
        let mut canvas = CutawayCanvas::new(Arc::clone(&pack));
        let stepped = |seed| {
            let layout = SceneLayout::compute_with_seed(160, 96, None, seed).expect("lays out");
            SteppedFloor {
                frame: empty_frame(&layout),
                layout: Arc::new(layout),
            }
        };
        let floor = clear_ground();
        canvas.frame(
            &stepped(0),
            normal(),
            scale,
            crate::display::compose::tests::showing(floor, now),
            crate::look::WorldText::Baked,
            (&mut cache, &mut outside),
        );
        let again = canvas.frame(
            &stepped(0),
            normal(),
            scale,
            crate::display::compose::tests::showing(floor, now),
            crate::look::WorldText::Baked,
            (&mut cache, &mut outside),
        );
        assert_eq!(
            again.dirty,
            Dirty::Unchanged,
            "the same layout, laid out again"
        );
        let b = stepped(1);
        let shown = canvas.frame(
            &b,
            normal(),
            scale,
            crate::display::compose::tests::showing(floor, now),
            crate::look::WorldText::Baked,
            (&mut cache, &mut outside),
        );
        assert_eq!(shown.dirty, Dirty::All);
        assert!(
            canvas.buf().as_slice()
                == full_render(
                    &b.layout,
                    &pack,
                    scale,
                    &b.frame,
                    crate::display::compose::tests::showing(clear_ground(), now)
                )
                .as_slice(),
            "the canvas shows other than the new layout"
        );
    }

    /// The digests [`the_canvas_paints_the_pinned_frames`] pins, one frame a
    /// line.
    #[cfg(feature = "pixel-font")]
    const PINNED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/cutaway/canvas.golden");

    /// Every pixel of a fixed set of frames, as committed: a refactor of the
    /// cutaway leaves this file as it found it, and a change to its look
    /// rewrites it (`just gen-cutaway-golden`). Each instant is a local
    /// hour ([`localclock`](crate::localclock)), so any `$TZ` paints it alike.
    // The golden pins baked text.
    #[cfg(feature = "pixel-font")]
    #[test]
    fn the_canvas_paints_the_pinned_frames() {
        use crate::sky::{Weather, WeatherPolicy};
        use std::fmt::Write as _;
        let pack = Arc::new(test_office());
        let (walk_layout, _, frames, _) = sit_down(crate::layout::Facing::North, 2);
        let walk_layout = Arc::new(walk_layout);
        let office = crate::floor::FloorSession::new(Arc::clone(&pack))
            .step(
                crate::floor::FloorInputs {
                    scene: &pixtuoid_core::SceneState::uniform(16),
                    now: SystemTime::UNIX_EPOCH,
                    floor: FloorMeta::ground(),
                    pets: crate::floor::PetInputs::default(),
                },
                crate::layout::Size { w: 240, h: 144 },
            )
            .expect("lays out");
        let lively = Arc::new(lively_office());
        let busy = SimFrame {
            door_frame: 2,
            neon: crate::floor::NeonLevels {
                alert: 1.0,
                ..crate::floor::NeonLevels::CALM
            },
            occupied_waypoints: (0..lively.waypoints.len()).collect(),
            ..empty_frame(&lively)
        };
        let scenes = [
            ("walk", &walk_layout, &frames[frames.len() / 2]),
            (
                "seated",
                &walk_layout,
                frames.last().expect("a seated frame"),
            ),
            ("office", &office.layout, &office.frame),
            ("lively", &lively, &busy),
        ];
        let moments = [
            ("normal", Weather::Clear, 12),
            ("cyberpunk", Weather::Rain, 17),
            ("dracula", Weather::Storm, 21),
            ("tokyo-night", Weather::Snow, 7),
            ("gruvbox", Weather::Fog, 2),
        ];
        let densities = [1, pack.max_density_variant().get()];
        // What the local hour leaves to the absolute instant, so to `$TZ`, held
        // still: the loops, and the moon, which a new moon keeps out of the
        // night sky.
        let new_moon = (0..31u32)
            .min_by(|&a, &b| {
                let phase = |day| {
                    crate::sky::Sky::at_with(crate::localclock::on_day(day, 0), Weather::Clear)
                        .moon_phase()
                };
                phase(a).total_cmp(&phase(b))
            })
            .expect("a lunation has days");
        let mut digests = String::new();
        for (scene, layout, frame) in scenes {
            for (i, (theme_name, weather, hour)) in moments.into_iter().enumerate() {
                let theme = crate::theme::theme_by_name(theme_name).expect("theme");
                let floor = FloorMeta::for_floor(i % 2 * 3, 4)
                    .with_weather(WeatherPolicy::Forced(weather))
                    .with_motion(Motion::Still);
                for d in densities {
                    let scale = RenderScale::new(d).expect("nonzero");
                    let mut canvas = CutawayCanvas::new(Arc::clone(&pack));
                    let stepped = SteppedFloor {
                        layout: Arc::clone(layout),
                        frame: frame.clone(),
                    };
                    canvas.frame(
                        &stepped,
                        theme,
                        scale,
                        crate::display::compose::tests::showing(
                            floor,
                            crate::localclock::on_day(new_moon, hour),
                        ),
                        crate::look::WorldText::Baked,
                        (
                            &mut crate::cutaway::paint::CutawayCache::default(),
                            &mut crate::outside::OutsideCache::default(),
                        ),
                    );
                    let buf = canvas.buf();
                    let digest = buf.as_slice().iter().fold(
                        u64::from(buf.width()) << 16 | u64::from(buf.height()),
                        |h, c| {
                            pixtuoid_core::id::splitmix64(
                                h ^ u64::from_be_bytes([0, 0, 0, 0, 0, c.r, c.g, c.b]),
                            )
                        },
                    );
                    let name = weather.name();
                    let floor_idx = floor.floor_idx;
                    writeln!(
                        digests,
                        "{scene} {theme_name} {name} {hour:02}h floor{floor_idx} d{d} {digest:016x}"
                    )
                    .expect("a String takes every write");
                }
            }
        }
        let pinned = snapbox::Data::read_from(
            std::path::Path::new(PINNED),
            Some(snapbox::data::DataFormat::Text),
        );
        snapbox::assert_data_eq!(digests, pinned.raw());
    }
}
