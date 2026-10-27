//! The sprite effects, pixel-free: which ride on whom this frame, where, and
//! how far along their animation they are. A painter draws each its own way;
//! none decides when one shows or which step it is at.

use crate::anim::Beat;
use crate::layout::Point;

pub(crate) mod look;

/// What an effect is, for the painter choosing its look. Each says what its
/// [`Effect::phase`] counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EffectKind {
    /// A "z" rising off a sleeper's head; ms into its rise of
    /// [`SLEEP_Z_RISE_MS`].
    SleepZ,
    /// The waiting-for-you mark over a figure; still.
    WaitingMark,
    /// The puff a walker's stride kicks up; the foot it rises under
    /// ([`planted_foot`]).
    WalkingDust,
    /// The flame crowning a burning head; which of its two frames shows.
    FlameCrown,
    /// One heart rising off a petted pet; ms into its [`HEART_LIFE_MS`].
    PetHeart,
    /// One puff of coffee steam; ms into its [`STEAM_CYCLE_MS`].
    SteamPuff,
    /// One bubble over a busy gateway mascot; rows it has risen.
    MascotBubble,
}

impl EffectKind {
    /// Whether it lies on the floor under its owner, so paints before it.
    pub(crate) fn beneath(self) -> bool {
        matches!(self, Self::WalkingDust)
    }
}

/// One effect this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Effect {
    pub(crate) kind: EffectKind,
    /// Where it rides, in layout units: the point its kind's look is drawn
    /// from.
    pub(crate) at: Point,
    /// How far along its animation it is, in the unit its [`EffectKind`] names.
    pub(crate) phase: u64,
}

/// How long a sleep z rises, fading, before it rests unseen for
/// [`SLEEP_Z_REST_MS`].
pub(crate) const SLEEP_Z_RISE_MS: u64 = 2000;
const SLEEP_Z_REST_MS: u64 = 400;
const SLEEP_Z_CYCLE_MS: u64 = SLEEP_Z_RISE_MS + SLEEP_Z_REST_MS;

/// The z over a sleeper whose head is at `at`, or `None` while it rests. The
/// `seed` staggers sleepers so their z's don't rise in lockstep.
pub(crate) fn sleep_z(at: Point, seed: u64, beat: Beat) -> Option<Effect> {
    let phase = beat.ms().wrapping_add(seed % SLEEP_Z_CYCLE_MS) % SLEEP_Z_CYCLE_MS;
    (phase < SLEEP_Z_RISE_MS).then_some(Effect {
        kind: EffectKind::SleepZ,
        at,
        phase,
    })
}

/// The waiting mark over the figure at `at`.
pub(crate) fn waiting_mark(at: Point) -> Effect {
    Effect {
        kind: EffectKind::WaitingMark,
        at,
        phase: 0,
    }
}

/// The dust under the walker at `at`, at its planted foot `foot` ([`planted_foot`]).
pub(crate) fn walking_dust(at: Point, foot: usize) -> Effect {
    Effect {
        kind: EffectKind::WalkingDust,
        at,
        phase: foot as u64,
    }
}

/// The foot, east (0) or west (1) as drawn, that a walk on `frame` of
/// `frames`, mirrored when `flip`, bears its weight on: the art's east foot
/// from the strike that opens its cycle, its west from the one halfway.
pub(crate) fn planted_foot(frame: usize, frames: usize, flip: bool) -> usize {
    usize::from(frame * 2 >= frames) ^ usize::from(flip)
}

/// How long the flame crown holds each of its two frames: whole Full beats,
/// so it flickers evenly.
const FLAME_FLICKER_MS: u64 = 2 * crate::anim::FULL_TICK_MS;

/// The flame crowning a `width`-wide figure whose frame's top-left is
/// `top_left`: it stands on the head's top row, centred on the frame.
pub(crate) fn flame_crown(top_left: Point, width: u16, beat: Beat) -> Effect {
    Effect {
        kind: EffectKind::FlameCrown,
        at: Point {
            x: top_left.x + width / 2,
            y: top_left.y,
        },
        // Integer division: epoch-ms as f32 loses precision and freezes the flicker.
        phase: (beat.ms() / FLAME_FLICKER_MS) % 2,
    }
}

/// One puff's rise, from its spout to gone.
pub(crate) const STEAM_CYCLE_MS: u64 = 1800;
/// Puffs in flight at once, spaced evenly through [`STEAM_CYCLE_MS`].
pub(crate) const STEAM_PUFFS: usize = 3;

