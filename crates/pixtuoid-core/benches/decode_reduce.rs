//! Wire→state benchmark: the decode→reduce half that runs on every transcript
//! append and every hook envelope, driven through `harness::Drive` so it can't
//! measure a decoder production doesn't use. The render benchmark next door
//! costs a FRAME; this costs an EVENT.
//!
//! `Drive` folds through a real `Reducer` and RETAINS every decoded event, so
//! the curve carries a constant harness component production doesn't pay (one
//! `AgentEvent` clone per event, plus a per-event `tick` the runtime schedules
//! on a timer). Read the memory mode's movement as directional, not absolute.
//!
//! Both transports are measured because their costs are structurally different:
//! the JSONL arm runs a transcript line decoder, the hook arm a hook decoder
//! plus the `recent_hook_tool_uses` dedup bookkeeping.
//!
//! Lines are synthesized rather than read from `tests/sources/fixtures/`, which
//! is the conformance harness's scanned population — one transcript per scenario
//! dir, every dir a registered source — so a bench-shaped fixture there would be
//! mis-scanned and panic.

use criterion::{Criterion, criterion_main};
use pixtuoid_core::harness::Drive;

const SESSION: &str = "01000000-0000-7000-8000-0000000000cc";
const CWD: &str = "/home/user/demo-project";
const TRANSCRIPT: &str = "/p/01000000-0000-7000-8000-0000000000cc.jsonl";
const OMP_TRANSCRIPT: &str =
    "/h/.omp/agent/sessions/-p/2026-01-01T00-00-00-000Z_01000000-0000-7000-8000-0000000000cc.jsonl";
/// One turn is a tool call and its result — two lines on either transport.
const TURNS: usize = 200;

fn jsonl_turns(n: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        out.push(format!(
            r#"{{"type":"message","id":"{i:08x}","parentId":"","timestamp":"2026-01-01T00:00:00.000Z","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"call_{i:016x}","name":"glob","arguments":{{"pattern":"crates/**/*.rs"}}}}]}}}}"#
        ));
        out.push(format!(
            r#"{{"type":"message","id":"{i:08x}","parentId":"","timestamp":"2026-01-01T00:00:00.500Z","message":{{"role":"toolResult","toolCallId":"call_{i:016x}","content":[{{"type":"text","text":"(matches)"}}]}}}}"#
        ));
    }
    out
}

fn hook_turns(n: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(n * 2 + 1);
    out.push(format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"{SESSION}","transcript_path":"{TRANSCRIPT}","cwd":"{CWD}","source":"startup"}}"#
    ));
    for i in 0..n {
        out.push(format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"{SESSION}","transcript_path":"{TRANSCRIPT}","cwd":"{CWD}","tool_name":"Glob","tool_input":{{"pattern":"crates/**/*.rs"}},"tool_use_id":"toolu_{i:016x}"}}"#
        ));
        out.push(format!(
            r#"{{"hook_event_name":"PostToolUse","session_id":"{SESSION}","transcript_path":"{TRANSCRIPT}","tool_name":"Glob","tool_use_id":"toolu_{i:016x}"}}"#
        ));
    }
    out
}

fn decode_reduce(c: &mut Criterion) {
    let jsonl = jsonl_turns(TURNS);
    let hooks = hook_turns(TURNS);
    let transcript = Drive::transcript("omp", OMP_TRANSCRIPT)
        .expect("omp is transcript-bearing")
        .seeded();
    let hook = Drive::hooks();

    // Exact counts, not a floor: a decoder arm that DECLINES (an empty result, the
    // shape of the documented quiet-drop paths) halves this while leaving
    // `assert_clean` green, and a floor would not notice.
    for (what, driven, want) in [
        ("jsonl", transcript.lines(&jsonl), 2 * TURNS),
        ("hooks", hook.lines(&hooks), 4 * TURNS + 1),
    ] {
        driven.assert_clean(what);
        assert_eq!(driven.registered(), 1, "{what}: one session must register");
        assert_eq!(driven.wire_events(), want, "{what}: wire event count");
    }

    let mut group = c.benchmark_group("decode_reduce");
    group.bench_function(format!("jsonl_{TURNS}_turns"), |b| {
        b.iter(|| transcript.lines(&jsonl))
    });
    group.bench_function(format!("hook_{TURNS}_turns"), |b| {
        b.iter(|| hook.lines(&hooks))
    });
    group.finish();
}

// A module, because rustc ignores a lint attribute on the macro call itself.
#[expect(
    clippy::disallowed_methods,
    reason = "codspeed's `criterion_group!` reads CODSPEED_ENV and CODSPEED_CARGO_WORKSPACE_ROOT with `env::var`"
)]
mod group {
    use super::decode_reduce;
    criterion::criterion_group!(benches, decode_reduce);
}
criterion_main!(group::benches);
