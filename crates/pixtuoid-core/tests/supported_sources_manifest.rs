//! Pins the marketing manifest (`site/src/sources.json`, which single-sources
//! the README glimpse and the site's support matrix) to the source registry.
//!
//! Runtime read, NOT `include_str!`: the latter would make `cargo publish`'s
//! compile-only verify choke on a path outside the crate package. `cargo test`
//! on an EXTRACTED .crate would still panic (no workspace tree), so this file
//! is in pixtuoid-core's `exclude` list — workspace-only test, workspace-only
//! file.

use std::collections::BTreeSet;

use pixtuoid_core::source::registry;

const MANIFEST_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../site/src/sources.json");

fn manifest() -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(MANIFEST_PATH)
        .unwrap_or_else(|e| panic!("read {MANIFEST_PATH}: {e}"));
    serde_json::from_str::<serde_json::Value>(&text)
        .expect("sources.json is valid JSON")
        .as_array()
        .expect("sources.json is a JSON array")
        .clone()
}

fn str_field<'a>(s: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    s.get(key).and_then(|v| v.as_str())
}

#[test]
fn manifest_supported_set_matches_registered_sources() {
    let manifest_supported: BTreeSet<String> = manifest()
        .iter()
        .filter(|s| str_field(s, "status") == Some("supported"))
        .map(|s| {
            str_field(s, "id")
                .unwrap_or_else(|| {
                    panic!("a `supported` source in sources.json has no string `id`: {s}")
                })
                .to_string()
        })
        .collect();

    let registered: BTreeSet<String> = registry::registered_source_names()
        .map(|s| s.to_string())
        .collect();

    assert_eq!(
        manifest_supported,
        registered,
        "site/src/sources.json `supported` set must EXACTLY match the source registry.\n  \
         claims supported but NOT wired: {:?}\n  \
         wired but NOT 'supported' in the manifest: {:?}\n  \
         Fix: edit site/src/sources.json (then `just gen-readme`).",
        manifest_supported
            .difference(&registered)
            .collect::<Vec<_>>(),
        registered
            .difference(&manifest_supported)
            .collect::<Vec<_>>(),
    );
}

#[test]
fn manifest_rows_are_well_formed() {
    const OSES: [&str; 3] = ["macos", "linux", "windows"];
    for s in manifest() {
        let name = str_field(&s, "name").unwrap_or_else(|| panic!("row missing `name`: {s}"));
        assert!(
            str_field(&s, "url").is_some_and(|u| u.starts_with("http")),
            "{name}: `url` must be an http(s) link"
        );
        let status = str_field(&s, "status").unwrap_or_else(|| panic!("{name}: missing `status`"));
        assert!(
            matches!(status, "supported" | "planned"),
            "{name}: `status` must be supported|planned, got {status:?}"
        );
        assert!(
            str_field(&s, "transport").is_some(),
            "{name}: missing `transport`"
        );

        let platforms = s
            .get("platforms")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| panic!("{name}: missing `platforms` object"));
        for os in OSES {
            let v = platforms
                .get(os)
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("{name}: `platforms.{os}` missing/not a string"));
            assert!(
                matches!(v, "yes" | "experimental" | "planned" | "no"),
                "{name}: `platforms.{os}` must be yes|experimental|planned|no, got {v:?}"
            );
        }

        if status == "planned" {
            assert!(
                s.get("id").is_none_or(|v| v.is_null()),
                "{name}: a `planned` source must not carry an `id` (it isn't wired yet)"
            );
        }
    }
}

/// The badge chip the site renders is the SAME two-char prefix the office
/// labels agents with (`cc·pixtuoid`), and its name the one the office shows.
#[test]
fn supported_rows_badge_and_name_as_the_registry_does() {
    use pixtuoid_core::source::registry::descriptor_for;
    for s in manifest() {
        if str_field(&s, "status") != Some("supported") {
            continue;
        }
        let id = str_field(&s, "id").expect("supported rows carry an id");
        let badge = str_field(&s, "badge")
            .unwrap_or_else(|| panic!("{id}: supported rows must carry a `badge` chip"));
        let descriptor = descriptor_for(id).unwrap_or_else(|| panic!("{id}: not in the registry"));
        assert_eq!(
            badge,
            format!("{}·", descriptor.label_prefix),
            "{id}: badge must be the registry label_prefix + '·'"
        );
        assert_eq!(
            str_field(&s, "name"),
            Some(descriptor.display_name),
            "{id}: `name` must be the registry display_name"
        );
    }
}
