//! Per-pose sprite top-left + breath bob + walking-position helpers.
//!
//! Pure geometry — no `RgbBuffer`, no rendering.

use std::time::SystemTime;

use crate::layout::{Pivot, SEAT_RENDER_Y_OFF, SceneLayout, Size, WALKING_Y_OFF};
use pixtuoid_core::AgentSlot;

use crate::layout::{Point, WaypointKind};
use crate::pose;
use crate::sim::seat::Seat;

use crate::layout::CHARACTER_SPRITE_W;

/// Where a desk's occupant RENDERS — the desk's seat cell put through the same
/// `Seat` model every other seat uses, so the chair, its occupant and the walk
/// that ends there cannot drift apart.
pub fn seated_top_left(desk: Point, sprite_w: u16, facing: crate::layout::Facing) -> Point {
    Seat::at_desk(desk, facing).render_top_left(sprite_w)
}

pub(crate) fn walking_top_left(p: Point, sprite_w: u16) -> Point {
    Point {
        x: p.x.saturating_sub(sprite_w / 2),
        y: p.y.saturating_sub(WALKING_Y_OFF),
    }
}

pub(crate) fn waypoint_top_left(wp: Point, sprite_w: u16) -> Point {
    Point {
        x: wp.x.saturating_sub(sprite_w / 2),
        y: wp.y.saturating_sub(WALKING_Y_OFF),
    }
}

/// One-pixel vertical bob on a `CYCLE_MS` cycle with a per-agent phase offset, so
/// static (seated / standing) characters look alive instead of frozen.
fn breath_offset_y(agent_id: pixtuoid_core::AgentId, beat: crate::anim::Beat) -> u16 {
    let elapsed_ms = beat.ms();
    const CYCLE_MS: u64 = 4500;
    let offset_ms = agent_id.raw() % CYCLE_MS;
    let phase = elapsed_ms.wrapping_add(offset_ms) % CYCLE_MS;
    if phase < CYCLE_MS / 2 { 0 } else { 1 }
}

pub(crate) fn with_breath(
    top_left: Point,
    agent_id: pixtuoid_core::AgentId,
    beat: crate::anim::Beat,
) -> Point {
    Point {
        x: top_left.x,
        y: top_left.y.saturating_sub(breath_offset_y(agent_id, beat)),
    }
}

/// Top-left of a back-view sitter on a mirror_vertical'd couch — higher than a
/// front-view seat's because `back_couch.sprite` has no transparent
/// head/face area (hair extends across all top rows), so sitting it lower
/// overlaps the couch back row.
pub(crate) fn back_couch_top_left(wp: Point, sprite_w: u16) -> Point {
    Point {
        x: wp.x.saturating_sub(sprite_w / 2),
        y: wp.y.saturating_sub(SEAT_RENDER_Y_OFF),
    }
}

/// Nudge a sprite so the whole frame lands inside the canvas, answering in the
/// SAME pivot space `pos` came in.
///
/// It moves a figure's paint top-left, never its sim position (invariant #6).
pub(crate) fn keep_sprite_on_canvas(pivot: Pivot, pos: Point, size: Size, buf: Size) -> Point {
    match pivot {
        // `min` before `max`: on a buffer narrower than the sprite the lower
        // bound wins instead of `clamp`'s inverted-range panic.
        Pivot::Center => Point {
            x: pos
                .x
                .min(buf.w.saturating_sub(size.w.div_ceil(2)))
                .max(size.w / 2),
            y: pos
                .y
                .min(buf.h.saturating_sub(size.h.div_ceil(2)))
                .max(size.h / 2),
        },
        // No lower bound needed — `u16` already floors a top-left `pos` at 0.
        Pivot::TopLeft => Point {
            x: pos.x.min(buf.w.saturating_sub(size.w)),
            y: pos.y.min(buf.h.saturating_sub(size.h)),
        },
    }
}

/// `pos`, in `pivot` space, moved so a `size` frame lands on `layout`'s canvas:
/// the one fit every figure's placement takes.
pub(crate) fn on_canvas(layout: &SceneLayout, pivot: Pivot, pos: Point, size: Size) -> Point {
    let canvas = Size {
        w: layout.buf_w,
        h: layout.buf_h,
    };
    keep_sprite_on_canvas(pivot, pos, size, canvas)
}

/// How far a later arrival steps aside along x so two agents at one
/// stand-beside spot don't render on top of each other: a sprite's width and a
/// pixel of daylight.
const STEP_ASIDE_DX: i16 = CHARACTER_SPRITE_W as i16 + 1;

