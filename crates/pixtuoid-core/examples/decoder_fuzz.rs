//! Real-corpus never-panic harness for a source's transcript LINE decoder.
//!
//! The source is an ARGUMENT, never sniffed from line shape: inferring it
//! silently MISROUTED sources whose shape didn't match a hard-coded predicate
//! (grok's `method` envelope, omp's bare `type`) to `decode_cc_line`, reporting
//! a false-green exit 0 having exercised the WRONG decoder.
//!
//! A TOOL, not a committed corpus — point it at any JSONL tree, e.g.
//! `just fuzz claude-code ~/.claude/projects`. `Driven` retains every decoded
//! event, so memory scales with the piped corpus; split a huge tree.
//!
//! Decode `Err` is allowed (the watcher logs + skips malformed lines); only a
//! PANIC is a contract violation.

use std::io::BufRead;

use pixtuoid_core::harness::Drive;
use pixtuoid_core::source::registry;

fn main() {
    let source = std::env::args().nth(1).unwrap_or_default();
    if registry::descriptor_for(&source).is_none() {
        let known: Vec<&str> = registry::registered_source_names().collect();
        eprintln!(
            "decoder_fuzz: unknown source {source:?} — pass a registered source name \
             (one of: {})",
            known.join(", ")
        );
        std::process::exit(2);
    }
    // A stand-in transcript path, unseeded: the never-panic contract is
    // path-independent and registration is not under test.
    let Some(drive) = Drive::transcript(&source, "/fuzz/session.jsonl") else {
        eprintln!(
            "decoder_fuzz: source {source:?} has no transcript line decoder (hook-only or \
             daemon) — its never-panic contract is covered by the \
             `every_hook_and_presence_decoder_never_panics` proptest, not this corpus tool"
        );
        std::process::exit(2);
    };

    // Split by error KIND: a non-UTF-8 line is SKIPPED (`read_line` already
    // consumed it; stopping would fuzz only the corpus PREFIX while still
    // exiting 0), but any other read error repeats without consuming,
    // so skipping it would spin forever.
    let lines = std::io::stdin().lock().lines().map_while(|r| match r {
        Ok(line) => Some(Some(line)),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Some(None),
        Err(_) => None,
    });
    let driven = drive.lines(lines.flatten());

    eprintln!(
        "decoder_fuzz[{source}]: {} lines, {} parsed, {} events, {} decode-err",
        driven.lines,
        driven.lines - driven.unparseable,
        driven.events.len(),
        driven.decode_errors.len()
    );
    // An empty corpus tested nothing, yet would exit 0 as "never-panic verified".
    if driven.lines == 0 {
        eprintln!("decoder_fuzz: no lines on stdin — nothing was fuzzed");
        std::process::exit(1);
    }
}
