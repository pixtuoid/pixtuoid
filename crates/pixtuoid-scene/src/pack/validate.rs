//! Test-only: the bundled pack must have no finding.

use std::collections::BTreeSet;
use std::num::NonZeroU16;

use crate::pack::{Hairstyle, Pack, Piece};
use crate::sprite::format::{Density, density_variant_name};
use crate::sprite::{Frame, HeadMark, HeadView, Sprite};
use strum::VariantArray as _;

use super::lookup;

/// What a [`Piece`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PieceKind {
    /// A character pose, which a hairstyle dresses.
    Character,
    /// Furniture or a fixture of the room.
    Furniture,
    /// A pet or a gateway mascot: it stands on its feet wherever its frame
    /// ends, so it is kept apart from the furniture.
    Creature,
}

impl Piece {
    /// What it is.
    pub(crate) fn kind(self) -> PieceKind {
        use Piece::*;
        match self {
            Seated | Typing | Standing | Walking | WalkingBack | SeatedSleeping
            | SeatedSleepingAlt | HoldingCoffee | BackCouch | WalkingCoffee | SideSeated
            | SeatedBack | TypingBack => PieceKind::Character,
            CatWalk | CatSit | CatSleep | DogWalk | DogSit | DogSleep | LobsterWalk
            | LobsterRest => PieceKind::Creature,
            _ => PieceKind::Furniture,
        }
    }
}

/// A character frame a hairstyle would dress but for its missing head mark: it
/// is drawn bare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnmarkedHead {
    /// The animation, e.g. `standing@4x`.
    pub(crate) name: String,
    /// Its first unmarked frame, counted from 0 in the order the
    /// pack loads them: every `@frame` block of each file its `frames` lists.
    pub(crate) frame: usize,
}

/// A view a hairstyle leaves out and a head at its density faces: that head is
/// drawn bare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingHairView {
    /// The style's key, e.g. `mop@4x`.
    pub(crate) style: String,
    /// The view.
    pub(crate) view: HeadView,
    /// The first animation, by name, with such a head.
    pub(crate) name: String,
}

/// A hairstyle view with a layer reaching past a character frame's sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HairOverhang {
    /// The style's key, e.g. `mop@4x`.
    pub(crate) style: String,
    /// The view.
    pub(crate) view: HeadView,
    /// The first animation, by name, it overhangs.
    pub(crate) name: String,
    /// That animation's first such frame.
    pub(crate) frame: usize,
}

/// What a loaded pack draws as its author may not have meant: a pack that
/// can't draw at all does not load ([`PackError`](crate::pack::PackError)).
#[derive(Debug, Default)]
pub(crate) struct ValidationReport {
    /// One per character animation.
    pub(crate) unmarked_heads: Vec<UnmarkedHead>,
    /// One per hairstyle view.
    pub(crate) missing_hair_views: Vec<MissingHairView>,
    /// One per hairstyle view.
    pub(crate) overhanging_hair: Vec<HairOverhang>,
    /// Each hairstyle key at a density the pack draws no character at.
    pub(crate) orphan_hairstyles: Vec<String>,
    /// Each density variant timed apart from its base: a renderer times a
    /// variant by its base, so the variant's own timing never plays.
    pub(crate) unread_variant_timing: Vec<UnreadTiming>,
    /// Each of the caller's loops whose frames don't hold whole beats: the
    /// beat skips or stretches one.
    pub(crate) off_beat_loops: Vec<OffBeatLoop>,
}

/// A density variant's timing that differs from its base's, which is the
/// timing that plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnreadTiming {
    /// The variant, e.g. `typing@4x`.
    pub(crate) name: String,
    /// The field it sets apart.
    pub(crate) field: UnreadField,
}

/// The timing field an [`UnreadTiming`] sets apart, with both values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnreadField {
    /// [`Sprite::frame_ms`].
    FrameMs {
        /// The base's, which plays.
        base: u32,
        /// The variant's.
        variant: u32,
    },
    /// [`Sprite::stride`].
    Stride {
        /// The base's, which plays.
        base: Option<NonZeroU16>,
        /// The variant's.
        variant: NonZeroU16,
    },
}

