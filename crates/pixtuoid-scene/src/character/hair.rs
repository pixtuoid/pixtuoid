//! Per-agent hairstyles: which of the pack's `[hairstyles]` an agent wears, and
//! a character frame dressed in it, its layers laid mark on mark over the
//! frame's head. A frame is dressed only at a density of 2 and up
//! ([`dress_for`]), so the classic 1x art never is.

use crate::pack::OfficeArt;
use pixtuoid_core::AgentId;
use pixtuoid_core::id::{fnv1a, splitmix64};
use pixtuoid_core::sprite::format::{Density, Hairstyle};
use pixtuoid_core::sprite::{Frame, HeadMark, Pixel, Rgb, Sprite};

/// Separates the pick's seed from the other per-agent draws that finalize the
/// same id through `splitmix64` (`physics`, `pose::pure`), so the style is no
/// function of theirs.
const HAIRSTYLE_SALT: u64 = 0x6861_6972_7374_796c;

/// The name of the style `agent` wears: the one that scores highest under
/// rendezvous hashing, so adding a style to a pack moves only the agents that
/// now prefer it. It is picked by name, whatever the density, so an agent keeps
/// their style when the renderer lands on another.
fn pick(pack: &OfficeArt, agent: AgentId) -> Option<&str> {
    let seed = splitmix64(splitmix64(agent.raw() ^ HAIRSTYLE_SALT));
    pack.hairstyles()
        .map(Hairstyle::name)
        .max_by_key(|name| splitmix64(seed ^ fnv1a(name.bytes().map(u64::from))))
}

/// How a character frame is dressed: its head, the style its agent wears at
/// its density where the pack draws that style there, and where the dressed
/// figure's top lands.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Dress {
    pub(crate) head: HeadMark,
    pub(crate) style: Option<String>,
    /// The dressed figure's topmost drawn row, counted from the frame's top:
    /// negative where it rises over it.
    pub(crate) crest: i32,
}

/// The [`Dress`] of `body`, frame `head`'s art at `density`, for `agent`:
/// `None` at 1x or on an unmarked frame, which is drawn as it is.
pub(crate) fn dress_for(
    pack: &OfficeArt,
    agent: AgentId,
    body: &Frame,
    head: Option<HeadMark>,
    density: Density,
) -> Option<Dress> {
    let head = head.filter(|_| density.get() > 1)?;
    let style = pick(pack, agent).and_then(|name| pack.hairstyle(name, density));
    Some(Dress::of(body, head, style))
}

impl Dress {
    /// `body`, its head `head`, dressed in `style`.
    pub(crate) fn of(body: &Frame, head: HeadMark, style: Option<&Hairstyle>) -> Self {
        Dress {
            head,
            style: style.map(|s| s.name().to_owned()),
            crest: crest(body, head, style),
        }
    }

    /// How many art rows the dressed frame rises over the frame as drawn.
    pub(crate) fn rise(&self) -> u16 {
        u16::try_from((-self.crest).max(0)).unwrap_or(0)
    }
}

/// The first row of `f` holding an opaque pixel.
fn opaque_top(f: &Frame) -> Option<u16> {
    (0..f.height()).find(|&y| (0..f.width()).any(|x| f.get(x, y).copied().flatten().is_some()))
}

/// [`Dress::crest`] of `body` dressed in `style`: its hair's top or its own,
/// and the outline's row above that.
fn crest(body: &Frame, head: HeadMark, style: Option<&Hairstyle>) -> i32 {
    let hair = style
        .and_then(|s| s.layers(head.view))
        .into_iter()
        .flat_map(|layers| {
            [layers.behind(), layers.over()]
                .into_iter()
                .flatten()
                .filter_map(|layer| layer.laid_on(head))
                .filter_map(|(f, _, dy)| Some(dy + i32::from(opaque_top(f)?)))
        });
    let top = hair.chain(opaque_top(body).map(i32::from)).min();
    top.map_or(0, |top| top - 1)
}

