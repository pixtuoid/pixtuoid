//! What the last frame shows a pointer over an area of the office, and what a
//! press on it does, for every painter: [`SceneHit`], the figure it names
//! ([`HoverTarget`], [`PetHover`]), and [`SceneHit::action`], the one answer to
//! what a press does. A tooltip and a press both resolve an area through the
//! same hits, so a press acts on exactly what the tooltip names.

use std::time::SystemTime;

use pixtuoid_core::AgentId;

use pixtuoid_core::source::daemon::DaemonInstanceKey;

use crate::layout::{Bounds, Point, SceneLayout};
use crate::pet::{PetKind, PetState};

/// The project repository: `Cargo.toml`'s `repository`, its one spelling.
#[doc(hidden)]
pub const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// Where the coffee machine links.
pub(crate) const COFFEE_URL: &str = "https://buymeacoffee.com/IvanWng97";

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

/// What an area shows the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneHit<'a> {
    /// An agent, a pet or a mascot.
    Figure(&'a HoverTarget),
    /// The wall board's star, a link to the repo.
    Star,
    /// The pantry's coffee machine, a link to buy the author a coffee.
    Coffee,
    /// A fixture, by its label.
    Furniture(&'static str),
}

/// What a press does: what the painter carries out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitAction {
    /// Focus the agent: a desktop painter raises its terminal.
    Focus(AgentId),
    /// Pet the floor's pet.
    Pet(PetKind),
    /// Open a page.
    Open(&'static str),
}

impl SceneHit<'_> {
    /// What a press on it does at `now`, with `petting` the last one; `None`
    /// where it does nothing — a fixture, a mascot, a pet mid-petting — and a
    /// painter treats the press as on the bare office.
    pub fn action(self, petting: Option<&PetState>, now: SystemTime) -> Option<HitAction> {
        match self {
            Self::Figure(&HoverTarget::Agent(id)) => Some(HitAction::Focus(id)),
            Self::Figure(&HoverTarget::Pet(PetHover { kind, .. })) => petting
                .is_none_or(|p| !p.is_active(now))
                .then_some(HitAction::Pet(kind)),
            Self::Star => Some(HitAction::Open(REPO_URL)),
            Self::Coffee => Some(HitAction::Open(COFFEE_URL)),
            Self::Figure(HoverTarget::Mascot(_)) | Self::Furniture(_) => None,
        }
    }
}

/// What `area` (in layout units) shows: `figure`, the topmost hover there,
/// else the board's `star`, else the coffee machine, else a labelled fixture
/// of `layout`.
#[doc(hidden)]
pub fn scene_hit<'a>(
    figure: Option<&'a HoverTarget>,
    star: Option<Bounds>,
    layout: &SceneLayout,
    area: Bounds,
) -> Option<SceneHit<'a>> {
    if let Some(target) = figure {
        Some(SceneHit::Figure(target))
    } else if star.is_some_and(|b| b.overlaps(area)) {
        Some(SceneHit::Star)
    } else if coffee_at(layout, area) {
        Some(SceneHit::Coffee)
    } else {
        fixture_label_at(layout, area).map(SceneHit::Furniture)
    }
}

/// Whether `area` shows the coffee-machine section of the pantry counter.
fn coffee_at(layout: &SceneLayout, area: Bounds) -> bool {
    layout.coffee_machine().is_some_and(|b| b.overlaps(area))
}

