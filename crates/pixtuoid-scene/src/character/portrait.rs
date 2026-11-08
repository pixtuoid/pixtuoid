//! [`OfficeArt::portrait`], here beside [`character_frame`](super::character_frame)
//! so [`crate::pack`] never depends on `character`.

use pixtuoid_core::AgentSlot;

use super::hair::max_rise;
use super::{SpritePose, character_key, keyed_character_frame};
use crate::display::effects::{Head, cap_look};
use crate::frame_cache::FrameCache;
use crate::layout::Point;
use crate::pack::{OfficeArt, Piece, densest_frame};
use crate::render_scale::RenderScale;
use crate::sim::team_cap;
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
    /// drawn at: bottom-aligned on a transparent canvas one size for every
    /// agent of the pack, so a list's rows line up. A subagent's cap is part of
    /// the figure, not a marker: the row names the agent's source in text.
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
        let scale = RenderScale::from(self.max_density_variant());
        let standing = densest_frame(self, STANDING.anim_name, STANDING.frame_idx, scale);
        let headroom = max_rise(self, &standing);
        let (width, height) = (
            standing.frame.width(),
            standing.frame.height().saturating_add(headroom),
        );
        let key = character_key(STANDING, agent, self, scale);
        let mut cache = FrameCache::new();
        let figure = keyed_character_frame(&key, self, scale, &mut cache).frame;
        let mut canvas = Frame::from_pixels(
            width,
            height,
            vec![None; usize::from(width) * usize::from(height)],
        );
        // Bottom-aligned, so every agent stands on one baseline whatever their hair.
        let top = height.saturating_sub(figure.height());
        for y in 0..figure.height() {
            for x in 0..figure.width() {
                if let Some(&px) = figure.get(x, y) {
                    canvas.set(x, top + y, px);
                }
            }
        }
        let base = densest_frame(
            self,
            STANDING.anim_name,
            STANDING.frame_idx,
            RenderScale::ONE,
        );
        let cap = base
            .head
            .and_then(|head| team_cap(agent, self, head.view, STANDING.flip_x));
        let head = Head::of(&key, (base.logical.0, Point { x: 0, y: 0 }, base.head));
        if let (Some(cap), Some(Head::Dressed { brow, .. })) = (cap, head) {
            cap_look(cap, brow, &mut |at, _, c, _| {
                // `at` is on the body's grid, `headroom` rows down the canvas.
                let y = at.y + i32::from(headroom);
                if let (Ok(x), Ok(y)) = (u16::try_from(at.x), u16::try_from(y)) {
                    canvas.set(x, y, Some(c));
                }
            });
        }
        (canvas, standing.density)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::test_support::make_slot_cwd;
    use crate::character::{cap_ink, character_frame};
    use pixtuoid_core::AgentId;
    use std::collections::BTreeMap;

    const POSE: SpritePose = SpritePose {
        anim_name: Piece::Standing,
        frame_idx: 0,
        flip_x: false,
        glow_tint: None,
    };

    /// `agent`'s standing figure as the office recolours and dresses it at
    /// `art`'s densest scale.
    fn office_figure(art: &OfficeArt, agent: &AgentSlot) -> Frame {
        let scale = RenderScale::from(art.max_density_variant());
        character_frame(POSE, agent, art, scale, &mut FrameCache::new())
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

    /// `agent` led by `lead`.
    fn subagent(agent: &AgentSlot, lead: AgentId) -> AgentSlot {
        AgentSlot {
            parent_id: Some(lead),
            ..agent.clone()
        }
    }

    /// The pixels the office's cutaway draws for `agent`'s cap, the agent in
    /// the portrait's pose at the bundled pack's densest scale, each from the
    /// top-left of the body under the hair.
    fn office_cap(agent: &AgentSlot) -> BTreeMap<(i32, i32), crate::sprite::Rgb> {
        let theme = crate::theme::theme_by_name("normal").expect("theme");
        let (layout, pack, frames, _) =
            crate::display::compose::tests::sit_down(crate::layout::Facing::South, 0);
        let mut frame = frames.last().expect("a frame").clone();
        let c = &mut frame.characters[0];
        (c.anim_name, c.frame_idx, c.flip_x) = (POSE.anim_name, POSE.frame_idx, POSE.flip_x);
        (c.seat_desk, c.seated) = (None, false);
        frame.agents[c.agent_idx] = agent.clone();
        c.effects = crate::sim::cap_for(c, agent, &pack).into_iter().collect();
        let at = c.top_left;
        let office = crate::display::Office {
            layout: &layout,
            pack: &pack,
            theme,
            scale: RenderScale::from(pack.max_density_variant()),
        };
        let list = crate::display::compose::tests::list_at(&frame, office, 12);
        let rider = crate::display::compose::tests::cap_rider(&list);
        let d = i32::from(rider.pen.density().get());
        let (left, top) = (i32::from(at.x) * d, i32::from(at.y) * d);
        let mut px = BTreeMap::new();
        rider.look(&mut |p, size, c, _| {
            for (dx, dy) in (0..size).flat_map(|dy| (0..size).map(move |dx| (dx, dy))) {
                px.insert((p.x + dx - left, p.y + dy - top), c);
            }
        });
        px
    }

    /// The rows over `art`'s standing body in its portrait `frame`: the
    /// canvas's headroom.
    fn headroom(art: &OfficeArt, frame: &Frame) -> i32 {
        let scale = RenderScale::from(art.max_density_variant());
        let body = densest_frame(art, POSE.anim_name, POSE.frame_idx, scale).frame;
        i32::from(frame.height() - body.height())
    }

    /// `frame`'s pixels at `cap`'s, each from the top-left of the body under
    /// the hair: `None` where it is transparent or the canvas ends.
    fn at_cap(
        art: &OfficeArt,
        frame: &Frame,
        cap: &BTreeMap<(i32, i32), crate::sprite::Rgb>,
    ) -> BTreeMap<(i32, i32), Option<crate::sprite::Rgb>> {
        let top = headroom(art, frame);
        let at = |x: i32, y: i32| {
            let (x, y) = (u16::try_from(x).ok()?, u16::try_from(y + top).ok()?);
            frame.get(x, y).copied().flatten()
        };
        cap.keys().map(|&(x, y)| ((x, y), at(x, y))).collect()
    }

    /// One palette and one path: a lead's portrait is the office's own recolour
    /// of the agent, capless, and nothing is drawn above it.
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

    /// A subagent's portrait wears the cap the office draws on its figure, pixel
    /// for pixel: its brim on the brow, ringed, in its lead's ink, which its
    /// siblings share whatever their own ids pick.
    #[test]
    fn a_subagents_portrait_wears_the_cap_the_office_draws() {
        let art = crate::pack::test_office();
        let lead = AgentId::from_transcript_path("/team/lead.jsonl");
        let child = |n: u32| make_slot_cwd(&format!("/team/child-{n}.jsonl"), "/work/atlas", false);
        let first = (0..64)
            .map(child)
            .find(|c| cap_ink(c.agent_id) != cap_ink(lead))
            .expect("a child whose own id picks another ink than its lead's");
        let second = (0..64)
            .map(child)
            .find(|c| ![cap_ink(lead), cap_ink(first.agent_id)].contains(&cap_ink(c.agent_id)))
            .expect("a sibling whose own id picks a third ink");
        for agent in [first, second] {
            let agent = subagent(&agent, lead);
            let cap = office_cap(&agent);
            assert!(
                cap.values().any(|&c| c == cap_ink(lead)),
                "premise: the office caps {:?} in its lead's ink",
                agent.agent_id
            );
            let (portrait, _) = art.portrait(&agent);
            let drawn: BTreeMap<_, _> = cap.iter().map(|(&at, &c)| (at, Some(c))).collect();
            assert_eq!(at_cap(&art, &portrait, &cap), drawn, "{:?}", agent.agent_id);
        }
    }

    /// Every agent's portrait in one pack is one size, whatever hairstyle they
    /// wear, capped or not: the tallest one's height, each whole figure on its
    /// bottom row, and each cap inside it.
    #[test]
    fn every_portrait_of_a_pack_is_one_size() {
        let art = crate::pack::test_office();
        let lead = AgentId::from_transcript_path("/p/lead.jsonl");
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
        let (mut width, mut tallest) = (0, 0);
        for i in 0.. {
            if worn.len() == styles.len() {
                break;
            }
            assert!(i < 10_000, "no agent wears {:?}", styles.difference(&worn));
            let agent = make_slot_cwd(&format!("/p/style-{i}.jsonl"), "/work/atlas", false);
            let key = character_key(POSE, &agent, &art, scale);
            let Some(style) = key.dress.and_then(|d| d.style) else {
                continue;
            };
            if !worn.insert(style) {
                continue;
            }
            let figure = office_figure(&art, &agent);
            (width, tallest) = (figure.width(), tallest.max(figure.height()));
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
            let capped = subagent(&agent, lead);
            let (portrait, _) = art.portrait(&capped);
            sizes.insert((portrait.width(), portrait.height()));
            let (top, cap) = (headroom(&art, &portrait), office_cap(&capped));
            assert!(
                cap.keys().all(|&(x, y)| {
                    (0..i32::from(portrait.width())).contains(&x)
                        && (0..i32::from(portrait.height())).contains(&(y + top))
                }),
                "agent {i}'s cap leaves the canvas"
            );
        }
        assert_eq!(sizes, [(width, tallest)].into());
    }

    /// At its figure's density, not the pack's densest: here only a sign has 8x art.
    #[test]
    fn a_portrait_is_at_the_density_its_figure_is_drawn_at() {
        let row = vec!["r"; 40].join(" ");
        let sign = format!("@frame 0\n{}", vec![row; 24].join("\n"));
        let art = crate::pack::test_office_with(
            "[animations.\"exit_sign@8x\"]\nframes=[\"sign8.sprite\"]\nframe_ms=600\n",
            &[("sign8.sprite", sign.as_str())],
        );
        let scale = RenderScale::from(art.max_density_variant());
        let standing = densest_frame(&art, POSE.anim_name, POSE.frame_idx, scale);
        assert!(
            art.max_density_variant() > standing.density,
            "premise: the fixture's densest outruns its standing figure"
        );
        let agent = make_slot_cwd("/p/dense.jsonl", "/work/atlas", false);
        let figure = office_figure(&art, &agent);
        let (portrait, density) = art.portrait(&agent);
        assert_eq!(density, standing.density);
        assert_eq!(
            (portrait.width(), portrait.height()),
            (figure.width(), figure.height())
        );
    }
}
