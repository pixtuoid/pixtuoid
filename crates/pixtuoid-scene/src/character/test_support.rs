//! `AgentSlot` fixtures the crate's tests share.

use std::time::SystemTime;

use pixtuoid_core::AgentSlot;

use super::colors::agent_overrides;

pub(crate) fn make_slot(
    id: pixtuoid_core::AgentId,
    state: pixtuoid_core::state::ActivityState,
) -> AgentSlot {
    let now = SystemTime::UNIX_EPOCH;
    AgentSlot {
        agent_id: id,
        source: std::sync::Arc::from("claude-code"),
        session_id: std::sync::Arc::from("s"),
        cwd: std::sync::Arc::from(std::path::Path::new("/x")),
        label: "x".into(),
        state,
        state_started_at: now,
        created_at: now,
        last_event_at: now,
        exiting_at: None,

        desk_index: pixtuoid_core::state::GlobalDeskIndex(0),
        floor_idx: 0,
        tool_call_count: 0,
        running_ms: 0,
        unknown_cwd: false,
        parent_id: None,
        pid: None,
        model: None,
        effort: None,
        tokens_used: 0,
        last_usage: None,
    }
}

pub(crate) fn make_slot_cwd(id_path: &str, cwd: &str, unknown_cwd: bool) -> AgentSlot {
    let id = pixtuoid_core::AgentId::from_transcript_path(id_path);
    let mut s = make_slot(id, pixtuoid_core::state::ActivityState::Idle);
    s.cwd = std::sync::Arc::from(std::path::Path::new(cwd));
    s.unknown_cwd = unknown_cwd;
    s
}

/// `key`'s color for `slot`, unlit and unburnt.
pub(crate) fn color_of(slot: &AgentSlot, key: char) -> pixtuoid_core::sprite::Pixel {
    override_of(
        &agent_overrides(slot, None, crate::burn::BurnTier::Normal),
        key,
    )
}

/// `key`'s color in an agent's overrides.
pub(crate) fn override_of(
    overrides: &[(char, pixtuoid_core::sprite::Pixel)],
    key: char,
) -> pixtuoid_core::sprite::Pixel {
    overrides
        .iter()
        .find(|(k, _)| *k == key)
        .unwrap_or_else(|| panic!("no override for {key:?}"))
        .1
}