/// The label of the fixture `area` shows, if it carries one; the coffee
/// machine is [`coffee_at`]'s, for its link.
fn fixture_label_at(layout: &SceneLayout, area: Bounds) -> Option<&'static str> {
    layout.fixture_at(area)?.hover_label()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A figure over the coffee machine is the hit, and the machine is
    /// without one.
    #[test]
    fn a_figure_over_the_coffee_machine_is_the_hit() {
        let layout = SceneLayout::compute(crate::layout::Viewport::full(160, 200), Some(4), 0)
            .expect("layout");
        let coffee = layout
            .coffee_machine()
            .expect("a pantry with a coffee machine");
        let mid = Bounds {
            x: coffee.x + coffee.width / 2,
            y: coffee.y + coffee.height / 2,
            width: 1,
            height: 1,
        };
        let cat = HoverTarget::Pet(crate::hit::PetHover {
            kind: crate::pet::PetKind::Cat,
            centre: crate::layout::Point { x: mid.x, y: mid.y },
            anim: crate::pack::Piece::CatWalk,
        });
        assert!(matches!(
            scene_hit(Some(&cat), None, &layout, mid),
            Some(SceneHit::Figure(t)) if *t == cat
        ));
        assert_eq!(scene_hit(None, None, &layout, mid), Some(SceneHit::Coffee));
    }

    /// A press focuses an agent, pets a pet unless a petting plays, and opens
    /// the star's and the coffee machine's pages; a fixture or a mascot does
    /// nothing.
    #[test]
    fn a_press_does_what_its_hit_names() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let id = AgentId::from_parts("claude-code", "s");
        let pet = HoverTarget::Pet(PetHover {
            kind: PetKind::Dog,
            centre: crate::layout::Point { x: 0, y: 0 },
            anim: crate::pack::Piece::DogSit,
        });
        let playing = PetState {
            petted_at: now,
            kind: PetKind::Dog,
            floor_idx: 0,
        };
        let over = PetState {
            petted_at: now - std::time::Duration::from_secs(60),
            ..playing
        };
        let act = |hit: SceneHit<'_>, petting| hit.action(petting, now);
        assert_eq!(
            act(SceneHit::Figure(&HoverTarget::Agent(id)), None),
            Some(HitAction::Focus(id))
        );
        assert_eq!(
            act(SceneHit::Figure(&pet), None),
            Some(HitAction::Pet(PetKind::Dog))
        );
        assert_eq!(
            act(SceneHit::Figure(&pet), Some(&over)),
            Some(HitAction::Pet(PetKind::Dog))
        );
        assert_eq!(act(SceneHit::Figure(&pet), Some(&playing)), None);
        assert_eq!(act(SceneHit::Star, None), Some(HitAction::Open(REPO_URL)));
        assert_eq!(
            act(SceneHit::Coffee, None),
            Some(HitAction::Open(COFFEE_URL))
        );
        assert_eq!(act(SceneHit::Furniture("Desk"), None), None);
    }

    /// Files outside Rust can't read [`COFFEE_URL`], so this holds every tracked
    /// `https://buymeacoffee.com/` link to it.
    #[test]
    fn every_coffee_link_in_the_workspace_is_coffee_url() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // A crates.io package or a source tarball has no checkout to scan.
        if !root.join(".git").exists() {
            return;
        }
        let host = "https://buymeacoffee.com/";
        let grep = std::process::Command::new("git")
            .current_dir(&root)
            .args(["grep", "-l", "-z", "-F", host, "--"])
            // Lockfiles name other projects' accounts, this file holds the
            // authority, and bmc-button.py's selftest feeds made-up accounts.
            .args([
                ":!*.lock",
                ":!*-lock.json",
                ":!crates/pixtuoid-scene/src/hit.rs",
                ":!scripts/bmc-button.py",
            ])
            .output()
            .expect("run git grep");
        // git grep exits 1 when nothing matches, which the emptiness check reports.
        assert!(
            grep.status.code().is_some_and(|c| c <= 1),
            "git grep failed: {}",
            String::from_utf8_lossy(&grep.stderr)
        );
        let files: Vec<&str> = std::str::from_utf8(&grep.stdout)
            .expect("git grep prints UTF-8 paths")
            .split('\0')
            .filter(|rel| !rel.is_empty())
            .collect();
        assert!(!files.is_empty(), "no tracked file links {host}");
        for rel in files {
            let text = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            for (at, _) in text.match_indices(host) {
                let link = text[at..]
                    .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')'))
                    .next()
                    .unwrap_or_default();
                assert_eq!(link, COFFEE_URL, "{rel}");
            }
        }
    }
}