/// A loop whose `frame_ms` is not a whole number of the caller's beats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OffBeatLoop {
    /// The animation.
    pub(crate) name: String,
    /// Its `frame_ms`.
    pub(crate) frame_ms: u32,
}

impl ValidationReport {
    /// How many findings the report holds.
    pub(crate) fn finding_count(&self) -> usize {
        // No `..`: a new report field must be counted here before this compiles.
        let ValidationReport {
            unmarked_heads,
            missing_hair_views,
            overhanging_hair,
            orphan_hairstyles,
            unread_variant_timing,
            off_beat_loops,
        } = self;
        unmarked_heads.len()
            + missing_hair_views.len()
            + overhanging_hair.len()
            + orphan_hairstyles.len()
            + unread_variant_timing.len()
            + off_beat_loops.len()
    }
}

/// Check a loaded pack's variants' timing, its hairstyles against the
/// characters they dress, and `loops` against `beat_ms`.
fn validate_loops(pack: &Pack, loops: &[(Piece, usize)], beat_ms: u64) -> ValidationReport {
    let mut report = ValidationReport {
        off_beat_loops: loops
            .iter()
            .map(|&(piece, start)| (piece, start, pack.piece(piece)))
            .filter(|(piece, start, a)| a.frames().len() > start + 1 && piece.walk().is_none())
            .filter(|(_, _, a)| {
                u64::from(a.frame_ms())
                    .checked_rem(beat_ms)
                    .is_some_and(|r| r != 0)
            })
            .map(|(piece, _, a)| OffBeatLoop {
                name: piece.name().to_string(),
                frame_ms: a.frame_ms(),
            })
            .collect(),
        ..ValidationReport::default()
    };

    for (piece, density, variant) in pack.variants() {
        let base = pack.piece(piece);
        let name = density_variant_name(piece.name(), density);
        // a single-frame base never steps, so no painter reads its `frame_ms`
        if base.frames().len() > 1 && variant.frame_ms() != base.frame_ms() {
            report.unread_variant_timing.push(UnreadTiming {
                name: name.clone(),
                field: UnreadField::FrameMs {
                    base: base.frame_ms(),
                    variant: variant.frame_ms(),
                },
            });
        }
        if let Some(stride) = variant.stride().filter(|&s| Some(s) != base.stride()) {
            report.unread_variant_timing.push(UnreadTiming {
                name,
                field: UnreadField::Stride {
                    base: base.stride(),
                    variant: stride,
                },
            });
        }
    }

    let characters: Vec<(String, &Sprite, Density)> = pack
        .variants()
        .filter(|(piece, ..)| piece.kind() == PieceKind::Character)
        .map(|(piece, density, sprite)| {
            (density_variant_name(piece.name(), density), sprite, density)
        })
        .collect();
    let dressed_densities: BTreeSet<Density> = pack.hairstyles().map(Hairstyle::density).collect();
    for (name, sprite, density) in &characters {
        let (sprite, density) = (*sprite, *density);
        if !dressed_densities.contains(&density) {
            continue;
        }
        if let Some(frame) = (0..sprite.frames().len()).find(|&i| sprite.head(i).is_none()) {
            report.unmarked_heads.push(UnmarkedHead {
                name: name.clone(),
                frame,
            });
        }
    }
    for style in pack.hairstyles() {
        let density = style.density();
        let key = density_variant_name(style.name(), density);
        let mut dressed = characters.iter().filter(|c| c.2 == density).peekable();
        if dressed.peek().is_none() {
            report.orphan_hairstyles.push(key);
            continue;
        }
        let (mut missing, mut overhung) = (Vec::new(), Vec::new());
        for (name, sprite, _) in dressed {
            for (frame, body) in sprite.frames().iter().enumerate() {
                let Some(head) = sprite.head(frame) else {
                    continue;
                };
                let Some(layers) = style.layers(head.view) else {
                    if !missing.contains(&head.view) {
                        missing.push(head.view);
                        report.missing_hair_views.push(MissingHairView {
                            style: key.clone(),
                            view: head.view,
                            name: name.clone(),
                        });
                    }
                    continue;
                };
                let clipped = [layers.behind(), layers.over()]
                    .into_iter()
                    .flatten()
                    .any(|layer| overhangs(layer, body, head));
                if clipped && !overhung.contains(&head.view) {
                    overhung.push(head.view);
                    report.overhanging_hair.push(HairOverhang {
                        style: key.clone(),
                        view: head.view,
                        name: name.clone(),
                        frame,
                    });
                }
            }
        }
    }

    report
}

