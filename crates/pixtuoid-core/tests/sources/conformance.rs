//! Golden-fixture decode + coalescing harness: bytes come from the COMMITTED
//! fixture dirs (`tests/sources/fixtures/<source>/<scenario>/`), the decoded
//! `AgentEvent` sequence is snapshotted (insta yaml), and every decoded event
//! must share ONE `AgentId` (a mismatch = two sprites for one session).
//!
//! Neither drive is SEEDED — this harness asserts what the WIRE alone produces,
//! so a transcript that registers nothing on its own is a fact the snapshot
//! shows rather than one a seed hides.
//!
//! Adding a CLI = drop a fixture dir; the decoder comes from the source's
//! `DESCRIPTOR` in its `source/` module — no harness edit. Snapshots
//! stay portable because the decoder is fed the fixture's *relative* path, and
//! `AgentId` is a deterministic hash of that key.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pixtuoid_core::harness::{Drive, Driven};
use pixtuoid_core::source::{AgentEvent, registry};

// ONE set of tree helpers, in `captures.rs`. This file kept byte-identical
// copies whose `read_dir` SWALLOWED errors where the other panics — an
// unreadable capture dir made `wire_files()` empty, so the redaction rule
// passed vacuously for it.
use super::captures::{fixture_lines, fixtures_root, sorted_dirs, transcript_in, transcripts_in};

/// Hook-only-ness comes from the registry row, never a harness-side list — a
/// second list could mark a JSONL source hook-only and pass the harness without
/// its LineDecoder ever running.
fn is_hook_only(source: &str) -> bool {
    registry::descriptor_for(source).is_some_and(|d| d.line_decoder().is_none())
}

/// Daemon sources decode to ZERO AgentEvents — presence rides a sibling channel
/// into `SceneState::daemons`, so the coalesce-to-one-AgentId contract doesn't
/// apply (no agent slots).
fn is_daemon(source: &str) -> bool {
    registry::descriptor_for(source).is_some_and(|d| d.is_daemon())
}

struct Decoded {
    jsonl: Option<Driven>,
    hooks: Option<Driven>,
    /// The hook payload LINES as committed, so a presence-only source can pin
    /// its own decoder against the byte-real fixture (its `hooks` drive decodes
    /// to zero `AgentEvent`s by design).
    hook_lines: Vec<String>,
}

impl Decoded {
    fn events(&self) -> Vec<AgentEvent> {
        self.jsonl
            .iter()
            .chain(self.hooks.iter())
            .flat_map(|d| d.events.iter().cloned())
            .collect()
    }
}

fn decode_fixture(source: &str, dir: &Path) -> Decoded {
    assert!(
        registry::descriptor_for(source).is_some(),
        "fixture dir {source:?} matches no SourceDescriptor row — dir-name typo, \
         or a removed source whose fixtures should be deleted"
    );
    let transcripts = transcripts_in(dir);
    let expected = if is_hook_only(source) { 0 } else { 1 };
    assert_eq!(
        transcripts.len(),
        expected,
        "{} must contain exactly {expected} transcript .jsonl (source {source:?} is {}), found {}",
        dir.display(),
        if expected == 0 {
            "hook-only"
        } else {
            "JSONL-bearing"
        },
        transcripts.len()
    );

    // Hook-only scenarios key the substitution on the scenario dir instead.
    // Separators are normalized to '/' so the key — and therefore every AgentId
    // hash baked into the snapshots — is byte-identical on Windows.
    let logical = transcripts
        .first()
        .map_or(dir, PathBuf::as_path)
        .strip_prefix(fixtures_root())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");

    let jsonl = transcripts.first().map(|transcript| {
        let drive = Drive::transcript(source, &logical).unwrap_or_else(|| {
            panic!(
                "fixture source {source:?} has no line_decoder — add/extend its \
                 DESCRIPTOR in its source/ module"
            )
        });
        let driven = drive.lines(fixture_lines(transcript));
        driven.assert_clean(&format!("transcript {}", transcript.display()));
        driven
    });

    let hooks_path = dir.join("hook-payloads.jsonl");
    // `{{TRANSCRIPT_PATH}}` lets a path-keyed hook (CC) line up with its
    // transcript.
    let hook_lines: Vec<String> = if hooks_path.exists() {
        fixture_lines(&hooks_path)
            .into_iter()
            .map(|l| l.replace("{{TRANSCRIPT_PATH}}", &logical))
            .collect()
    } else {
        Vec::new()
    };
    let hooks = hooks_path.exists().then(|| {
        let driven = Drive::hooks().lines(&hook_lines);
        driven.assert_clean(&format!("hook payloads {}", hooks_path.display()));
        driven
    });

    Decoded {
        jsonl,
        hooks,
        hook_lines,
    }
}

