//! A whole frame through the real TUI painter at the owner's 16x: the scene
//! repainted and every tile cut, compressed and encoded. CodSpeed's trend is
//! its instructions on every thread, not the wall time the encode's split
//! across cores saves: Valgrind runs one thread at a time and callgrind counts
//! them all (CodSpeedHQ/runner `src/executor/valgrind/measure.rs`).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_main};
use pixtuoid::dev::{Protocol, forget_frame, renderer, warm};
use pixtuoid_core::SceneState;

/// The owner's terminal: 202x50 cells of 17x41 px, a 214x125 office at 16x.
const TERMINAL: (u16, u16) = (202, 50);
const CELL: (u16, u16) = (17, 41);
/// Desks on the floor.
const DESKS: usize = 8;
/// Longer than any protocol's cadence: frames this far apart each send, where
/// SIXEL's and iTerm2's would otherwise hold every frame after the first.
const APART: Duration = Duration::from_secs(1);

fn whole_frame(c: &mut Criterion) {
    let pack = Arc::new(pixtuoid_scene::pack::load_bundled_pack().expect("the bundled pack"));
    let scene = SceneState::uniform(DESKS);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut group = c.benchmark_group("render_tiles");
    for protocol in [Protocol::Kitty, Protocol::Sixel, Protocol::Iterm2] {
        let (mut r, _wire) = renderer(
            protocol,
            TERMINAL.0,
            TERMINAL.1,
            CELL,
            vec![],
            Arc::clone(&pack),
        )
        .expect("a 16x cutaway");
        warm(&mut r, &scene, now);
        let mut later = false;
        group.bench_function(format!("{}_whole_frame_16x", protocol.name()), |b| {
            b.iter(|| {
                forget_frame(&mut r);
                later = !later;
                let at = if later { now + APART } else { now };
                r.render(&scene, at).expect("render");
            });
        });
    }
    group.finish();
}

#[expect(
    clippy::disallowed_methods,
    reason = "codspeed's `criterion_group!` reads CODSPEED_ENV and CODSPEED_CARGO_WORKSPACE_ROOT with `env::var`"
)]
mod group {
    use super::whole_frame;
    criterion::criterion_group!(benches, whole_frame);
}
criterion_main!(group::benches);
