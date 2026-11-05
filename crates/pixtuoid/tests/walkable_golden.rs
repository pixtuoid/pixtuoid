//! Walkable-mask visualization snapshots: the mask as an ASCII grid, so a changed
//! obstacle pad or wall placement shows up as a readable diff rather than the BFS
//! connectivity test's boolean.

use pixtuoid_scene::layout::SceneLayout;

fn mask_to_ascii(layout: &SceneLayout) -> String {
    let w = layout.buf_w as usize;
    let h = layout.buf_h as usize;
    let mut s = String::with_capacity(w * h + h);
    for y in 0..h {
        for x in 0..w {
            s.push(if layout.is_walkable(x as u16, y as u16) {
                '.'
            } else {
                '#'
            });
        }
        s.push('\n');
    }
    s
}

#[test]
fn walkable_mask_standard_96x70() {
    let l =
        SceneLayout::compute(pixtuoid_scene::layout::Viewport::full(96, 70), Some(2), 0).unwrap();
    insta::assert_snapshot!("walkable_mask_standard_96x70", mask_to_ascii(&l));
}

#[test]
fn walkable_mask_open_plan_seed11_96x70() {
    let l =
        SceneLayout::compute(pixtuoid_scene::layout::Viewport::full(96, 70), Some(2), 11).unwrap();
    assert!(
        l.meeting_rooms.is_empty() && l.pantry.is_some(),
        "seed 11 lays out the open plan"
    );
    insta::assert_snapshot!("walkable_mask_open_plan_seed11_96x70", mask_to_ascii(&l));
}
