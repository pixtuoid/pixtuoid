//! Each profiled CLI's [`Profile`](super::Profile), in a file named as its
//! `source/` module, and what more than one of them reads.

use std::borrow::Cow;

use serde_json::Value;

pub(super) mod antigravity;
pub(super) mod claude_code;
pub(super) mod codewhale;
pub(super) mod codex;
pub(super) mod copilot;
pub(super) mod dsh;
pub(super) mod grok;
pub(super) mod hermes;
pub(super) mod kimi;
pub(super) mod omp;
pub(super) mod openclaw;
pub(super) mod opencode;
pub(super) mod reasonix;

/// The scripted model's `/v1` base, as a profile's env spells it.
const AIMOCK_V1: &str = "http://127.0.0.1:{port}/v1";

/// The prompt a TUI's second turn types: its script tells the turns apart by
/// it.
const SECOND_TURN: &str = "Second turn: read NOTE.txt again.";

fn hook_event_name(v: &Value) -> Option<Cow<'_, str>> {
    v.get("hook_event_name")?.as_str().map(Cow::Borrowed)
}

fn type_field(v: &Value) -> Option<Cow<'_, str>> {
    v.get("type")?.as_str().map(Cow::Borrowed)
}

fn event_field(v: &Value) -> Option<Cow<'_, str>> {
    v.get("event")?.as_str().map(Cow::Borrowed)
}
