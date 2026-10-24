//! Golden image regression tests: render deterministic scenes and hash the
//! pixels, so any visual regression changes the hash.
//!
//! The assertions only compare same-machine renders against each other, so
//! timezone-dependent paths like the sky emitter's golden-hour blaze can't cause
//! cross-platform failures.

mod common;

use std::time::{Duration, SystemTime};

use common::{fixture_scene, render_hash};
use pixtuoid_core::SceneState;
use pixtuoid_scene::floor::FloorMeta;
use pixtuoid_scene::theme;

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_292_800)
}

fn empty_scene() -> SceneState {
    SceneState::uniform(12)
}

#[test]
fn golden_empty_office_is_deterministic() {
    let scene = empty_scene();
    let h1 = render_hash(&scene, now(), &theme::NORMAL, FloorMeta::ground());
    let h2 = render_hash(&scene, now(), &theme::NORMAL, FloorMeta::ground());
    assert_eq!(h1, h2, "empty office render is non-deterministic");
}

#[test]
fn golden_populated_vs_empty_differ() {
    let n = now();
    let h_empty = render_hash(&empty_scene(), n, &theme::NORMAL, FloorMeta::ground());
    let h_pop = render_hash(&fixture_scene(n), n, &theme::NORMAL, FloorMeta::ground());
    assert_ne!(h_empty, h_pop, "populated and empty scenes look identical");
}
