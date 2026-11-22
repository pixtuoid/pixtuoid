//! Routing benchmarks. `layout_compute` costs `SceneLayout::compute`: whatever
//! routing precomputes per layout lands there, so it is measured beside the
//! queries that precompute serves. `find_path` costs the A\* router over the
//! wide office: the door to every home desk in one iteration, the routes a
//! floor's arrivals take, and the two home desks farthest apart, the longest
//! route a desk-to-desk visit makes.
//! Run via `just bench`; the numbers are LOCAL statistical evidence, as
//! `render_frame`'s header says.

use criterion::{Criterion, criterion_main};
use pixtuoid_scene::floor::floor_seed;
use pixtuoid_scene::layout::{Point, SceneLayout, Size, Viewport};
use pixtuoid_scene::pathfind::find_path;

/// A compact and a wide classic office.
const SIZES: [Size; 2] = [Size { w: 192, h: 160 }, Size { w: 360, h: 240 }];
/// The office `find_path` routes over: the most desks, so the longest routes.
const ROUTED: Size = SIZES[1];

fn layout(size: Size) -> Option<SceneLayout> {
    SceneLayout::compute(Viewport::from(size), None, floor_seed(0))
}

fn layout_compute(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout_compute");
    for size in SIZES {
        group.bench_function(format!("{}x{}", size.w, size.h), |b| {
            b.iter(|| layout(size));
        });
    }
    group.finish();
}

fn find_path_group(c: &mut Criterion) {
    let l = layout(ROUTED).expect("the routed office lays out");
    let route = |from: Point, to: Point| find_path(&l.walkable, None, from, to, to);
    let door = l.door_threshold;
    let desks: Vec<Point> = l.home_desks.iter().map(|d| d.at).collect();
    // Each case must route as its name claims, else it measures another query.
    assert!(
        desks.iter().all(|&d| route(door, d).is_some()),
        "every desk routes from the door"
    );
    let span = |(a, b): (Point, Point)| a.x.abs_diff(b.x) + a.y.abs_diff(b.y);
    let farthest = desks
        .iter()
        .flat_map(|&a| desks.iter().map(move |&b| (a, b)))
        .max_by_key(|&pair| span(pair))
        .expect("the office has desks");
    assert!(
        route(farthest.0, farthest.1).is_some(),
        "{farthest:?} routes"
    );

    let Size { w, h } = ROUTED;
    let mut group = c.benchmark_group("find_path");
    group.bench_function(format!("door_to_every_desk_{w}x{h}"), |b| {
        b.iter(|| desks.iter().filter_map(|&d| route(door, d)).count());
    });
    group.bench_function(format!("farthest_desks_{w}x{h}"), |b| {
        b.iter(|| route(farthest.0, farthest.1));
    });
    group.finish();
}

// A module, because rustc ignores a lint attribute on the macro call itself.
#[expect(
    clippy::disallowed_methods,
    reason = "codspeed's `criterion_group!` reads CODSPEED_ENV and CODSPEED_CARGO_WORKSPACE_ROOT with `env::var`"
)]
mod group {
    use super::{find_path_group, layout_compute};
    criterion::criterion_group!(benches, layout_compute, find_path_group);
}
criterion_main!(group::benches);
