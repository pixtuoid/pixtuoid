//! The §3 proof panel and the statusline's agent-activity ticker are two
//! agent-narration surfaces sharing one viewport at 4F: a reused file name or
//! task phrase would read as the same event narrated twice.
//!
//! Runtime read of `site/...`, NOT `include_str!`: a published `.crate` tarball
//! has no sibling `site/` tree.

use std::path::{Path, PathBuf};

const STATUSLINE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../site/src/components/Statusline.astro"
);

/// The session the proof clip types, beside its renderer.
fn proof_session() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/snapshot/proof-session/01000000-0000-7000-8000-0000000000f4.jsonl")
}

/// The same content a viewer reads off the proof panel; JSON syntax and ids
/// aren't "visible" in that sense and are deliberately excluded.
fn session_visible_strings() -> Vec<String> {
    let path = proof_session();
    let transcript =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let mut out = Vec::new();
    for line in transcript.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("bad json in {}: {e}", path.display()));
        let content = &v["message"]["content"];
        if let Some(text) = content.as_str() {
            out.push(text.to_string());
        }
        if let Some(blocks) = content.as_array() {
            for b in blocks {
                if b["type"] != "tool_use" {
                    continue;
                }
                let Some(input) = b["input"].as_object() else {
                    continue;
                };
                for key in ["file_path", "command", "pattern", "path"] {
                    if let Some(s) = input.get(key).and_then(|v| v.as_str()) {
                        out.push(s.to_string());
                    }
                }
            }
        }
    }
    out
}

fn ticker_task_phrases() -> Vec<String> {
    let src = std::fs::read_to_string(STATUSLINE_PATH)
        .unwrap_or_else(|e| panic!("read {STATUSLINE_PATH}: {e}"));
    let start = src
        .find("const FALLBACK = [")
        .expect("Statusline.astro's `const FALLBACK = [` array moved or was renamed");
    let end = src[start..]
        .find("];")
        .expect("Statusline.astro's FALLBACK array has no closing `];`")
        + start;
    let block = &src[start..end];

    let mut out = Vec::new();
    let needle = "what: '";
    let mut cursor = 0;
    while let Some(rel) = block[cursor..].find(needle) {
        let after = cursor + rel + needle.len();
        let close = block[after..]
            .find('\'')
            .expect("unterminated FALLBACK `what` string literal");
        out.push(block[after..after + close].to_string());
        cursor = after + close;
    }
    out
}

#[test]
fn proof_session_story_is_disjoint_from_the_statusline_ticker_corpus() {
    let session_strings = session_visible_strings();
    assert!(
        session_strings.len() >= 4,
        "expected the task prompt + 3 tool args, got {}: {session_strings:?} — \
         the proof session's shape changed",
        session_strings.len()
    );

    let ticker_phrases = ticker_task_phrases();
    assert!(
        ticker_phrases.len() >= 6,
        "expected at least 6 FALLBACK task phrases, parsed {}: {ticker_phrases:?} — \
         the extraction may have drifted from Statusline.astro's shape",
        ticker_phrases.len()
    );

    for session_str in &session_strings {
        for phrase in &ticker_phrases {
            assert!(
                !session_str.contains(phrase.as_str()) && !phrase.contains(session_str.as_str()),
                "proof session string {session_str:?} collides with the statusline \
                 ticker's FALLBACK phrase {phrase:?} — the two share a viewport at 4F and must \
                 read as distinct events (STATUSLINE-COLLISION handoff, pick different session \
                 content)"
            );
        }
    }
}
