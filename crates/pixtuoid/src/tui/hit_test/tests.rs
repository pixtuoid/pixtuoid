use super::*;
use crate::tui::geometry::CellArea;
use pixtuoid_scene::layout::{Point, Size};

#[test]
fn coffee_machine_hit_test_returns_false_for_origin() {
    let layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    assert!(!hit_test_coffee_machine(
        &layout,
        CellArea::half_block(0, 0)
    ));
}

/// The middle cell of the coffee machine on `layout`'s pantry counter.
fn coffee_mid_cell(layout: &SceneLayout) -> (u16, u16) {
    let b = layout.coffee_machine().expect("a coffee machine");
    (b.x + b.width / 2, CellArea::row_of(b.y + b.height / 2))
}

#[test]
fn coffee_machine_hit_test_returns_true_for_machine_area() {
    let layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    let (mid_x, mid_cell_y) = coffee_mid_cell(&layout);
    assert!(
        hit_test_coffee_machine(&layout, CellArea::half_block(mid_x, mid_cell_y)),
        "expected hit at coffee machine area ({mid_x}, {mid_cell_y})"
    );
}

#[test]
fn furniture_hit_test_returns_none_for_empty_space() {
    let layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    // Scan for an empty cell rather than hardcoding one: which mid-floor cells
    // are open shifts whenever the pod aisle spacing is retuned.
    let empty = (0..(CellArea::row_of(layout.buf_h)))
        .flat_map(|cy| (0..layout.buf_w).map(move |cx| (cx, cy)))
        .find(|&(cx, cy)| hit_test_furniture(&layout, CellArea::half_block(cx, cy)).is_none())
        .expect("some open-floor cell must report no furniture");
    assert_eq!(
        hit_test_furniture(&layout, CellArea::half_block(empty.0, empty.1)),
        None
    );
}

#[test]
fn furniture_hit_test_finds_desk() {
    let layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    let desk = layout.home_desks.first().expect("desk");
    let cell_y = CellArea::row_of(desk.at.y + 2);
    assert_eq!(
        hit_test_furniture(&layout, CellArea::half_block(desk.at.x + 2, cell_y)),
        Some("Desk")
    );
    let vis_w = pixtuoid_scene::layout::furniture_def(pixtuoid_scene::layout::Furniture::Desk)
        .visual
        .w;
    assert_eq!(
        hit_test_furniture(&layout, CellArea::half_block(desk.at.x + vis_w - 1, cell_y)),
        Some("Desk"),
        "the desk's east overhang column must hover it"
    );
}

#[test]
fn furniture_hit_test_finds_elevator() {
    let layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    let door = layout.door;
    let cell_y = CellArea::row_of(door.y + pixtuoid_scene::layout::ELEVATOR_H / 2);
    assert_eq!(
        hit_test_furniture(
            &layout,
            CellArea::half_block(door.x + pixtuoid_scene::layout::ELEVATOR_W / 2, cell_y)
        ),
        Some("Elevator")
    );
}

#[test]
fn dense_room_1_has_coat_rack_and_doormat() {
    // Regression (#555): the meeting-decor painters and hover labels must iterate
    // ALL meeting_rooms, not just room 0.
    let mut saw_dual = false;
    for seed in 0..10u64 {
        let layout = SceneLayout::compute_with_seed(192, 160, Some(8), seed).expect("layout");
        if layout.meeting_rooms.len() < 2 {
            continue;
        }
        saw_dual = true;
        let mr = layout.meeting_rooms[1].bounds;
        assert!(mr.width > 20, "seed {seed}: dense room 1 hosts the rack");
        let cx = mr.x + mr.width - 5;
        let cy = mr.y + mr.height / 2 - 4;
        assert_eq!(
            hit_test_furniture(&layout, CellArea::half_block(cx, CellArea::row_of(cy + 3))),
            Some("Coat Rack"),
            "seed {seed}: room 1 must hover its own coat rack"
        );
        let mat_x = mr.x + mr.width + 1;
        let mat_y = mr.y + mr.height / 2 - 2;
        assert_eq!(
            hit_test_furniture(
                &layout,
                CellArea::half_block(mat_x + 1, CellArea::row_of(mat_y + 2))
            ),
            Some("Doormat"),
            "seed {seed}: room 1 must hover its own doormat"
        );
    }
    assert!(saw_dual, "192x160 seeds 0..10 must reach a dual floor");
}

#[test]
fn furniture_hit_test_finds_meeting_table() {
    let layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    let table = layout.meeting_rooms[0].trio.expect("trio").table;
    let cell_y = CellArea::row_of(table.y);
    assert_eq!(
        hit_test_furniture(&layout, CellArea::half_block(table.x, cell_y)),
        Some("Meeting Table")
    );
}

