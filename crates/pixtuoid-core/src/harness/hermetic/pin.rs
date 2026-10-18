//! A profiled source's pin, its [`SourceDescriptor::verified_version`]: the
//! release it names, whether a run reported the release it installed, and the
//! one-line edit that moves it.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::source::registry::{self, Hyphen, SourceDescriptor};

/// The source file holding every pin.
pub fn registry_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/source/registry.rs")
}

/// The release `d`'s pin names, strict as npm publishes them: it becomes an
/// install spec, and every comparison with an installed release reads it.
///
/// # Errors
///
/// When the pin is not a strict semver release.
pub fn pinned(d: &SourceDescriptor) -> Result<Version, String> {
    release(d.name, d.verified_version)
}

fn release(source: &str, pin: &str) -> Result<Version, String> {
    Version::parse(pin).map_err(|e| format!("{source}'s pin {pin:?} is not a release: {e}"))
}

/// Whether every run's CLI reported `installed`: at least one banner, each
/// read as `hyphen` says, at the same release by precedence.
///
/// # Errors
///
/// The reason, naming the first banner that disagrees.
pub fn every_run_reported(
    installed: &Version,
    hyphen: Hyphen,
    banners: &[String],
) -> Result<(), String> {
    if banners.is_empty() {
        return Err(format!(
            "no run reported a version; npm installed {installed}"
        ));
    }
    match banners.iter().find(|b| {
        registry::parse_version(b, hyphen).is_none_or(|v| !v.cmp_precedence(installed).is_eq())
    }) {
        Some(b) => Err(format!(
            "a run's CLI reported {b:?}; npm installed {installed}"
        )),
        None => Ok(()),
    }
}

/// The registry a recording of `d`'s source at `installed` leaves: `None`
/// while the pin stays.
///
/// # Errors
///
/// When `installed` is below the pin, the pin is not a release, or the row or
/// its pin line can't be found exactly.
pub fn repinned(
    registry: &str,
    d: &SourceDescriptor,
    installed: &Version,
) -> Result<Option<String>, String> {
    if moves(d, installed)? {
        repin(registry, d.name, installed).map(Some)
    } else {
        Ok(None)
    }
}

/// Whether `installed` is above `d`'s pin.
///
/// # Errors
///
/// When `installed` is below the pin, or the pin is not a release.
pub fn moves(d: &SourceDescriptor, installed: &Version) -> Result<bool, String> {
    let pin = pinned(d)?;
    match installed.cmp_precedence(&pin) {
        Ordering::Less => Err(format!(
            "{}: {installed} is below its pin {pin}, so nothing is recorded",
            d.name
        )),
        Ordering::Equal => Ok(false),
        Ordering::Greater => Ok(true),
    }
}

const PIN_FIELD: &str = "verified_version: ";

