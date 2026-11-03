//! What a frame answers a pointer with: each figure's hit box, in paint order.
//! Only a later hover covers one; nothing else a frame draws does.

use pixtuoid_core::AgentId;
use pixtuoid_core::source::daemon::DaemonInstanceKey;

use crate::layout::{Bounds, Pivot, Point, Size, anchored_top_left};
use crate::pet::PetKind;

/// A figure's hit box, in LOGICAL units whatever the scale, and who it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hover {
    pub(crate) at: Bounds,
    pub(crate) target: HoverTarget,
}

impl Hover {
    /// A figure of `size` placed at `pos` by `pivot`, where the painters place
    /// its art ([`anchored_top_left`]).
    pub(crate) fn figure(pivot: Pivot, pos: Point, size: Size, target: HoverTarget) -> Self {
        let tl = anchored_top_left(pivot, pos, size.w, size.h);
        Self {
            at: Bounds {
                x: tl.x,
                y: tl.y,
                width: size.w,
                height: size.h,
            },
            target,
        }
    }
}

/// Who a hover names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverTarget {
    /// An agent's character.
    Agent(AgentId),
    /// The floor's pet.
    Pet(PetHover),
    /// A gateway's mascot.
    Mascot(DaemonInstanceKey),
}

/// The pet as hovered: what its tooltip says and where petting holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PetHover {
    /// Which pet.
    pub kind: PetKind,
    /// Its centre, in logical units.
    pub centre: Point,
    /// The animation it shows.
    pub anim: crate::pack::Piece,
}

/// One frame's hovers, back to front.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Hovers(Vec<Hover>);

impl FromIterator<Hover> for Hovers {
    fn from_iter<I: IntoIterator<Item = Hover>>(hovers: I) -> Self {
        Self(hovers.into_iter().collect())
    }
}

impl Hovers {
    /// List `hover` over every hover listed before it.
    pub(crate) fn push(&mut self, hover: Hover) {
        self.0.push(hover);
    }

    pub(crate) fn listed(&self) -> &[Hover] {
        &self.0
    }

    /// Who the topmost hover meeting `area` names: the last painted. `area` is
    /// in logical units.
    pub(crate) fn at(&self, area: Bounds) -> Option<&HoverTarget> {
        self.0
            .iter()
            .rev()
            .find(|h| h.at.overlaps(area))
            .map(|h| &h.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(n: u8) -> HoverTarget {
        HoverTarget::Agent(AgentId::from_transcript_path(&format!("/hover/{n}.jsonl")))
    }

    fn cell(x: u16, y: u16) -> Bounds {
        Bounds {
            x,
            y,
            width: 1,
            height: 2,
        }
    }

    #[test]
    fn the_topmost_hover_over_an_area_is_the_last_painted() {
        let size = Size { w: 4, h: 6 };
        let mut hovers = Hovers::default();
        hovers.push(Hover::figure(
            Pivot::TopLeft,
            Point { x: 10, y: 10 },
            size,
            agent(0),
        ));
        hovers.push(Hover::figure(
            Pivot::TopLeft,
            Point { x: 12, y: 12 },
            size,
            agent(1),
        ));
        assert_eq!(hovers.at(cell(13, 13)), Some(&agent(1)), "both meet it");
        assert_eq!(hovers.at(cell(10, 10)), Some(&agent(0)), "only the first");
        assert_eq!(hovers.at(cell(30, 30)), None, "neither");
        let empty = Bounds {
            width: 0,
            ..cell(13, 13)
        };
        assert_eq!(hovers.at(empty), None, "an empty area");
    }
}