#[test]
fn furniture_hit_test_respects_floor_seed() {
    // seed=1 → Lounge variant (no meeting room)
    let layout1 = SceneLayout::compute_with_seed(160, 200, Some(4), 1).expect("layout");
    assert!(layout1.meeting_rooms.is_empty());
    let layout0 = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    if let Some(trio) = layout0.meeting_rooms.first().and_then(|r| r.trio) {
        let table = trio.table;
        let cell_y = CellArea::row_of(table.y);
        assert_ne!(
            hit_test_furniture(&layout1, CellArea::half_block(table.x, cell_y)),
            Some("Meeting Table"),
        );
    }
}

// BulletinBoard is never emitted by compute_with_seed and Ficus only appears on
// ROOMY-band floors, so both are placed synthetically below.

#[test]
fn furniture_hit_test_ficus_via_synthetic_plant() {
    use pixtuoid_scene::layout::Point;
    let mut layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    let pos = Point { x: 40, y: 40 };
    layout.plants.push(pixtuoid_scene::layout::PlantItem {
        kind: pixtuoid_scene::layout::PlantKind::Ficus,
        pos,
    });
    // Plants are center-anchored on `pos`; hover the center cell.
    assert_eq!(
        hit_test_furniture(
            &layout,
            CellArea::half_block(pos.x, CellArea::row_of(pos.y))
        ),
        Some("Ficus")
    );
}

#[test]
fn furniture_hit_test_bulletin_board_via_synthetic_wall_decor() {
    use pixtuoid_scene::layout::Point;
    let mut layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    // Wall decor is TOP-LEFT anchored at `pos` (not centered). Place it in
    // open space so no earlier furniture arm shadows it.
    let pos = Point { x: 60, y: 30 };
    layout
        .wall_decor
        .push(pixtuoid_scene::layout::WallDecorItem {
            kind: pixtuoid_scene::layout::WallDecor::BulletinBoard,
            pos,
        });
    assert_eq!(
        hit_test_furniture(
            &layout,
            CellArea::half_block(pos.x, CellArea::row_of(pos.y))
        ),
        Some("Bulletin Board")
    );
}

#[test]
fn coffee_machine_returns_false_without_a_pantry() {
    let mut layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    let (mid_x, mid_cell_y) = coffee_mid_cell(&layout);
    assert!(
        hit_test_coffee_machine(&layout, CellArea::half_block(mid_x, mid_cell_y)),
        "precondition: coffee machine area should hit with the pantry present"
    );
    layout.pantry = None;
    assert!(
        !hit_test_coffee_machine(&layout, CellArea::half_block(mid_x, mid_cell_y)),
        "no pantry ⇒ the early return must yield false at the machine coords"
    );
    assert!(!hit_test_coffee_machine(
        &layout,
        CellArea::half_block(0, 0)
    ));
}

// The lounge and pod-decor arms below aren't all reachable from
// `compute_with_seed` at the tested sizes, so each is placed synthetically.

/// `label` fires on exactly the cells that show some pixel — either half — of
/// a `size` sprite centred on `pos`, swept half a sprite beyond it on every
/// side.
fn assert_centered_hover_box(
    layout: &SceneLayout,
    label: &str,
    pos: pixtuoid_scene::layout::Point,
    size: Size,
) {
    let (x0, y0) = (pos.x - size.w / 2, pos.y - size.h / 2);
    for mx in pos.x - size.w..pos.x + size.w {
        for my in CellArea::row_of(pos.y - size.h)..CellArea::row_of(pos.y + size.h) {
            let py = my * 2;
            let inside = mx >= x0 && mx < x0 + size.w && py + 1 >= y0 && py < y0 + size.h;
            assert_eq!(
                hit_test_furniture(layout, CellArea::half_block(mx, my)) == Some(label),
                inside,
                "{label} at cell ({mx}, {my})"
            );
        }
    }
}

/// A layout whose lounge pieces sit where the caller puts them.
fn layout_with_lounge(lounge: pixtuoid_scene::layout::Lounge) -> SceneLayout {
    let mut layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    layout.lounge = Some(lounge);
    // The probes stand the pieces where the real office has its plants and its
    // meeting room's band decor: either would be the topmost fixture there.
    layout.plants.clear();
    layout.meeting_rooms.clear();
    layout
}

// The lounge is one aggregate, so the co-present pieces are parked far from
// every probe.
const PARK: pixtuoid_scene::layout::Point = pixtuoid_scene::layout::Point { x: 130, y: 6 };

#[test]
fn the_lounge_sofa_hovers_on_its_painted_sprite() {
    use pixtuoid_scene::layout::{Furniture, furniture_def};
    let c = Point { x: 40, y: 50 };
    let layout = layout_with_lounge(pixtuoid_scene::layout::Lounge {
        couch_center: c,
        floor_lamp: PARK,
        side_table: PARK,
        fish_tank: None,
    });
    assert_centered_hover_box(
        &layout,
        "Lounge Sofa",
        c,
        furniture_def(Furniture::MeetingSofaBody).visual,
    );
}

