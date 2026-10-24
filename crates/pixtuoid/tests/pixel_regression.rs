mod common;

use std::time::{Duration, SystemTime};

use common::{fixture_scene, render_hash};
use pixtuoid_scene::floor::FloorMeta;
use pixtuoid_scene::sky::{Weather, WeatherPolicy};
use pixtuoid_scene::theme;

#[test]
fn floor_seed_affects_render() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let scene = fixture_scene(now);

    let ground = FloorMeta::ground();
    let upper = FloorMeta::for_floor(2, 4);

    let hash_ground = render_hash(&scene, now, &theme::NORMAL, ground);
    let hash_upper = render_hash(&scene, now, &theme::NORMAL, upper);

    assert_ne!(
        hash_ground, hash_upper,
        "ground floor and floor 2 produced identical pixels -- floor seed has no effect"
    );
}

#[test]
fn weather_cycle_affects_render() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_716_286_800);
    let scene = fixture_scene(now);

    let under = |w| FloorMeta::ground().with_weather(WeatherPolicy::Forced(w));
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
    let floor = FloorMeta::ground();

    let hashes: Vec<(&str, u64)> = theme::ALL_THEMES
        .iter()
        .map(|t| (t.name, render_hash(&scene, now, t, floor)))
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
