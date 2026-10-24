//! Widget cell assertion tests: inspect the ratatui buffer after `draw_scene`.

mod common;

use std::time::{Duration, SystemTime};

use common::fixture_scene;
use pixtuoid::dev::draw_scene;
use pixtuoid_scene::pack::load_bundled_pack;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

const NOW_SECS: u64 = 1_716_286_800;

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

fn render_and_get_buffer(now: SystemTime) -> (Buffer, u16, u16) {
    let w = 96u16;
    let h = 48u16;
    let scene = fixture_scene(now);
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    let pack = std::sync::Arc::new(load_bundled_pack().unwrap());
    make_draw_ctx!(draw_ctx, &scene, &pack, now);
    draw_scene(&mut term, &mut draw_ctx).unwrap();
    let buffer = term.backend().buffer().clone();
    (buffer, w, h)
}

fn row_text(buf: &Buffer, y: u16, w: u16) -> String {
    (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect()
}

#[test]
fn branding_visible_in_wall_display() {
    let (buf, w, h) = render_and_get_buffer(now());
    let upper_quarter = h / 4;
    let mut found = false;
    for y in 0..upper_quarter {
        let row = row_text(&buf, y, w);
        if row.contains("pixtuoid") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "branding 'pixtuoid' not found in the upper quarter of the display"
    );
}