#[test]
fn the_side_table_hovers_on_its_painted_sprite() {
    use pixtuoid_scene::layout::{Furniture, furniture_def};
    let t = Point { x: 40, y: 50 };
    let layout = layout_with_lounge(pixtuoid_scene::layout::Lounge {
        couch_center: PARK,
        floor_lamp: PARK,
        side_table: t,
        fish_tank: None,
    });
    assert_centered_hover_box(
        &layout,
        "Side Table",
        t,
        furniture_def(Furniture::LoungeSideTable).visual,
    );
}

#[test]
fn furniture_hit_test_finds_floor_lamp_via_synthetic() {
    let p = Point { x: 40, y: 40 };
    let layout = layout_with_lounge(pixtuoid_scene::layout::Lounge {
        couch_center: PARK,
        floor_lamp: p,
        side_table: PARK,
        fish_tank: None,
    });
    assert_eq!(
        hit_test_furniture(&layout, CellArea::half_block(p.x, CellArea::row_of(p.y))),
        Some("Floor Lamp")
    );
}

#[test]
fn furniture_hit_test_finds_fish_tank_via_synthetic() {
    let p = Point { x: 40, y: 40 };
    let layout = layout_with_lounge(pixtuoid_scene::layout::Lounge {
        couch_center: PARK,
        floor_lamp: PARK,
        side_table: PARK,
        fish_tank: Some(p),
    });
    assert_eq!(
        hit_test_furniture(&layout, CellArea::half_block(p.x, CellArea::row_of(p.y))),
        Some("Fish Tank")
    );
}

#[test]
fn snack_shelf_hovers_across_its_whole_sprite_not_just_the_footprint() {
    // The shelf sprite is CENTRED on the waypoint while the walkable footprint
    // is its End-anchored south strip; hover must cover the sprite the user
    // sees, not that strip.
    let layout = SceneLayout::compute(192, 160, Some(12)).expect("layout");
    let shelf = layout
        .waypoints
        .iter()
        .find(|w| w.kind == pixtuoid_scene::layout::WaypointKind::SnackShelf)
        .map(|w| w.pos)
        .expect("192x160 places the snack shelf");
    let vis =
        pixtuoid_scene::layout::furniture_def(pixtuoid_scene::layout::Furniture::SnackShelf).visual;
    let top_y = shelf.y.saturating_sub(vis.h / 2);
    assert_eq!(top_y % 2, 1, "192x160 must keep the shelf's top edge odd");
    // `row_of(top_y)` holds the shelf's top row; on an odd edge only its lower half
    // shows it.
    assert_eq!(
        hit_test_furniture(
            &layout,
            CellArea::half_block(shelf.x, CellArea::row_of(top_y))
        ),
        Some("Snack Shelf"),
        "top shelf row hovers"
    );
    assert_eq!(
        hit_test_furniture(
            &layout,
            CellArea::half_block(shelf.x, CellArea::row_of(shelf.y))
        ),
        Some("Snack Shelf"),
        "sprite centre hovers"
    );
}

#[test]
fn furniture_hit_test_finds_meeting_chairs_on_a_real_layout() {
    let layout = SceneLayout::compute(192, 160, Some(12)).expect("layout");
    let chairs: Vec<_> = layout
        .waypoints
        .iter()
        .filter(|w| w.kind == pixtuoid_scene::layout::WaypointKind::MeetingChair)
        .map(|w| w.pos)
        .collect();
    assert_eq!(chairs.len(), 2);
    for c in chairs {
        assert_eq!(
            hit_test_furniture(&layout, CellArea::half_block(c.x, CellArea::row_of(c.y))),
            Some("Meeting Chair")
        );
    }
}

#[test]
fn furniture_hit_test_finds_tv_stand_via_synthetic_pod_decor() {
    use pixtuoid_scene::layout::{PodDecor, PodDecorItem, Point};
    let mut layout = SceneLayout::compute(160, 200, Some(4)).expect("layout");
    let p = Point { x: 50, y: 40 };
    layout.pod_decor.push(PodDecorItem {
        kind: PodDecor::Tv,
        pos: p,
    });
    assert_eq!(
        hit_test_furniture(&layout, CellArea::half_block(p.x, CellArea::row_of(p.y))),
        Some("TV Stand")
    );
}

/// The wall board's text sits on the neon sign, so hovering the sign must not
/// raise a furniture tooltip over it.
#[test]
fn the_neon_sign_raises_no_tooltip() {
    use pixtuoid_scene::layout::{NEON_PANEL_INNER_X, NEON_PANEL_INNER_Y};
    let layout = SceneLayout::compute(160, 200, Some(16)).expect("layout");
    let cell = CellArea::half_block(NEON_PANEL_INNER_X, CellArea::row_of(NEON_PANEL_INNER_Y));
    assert_eq!(
        layout.fixture_at(cell.bounds()),
        Some(pixtuoid_scene::layout::FixtureKind::NeonSign)
    );
    assert_eq!(hit_test_furniture(&layout, cell), None);
}
