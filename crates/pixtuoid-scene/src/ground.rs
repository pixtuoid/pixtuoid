//! What lies on the floor under the room's light, pixel-free: elliptical
//! shadows.

use crate::sprite::format::Density;

/// A shadow's strength at noon, where a solid's shadow is crispest.
const NOON_SHADOW: f32 = 0.5;
/// How much of [`NOON_SHADOW`] night takes away: the lamps' shadows are softer.
const NIGHT_SHADOW_LOSS: f32 = 0.3;

/// How far a contact shadow reaches past each side of its solid, in logical
/// units.
pub(crate) const CONTACT_REACH: u16 = 1;
/// How much flatter than wide a contact shadow lies on the floor.
const CONTACT_FLATTEN: u16 = 3;
/// How deep a contact shadow reaches either side of its base, in rows: at
/// least enough to read as a pool rather than a stroke under a narrow solid.
const CONTACT_HALF_H: std::ops::RangeInclusive<u16> = 2..=3;

/// The shadow a solid casts where it meets the floor, in logical units: centred
/// under the solid's middle on the row under its south edge, so the solid hides
/// its north half and the south half falls toward the viewer, away from the
/// windows' light. An odd-width solid's middle falls mid-column, so its centre
/// and half-width are kept doubled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Contact {
    cx2: u32,
    cy: u16,
    half_w2: u32,
    half_h: u16,
}

impl Contact {
    /// Under a solid `w` wide from `x`, meeting the floor on row `base`.
    pub(crate) fn under(x: u16, w: u16, base: u16) -> Self {
        let half_w2 = u32::from(w) + 2 * u32::from(CONTACT_REACH);
        Self {
            cx2: 2 * u32::from(x) + u32::from(w),
            cy: base,
            half_w2,
            half_h: ((half_w2 / 2) as u16 / CONTACT_FLATTEN)
                .clamp(*CONTACT_HALF_H.start(), *CONTACT_HALF_H.end()),
        }
    }

    /// How much of it lands at `(x, y)`: full at its centre, none past its rim.
    pub(crate) fn falloff(self, x: f32, y: f32) -> Option<f32> {
        falloff(
            (2.0 * x - self.cx2 as f32) / self.half_w2 as f32,
            (y - f32::from(self.cy)) / f32::from(self.half_h),
        )
    }

    /// The first column and row it can reach, and the ones just past it.
    pub(crate) fn bounds(self) -> ((u16, u16), (u16, u16)) {
        let x0 = self.cx2.saturating_sub(self.half_w2) / 2;
        let x1 = (self.cx2 + self.half_w2).div_ceil(2);
        (
            (x0 as u16, self.cy.saturating_sub(self.half_h)),
            (x1 as u16, self.cy + self.half_h),
        )
    }
}

/// Several shadows over their joint bounds, cut into `per` cells to a logical
/// unit: at each cell the deepest of them, so where two overlap the deeper one
/// wins rather than the two compounding.
pub(crate) struct Depths {
    x0: u16,
    y0: u16,
    w: u16,
    depth: Vec<f32>,
}

impl Depths {
    /// `contacts` sampled at each cell's centre, or `None` for no contacts.
    pub(crate) fn of(
        contacts: impl Iterator<Item = Contact> + Clone,
        per: Density,
    ) -> Option<Self> {
        let per = per.get();
        let cell = |logical: u16| logical.saturating_mul(per);
        let ((x0, y0), (x1, y1)) = contacts.clone().map(Contact::bounds).reduce(|a, b| {
            (
                (a.0.0.min(b.0.0), a.0.1.min(b.0.1)),
                (a.1.0.max(b.1.0), a.1.1.max(b.1.1)),
            )
        })?;
        let (x0, y0, w, h) = (cell(x0), cell(y0), cell(x1 - x0), cell(y1 - y0));
        let mut depth = vec![0.0_f32; usize::from(w) * usize::from(h)];
        let centre = |c: u16| (f32::from(c) + 0.5) / f32::from(per);
        for c in contacts {
            let ((cx0, cy0), (cx1, cy1)) = c.bounds();
            for y in cell(cy0)..cell(cy1) {
                for x in cell(cx0)..cell(cx1) {
                    if let Some(f) = c.falloff(centre(x), centre(y)) {
                        let i = usize::from(y - y0) * usize::from(w) + usize::from(x - x0);
                        depth[i] = depth[i].max(f);
                    }
                }
            }
        }
        Some(Self { x0, y0, w, depth })
    }

    /// Every cell a shadow reaches, with its depth.
    pub(crate) fn cells(&self) -> impl Iterator<Item = (u16, u16, f32)> + '_ {
        let w = usize::from(self.w);
        self.depth
            .iter()
            .enumerate()
            .filter(|&(_, &d)| d > 0.0)
            .map(move |(i, &d)| (self.x0 + (i % w) as u16, self.y0 + (i / w) as u16, d))
    }
}