/// Whether `layer`, [laid on](Sprite::laid_on) `head`, puts an opaque pixel
/// past `body`'s sides: a dressed frame grows up, never wider. Not its bottom,
/// where the body's art ends at whatever hides the rest (`back_couch`'s seat back).
fn overhangs(layer: &Sprite, body: &Frame, head: HeadMark) -> bool {
    let Some((art, dx, _)) = layer.laid_on(head) else {
        return false;
    };
    (0..art.height()).any(|y| {
        (0..art.width()).any(|x| {
            let tx = i32::from(x) + dx;
            art.get(x, y).copied().flatten().is_some() && (tx < 0 || tx >= i32::from(body.width()))
        })
    })
}
/// Every piece the painters loop on the beat, each with the frame its loop
/// starts at: the looping fixtures, the appliances' busy loops
/// ([`appliance_frame_index`](super::lookup::appliance_frame_index)), the typists (`pose::typing_frame`), and every
/// creature pose that is not a walk.
fn looped_animations() -> Vec<(Piece, usize)> {
    let appliances =
        [Piece::VendingMachine, Piece::Printer].map(|p| (p, lookup::APPLIANCE_IDLE_FRAMES));
    let creatures = Piece::VARIANTS
        .iter()
        .copied()
        .filter(|p| p.kind() == PieceKind::Creature);
    [
        Piece::FishTank,
        Piece::WaterCooler,
        Piece::Typing,
        Piece::TypingBack,
    ]
    .into_iter()
    .chain(creatures)
    .map(|p| (p, 0))
    .chain(appliances)
    .collect()
}

/// [`validate_loops`], against the loops this crate's painters play
/// on the Full beat.
pub(crate) fn validate_pack(pack: &Pack) -> ValidationReport {
    validate_loops(pack, &looped_animations(), crate::anim::FULL_TICK_MS)
}

#[cfg(test)]
mod tests {
    use crate::pack::load_filled_pack;

    use super::*;

    /// A pack of `animations` over the filler, whose frame files are `frames`.
    fn pack_with_frames(animations: &str, frames: &[(&str, &str)]) -> Pack {
        load_filled_pack(
            &format!("[palette]\n\"A\"=\"#010203\"\n{animations}"),
            frames,
        )
        .expect("pack builds")
    }

    fn pack_with(animations: &str) -> Pack {
        pack_with_frames(animations, &[("f.sprite", "@frame 0\nA")])
    }

    /// 1x1 and 2x2 base frames and a 4x4 of the 1x1.
    const SIZED_FRAMES: &[(&str, &str)] = &[
        ("one.sprite", "@frame 0\nA"),
        ("two.sprite", "@frame 0\nA A\nA A"),
        (
            "four.sprite",
            "@frame 0\nA A A A\nA A A A\nA A A A\nA A A A",
        ),
    ];

    /// A 1x `standing` and its 2x redraw `body`, dressed by `hairstyles`.
    fn dressed_pack(body: &str, hairstyles: &str, hair: &[(&str, &str)]) -> Pack {
        let mut frames = vec![("one.sprite", "@frame 0\nA"), ("body.sprite", body)];
        frames.extend_from_slice(hair);
        load_filled_pack(
            &format!(
                "[palette]\n\"A\"=\"#010203\"\n\".\"=\"transparent\"\n\
                 [animations.standing]\nframes=[\"one.sprite\"]\nframe_ms=100\n\
                 [animations.\"standing@2x\"]\nframes=[\"body.sprite\"]\nframe_ms=100\n\
                 {hairstyles}"
            ),
            &frames,
        )
        .expect("pack builds")
    }

    const FRONT_BODY: &str = "@frame 0\n@mark head.front 0 0\nA A\nA A";

    const MOP: &str = "[hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n";

