//! Snapshot regression tests for `draw_scene`.
//!
//! No golden pixel buffer: the daylight code reads `chrono::Local`, which makes
//! a golden hash machine-dependent. The determinism + time-sensitivity checks
//! together catch the likely regressions without needing per-machine goldens.

mod common;

use std::time::{Duration, SystemTime};

use common::{fixture_scene, render_hash};
use pixtuoid::dev::draw_scene;
use pixtuoid_core::state::ActivityState;
use pixtuoid_scene::floor::FloorMeta;
use pixtuoid_scene::pack::load_bundled_pack;
use pixtuoid_scene::theme::NORMAL;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn render_pixel_hash(now: SystemTime) -> u64 {
    render_hash(&fixture_scene(now), now, &NORMAL, FloorMeta::ground())
}

#[test]
fn render_is_deterministic_for_same_now() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let hash_a = render_pixel_hash(now);
    let hash_b = render_pixel_hash(now);
    assert_eq!(
        hash_a, hash_b,
        "render is non-deterministic — repeat calls with identical input produce different output"
    );
}

#[test]
fn render_changes_when_time_advances_by_hours() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let later = base + Duration::from_secs(8 * 3600);
    let hash_a = render_pixel_hash(base);
    let hash_b = render_pixel_hash(later);
    assert_ne!(
        hash_a, hash_b,
        "render is identical 8 hours apart — time-of-day system appears bypassed"
    );
}

#[test]
fn render_produces_distinct_wall_band_and_floor_regions() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let scene = fixture_scene(now);
    let backend = TestBackend::new(96, 36);
    let mut term = Terminal::new(backend).expect("terminal");
    let pack = std::sync::Arc::new(load_bundled_pack().expect("pack"));
    make_draw_ctx!(draw_ctx, &scene, &pack, now);
    draw_scene(&mut term, &mut draw_ctx).expect("render");
    let buf = draw_ctx.floor.raster.pixels().expect("a frame");

    let mut colors = std::collections::HashSet::new();
    for px in buf.as_slice() {
        colors.insert((px.r, px.g, px.b));
    }
    assert!(
        colors.len() > 32,
        "expected non-trivial color diversity, got {} distinct colors",
        colors.len()
    );

    // The upper quarter of the buffer is the wall band, the lower half the floor.
    let w = buf.width() as usize;
    let h = buf.height() as usize;
    let wall_h = h / 4;
    let floor_y0 = h / 2;

    let avg = |y0: usize, y1: usize| -> (f64, f64, f64) {
        let mut r = 0u64;
        let mut g = 0u64;
        let mut b = 0u64;
        let mut n = 0u64;
        for y in y0..y1 {
            for x in 0..w {
                let p = buf.as_slice()[y * w + x];
                r += u64::from(p.r);
                g += u64::from(p.g);
                b += u64::from(p.b);
                n += 1;
            }
        }
        let n = n.max(1) as f64;
        (r as f64 / n, g as f64 / n, b as f64 / n)
    };

    let wall = avg(0, wall_h);
    let floor = avg(floor_y0, h);
    let (dr, dg, db) = (wall.0 - floor.0, wall.1 - floor.1, wall.2 - floor.2);
    let dist = (dr * dr + dg * dg + db * db).sqrt();
    assert!(
        dist > 20.0,
        "wall band and floor regions look identical (dist={dist:.1}, wall={wall:?}, floor={floor:?})"
    );
}

#[test]
fn render_changes_when_an_agent_state_changes() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let mut scene_idle = fixture_scene(now);
    for slot in scene_idle.agents.values_mut() {
        slot.state = ActivityState::Idle;
    }
    let idle_hash = render_hash(&scene_idle, now, &NORMAL, FloorMeta::ground());

    let active_hash = render_pixel_hash(now);
    assert_ne!(
        idle_hash, active_hash,
        "all-idle and mixed-state scenes produced identical pixels"
    );
}