/// The WATCHER half of coalescing: `all_source_fixtures_decode_and_coalesce`
/// drives the wire ALONE, so the first-sight seed — the thing production
/// actually registers with — is never exercised there. This is also the
/// windows-only catch for the path-key fold: on Unix `normalize_path_key` is
/// the identity, so a raw vs normalized key divergence is invisible locally.
#[test]
fn a_seeded_drive_coalesces_with_each_transcripts_own_decoder() {
    let root = fixtures_root();
    let mut ran = 0;
    for source_dir in sorted_dirs(&root) {
        let source = source_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if is_hook_only(&source) || is_daemon(&source) {
            continue;
        }
        for scenario_dir in sorted_dirs(&source_dir) {
            let transcript = transcript_in(&scenario_dir);
            let driven = Drive::transcript_at(&source, &transcript)
                .unwrap_or_else(|| panic!("{source}: no transcript decoder"))
                .seeded()
                .lines(fixture_lines(&transcript));
            driven.assert_clean(&format!("seeded transcript {}", transcript.display()));

            let ids: BTreeSet<_> = driven.events.iter().map(AgentEvent::agent_id).collect();
            assert!(
                driven.events.len() >= 2,
                "{source}: the seed plus this transcript's own events, got {:?}",
                driven.events
            );
            assert_eq!(
                ids.len(),
                1,
                "{source}/{}: the first-sight seed and the decoded lines must be ONE agent \
                 — the registry row's id deriver disagrees with this source's own decoder \
                 keying, got {ids:?}",
                scenario_dir.file_name().unwrap().to_string_lossy(),
            );
            assert_eq!(
                driven.registered(),
                1,
                "{source}: a seeded transcript must register exactly one slot"
            );
            ran += 1;
        }
    }
    assert!(
        ran > 0,
        "no transcript fixtures found under {}",
        root.display()
    );
}

/// A source whose coalescing its own test pins, on rounds that can't live
/// here: omp's hook keys fold per platform, while these goldens can't
/// (`omp/mod.rs`'s module doc).
const COALESCED_ELSEWHERE: &[(&str, fn())] = &[(
    pixtuoid_core::source::omp::SOURCE_NAME,
    super::omp::each_bridge_round_coalesces_both_transports_onto_one_id,
)];

/// Without this, `all_source_fixtures_decode_and_coalesce` only covers sources
/// that happen to have a dir — a contributor could register a new CLI and ship a
/// broken decoder while the harness stays green. Registration is not coverage.
#[test]
fn every_registered_source_has_a_coalescing_fixture() {
    let root = fixtures_root();
    for src in registry::registered_source_names() {
        let dir = root.join(src);
        if COALESCED_ELSEWHERE.iter().any(|(s, _)| *s == src) {
            assert!(
                !dir.is_dir() || sorted_dirs(&dir).is_empty(),
                "{src}: a fixture here makes its COALESCED_ELSEWHERE row stale"
            );
            continue;
        }
        let shape = if is_hook_only(src) {
            "hook-payloads.jsonl ONLY (hook-only row)"
        } else {
            "transcript.jsonl [+ hook-payloads.jsonl]"
        };
        assert!(
            dir.is_dir(),
            "registered source {src:?} has no fixture dir {} — add a coalescing fixture ({shape})",
            dir.display()
        );
        assert!(
            !sorted_dirs(&dir).is_empty(),
            "registered source {src:?} fixture dir {} has no scenario subdir",
            dir.display()
        );
    }
}

