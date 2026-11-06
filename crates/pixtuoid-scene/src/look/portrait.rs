use pixtuoid_core::AgentSlot;

use crate::character::{SpritePose, character_frame, max_rise};
use crate::frame_cache::FrameCache;
use crate::pack::{OfficeArt, Piece, densest_frame};
use crate::render_scale::RenderScale;
use crate::sprite::Frame;
use crate::sprite::format::Density;

/// The pose a portrait shows: standing, facing the viewer, unlit by a screen.
const STANDING: SpritePose = SpritePose {
    anim_name: Piece::Standing,
    frame_idx: 0,
    flip_x: false,
    glow_tint: None,
};

impl OfficeArt {
    /// `agent`'s figure as the office draws it, standing and facing the viewer,
    /// for an out-of-workspace painter's list row, with the [`Density`] it is
    /// drawn at. It is bottom-aligned on a canvas one size for every agent of
    /// the pack, so a list's rows line up, and transparent around the figure. It
    /// carries no marker: the row names the agent's source in text. Its sibling
    /// export is [`frames_at`](Self::frames_at).
    ///
    /// ```
    /// use pixtuoid_core::AgentSlot;
    /// use pixtuoid_scene::pack::OfficeArt;
    ///
    /// fn row_image(art: &OfficeArt, agent: &AgentSlot) -> (u16, u16, Vec<[u8; 4]>) {
    ///     let (frame, _density) = art.portrait(agent);
    ///     let rgba = frame
    ///         .as_slice()
    ///         .iter()
    ///         .map(|px| px.map_or([0; 4], |c| [c.r, c.g, c.b, u8::MAX]))
    ///         .collect();
    ///     (frame.width(), frame.height(), rgba)
    /// }
    /// ```
    pub fn portrait(&self, agent: &AgentSlot) -> (Frame, Density) {
        let density = self.max_density_variant();
        let scale = RenderScale::from(density);
        let standing = densest_frame(self, Piece::Standing, 0, scale);
        let rise = max_rise(self, standing.frame, standing.head, density);
        let (width, height) = (
            standing.frame.width(),
            standing.frame.height().saturating_add(rise),
        );
        let mut cache = FrameCache::new();
        let figure = character_frame(STANDING, agent, self, scale, &mut cache).frame;
        let mut pixels = vec![None; usize::from(width) * usize::from(height)];
        // Bottom-aligned, so every agent stands on one baseline whatever their hair.
        let top = height.saturating_sub(figure.height());
        for y in 0..figure.height().min(height) {
            for x in 0..figure.width().min(width) {
                let at = usize::from(top + y) * usize::from(width) + usize::from(x);
                if let (Some(&px), Some(slot)) = (figure.get(x, y), pixels.get_mut(at)) {
                    *slot = px;
                }
            }
        }
        (Frame::from_pixels(width, height, pixels), density)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::character_key;
    use crate::character::test_support::make_slot_cwd;

    /// `agent`'s standing figure as the office recolours and dresses it at
    /// `art`'s densest scale.
    fn office_figure(art: &OfficeArt, agent: &AgentSlot) -> Frame {
        let scale = RenderScale::from(art.max_density_variant());
        character_frame(STANDING, agent, art, scale, &mut FrameCache::new())
            .frame
            .clone()
    }

    /// `frame`'s bottom `figure.height()` rows, which hold the figure.
    fn figure_rows(frame: &Frame, figure: &Frame) -> Vec<Option<crate::sprite::Rgb>> {
        let top = frame.height() - figure.height();
        (top..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| frame.get(x, y).copied().flatten()))
            .collect()
    }

    fn pixels(frame: &Frame) -> Vec<Option<crate::sprite::Rgb>> {
        frame.as_slice().to_vec()
    }

    /// One palette and one path: the portrait's figure is the office's own
    /// recolour of the agent, and nothing is drawn above it.
    #[test]
    fn a_portrait_is_the_agents_standing_figure_in_the_office() {
        let art = crate::pack::test_office();
        let agent = make_slot_cwd("/p/portrait.jsonl", "/work/atlas", false);
        let figure = office_figure(&art, &agent);
        let (portrait, _) = art.portrait(&agent);
        assert!(portrait.height() >= figure.height(), "the figure fits");
        assert_eq!(figure_rows(&portrait, &figure), pixels(&figure));
        let headroom = portrait.height() - figure.height();
        assert!(
            (0..headroom).all(|y| (0..portrait.width()).all(|x| portrait.get(x, y) == Some(&None))),
            "only transparency above the figure"
        );
    }

    /// Every agent's portrait in one pack is one size, whatever hairstyle
    /// they wear, and holds their whole figure on its bottom row.
    #[test]
    fn every_portrait_of_a_pack_is_one_size() {
        let art = crate::pack::test_office();
        let density = art.max_density_variant();
        let scale = RenderScale::from(density);
        let styles: std::collections::BTreeSet<String> = art
            .inner()
            .hairstyles()
            .filter(|s| s.density() == density)
            .map(|s| s.name().to_owned())
            .collect();
        assert!(
            styles.len() > 1,
            "premise: the pack draws styles to tell apart"
        );
        let mut worn = std::collections::BTreeSet::new();
        let mut sizes = std::collections::BTreeSet::new();
        for i in 0.. {
            if worn.len() == styles.len() {
                break;
            }
            assert!(i < 10_000, "no agent wears {:?}", styles.difference(&worn));
            let agent = make_slot_cwd(&format!("/p/style-{i}.jsonl"), "/work/atlas", false);
            let key = character_key(STANDING, &agent, &art, scale);
            let Some(style) = key.dress.and_then(|d| d.style) else {
                continue;
            };
            if !worn.insert(style) {
                continue;
            }
            let figure = office_figure(&art, &agent);
            let (frame, _) = art.portrait(&agent);
            sizes.insert((frame.width(), frame.height()));
            assert!(
                frame.height() >= figure.height(),
                "agent {i}'s figure is cut"
            );
            assert_eq!(
                figure_rows(&frame, &figure),
                pixels(&figure),
                "agent {i}'s figure is not on the bottom row"
            );
        }
        assert_eq!(sizes.len(), 1, "one size: {sizes:?}");
    }

    #[test]
    fn a_portrait_is_at_the_packs_max_density() {
        let art = crate::pack::test_office();
        let agent = make_slot_cwd("/p/dense.jsonl", "/work/atlas", false);
        let densest = art.max_density_variant();
        let (portrait, density) = art.portrait(&agent);
        assert_eq!(density, densest);
        let standing = densest_frame(&art, Piece::Standing, 0, RenderScale::from(densest));
        assert_eq!(portrait.width(), standing.frame.width());
    }
}