/// `registry` with `source`'s pin, and nothing else, now naming `new`.
///
/// The row is the block whose `name:` line is `<source with - as _>::SOURCE_NAME`,
/// up to its `};`, and the pin is that block's one `verified_version:` line:
/// a bare version also sits in other rows and in the registry's tests. Only the
/// literal's bytes change, so line endings survive.
///
/// # Errors
///
/// When the row's anchor or its pin line is not exactly one line, or the pin
/// line holds no string literal.
fn repin(registry: &str, source: &str, new: &Version) -> Result<String, String> {
    let anchor = format!("name: {}::SOURCE_NAME,", source.replace('-', "_"));
    let mut lines = Vec::new();
    let mut at = 0;
    for line in registry.split_inclusive('\n') {
        lines.push((at, line));
        at += line.len();
    }
    let rows: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].1.trim() == anchor)
        .collect();
    let [row] = rows[..] else {
        return Err(format!(
            "`{anchor}` is on {} registry lines, not 1",
            rows.len()
        ));
    };
    let pins: Vec<&(usize, &str)> = lines[row..]
        .iter()
        .take_while(|(_, l)| l.trim() != "};")
        .filter(|(_, l)| l.trim_start().starts_with(PIN_FIELD))
        .collect();
    let [&(start, line)] = pins[..] else {
        return Err(format!(
            "{source}'s row has {} pin lines, not 1",
            pins.len()
        ));
    };
    let literal = line
        .find('"')
        .and_then(|open| Some((open + 1, open + 1 + line[open + 1..].find('"')?)))
        .ok_or_else(|| format!("{source}'s pin line holds no string literal"))?;
    let mut out = String::with_capacity(registry.len());
    out.push_str(&registry[..start + literal.0]);
    out.push_str(&new.to_string());
    out.push_str(&registry[start + literal.1..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::hermetic::PROFILES;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("a test version")
    }

    #[test]
    fn every_profiled_pin_is_a_release_whose_hyphen_is_a_prerelease() {
        // `pinned` is strict, and an installed release is compared by semver
        // precedence: an `unknown` pin or a build-id hyphen needs its own rule
        // before its source gets a profile.
        for p in PROFILES {
            let d = p.descriptor();
            assert!(pinned(d).is_ok(), "{}", pinned(d).unwrap_err());
            assert_eq!(d.version_hyphen, Hyphen::Prerelease, "{}", d.name);
        }
    }

    #[test]
    fn a_pin_is_a_release_npm_could_publish() {
        let d = registry::descriptor_for("codex").expect("codex");
        assert_eq!(pinned(d), Ok(v(d.verified_version)));
        for bad in ["unknown", "1.2.3\"", "v1.2.3", "2026.09.18"] {
            assert!(release("codex", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_run_must_report_the_installed_release() {
        let pre = Hyphen::Prerelease;
        let ran = |b: &[&str]| b.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let installed = v("0.162.1");
        assert!(every_run_reported(&installed, pre, &ran(&["codex-cli 0.162.1"])).is_ok());
        assert!(
            every_run_reported(&installed, pre, &ran(&["codex-cli 0.162.1", "0.162.1"])).is_ok()
        );
        assert!(
            every_run_reported(
                &installed,
                pre,
                &ran(&["codex-cli 0.162.1", "codex-cli 0.155.1"])
            )
            .is_err(),
            "every run, not one"
        );
        assert!(every_run_reported(&installed, pre, &ran(&["codex-cli"])).is_err());
        assert!(
            every_run_reported(&installed, pre, &[]).is_err(),
            "no run, no report"
        );
        assert!(every_run_reported(&v("0.2.0-rc.2"), pre, &ran(&["dsh 0.2.0"])).is_err());
        let build_id = ran(&["cli 1.0.0-abc"]);
        assert!(
            every_run_reported(&v("1.0.0"), Hyphen::Build, &build_id).is_ok(),
            "a build id is the same release"
        );
        assert!(every_run_reported(&v("1.0.0"), pre, &build_id).is_err());
    }

    #[test]
    fn a_recording_moves_the_pin_only_up() {
        let src = std::fs::read_to_string(registry_path()).expect("registry.rs");
        let d = registry::descriptor_for("codex").expect("codex");
        let pin = pinned(d).expect("codex's pin");
        assert_eq!(repinned(&src, d, &pin), Ok(None));
        let up = Version::new(pin.major, pin.minor, pin.patch + 1);
        assert!(
            matches!(repinned(&src, d, &up), Ok(Some(moved)) if moved == repin(&src, d.name, &up).expect("codex's row"))
        );
        assert!(repinned("", d, &up).is_err(), "no row to move");
        let below = Version {
            pre: semver::Prerelease::new("rc.1").expect("a prerelease"),
            ..pin.clone()
        };
        assert!(
            repinned(&src, d, &below).is_err(),
            "a `latest` tag moved back"
        );
    }

    #[test]
    fn a_repin_moves_only_its_own_rows_pin() {
        let src = std::fs::read_to_string(registry_path()).expect("registry.rs");
        let new = "999.0.0-sentinel";
        assert!(!src.contains(new));
        for d in registry::REGISTRY {
            let out = repin(&src, d.name, &v(new)).unwrap_or_else(|e| panic!("{}: {e}", d.name));
            let changed: Vec<(usize, &str)> = src
                .lines()
                .zip(out.lines())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (_, b))| (i, b))
                .collect();
            let [(i, line)] = changed[..] else {
                panic!("{}: {} lines changed", d.name, changed.len());
            };
            assert_eq!(line.trim(), format!("{PIN_FIELD}\"{new}\","), "{}", d.name);
            let above: Vec<&str> = src.lines().take(i).collect();
            let row = above
                .iter()
                .rev()
                .find(|l| l.trim_start().starts_with("name: "));
            assert_eq!(
                row.map(|l| l.trim()),
                Some(format!("name: {}::SOURCE_NAME,", d.name.replace('-', "_")).as_str()),
                "{}: the pin moved is its own row's",
                d.name
            );
            assert_eq!(out.len() + d.verified_version.len(), src.len() + new.len());
        }
    }

    #[test]
    fn a_repin_refuses_a_row_it_cannot_find_exactly() {
        let row = "const A: D = D {\n    name: a_cli::SOURCE_NAME,\n    verified_version: \"1.0.0\",\n};\n";
        let new = v("1.1.0");
        assert!(repin(row, "b-cli", &new).is_err(), "no row");
        assert!(
            repin(&format!("{row}{row}"), "a-cli", &new).is_err(),
            "two rows"
        );
        let pinless = "const A: D = D {\n    name: a_cli::SOURCE_NAME,\n};\n\
                       const B: D = D {\n    name: b_cli::SOURCE_NAME,\n    verified_version: \"2.0.0\",\n};\n";
        assert!(
            repin(pinless, "a-cli", &new).is_err(),
            "the next row's pin is not its own"
        );
        let quoted = format!("// a row opens with `name: a_cli::SOURCE_NAME,`\n{row}");
        assert!(
            repin(&quoted, "a-cli", &new).is_ok(),
            "the anchor is the whole line"
        );
    }

    #[test]
    fn a_repin_rewrites_the_literal_alone() {
        let row = "const A: D = D {\r\n    name: a_cli::SOURCE_NAME,\r\n    // verified_version: \"0.1.0\", once\r\n    verified_version: \"1.0.0\",\r\n};\r\n";
        assert_eq!(
            repin(row, "a-cli", &v("1.1.0")).as_deref(),
            Ok(row.replace("\"1.0.0\"", "\"1.1.0\"").as_str()),
            "a comment naming the field stays, and CRLF survives"
        );
    }
}
