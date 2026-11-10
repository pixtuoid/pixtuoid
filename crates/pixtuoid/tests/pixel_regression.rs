mod common;

use std::time::{Duration, SystemTime};

use common::{fixture_scene, render_hash};
use pixtuoid_scene::floor::{Conditions, FloorInputs, FloorSession, PetInputs, floor_seed};
use pixtuoid_scene::sky::{Weather, WeatherPolicy};
use pixtuoid_scene::theme;

/// One floor drawn from two floor seeds differs: the seed alone picks its
/// layout.
#[test]
fn floor_seed_affects_render() {
    use std::hash::{Hash, Hasher};
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let scene = fixture_scene(now);
    let pack = std::sync::Arc::new(pixtuoid_scene::pack::load_bundled_pack().expect("pack"));
    let hash = |seed| {
        let mut session = FloorSession::new(std::sync::Arc::clone(&pack)).with_floor_seed(seed);
        session.render(
            pixtuoid_scene::look::Look::HALF_BLOCKS,
            pixtuoid_scene::look::RenderInputs {
                world: FloorInputs {
                    scene: &scene,
                    now,
                    conditions: Conditions::default(),
                    pets: PetInputs::default(),
                },
                theme: &theme::NORMAL,
                viewport: pixtuoid_scene::layout::Viewport::full(160, 96),
                place: pixtuoid_scene::look::Place::default(),
                debug_walkable: false,
                dim: pixtuoid_scene::look::Dim::NONE,
            },
        );
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for px in session.picture().expect("a frame").pixels.as_slice() {
            (px.r, px.g, px.b).hash(&mut hasher);
        }
        hasher.finish()
    };

    assert_ne!(
        hash(floor_seed(0)),
        hash(floor_seed(2)),
        "one floor drew alike from two floor seeds -- the seed has no effect"
    );
}

#[test]
fn weather_cycle_affects_render() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let scene = fixture_scene(now);

    let under = |w| Conditions::default().with_weather(WeatherPolicy::Forced(w));
    let hash_clear = render_hash(&scene, now, &theme::NORMAL, under(Weather::Clear));
    let hash_storm = render_hash(&scene, now, &theme::NORMAL, under(Weather::Storm));

    assert_ne!(
        hash_clear, hash_storm,
        "clear vs storm produced identical pixels -- weather render path appears no-oped"
    );
}

#[test]
fn all_themes_render_distinctly() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let scene = fixture_scene(now);
    let hashes: Vec<(&str, u64)> = theme::ALL_THEMES
        .iter()
        .map(|t| (t.name, render_hash(&scene, now, t, Conditions::default())))
        .collect();

    for i in 0..hashes.len() {
        for j in (i + 1)..hashes.len() {
            assert_ne!(
                hashes[i].1, hashes[j].1,
                "themes '{}' and '{}' produced identical pixels",
                hashes[i].0, hashes[j].0
            );
        }
    }
}
