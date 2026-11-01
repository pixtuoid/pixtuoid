//! A pointer's gestures over the office, for every painter: a press on what
//! the frame shows, then a click or a drag. A painter turns its own events
//! into [`Pointer::down`] / [`Pointer::moved`] / [`Pointer::up`] in layout
//! units and carries out the [`Gesture`]s; what a press affords, a click or a
//! lift, is [`Affordance`]'s.

use std::time::SystemTime;

use pixtuoid_core::AgentId;
use pixtuoid_core::source::daemon::DaemonInstanceKey;

use crate::display::{HoverTarget, PetHover};
use crate::hit::{HitAction, SceneHit};
use crate::layout::Point;
use crate::pet::{PetKind, PetState};

/// How far a press may wander, in layout units on each axis, and still be a
/// click: past it, it lifts what it pressed, and a release there clicks
/// nothing. The painter's, whose pointer moves in its own steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slop {
    pub x: u16,
    pub y: u16,
}

impl Slop {
    fn exceeded(self, from: Point, to: Point) -> bool {
        from.x.abs_diff(to.x) >= self.x || from.y.abs_diff(to.y) >= self.y
    }
}

/// A figure a pointer can lift.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Figure {
    Agent(AgentId),
    Pet(PetKind),
    Mascot(DaemonInstanceKey),
}

impl Figure {
    /// The figure `target` shows.
    pub(crate) fn of(target: &HoverTarget) -> Self {
        match target {
            HoverTarget::Agent(id) => Self::Agent(*id),
            &HoverTarget::Pet(PetHover { kind, .. }) => Self::Pet(kind),
            HoverTarget::Mascot(key) => Self::Mascot(key.clone()),
        }
    }
}

/// What a press on a hit affords: the action a click there carries out and
/// the figure a drag lifts, either, both or neither. [`Pointer::down`] acts
/// on it, and a painter's cursor shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Affordance {
    pub click: Option<HitAction>,
    pub lift: Option<Figure>,
}

impl Affordance {
    /// What a press on `hit` affords at `now`, with `petting` the last one.
    pub fn of(hit: Option<SceneHit<'_>>, petting: Option<&PetState>, now: SystemTime) -> Self {
        Self {
            click: hit.and_then(|hit| hit.action(petting, now)),
            lift: match hit {
                Some(SceneHit::Figure(target)) => Some(Figure::of(target)),
                _ => None,
            },
        }
    }

    /// What the press lands on.
    pub(crate) fn pressed(&self) -> Pressed {
        if self.click.is_some() || self.lift.is_some() {
            Pressed::Something
        } else {
            Pressed::Bare
        }
    }
}

/// What a press landed on, which a painter acts on at once: a press on the
/// bare office is the floating window's to drag the window by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressed {
    /// A figure, or a link: a click or a drag follows.
    Something,
    /// Nothing that answers a pointer.
    Bare,
}

/// A press: what it landed on, and the carry it ended, set down where it was
/// last carried, which the painter hands its floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Down {
    pub pressed: Pressed,
    pub ended: Option<Gesture>,
}

/// What a pointer's event amounts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gesture {
    /// Released where it pressed: carry out the hit's action.
    Click(HitAction),
    /// Moved far enough on a figure to pick it up, the pointer at `at`.
    Lift { figure: Figure, at: Point },
    /// The lifted figure follows the pointer to `at`.
    Carry(Point),
    /// Released, carrying: set the figure down at `at`.
    Drop(Point),
}

/// Which floor a gesture belongs to, for a painter of several: a lift to the
/// floor showing, unless a slide shows none; its carry and drop to the floor
/// it lifted on, whatever shows since.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GripFloor(Option<usize>);

impl GripFloor {
    /// The floor `gesture` goes to as `nav` shows them; `None` for a click,
    /// or with nothing lifted.
    pub(crate) fn of(&mut self, gesture: &Gesture, nav: &crate::floor::FloorNav) -> Option<usize> {
        match gesture {
            Gesture::Lift { .. } => {
                self.0 = nav.showing();
                self.0
            }
            Gesture::Carry(_) => self.0,
            Gesture::Drop(_) => self.0.take(),
            Gesture::Click(_) => None,
        }
    }
}

/// A pointer's grip on one floor, which the floor's next step carries out:
/// every figure it set down since that step, oldest first, then the one it
/// holds. Lossless, so however many gestures come between two steps, each
/// drop lands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Grip {
    pub(crate) dropped: Vec<(Figure, Point)>,
    pub(crate) held: Option<(Figure, Point)>,
}

