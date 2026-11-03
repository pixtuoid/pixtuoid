//! [`SceneHit`] for the terminal, and the bare-office probes the layout tests
//! read a cell through.

#[cfg(test)]
use pixtuoid_scene::layout::SceneLayout;

#[cfg(test)]
use crate::tui::geometry::CellArea;

pub(crate) use pixtuoid_scene::hit::SceneHit;

/// What the bare office shows at `cell`, no figure or star over it: the
/// probe the tests read the layout's fixtures through.
#[cfg(test)]
fn bare_hit(layout: &SceneLayout, cell: CellArea) -> Option<SceneHit<'static>> {
    pixtuoid_scene::hit::scene_hit(None, None, layout, cell.bounds())
}

/// Whether the bare office shows the coffee machine at `cell`.
#[cfg(test)]
pub(crate) fn hit_test_coffee_machine(layout: &SceneLayout, cell: CellArea) -> bool {
    bare_hit(layout, cell) == Some(SceneHit::Coffee)
}

/// The fixture label the bare office shows at `cell`.
#[cfg(test)]
pub(crate) fn hit_test_furniture(layout: &SceneLayout, cell: CellArea) -> Option<&'static str> {
    match bare_hit(layout, cell) {
        Some(SceneHit::Furniture(label)) => Some(label),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