/// One floor's [`Depths::cells`], kept across frames: the contacts change only
/// with the layout, so the cells are rebuilt only when they do.
#[derive(Debug, Default)]
pub(crate) struct DepthsCache {
    key: Option<(Vec<Contact>, Density)>,
    cells: Vec<(u16, u16, f32)>,
}

impl DepthsCache {
    /// The cells of `contacts` at `per`, rebuilt only when either changed.
    pub(crate) fn cells(&mut self, contacts: Vec<Contact>, per: Density) -> &[(u16, u16, f32)] {
        if self
            .key
            .as_ref()
            .is_none_or(|(cached, p)| *cached != contacts || *p != per)
        {
            self.cells = Depths::of(contacts.iter().copied(), per)
                .map_or_else(Vec::new, |d| d.cells().collect());
            self.key = Some((contacts, per));
        }
        &self.cells
    }
}

/// How much of a pool lands `nx`, `ny` radii from its centre: most at its
/// centre and fading to nothing at its rim, so a pool, stepped into its
/// painter's tones, reads as rings and not a stamped oval.
pub(crate) fn falloff(nx: f32, ny: f32) -> Option<f32> {
    let r2 = nx * nx + ny * ny;
    (r2 <= 1.0).then_some(1.0 - r2)
}

/// A shadow's strength under `darkness` (0 at noon, 1 at night).
pub(crate) fn shadow_strength(darkness: f32) -> f32 {
    NOON_SHADOW - NIGHT_SHADOW_LOSS * darkness
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contact_shadow_is_centred_under_its_solid_and_flatter_than_wide() {
        for (x, w) in [(10, 8), (10, 7)] {
            let c = Contact::under(x, w, 30);
            let mid = f32::from(x) + f32::from(w) / 2.0;
            assert_eq!(
                c.falloff(mid, 30.0),
                Some(1.0),
                "full under the middle of {w}"
            );
            let off = f32::from(w) / 2.0 + f32::from(CONTACT_REACH) - 0.25;
            assert_eq!(
                c.falloff(mid - off, 30.0),
                c.falloff(mid + off, 30.0),
                "a {w}-wide solid's shadow is as wide east as west"
            );
            assert!(
                c.falloff(mid - off, 30.0).is_some(),
                "it reaches past the sides"
            );
            let ((x0, _), (x1, _)) = c.bounds();
            assert_eq!(x1 - (x + w), x - x0, "bounds as wide east as west for {w}");
            assert!(c.half_h * 2 < w + 2 * CONTACT_REACH, "it lies flat");
        }
    }

    /// Where two shadows overlap, the cell takes the deeper, never their sum;
    /// every cell a shadow reaches is listed once, at its own coordinates.
    #[test]
    fn overlapping_shadows_keep_the_deeper_at_each_cell() {
        let (a, b) = (Contact::under(2, 6, 5), Contact::under(4, 6, 5));
        for per in [1, 4] {
            let depths = Depths::of([a, b].into_iter(), crate::display::pen::test_density(per))
                .expect("two contacts");
            let centre = |c: u16| (f32::from(c) + 0.5) / f32::from(per);
            let mut seen = std::collections::HashSet::new();
            for (x, y, d) in depths.cells() {
                assert!(seen.insert((x, y)), "({x},{y}) listed twice");
                let deeper = [a, b]
                    .iter()
                    .filter_map(|c| c.falloff(centre(x), centre(y)))
                    .fold(0.0, f32::max);
                assert_eq!(d, deeper, "({x},{y}) at {per} cells a unit");
            }
            let reached = (0..40 * per)
                .flat_map(|y| (0..40 * per).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    [a, b]
                        .iter()
                        .any(|c| c.falloff(centre(x), centre(y)).is_some_and(|f| f > 0.0))
                })
                .count();
            assert_eq!(seen.len(), reached, "every reached cell is listed");
        }
    }

    #[test]
    fn night_softens_a_shadow() {
        assert!(shadow_strength(1.0) < shadow_strength(0.0));
        assert!(shadow_strength(1.0) > 0.0, "a lamp still casts one");
    }

    #[test]
    fn the_depths_cache_rebuilds_only_for_new_contacts() {
        let fresh = |contacts: &[Contact]| -> Vec<(u16, u16, f32)> {
            Depths::of(contacts.iter().copied(), Density::ONE)
                .expect("contacts")
                .cells()
                .collect()
        };
        let a = [Contact::under(5, 10, 10)];
        let b = [Contact::under(5, 10, 10), Contact::under(30, 6, 20)];
        let mut cache = DepthsCache::default();
        assert_eq!(cache.cells(a.to_vec(), Density::ONE), fresh(&a));
        // A rebuild allocates its cells while the old ones still live, so the
        // same address means they were kept.
        let built = cache.cells(a.to_vec(), Density::ONE).as_ptr();
        assert_eq!(
            cache.cells(a.to_vec(), Density::ONE).as_ptr(),
            built,
            "kept"
        );
        assert_eq!(cache.cells(b.to_vec(), Density::ONE), fresh(&b), "rebuilt");
        assert!(
            cache.cells(Vec::new(), Density::ONE).is_empty(),
            "no contacts, no shadow"
        );
    }
}