    fn hair_findings(pack: &Pack) -> ValidationReport {
        validate_loops(pack, &[], 0)
    }

    /// `report`'s findings past those of the same pack undressed.
    fn hair_finding_count(report: &ValidationReport) -> usize {
        let bare = validate_loops(&dressed_pack(FRONT_BODY, "", &[]), &[], 0);
        report.finding_count() - bare.finding_count()
    }

    /// A variant timed apart from its base is reported, each field its own
    /// finding; one that keeps its base's timing, or leaves the stride to it,
    /// is not, and nor is a single-frame base's `frame_ms`, which nothing steps.
    #[test]
    fn a_variant_timed_apart_from_its_base_is_reported() {
        let pack = pack_with_frames(
            "[animations.walking]\nframes=[\"one.sprite\", \"one.sprite\"]\nframe_ms=100\nstride=2\n\
             [animations.\"walking@2x\"]\nframes=[\"two.sprite\", \"two.sprite\"]\nframe_ms=200\nstride=3\n\
             [animations.desk]\nframes=[\"one.sprite\"]\nframe_ms=600\n\
             [animations.\"desk@2x\"]\nframes=[\"two.sprite\"]\nframe_ms=100\n\
             [animations.typing]\nframes=[\"one.sprite\", \"one.sprite\"]\nframe_ms=100\n\
             [animations.\"typing@2x\"]\nframes=[\"two.sprite\", \"two.sprite\"]\nframe_ms=100\n\
             [animations.\"typing@4x\"]\nframes=[\"four.sprite\", \"four.sprite\"]\nframe_ms=100\nstride=2\n",
            SIZED_FRAMES,
        );
        let report = validate_loops(&pack, &[], 0);
        let findings = report.finding_count();
        let stride = |n| NonZeroU16::new(n).expect("nonzero");
        assert_eq!(
            report.unread_variant_timing,
            vec![
                UnreadTiming {
                    name: "typing@4x".to_string(),
                    field: UnreadField::Stride {
                        base: None,
                        variant: stride(2),
                    },
                },
                UnreadTiming {
                    name: "walking@2x".to_string(),
                    field: UnreadField::FrameMs {
                        base: 100,
                        variant: 200,
                    },
                },
                UnreadTiming {
                    name: "walking@2x".to_string(),
                    field: UnreadField::Stride {
                        base: Some(stride(2)),
                        variant: stride(3),
                    },
                },
            ]
        );
        let timed_alike = ValidationReport {
            unread_variant_timing: Vec::new(),
            ..report
        };
        assert_eq!(timed_alike.finding_count() + 3, findings);
    }

    /// A caller's loop off its beat is reported; one on it, a
    /// one-frame one, a strided one, one whose loop past its still frames is a
    /// single frame, and any the caller doesn't loop are not.
    #[test]
    fn a_loop_off_the_beat_is_reported() {
        let pack = pack_with(
            "[animations.typing]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=400\n\
             [animations.typing_back]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=250\n\
             [animations.fish_tank]\nframes=[\"f.sprite\"]\nframe_ms=300\n\
             [animations.cat_walk]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=300\nstride=2\n\
             [animations.vending_machine]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=300\n",
        );
        let report = validate_loops(
            &pack,
            &[
                (Piece::Typing, 0),
                (Piece::TypingBack, 0),
                (Piece::FishTank, 0),
                (Piece::CatWalk, 0),
                (Piece::VendingMachine, 1),
            ],
            125,
        );
        assert_eq!(
            report.off_beat_loops,
            vec![OffBeatLoop {
                name: "typing".to_string(),
                frame_ms: 400,
            }]
        );
    }