/// A source whose id comes from the transcript's PARENT DIR needs the fixture to
/// reproduce that dir, or the session id silently becomes the SCENARIO NAME. The
/// README states the rule; this is what makes it fail. The probe catches a
/// deriver that returns the parent VERBATIM; one that DERIVES from it (omp's
/// `stem_chain` shape) reads as filename-keyed and is skipped.
#[test]
fn a_parent_dir_keyed_transcript_is_nested_under_its_session_id() {
    let root = fixtures_root();
    for source_dir in sorted_dirs(&root) {
        let source = source_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if is_hook_only(&source) || is_daemon(&source) {
            continue;
        }
        let derive = registry::id_deriver_for(&source);
        // The probe names a dir that no scenario could be called; a filename-keyed
        // deriver ignores it, a parent-dir-keyed one hands it straight back.
        const PROBE: &str = "0000-parent-probe";
        if derive(Path::new(&format!("{PROBE}/x.jsonl"))) != PROBE {
            continue;
        }
        for scenario_dir in sorted_dirs(&source_dir) {
            let scenario = scenario_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            for t in transcripts_in(&scenario_dir) {
                let parent = t.parent().unwrap().file_name().unwrap().to_string_lossy();
                assert_ne!(
                    parent,
                    scenario,
                    "{}: {source} keys the session on the transcript's PARENT DIR, so a flat \
                     fixture makes the scenario name the session id — nest it under the real id",
                    t.display()
                );
            }
        }
    }
}

#[test]
fn all_source_fixtures_decode_and_coalesce() {
    let root = fixtures_root();
    let mut ran = 0;
    for source_dir in sorted_dirs(&root) {
        let source = source_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for scenario_dir in sorted_dirs(&source_dir) {
            let scenario = scenario_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let d = decode_fixture(&source, &scenario_dir);
            let events = d.events();

            if is_daemon(&source) {
                assert!(
                    !d.hook_lines.is_empty(),
                    "{source}/{scenario}: a daemon source must ship a NON-EMPTY \
                     hook-payloads.jsonl (an empty fixture passes the zero-events check vacuously)"
                );
                assert!(
                    events.is_empty(),
                    "{source}/{scenario}: a daemon source must decode to ZERO AgentEvents \
                     (presence rides the sibling channel), got {events:?}"
                );
                // The PRESENCE lane, not the agent one: these payloads ride the
                // sibling channel, so they are decoded here from the committed
                // lines rather than through `Drive` (whose `AgentEvent` output
                // for a daemon is empty by construction, asserted just above).
                let decoded: Vec<_> = d
                    .hook_lines
                    .iter()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| {
                        pixtuoid_core::source::openclaw::decode_openclaw_hook_payload(
                            v.as_object()?,
                        )
                        .ok()
                    })
                    .collect();
                assert!(
                    decoded.iter().any(|d| !d.updates.is_empty()),
                    "{source}/{scenario}: the byte-real fixture decoded to ZERO presence deltas \
                     — the presence decoder's field names drifted from the captured wire format"
                );
                let instances: std::collections::BTreeSet<_> =
                    decoded.iter().map(|d| d.instance.as_str()).collect();
                assert_eq!(
                    instances.len(),
                    1,
                    "{source}/{scenario}: one captured gateway must resolve to exactly one \
                     instance id, got {instances:?}"
                );
                assert!(
                    instances
                        .iter()
                        .all(|i| i.chars().all(|c| c.is_ascii_digit())),
                    "{source}/{scenario}: the captured wire must carry a real gateway port \
                     (got {instances:?})"
                );
                insta::assert_yaml_snapshot!(format!("{source}__{scenario}"), events);
                ran += 1;
                continue;
            }

            // Each present transport must actually contribute — else a
            // degenerate fixture (e.g. all-no-op JSONL) could pass coalescing
            // on hooks alone, silently skipping the keying path this guards.
            if is_hook_only(&source) {
                assert!(
                    d.hooks.as_ref().is_some_and(|h| !h.events.is_empty()),
                    "{source}/{scenario}: a hook-only source's scenario must ship a non-empty hook-payloads.jsonl"
                );
            } else {
                assert!(
                    d.jsonl.as_ref().is_some_and(|j| !j.events.is_empty()),
                    "{source}/{scenario}: transcript decoded to ZERO events"
                );
            }
            if let Some(hooks) = &d.hooks {
                assert!(
                    !hooks.events.is_empty(),
                    "{source}/{scenario}: hook-payloads.jsonl decoded to ZERO events"
                );
            }

            insta::assert_yaml_snapshot!(format!("{source}__{scenario}"), events);

            let ids: BTreeSet<_> = events.iter().map(|e| e.agent_id()).collect();
            assert_eq!(
                ids.len(),
                1,
                "{source}/{scenario}: hook+JSONL events must coalesce to ONE agent_id, got {}: {:?}",
                ids.len(),
                ids
            );
            ran += 1;
        }
    }
    assert!(ran > 0, "no fixtures found under {}", root.display());
}