/// X-offset applied to a waypoint top-left when multiple agents land at the
/// SAME waypoint in the same cycle. rank 0 = first arrival (no offset); later
/// arrivals step aside.
///
/// An EXCLUSIVE spot never steps aside: sliding an occupant sideways off a
/// discrete slot renders them on thin air. Gating on `exclusive` — the one authority
/// for "single-occupancy destination" — covers every seat, the stand-beside
/// singles, and anything added later without a second list to keep in sync.
/// Shareable spots (pantry counter / vending / printer / snack shelf) still step
/// aside; queueing is the intent there.
pub(crate) fn waypoint_rank_offset_x(kind: WaypointKind, rank: usize) -> i16 {
    if crate::layout::furniture_def(kind.furniture()).exclusive {
        return 0;
    }
    match rank {
        1 => STEP_ASIDE_DX,
        2 => -STEP_ASIDE_DX,
        _ => 0,
    }
}

/// Where a figure of `size` drawn at `top_left` hangs its name badge: over its
/// top-centre, and no lower than `ceiling`.
pub(crate) fn badge_anchor(top_left: Point, size: Size, ceiling: Option<u16>) -> Point {
    Point {
        x: top_left.x + size.w / 2,
        y: ceiling.map_or(top_left.y, |row| top_left.y.min(row)),
    }
}

/// A test oracle: the sprite's default-size top-left, re-derived from the pose
/// alone.
#[cfg(test)]
pub(crate) fn character_top_left(
    agent: &AgentSlot,
    layout: &crate::layout::SceneLayout,
    now: SystemTime,
    rctx: &mut pose::RouteCtx<'_>,
) -> Option<Point> {
    let desk = layout.home_desk(agent.desk_index.single_floor_local())?;
    use crate::pose::Pose;
    let pose = pose::derive_with_routing(agent, now, layout, rctx)?;
    let w = CHARACTER_SPRITE_W;
    let top_left = match pose {
        Pose::SeatedIdle | Pose::SeatedThinking | Pose::SeatedTyping => {
            seated_top_left(desk.at, w, desk.facing)
        }
        Pose::AtWaypoint { wp, kind } => {
            let wp_obj = layout.waypoints.get(wp)?;
            let stand = layout.stand_point(wp_obj.kind, wp_obj.pos, desk.at, wp_obj.facing);
            // Via [`Seat::render_top_left`], the sprite blit's authority.
            Seat::at_waypoint(kind, stand, wp_obj.facing).render_top_left(w)
        }
        Pose::AimlessAt { dest: at } | Pose::Held { at } => waypoint_top_left(at, w),
        Pose::Walking { segment, .. } => walking_top_left(segment.at(), w),
    };
    Some(on_canvas(
        layout,
        Pivot::TopLeft,
        top_left,
        Size {
            w,
            h: crate::layout::CHARACTER_SPRITE_H,
        },
    ))
}

/// How long the elevator's open/close transition takes, used as both the opening
/// ramp at the START of an agent's entry/exit window and the closing ramp at the
/// END. 200 ms feels snappy without being abrupt.
const DOOR_TRANSITION_MS: u64 = 200;

/// Compute the elevator door frame (0=closed, 1=half, 2=open) from the agents
/// currently in flight. Stateless: we take the MAX across agents so the door is
/// at least as open as the most-in-progress one needs.
///
/// `door_anim_max_ms` is the per-floor cached maximum entry/exit physics
/// duration; it falls back to `ENTRY_ANIMATION_MS` when zero (before any entry
/// walk is in flight).
pub(crate) fn compute_door_frame_idx(
    agents: &[AgentSlot],
    now: SystemTime,
    door_anim_max_ms: u64,
) -> usize {
    fn frame_for_progress(elapsed_ms: u64, total_ms: u64) -> usize {
        if elapsed_ms < DOOR_TRANSITION_MS {
            if elapsed_ms < DOOR_TRANSITION_MS / 2 {
                1
            } else {
                2
            }
        } else if elapsed_ms + DOOR_TRANSITION_MS > total_ms {
            let remaining = total_ms.saturating_sub(elapsed_ms);
            if remaining < DOOR_TRANSITION_MS / 2 {
                0
            } else {
                1
            }
        } else {
            2
        }
    }
    let entry_window_ms = if door_anim_max_ms > 0 {
        door_anim_max_ms
    } else {
        pose::ENTRY_ANIMATION_MS
    };

    let mut max_frame: usize = 0;
    for a in agents {
        if a.exiting_at.is_none()
            && let Ok(d) = now.duration_since(a.created_at)
        {
            let ms = d.as_millis() as u64;
            if ms < entry_window_ms {
                max_frame = max_frame.max(frame_for_progress(ms, entry_window_ms));
            }
        }
        if let Some(exit_at) = a.exiting_at
            && let Ok(d) = now.duration_since(exit_at)
        {
            let ms = d.as_millis() as u64;
            // The same window the reducer uses to GC exiting slots, so the
            // door closes right as the agent's slot disappears.
            let exit_window_ms =
                pixtuoid_core::state::reducer::EXIT_GRACE_WINDOW.as_millis() as u64;
            if ms < exit_window_ms {
                max_frame = max_frame.max(frame_for_progress(ms, exit_window_ms));
            }
        }
    }
    max_frame
}