impl Grip {
    /// `gesture`, after those before it. A carry or a drop moves the figure
    /// in hand, if any; a hand holds one, so a lift sets down one still in it.
    pub(crate) fn push(&mut self, gesture: &Gesture) {
        match gesture {
            Gesture::Lift { figure, at } => {
                self.dropped.extend(self.held.take());
                self.held = Some((figure.clone(), *at));
            }
            &Gesture::Carry(to) => {
                if let Some((_, at)) = &mut self.held {
                    *at = to;
                }
            }
            &Gesture::Drop(to) => {
                if let Some((figure, _)) = self.held.take() {
                    self.dropped.push((figure, to));
                }
            }
            Gesture::Click(_) => {}
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum State {
    #[default]
    Up,
    Pressed {
        at: Point,
        slop: Slop,
        figure: Option<Figure>,
        action: Option<HitAction>,
    },
    /// Carrying a figure, last to `at`.
    Carrying { at: Point },
}

/// One pointer's gesture in progress.
#[derive(Debug, Clone, Default)]
pub struct Pointer {
    state: State,
}

impl Pointer {
    /// A press at `at` on `hit`, with `petting` the last petting, at `now`.
    pub fn down(
        &mut self,
        hit: Option<SceneHit<'_>>,
        at: Point,
        slop: Slop,
        petting: Option<&PetState>,
        now: SystemTime,
    ) -> Down {
        // A carry whose release never came — the window lost focus, the
        // terminal dropped the mouse-up — sets its figure down first.
        let ended = self.cancel();
        let affords = Affordance::of(hit, petting, now);
        let pressed = affords.pressed();
        self.state = match pressed {
            Pressed::Something => State::Pressed {
                at,
                slop,
                figure: affords.lift,
                action: affords.click,
            },
            Pressed::Bare => State::Up,
        };
        Down { pressed, ended }
    }

    /// End the gesture without a release, as when the window loses focus: a
    /// figure carried lands where it was last carried, and a press clicks
    /// nothing.
    pub fn cancel(&mut self) -> Option<Gesture> {
        match std::mem::take(&mut self.state) {
            State::Carrying { at } => Some(Gesture::Drop(at)),
            State::Up | State::Pressed { .. } => None,
        }
    }

    /// The pointer moved to `at`.
    pub fn moved(&mut self, at: Point) -> Option<Gesture> {
        match &self.state {
            State::Pressed {
                at: from,
                slop,
                figure: Some(figure),
                ..
            } if slop.exceeded(*from, at) => {
                let figure = figure.clone();
                self.state = State::Carrying { at };
                Some(Gesture::Lift { figure, at })
            }
            State::Carrying { .. } => {
                self.state = State::Carrying { at };
                Some(Gesture::Carry(at))
            }
            State::Up | State::Pressed { .. } => None,
        }
    }

    /// The press released at `at`, or off the office: there a press clicks
    /// nothing, and a figure carried lands where it was last carried.
    pub fn up(&mut self, at: Option<Point>) -> Option<Gesture> {
        match std::mem::take(&mut self.state) {
            // Released where it pressed, give or take the slop.
            State::Pressed {
                at: from,
                slop,
                action,
                ..
            } => action
                .filter(|_| at.is_some_and(|at| !slop.exceeded(from, at)))
                .map(Gesture::Click),
            State::Carrying { at: last } => Some(Gesture::Drop(at.unwrap_or(last))),
            State::Up => None,
        }
    }

    /// Whether a figure is lifted: its tooltip and the window's hover rest.
    pub fn carrying(&self) -> bool {
        matches!(self.state, State::Carrying { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: u16, y: u16) -> Point {
        Point { x, y }
    }

    const SLOP: Slop = Slop { x: 2, y: 2 };

    /// A press on an agent clicks where it releases, and lifts it once it
    /// moves past the jitter: then it carries, and drops on release.
    #[test]
    fn a_press_on_a_figure_clicks_or_drags_it() {
        let now = SystemTime::UNIX_EPOCH;
        let id = AgentId::from_parts("claude-code", "s");
        let agent = HoverTarget::Agent(id);
        let mut p = Pointer::default();
        assert_eq!(
            p.down(Some(SceneHit::Figure(&agent)), pt(10, 10), SLOP, None, now)
                .pressed,
            Pressed::Something
        );
        assert_eq!(p.moved(pt(11, 10)), None, "within the jitter");
        assert_eq!(
            p.up(Some(pt(11, 10))),
            Some(Gesture::Click(HitAction::Focus(id)))
        );

        p.down(Some(SceneHit::Figure(&agent)), pt(10, 10), SLOP, None, now);
        assert_eq!(
            p.moved(pt(10, 10 + SLOP.y)),
            Some(Gesture::Lift {
                figure: Figure::Agent(id),
                at: pt(10, 10 + SLOP.y)
            })
        );
        assert!(p.carrying());
        assert_eq!(p.moved(pt(30, 40)), Some(Gesture::Carry(pt(30, 40))));
        assert_eq!(p.up(Some(pt(31, 41))), Some(Gesture::Drop(pt(31, 41))));
        assert!(!p.carrying());
        assert_eq!(p.up(Some(pt(31, 41))), None, "one release per press");
        p.down(Some(SceneHit::Figure(&agent)), pt(10, 10), SLOP, None, now);
        p.moved(pt(10, 20));
        p.moved(pt(12, 24));
        assert_eq!(
            p.up(None),
            Some(Gesture::Drop(pt(12, 24))),
            "released off the office, it lands where it was last carried"
        );
        p.down(Some(SceneHit::Figure(&agent)), pt(10, 10), SLOP, None, now);
        assert_eq!(
            p.up(None),
            None,
            "a press released off the office clicks nothing"
        );
    }

    /// A carry whose release never came ends at the next press, or when the
    /// painter cancels it, setting its figure down where it was last carried.
    #[test]
    fn a_carry_without_its_release_still_sets_the_figure_down() {
        let now = SystemTime::UNIX_EPOCH;
        let agent = HoverTarget::Agent(AgentId::from_parts("claude-code", "s"));
        let mut p = Pointer::default();
        p.down(Some(SceneHit::Figure(&agent)), pt(10, 10), SLOP, None, now);
        p.moved(pt(20, 20));
        let down = p.down(None, pt(50, 50), SLOP, None, now);
        assert_eq!(
            down.ended,
            Some(Gesture::Drop(pt(20, 20))),
            "the next press sets it down"
        );
        assert!(!p.carrying());
        p.down(Some(SceneHit::Figure(&agent)), pt(10, 10), SLOP, None, now);
        p.moved(pt(30, 30));
        assert_eq!(
            p.cancel(),
            Some(Gesture::Drop(pt(30, 30))),
            "a cancel sets it down"
        );
        assert_eq!(p.cancel(), None, "once");
        assert_eq!(
            p.up(Some(pt(30, 30))),
            None,
            "and its release, late, does nothing"
        );
    }

    /// A floor's grip loses no drop between two steps and holds one figure:
    /// a stray carry or drop moves nothing, and a lift over a figure still
    /// in hand sets that one down where it is.
    #[test]
    fn a_grip_keeps_every_drop_and_holds_one_figure() {
        let (a, b) = (Figure::Pet(PetKind::Cat), Figure::Pet(PetKind::Dog));
        let mut grip = Grip::default();
        grip.push(&Gesture::Lift {
            figure: a.clone(),
            at: pt(1, 1),
        });
        grip.push(&Gesture::Carry(pt(2, 2)));
        grip.push(&Gesture::Drop(pt(3, 3)));
        grip.push(&Gesture::Carry(pt(9, 9)));
        grip.push(&Gesture::Drop(pt(9, 9)));
        grip.push(&Gesture::Lift {
            figure: b.clone(),
            at: pt(4, 4),
        });
        grip.push(&Gesture::Lift {
            figure: a.clone(),
            at: pt(5, 5),
        });
        assert_eq!(
            grip,
            Grip {
                dropped: vec![(a.clone(), pt(3, 3)), (b, pt(4, 4))],
                held: Some((a, pt(5, 5))),
            }
        );
    }

    /// A link clicks where it is released in place, but never lifts, and the
    /// bare office answers nothing: it is the painter's to drag the window by.
    #[test]
    fn a_link_only_clicks_and_the_bare_office_is_the_painters() {
        let now = SystemTime::UNIX_EPOCH;
        let mut p = Pointer::default();
        assert_eq!(
            p.down(Some(SceneHit::Star), pt(5, 5), SLOP, None, now)
                .pressed,
            Pressed::Something
        );
        assert_eq!(p.moved(pt(50, 50)), None, "a link does not lift");
        assert_eq!(
            p.up(Some(pt(50, 50))),
            None,
            "released away, it clicks nothing"
        );
        p.down(Some(SceneHit::Star), pt(5, 5), SLOP, None, now);
        assert_eq!(
            p.up(Some(pt(6, 5))),
            Some(Gesture::Click(HitAction::Open(crate::hit::REPO_URL))),
            "released in place, within the slop"
        );
        assert_eq!(
            p.down(Some(SceneHit::Furniture("Desk")), pt(5, 5), SLOP, None, now)
                .pressed,
            Pressed::Bare
        );
        assert_eq!(
            p.down(None, pt(5, 5), SLOP, None, now).pressed,
            Pressed::Bare
        );
        assert_eq!(p.moved(pt(50, 50)), None);
        assert_eq!(p.up(Some(pt(50, 50))), None);
    }

    /// A pet mid-petting has no click, but still lifts.
    #[test]
    fn a_pet_mid_petting_still_lifts() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let pet = HoverTarget::Pet(PetHover {
            kind: PetKind::Cat,
            centre: pt(0, 0),
            anim: crate::pack::Piece::CatSit,
        });
        let playing = PetState {
            petted_at: now,
            kind: PetKind::Cat,
            floor_idx: 0,
        };
        let mut p = Pointer::default();
        assert_eq!(
            p.down(
                Some(SceneHit::Figure(&pet)),
                pt(5, 5),
                SLOP,
                Some(&playing),
                now
            )
            .pressed,
            Pressed::Something
        );
        assert_eq!(p.up(Some(pt(5, 5))), None, "no click while a petting plays");
        p.down(
            Some(SceneHit::Figure(&pet)),
            pt(5, 5),
            SLOP,
            Some(&playing),
            now,
        );
        assert!(matches!(
            p.moved(pt(9, 5)),
            Some(Gesture::Lift {
                figure: Figure::Pet(PetKind::Cat),
                ..
            })
        ));
    }

    /// A press affords its hit's click and its figure's lift: both on an agent
    /// or an idle pet, a lift alone on a mascot or a pet mid-petting, a click
    /// alone on a link, and nothing on a fixture or the bare office.
    #[test]
    fn a_press_affords_its_hits_click_and_figure() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let id = AgentId::from_parts("claude-code", "s");
        let agent = HoverTarget::Agent(id);
        let key = DaemonInstanceKey::new(
            "openclaw",
            pixtuoid_core::state::DaemonInstanceId::new("18789").expect("non-empty"),
        );
        let mascot = HoverTarget::Mascot(key.clone());
        let pet = HoverTarget::Pet(PetHover {
            kind: PetKind::Cat,
            centre: pt(0, 0),
            anim: crate::pack::Piece::CatSit,
        });
        let playing = PetState {
            petted_at: now,
            kind: PetKind::Cat,
            floor_idx: 0,
        };
        let affords = |click, lift| Affordance { click, lift };
        for (hit, petting, want) in [
            (
                Some(SceneHit::Figure(&agent)),
                None,
                affords(Some(HitAction::Focus(id)), Some(Figure::Agent(id))),
            ),
            (
                Some(SceneHit::Figure(&pet)),
                None,
                affords(
                    Some(HitAction::Pet(PetKind::Cat)),
                    Some(Figure::Pet(PetKind::Cat)),
                ),
            ),
            (
                Some(SceneHit::Figure(&pet)),
                Some(&playing),
                affords(None, Some(Figure::Pet(PetKind::Cat))),
            ),
            (
                Some(SceneHit::Figure(&mascot)),
                None,
                affords(None, Some(Figure::Mascot(key.clone()))),
            ),
            (
                Some(SceneHit::Star),
                None,
                affords(Some(HitAction::Open(crate::hit::REPO_URL)), None),
            ),
            (Some(SceneHit::Furniture("Desk")), None, affords(None, None)),
            (None, None, affords(None, None)),
        ] {
            let pressed = if want == Affordance::default() {
                Pressed::Bare
            } else {
                Pressed::Something
            };
            let got = Affordance::of(hit, petting, now);
            assert_eq!(got, want, "{hit:?}");
            assert_eq!(got.pressed(), pressed, "{hit:?}");
            assert_eq!(
                Pointer::default()
                    .down(hit, pt(5, 5), SLOP, petting, now)
                    .pressed,
                pressed,
                "a press lands where it affords: {hit:?}"
            );
        }
    }
}