/// The steam rising from a spout at `at`.
pub(crate) fn steam(at: Point, beat: Beat) -> [Effect; STEAM_PUFFS] {
    let elapsed = beat.ms();
    let spacing = STEAM_CYCLE_MS / STEAM_PUFFS as u64;
    std::array::from_fn(|puff| Effect {
        kind: EffectKind::SteamPuff,
        at,
        phase: (elapsed + puff as u64 * spacing) % STEAM_CYCLE_MS,
    })
}

/// One heart's life, from the pet to gone.
pub(crate) const HEART_LIFE_MS: u64 = 1550;
/// How much later each heart leaves than the one before.
const HEART_STAGGER_MS: u64 = 150;
const HEARTS: u64 = 4;
/// Columns between neighbouring hearts.
const HEART_SPACING: i32 = 2;
const _: () = assert!(
    (HEARTS - 1) * HEART_STAGGER_MS + HEART_LIFE_MS <= crate::pet::PET_DURATION_MS,
    "the last heart must finish while the petting plays"
);

/// The hearts over a pet at `pet`, `petted_ms` into its petting: those in
/// flight, each spread a column pair from the last and centred on the pet.
pub(crate) fn pet_hearts(pet: Point, petted_ms: u64) -> impl Iterator<Item = Effect> {
    (0..HEARTS).filter_map(move |i| {
        let phase = petted_ms.checked_sub(i * HEART_STAGGER_MS)?;
        if phase >= HEART_LIFE_MS {
            return None;
        }
        let dx = i as i32 * HEART_SPACING - (HEARTS as i32 - 1);
        Some(Effect {
            kind: EffectKind::PetHeart,
            at: Point {
                x: (i32::from(pet.x) + dx).max(0) as u16,
                y: pet.y,
            },
            phase,
        })
    })
}

/// How long a mascot's bubble holds each row of its rise: one Full beat, so
/// it never skips a row.
const BUBBLE_STEP_MS: u64 = crate::anim::FULL_TICK_MS;
/// The rows a bubble rises before it starts again.
const BUBBLE_RISE_ROWS: u64 = 6;
/// Steps between neighbouring bubbles' rises.
const BUBBLE_STAGGER: u64 = 7;
const MAX_BUBBLES: u32 = 4;
/// Columns between neighbouring bubbles.
const BUBBLE_SPACING: u16 = 2;