    /// Pins [`ValidationReport::finding_count`]: one finding in every field.
    #[test]
    fn every_finding_is_counted_once() {
        let report = ValidationReport {
            unmarked_heads: vec![UnmarkedHead {
                name: "standing@4x".to_string(),
                frame: 0,
            }],
            missing_hair_views: vec![MissingHairView {
                style: "mop@4x".to_string(),
                view: HeadView::Back,
                name: "walking_back@4x".to_string(),
            }],
            overhanging_hair: vec![HairOverhang {
                style: "mop@4x".to_string(),
                view: HeadView::Front,
                name: "standing@4x".to_string(),
                frame: 0,
            }],
            orphan_hairstyles: vec!["mop@2x".to_string()],
            unread_variant_timing: vec![UnreadTiming {
                name: "typing@4x".to_string(),
                field: UnreadField::FrameMs {
                    base: 125,
                    variant: 250,
                },
            }],
            off_beat_loops: vec![OffBeatLoop {
                name: "typing".to_string(),
                frame_ms: 400,
            }],
        };
        assert_eq!(report.finding_count(), 6);
    }

    #[test]
    fn a_style_that_fits_every_head_it_dresses_is_clean() {
        let above_top = "@frame 0\n@mark head.front 0 1\nA\nA";
        let report = hair_findings(&dressed_pack(FRONT_BODY, MOP, &[("o.sprite", above_top)]));
        assert!(report.unmarked_heads.is_empty(), "{report:?}");
        assert!(report.missing_hair_views.is_empty(), "{report:?}");
        assert!(report.overhanging_hair.is_empty(), "{report:?}");
        assert!(report.orphan_hairstyles.is_empty(), "{report:?}");
    }

    #[test]
    fn a_frame_the_styles_would_dress_without_a_head_mark_is_reported() {
        let hair = ("o.sprite", "@frame 0\n@mark head.front 0 0\nA");
        let bald = "@frame 0\nA A\nA A";
        let report = hair_findings(&dressed_pack(bald, MOP, &[hair]));
        assert_eq!(
            report.unmarked_heads,
            vec![UnmarkedHead {
                name: "standing@2x".into(),
                frame: 0
            }]
        );
        assert_eq!(hair_finding_count(&report), 1);

        let undressed = hair_findings(&dressed_pack(bald, "", &[]));
        assert!(undressed.unmarked_heads.is_empty(), "no style to dress it");
    }

    #[test]
    fn a_style_without_a_view_a_head_faces_is_reported() {
        let hair = ("o.sprite", "@frame 0\n@mark head.front 0 0\nA");
        let back = "@frame 0\n@mark head.back 0 0\nA A\nA A";
        let report = hair_findings(&dressed_pack(back, MOP, &[hair]));
        assert_eq!(
            report.missing_hair_views,
            vec![MissingHairView {
                style: "mop@2x".into(),
                view: HeadView::Back,
                name: "standing@2x".into(),
            }]
        );
        assert_eq!(hair_finding_count(&report), 1);
    }

    #[test]
    fn hair_past_a_frames_side_is_reported() {
        for (hair, what) in [
            ("@frame 0\n@mark head.front 0 0\nA A A", "right"),
            ("@frame 0\n@mark head.front 1 0\nA A", "left"),
        ] {
            let report = hair_findings(&dressed_pack(FRONT_BODY, MOP, &[("o.sprite", hair)]));
            assert_eq!(
                report.overhanging_hair,
                vec![HairOverhang {
                    style: "mop@2x".into(),
                    view: HeadView::Front,
                    name: "standing@2x".into(),
                    frame: 0,
                }],
                "{what}"
            );
            assert_eq!(hair_finding_count(&report), 1);
        }
        for (hair, what) in [
            (
                "@frame 0\n@mark head.front 0 0\nA . .",
                "transparent padding",
            ),
            ("@frame 0\n@mark head.front 0 0\nA\nA\nA", "past the bottom"),
        ] {
            let report = hair_findings(&dressed_pack(FRONT_BODY, MOP, &[("o.sprite", hair)]));
            assert!(report.overhanging_hair.is_empty(), "{what}");
        }
    }

    #[test]
    fn a_style_at_a_density_no_character_is_drawn_at_is_reported() {
        let hair = ("o.sprite", "@frame 0\n@mark head.front 0 0\nA");
        let four = "[hairstyles.\"mop@4x\"]\nfront={ over=\"o.sprite\" }\n";
        let report = hair_findings(&dressed_pack(FRONT_BODY, four, &[hair]));
        assert_eq!(report.orphan_hairstyles, vec!["mop@4x".to_string()]);
        assert_eq!(hair_finding_count(&report), 1);
    }
}
