//! Corpus check — real transcripts in, "did we parse it AND would the UI show
//! it" out, for the whole corpus on the machine. The ONE shell that closes the
//! loop to the render layer: `harness::Drive` (decode → reduce) →
//! `FloorSession::step`, whose frame's `characters` are the fully resolved
//! sprites the painter would draw. The first half is the shared pipeline every
//! other driver runs, so a difference here is a difference in the BYTES.
//!
//! It REPORTS rather than asserts. Corpus content is unbounded and partly
//! historical, so a failing file is not automatically a bug — the value is the
//! census: decode errors and panics (which ARE bugs, always), how many
//! transcripts register, how many render, and the provenance spread.
//!
//! Usage: `corpus_check <source> [root] [--json]`, plus `--roster` — the
//! id/kind rows `just corpus-all` reads instead of keeping a second copy of the
//! roster.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use pixtuoid_core::harness::{Drive, LineFailure};
use pixtuoid_core::source::decoder::TailActivity;
use pixtuoid_core::source::registry;
use pixtuoid_scene::floor::{FloorMeta, FloorSession};
use pixtuoid_scene::pack::OfficeArt;
use pixtuoid_scene::pack::load_bundled_pack;

/// The instant the whole census runs at — the drive's fold and the step
/// below MUST share it, or every sprite is judged mid-entry-walk.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// One transcript's verdict.
#[derive(Default)]
struct Verdict {
    lines: usize,
    /// Lines that were not valid JSON at all — a torn write, not our problem.
    unparseable: usize,
    /// The decoder returned Err. ALWAYS a defect: the contract is
    /// log-and-continue, never a hard error on a line the source wrote.
    decode_errors: usize,
    /// The decoder PANICKED — the never-panic contract violated.
    panics: Vec<LineFailure>,
    /// Events the WIRE produced — the first-sight seed is NOT counted.
    events: usize,
    /// Slots on the floor, reported only when the wire produced events — the
    /// seed registers unconditionally, so counting it ungated made this read
    /// 1/1 for a file of pure garbage.
    registered: usize,
    /// Did the wire drive the slot through a lifecycle class? The
    /// anti-tautology column — a transcript can decode and register while
    /// every event lands on an id the seed does not share.
    drove: bool,
    /// Sprites the painter would draw for this transcript's agents.
    drawn: usize,
    /// Newest agent-activity stamp the file carries, epoch seconds.
    newest_activity: Option<u64>,
    mtime: Option<u64>,
}

impl Verdict {
    /// The provenance gap the ghost-session class lives in: how far the file's
    /// mtime runs ahead of anything the SESSION itself wrote. A large gap means
    /// something other than the owning session touched the file.
    fn provenance_gap_secs(&self) -> Option<u64> {
        Some(self.mtime?.saturating_sub(self.newest_activity?))
    }
}