/// The bubbles over a busy gateway mascot centred at `pos`, its frame
/// `frame_h` tall, with `turns` in flight: one per turn over a baseline of one,
/// capped, rising from just above its head.
pub(crate) fn mascot_bubbles(
    pos: Point,
    frame_h: u16,
    turns: u32,
    beat: Beat,
) -> impl Iterator<Item = Effect> {
    let step = beat.ms() / BUBBLE_STEP_MS;
    let top = pos.y.saturating_sub(frame_h / 2 + 1);
    // Below `MAX_BUBBLES`, a u32 that fits a u16.
    let n = turns.saturating_add(1).min(MAX_BUBBLES) as u16;
    (0..n).map(move |i| Effect {
        kind: EffectKind::MascotBubble,
        at: Point {
            x: (pos.x + i * BUBBLE_SPACING).saturating_sub(n),
            y: top,
        },
        phase: (step + u64::from(i) * BUBBLE_STAGGER) % BUBBLE_RISE_ROWS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Point = Point { x: 20, y: 30 };

    fn at_ms(ms: u64) -> Beat {
        Beat::at_ms(ms)
    }

    #[test]
    fn a_sleep_z_rises_then_rests_unseen() {
        assert_eq!(sleep_z(AT, 0, at_ms(0)).map(|e| e.phase), Some(0));
        assert_eq!(
            sleep_z(AT, 0, at_ms(SLEEP_Z_RISE_MS - 1)).map(|e| e.phase),
            Some(SLEEP_Z_RISE_MS - 1)
        );
        for ms in SLEEP_Z_RISE_MS..SLEEP_Z_CYCLE_MS {
            assert_eq!(sleep_z(AT, 0, at_ms(ms)), None, "resting at {ms} ms");
        }
        assert_eq!(
            sleep_z(AT, 0, at_ms(SLEEP_Z_CYCLE_MS)).map(|e| e.phase),
            Some(0),
            "a fresh z after the rest"
        );
    }

    #[test]
    fn a_sleep_zs_seed_staggers_it() {
        let seed = 500;
        assert_eq!(
            sleep_z(AT, seed, at_ms(0)).map(|e| e.phase),
            Some(seed),
            "the seed shifts its phase"
        );
        assert_eq!(
            sleep_z(AT, seed + SLEEP_Z_CYCLE_MS, at_ms(0)),
            sleep_z(AT, seed, at_ms(0)),
            "a whole cycle of seed changes nothing"
        );
    }

    #[test]
    fn the_crown_flips_frame_every_flicker() {
        let frame = |ms| flame_crown(AT, 8, at_ms(ms)).phase;
        assert_eq!(frame(0), 0);
        assert_eq!(frame(FLAME_FLICKER_MS - 1), 0);
        assert_eq!(frame(FLAME_FLICKER_MS), 1);
        assert_eq!(frame(2 * FLAME_FLICKER_MS), 0);
        // Far past where an f32 epoch would have frozen it.
        let late = u64::from(u32::MAX) * FLAME_FLICKER_MS;
        assert_ne!(frame(late), frame(late + FLAME_FLICKER_MS));
        assert_eq!(
            flame_crown(AT, 8, at_ms(0)).at,
            Point {
                x: AT.x + 4,
                y: AT.y
            },
            "it stands centred on the head's top row"
        );
    }

    #[test]
    fn hearts_leave_staggered_and_expire() {
        let live = |ms| pet_hearts(AT, ms).map(|e| e.phase).collect::<Vec<_>>();
        assert_eq!(live(0), [0], "the first heart leaves at once");
        assert_eq!(live(HEART_STAGGER_MS), [HEART_STAGGER_MS, 0]);
        assert_eq!(
            live(HEART_LIFE_MS),
            [
                HEART_LIFE_MS - HEART_STAGGER_MS,
                HEART_LIFE_MS - 2 * HEART_STAGGER_MS,
                HEART_LIFE_MS - 3 * HEART_STAGGER_MS
            ],
            "the first has expired"
        );
        assert!(live(crate::pet::PET_DURATION_MS).is_empty(), "all gone");
        let xs: Vec<u16> = pet_hearts(AT, 3 * HEART_STAGGER_MS)
            .map(|e| e.at.x)
            .collect();
        assert_eq!(
            xs,
            [AT.x - 3, AT.x - 1, AT.x + 1, AT.x + 3],
            "spread evenly"
        );
        assert!(
            pet_hearts(Point { x: 0, y: 5 }, 0).all(|e| e.at.x == 0),
            "held on the canvas at its west edge"
        );
    }

    #[test]
    fn steam_puffs_are_spaced_a_third_of_a_cycle() {
        let third = STEAM_CYCLE_MS / 3;
        for ms in [0, 1, third + 7, STEAM_CYCLE_MS - 1, 1_700_000_000_123] {
            let puffs = steam(AT, at_ms(ms));
            for pair in puffs.windows(2) {
                assert_eq!(
                    (pair[1].phase + STEAM_CYCLE_MS - pair[0].phase) % STEAM_CYCLE_MS,
                    third,
                    "at {ms} ms"
                );
            }
            assert!(puffs.iter().all(|p| p.phase < STEAM_CYCLE_MS && p.at == AT));
        }
    }

    #[test]
    fn a_mascot_bubbles_once_per_turn_over_one_capped() {
        let count = |turns| mascot_bubbles(AT, 12, turns, at_ms(0)).count();
        assert_eq!(count(1), 2);
        assert_eq!(count(2), 3);
        assert_eq!(count(u32::MAX), MAX_BUBBLES as usize);
        let rows: Vec<u64> = mascot_bubbles(AT, 12, 3, at_ms(0))
            .map(|e| e.phase)
            .collect();
        assert_eq!(rows, [0, 1, 2, 3], "each a stagger's rows behind the last");
        let later: Vec<u64> = mascot_bubbles(AT, 12, 3, at_ms(BUBBLE_STEP_MS))
            .map(|e| e.phase)
            .collect();
        assert_eq!(later, [1, 2, 3, 4], "a step later each has risen a row");
        assert!(
            mascot_bubbles(AT, 12, 3, at_ms(0)).all(|e| e.at.y == AT.y - 7),
            "they rise from just above the head"
        );
    }
}