/// `body` dressed as `dress` says: `style`'s behind layer for its head's view,
/// the body, the over layer, then one `outline` round the union, so no line runs
/// between hair and face. A bare head, with no style or none for its view, gets
/// the outline alone. The layers take the body's own recolor `overrides`, so the
/// hair wears the agent's colour. The frame grows [`Dress::rise`] rows upward;
/// its width is the body's.
pub(crate) fn dress(
    body: &Frame,
    dress: &Dress,
    style: Option<&Hairstyle>,
    overrides: &[(char, Pixel)],
    line: Rgb,
) -> Frame {
    let (head, up) = (dress.head, dress.rise());
    let layers = style.and_then(|s| s.layers(head.view));
    let (w, h) = (body.width(), body.height().saturating_add(up));
    let mut px: Vec<Pixel> = vec![None; usize::from(w) * usize::from(h)];
    let mut lay = |f: &Frame, dx: i32, dy: i32| {
        for y in 0..f.height() {
            for x in 0..f.width() {
                let Some(c) = f.get(x, y).copied().flatten() else {
                    continue;
                };
                let (tx, ty) = (i32::from(x) + dx, i32::from(y) + dy + i32::from(up));
                if let (Ok(tx), Ok(ty)) = (u16::try_from(tx), u16::try_from(ty))
                    && tx < w
                    && ty < h
                {
                    px[usize::from(ty) * usize::from(w) + usize::from(tx)] = Some(c);
                }
            }
        }
    };
    let dressed = |layer: Option<&Sprite>| {
        let sprite = layer?;
        let (_, dx, dy) = sprite.laid_on(head)?;
        Some((sprite.recolorable_at(0).recolored(overrides), dx, dy))
    };
    if let Some((f, dx, dy)) = dressed(layers.and_then(|l| l.behind())) {
        lay(&f, dx, dy);
    }
    lay(body, 0, 0);
    if let Some((f, dx, dy)) = dressed(layers.and_then(|l| l.over())) {
        lay(&f, dx, dy);
    }
    let (w_i, h_i) = (i32::from(w), i32::from(h));
    let idx = |x: i32, y: i32| y as usize * usize::from(w) + x as usize;
    let opaque = |px: &[Pixel], x: i32, y: i32| {
        (0..w_i).contains(&x) && (0..h_i).contains(&y) && px[idx(x, y)].is_some()
    };
    let cells = || (0..h_i).flat_map(move |y| (0..w_i).map(move |x| (x, y)));
    let edge: Vec<usize> = cells()
        .filter(|&(x, y)| {
            !opaque(&px, x, y)
                && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(dx, dy)| opaque(&px, x + dx, y + dy))
        })
        .map(|(x, y)| idx(x, y))
        .collect();
    for i in edge {
        px[i] = Some(line);
    }
    // A gap the line closes to one pixel takes the line too: a style laid
    // on a body it was not drawn against can leave one between chin and
    // hair, and no author can see it to close it.
    let holes: Vec<usize> = cells()
        .filter(|&(x, y)| {
            !opaque(&px, x, y)
                && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .all(|(dx, dy)| opaque(&px, x + dx, y + dy))
        })
        .map(|(x, y)| idx(x, y))
        .collect();
    for i in holes {
        px[i] = Some(line);
    }
    Frame::from_pixels(w, h, px)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::pen::test_density;
    use pixtuoid_core::sprite::format::{Piece, load_filled_pack};
    use strum::VariantArray as _;

    const H: Rgb = Rgb {
        r: 200,
        g: 100,
        b: 50,
    };
    const S: Rgb = Rgb {
        r: 240,
        g: 192,
        b: 160,
    };
    const LINE: Rgb = Rgb { r: 1, g: 1, b: 1 };
    const TWO: Density = test_density(2);

    /// A pack of `body` marked `head` (the `head.front` mark's column and row),
    /// outlined, and the styles `styles` at 2x, each a front-view over layer of
    /// one pixel a row above its mark.
    fn pack_of(body: &str, head: (u16, u16), styles: &[&str]) -> OfficeArt {
        let mut toml = String::from(
            "[palette]\n\".\"=\"transparent\"\n\"H\"=\"#c86432\"\n\"S\"=\"#f0c0a0\"\n\
             \"k\"=\"#010101\"\n[characters]\noutline=\"k\"\n\
             [animations.seated]\nframes=[\"b.sprite\"]\nframe_ms=100\n",
        );
        for s in styles {
            toml.push_str(&format!(
                "[hairstyles.\"{s}@2x\"]\nfront={{ over=\"o.sprite\" }}\n"
            ));
        }
        let body = format!("@frame 0\n@mark head.front {} {}\n{body}", head.0, head.1);
        crate::pack::test_office_with(
            &toml,
            &[
                ("b.sprite", body.as_str()),
                ("o.sprite", "@frame 0\n@mark head.front 0 1\nH\n. \n"),
            ],
        )
    }

    /// The one-skin-pixel body the tests dress.
    const BODY: &str = ". . .\n. . .\n. S .\n. . .\n";

    /// `pack`'s body dressed as it would be for `agent` at 2x.
    fn dressed(pack: &OfficeArt, agent: AgentId) -> (Dress, Frame) {
        let sprite = pack.piece(Piece::Seated);
        let body = sprite.first();
        let dress = dress_for(pack, agent, body, sprite.head(0), TWO).expect("a marked 2x frame");
        let style = dress.style.as_deref().and_then(|n| pack.hairstyle(n, TWO));
        let f = super::dress(
            body,
            &dress,
            style,
            &[('H', Some(H))],
            pack.character_outline(),
        );
        (dress, f)
    }

    #[test]
    fn a_pick_is_stable_and_adding_a_style_only_moves_who_prefers_it() {
        let few = pack_of(BODY, (1, 0), &["mop", "bun", "crop"]);
        let more = pack_of(BODY, (1, 0), &["mop", "bun", "crop", "curls"]);
        let mut moved = 0;
        for i in 0..200u64 {
            let id = AgentId::from_parts("claude-code", &i.to_string());
            let before = pick(&few, id).expect("a style");
            assert_eq!(pick(&few, id), Some(before), "stable");
            let after = pick(&more, id).expect("a style");
            if after != before {
                assert_eq!(after, "curls", "an agent only ever moves to the new style");
                moved += 1;
            }
        }
        assert!(
            (20..=80).contains(&moved),
            "about a quarter move to the new style: {moved}"
        );
    }

    #[test]
    fn a_frame_is_dressed_only_at_2x_and_up_and_where_it_marks_a_head() {
        let pack = pack_of(BODY, (1, 0), &["mop"]);
        let sprite = pack.piece(Piece::Seated);
        let body = sprite.first();
        let agent = AgentId::from_parts("x", "y");
        assert!(dress_for(&pack, agent, body, sprite.head(0), Density::ONE).is_none());
        assert!(dress_for(&pack, agent, body, None, TWO).is_none());
        let four = test_density(4);
        let bare = dress_for(&pack, agent, body, sprite.head(0), four).expect("marked at 4x");
        assert_eq!(bare.style, None, "no style at 4x: the head is bare");
    }

    #[test]
    fn a_dressed_frame_lays_the_hair_mark_on_mark_and_outlines_the_union() {
        let pack = pack_of(BODY, (1, 0), &["mop"]);
        let (dress, f) = dressed(&pack, AgentId::from_parts("x", "y"));
        // The layer's pixel sits a row above its mark, so above the body's top:
        // the frame rises a row for the hair and one more for its outline.
        assert_eq!(dress.rise(), 2);
        assert_eq!((f.width(), f.height()), (3, 6));
        let at = |x, y| f.get(x, y).copied().flatten();
        assert_eq!(
            at(1, 1),
            Some(H),
            "the hair, in the agent's colour, over the mark"
        );
        assert_eq!(at(1, 0), Some(LINE), "the outline above the hair");
        assert_eq!(at(0, 1), Some(LINE), "and beside it");
        assert_eq!(at(1, 4), Some(S), "the body, moved down with it");
        assert_eq!(
            at(1, 3),
            Some(LINE),
            "one outline round hair and body alike"
        );
    }

    #[test]
    fn a_bare_head_is_outlined_as_a_dressed_one_is() {
        let pack = pack_of(". S .\n. . .\n", (1, 0), &[]);
        let (dress, f) = dressed(&pack, AgentId::from_parts("x", "y"));
        assert_eq!(
            (dress.style.clone(), dress.rise()),
            (None, 1),
            "a row for the line alone"
        );
        let at = |x, y| f.get(x, y).copied().flatten();
        assert_eq!(at(1, 1), Some(S));
        assert_eq!(
            (at(1, 0), at(0, 1), at(1, 2)),
            (Some(LINE), Some(LINE), Some(LINE))
        );
    }

    #[test]
    fn a_gap_the_outline_closes_to_one_pixel_takes_the_line() {
        // Four pixels two apart round a centre none of them touches: the line
        // round each walls the centre in.
        let pack = pack_of(
            ". . S . .\n. . . . .\nS . . . S\n. . . . .\n. . S . .\n",
            (2, 4),
            &["mop"],
        );
        let (dress, f) = dressed(&pack, AgentId::from_parts("x", "y"));
        assert_eq!(
            f.get(2, 2 + dress.rise()).copied().flatten(),
            Some(LINE),
            "the centre, walled in"
        );
    }

    #[test]
    fn dress_drops_hair_past_a_side_as_validation_flags_and_past_the_bottom_unflagged() {
        let (side, mark) = (4u16, 1u16);
        let reach = 2 * side + 1;
        let rows = |n: u16, key: &dyn Fn(u16, u16) -> &'static str| -> String {
            (0..n)
                .map(|y| (0..n).map(|x| key(x, y)).collect::<Vec<_>>().join(" ") + "\n")
                .collect()
        };
        let body = format!(
            "@frame 0\n@mark head.front {mark} {mark}\n{}",
            rows(side, &|_, _| "S")
        );
        let toml = "[palette]\n\".\"=\"transparent\"\n\"H\"=\"#c86432\"\n\"S\"=\"#f0c0a0\"\n\
                    [animations.seated]\nframes=[\"one.sprite\"]\nframe_ms=100\n\
                    [animations.\"seated@2x\"]\nframes=[\"b.sprite\"]\nframe_ms=100\n\
                    [hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n";
        let (mut flagged_seen, mut bottom_seen) = (false, false);
        for hy in 0..reach {
            for hx in 0..reach {
                let pixel = |x, y| if (x, y) == (hx, hy) { "H" } else { "." };
                let hair = format!(
                    "@frame 0\n@mark head.front {side} {side}\n{}",
                    rows(reach, &pixel)
                );
                let pack = load_filled_pack(
                    toml,
                    &[
                        ("one.sprite", "@frame 0\nS S\nS S"),
                        ("b.sprite", &body),
                        ("o.sprite", &hair),
                    ],
                )
                .expect("the test pack loads");
                let sprite = &pack.variants_of(Piece::Seated)[&test_density(2)];
                let (frame, head) = (sprite.first(), sprite.head(0).expect("marked"));
                let style = pack.hairstyles().next();
                let f = dress(
                    frame,
                    &Dress::of(frame, head, style),
                    style,
                    &[],
                    pack.character_outline(),
                );
                let dropped = !(0..f.height())
                    .any(|y| (0..f.width()).any(|x| f.get(x, y).copied().flatten() == Some(H)));
                let flagged = !crate::pack::validate::validate_pack(&pack)
                    .overhanging_hair
                    .is_empty();
                let below = hy + mark >= side + side;
                flagged_seen |= flagged;
                bottom_seen |= below && !flagged;
                assert_eq!(dropped, flagged || below, "hair at ({hx}, {hy})");
            }
        }
        assert!(flagged_seen && bottom_seen);
    }

    /// Every bundled character frame, bare and dressed in every bundled style,
    /// keeps its one outline whole: nothing but the line on a side edge, where
    /// the line has no column left to run in, and no pinhole inside it. A
    /// dressed frame first exists here, at runtime, so no generator check can
    /// see it.
    #[test]
    fn every_bundled_character_dressed_in_every_style_keeps_its_outline_whole() {
        let pack = crate::pack::test_office();
        let line = pack.character_outline();
        let (mut dressed, mut flaws) = (0, Vec::new());
        let arts = Piece::VARIANTS
            .iter()
            .filter(|p| p.kind() == pixtuoid_core::sprite::format::PieceKind::Character)
            .flat_map(|&p| {
                std::iter::once((p.name().to_owned(), pack.piece(p))).chain(
                    pack.variants_of(p)
                        .iter()
                        .map(move |(d, s)| (format!("{}@{d}x", p.name()), s)),
                )
            });
        for (name, sprite) in arts {
            for (i, body) in sprite.frames().iter().enumerate() {
                let Some(head) = sprite.head(i) else {
                    continue;
                };
                let styles = pack.hairstyles().map(Some).chain([None]);
                for style in styles {
                    let d = Dress::of(body, head, style);
                    let f = dress(body, &d, style, &[], line);
                    let (w, h) = (f.width(), f.height());
                    let at = |x: u16, y: u16| f.get(x, y).copied().flatten();
                    let mut flaw = |what: &str, x: u16, y: u16| {
                        flaws.push(format!(
                            "{name}[{i}] in {}: {what} at ({x}, {y})",
                            style.map_or("no style", Hairstyle::name)
                        ));
                    };
                    for y in 0..h {
                        for x in [0, w - 1] {
                            if at(x, y).is_some() && at(x, y) != Some(line) {
                                flaw("unoutlined on the edge", x, y);
                            }
                        }
                    }
                    for y in 1..h - 1 {
                        for x in 1..w - 1 {
                            let walled = [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
                                .iter()
                                .all(|&(nx, ny)| at(nx, ny).is_some());
                            if at(x, y).is_none() && walled {
                                flaw("a pinhole", x, y);
                            }
                        }
                    }
                    dressed += 1;
                }
            }
        }
        assert!(flaws.is_empty(), "{flaws:#?}");
        assert!(dressed > 0, "the bundled pack dresses its characters");
    }
}