fn epoch(t: SystemTime) -> Option<u64> {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Drive one transcript through the production path. Seeded, because the
/// WATCHER is what registers a transcript in production and a JSONL event for an
/// unknown id is a documented no-op — unseeded, every file would report "parsed
/// but never rendered" for a reason that has nothing to do with the decoder. The
/// seed's own events are then excluded from every count, or the census would
/// report a verdict on bytes that decoded to nothing.
fn check_file(source: &str, path: &Path, pack: &std::sync::Arc<OfficeArt>) -> Verdict {
    let mut v = Verdict {
        mtime: std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(epoch),
        ..Verdict::default()
    };

    let Ok(body) = std::fs::read_to_string(path) else {
        return v;
    };
    v.newest_activity = newest_activity(source, body.as_bytes());

    let Some(drive) = Drive::transcript_at(source, path) else {
        return v;
    };
    let driven = drive.seeded().at(now()).lines(body.lines());

    v.lines = driven.lines;
    v.unparseable = driven.unparseable;
    v.decode_errors = driven.decode_errors.len();
    v.events = driven.wire_events();
    if v.events == 0 {
        v.panics = driven.panics;
        return v;
    }
    v.registered = driven.registered();
    v.drove = !driven.reaches.is_empty();
    v.panics = driven.panics.clone();
    if v.registered == 0 {
        return v;
    }

    // One `FloorSession` per file so no cross-file render state can carry a
    // verdict.
    let mut session = FloorSession::new(std::sync::Arc::clone(pack));
    v.drawn = session
        .step(
            pixtuoid_scene::floor::FloorInputs {
                scene: &driven.scene,
                pack,
                now: now(),
                floor: FloorMeta::ground(),
                pets: pixtuoid_scene::floor::PetInputs::default(),
            },
            pixtuoid_scene::layout::Size { w: 192, h: 80 },
        )
        .map_or(0, |stepped| stepped.frame.characters.len());
    v
}

/// The newest turn this file's SESSION wrote, epoch seconds — read with the
/// source's own `ActivityRecency` over the raw bytes, exactly as the first-sight
/// gate reads a tail. Only CC has a published answer today; every other source
/// reports no stamp, which shows up as an empty provenance column rather than a
/// wrong one.
fn newest_activity(source: &str, body: &[u8]) -> Option<u64> {
    if source != pixtuoid_core::source::claude_code::SOURCE_NAME {
        return None;
    }
    match pixtuoid_core::source::claude_code::cc_activity_recency(body) {
        TailActivity::At(secs) => Some(secs),
        _ => None,
    }
}

/// The `--roster` rows, `id \t kind`, which `just corpus-all` reads by column:
/// `the_whole_roster_is_pinned_row_by_row` is the pin AGENTS.md's magic-number
/// rule asks for at that cross-language boundary.
fn roster() -> Vec<String> {
    registry::registered_source_names()
        .map(|name| {
            let kind = if Drive::transcript(name, "/probe.jsonl").is_some() {
                "transcript"
            } else {
                "hook"
            };
            format!("{name}\t{kind}")
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--roster") {
        for row in roster() {
            println!("{row}");
        }
        return;
    }
    let json = args.iter().any(|a| a == "--json");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.is_empty() {
        eprintln!("usage: corpus_check <source> [root] [--json]  |  --roster");
        std::process::exit(2);
    }
    let source = positional[0].as_str();

    // A source with no transcript decoder would otherwise report a clean census
    // of nothing. Above the root resolution, so a hook-only id reds with this
    // rather than with a missing-root message.
    if Drive::transcript(source, "/probe.jsonl").is_none() {
        let known: Vec<&str> = registry::registered_source_names().collect();
        eprintln!(
            "error: {source:?} is not a transcript-bearing registered source \
             (registered: {})",
            known.join(", ")
        );
        std::process::exit(2);
    }

    let root = match positional.get(1) {
        Some(r) => PathBuf::from(r),
        None => match pixtuoid_core::source::resolved_source_root(source) {
            Some(r) => r,
            None => {
                eprintln!("error: {source:?} has no resolvable root on this host — pass one");
                std::process::exit(2);
            }
        },
    };
    let mut files = pixtuoid_core::harness::transcripts_under(source, &root);
    files.sort();
    if files.is_empty() {
        // 3, not 2: an absent corpus is a coverage gap a caller may accept.
        eprintln!("absent: no .jsonl under {}", root.display());
        std::process::exit(3);
    }

    let pack = std::sync::Arc::new(load_bundled_pack().expect("bundled pack"));
    let mut totals = Verdict::default();
    let mut registered_files = 0usize;
    let mut drove_files = 0usize;
    let mut drawn_files = 0usize;
    let mut gap_buckets: BTreeMap<&str, usize> = BTreeMap::new();
    let mut error_files: Vec<(PathBuf, usize)> = Vec::new();
    let mut panic_files: Vec<(PathBuf, LineFailure)> = Vec::new();

    for f in &files {
        let v = check_file(source, f, &pack);
        totals.lines += v.lines;
        totals.unparseable += v.unparseable;
        totals.decode_errors += v.decode_errors;
        totals.events += v.events;
        if v.registered > 0 {
            registered_files += 1;
        }
        if v.drove {
            drove_files += 1;
        }
        if v.drawn > 0 {
            drawn_files += 1;
        }
        if v.decode_errors > 0 {
            error_files.push((f.clone(), v.decode_errors));
        }
        if let Some(p) = v.panics.first() {
            panic_files.push((f.clone(), p.clone()));
        }
        totals.panics.extend(v.panics.iter().cloned());
        let bucket = match v.provenance_gap_secs() {
            None => "no-stamp",
            Some(s) if s < 3600 => "<1h",
            Some(s) if s < 86_400 => "<1d",
            Some(s) if s < 7 * 86_400 => "<7d",
            Some(_) => ">=7d",
        };
        *gap_buckets.entry(bucket).or_default() += 1;
    }

    if json {
        println!(
            r#"{{"source":"{source}","files":{},"lines":{},"unparseable":{},"decode_errors":{},"panics":{},"events":{},"registered_files":{registered_files},"drove_files":{drove_files},"drawn_files":{drawn_files}}}"#,
            files.len(),
            totals.lines,
            totals.unparseable,
            totals.decode_errors,
            totals.panics.len(),
            totals.events
        );
    } else {
        println!("corpus: {source}  root={}", root.display());
        println!("  files             {}", files.len());
        println!("  lines             {}", totals.lines);
        println!(
            "  unparseable       {} (torn writes; not ours)",
            totals.unparseable
        );
        println!("  DECODE ERRORS     {}  <- must be 0", totals.decode_errors);
        println!("  PANICS            {}  <- must be 0", totals.panics.len());
        println!(
            "  events decoded    {} (the wire's own; first-sight seeds excluded)",
            totals.events
        );
        println!(
            "  registered        {registered_files}/{} files with >=1 wire event AND a slot on the floor",
            files.len()
        );
        println!(
            "  drove the slot    {drove_files}/{registered_files} reached >=1 lifecycle class (Active/Waiting/Delegating)"
        );
        println!(
            "  reached the UI    {drawn_files}/{registered_files} registered files would paint a sprite"
        );
        println!("  provenance (mtime ahead of the newest turn):");
        for (k, n) in &gap_buckets {
            println!("      {k:<9} {n}");
        }
        // Paths + line SHAPES only — a transcript line carries real prose/code.
        for (p, n) in error_files.iter().take(10) {
            println!("  decode error x{n}: {}", p.display());
        }
        for (p, f) in panic_files.iter().take(10) {
            println!("  PANIC {}: {f}", p.display());
        }
    }

    // The ONLY hard failures: a decoder that PANICS on bytes its own source
    // wrote (it would take the whole watcher down), or one that returns Err
    // there (the contract is log-and-continue).
    if totals.decode_errors > 0 || !totals.panics.is_empty() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    /// EVERY row, byte for byte: a column-count check could not fail from any
    /// data change, since `roster`'s `\t` is in its own format literal.
    fn the_whole_roster_is_pinned_row_by_row() {
        assert_eq!(
            roster(),
            [
                "claude-code\ttranscript",
                "codex\ttranscript",
                "antigravity\ttranscript",
                "dsh\thook",
                "reasonix\thook",
                "codewhale\thook",
                "opencode\thook",
                "copilot\ttranscript",
                "cursor\thook",
                "hermes\thook",
                "omp\ttranscript",
                "openclaw\thook",
                "grok\ttranscript",
                "kimi\thook",
                "jcode\thook",
            ],
            "the --roster columns are indexed POSITIONALLY by \
             `just corpus-all`. Update it in the same change, then this pin."
        );
    }
}
