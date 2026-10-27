use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use pixtuoid_core::sprite::format::Piece;
use pixtuoid_core::sprite::{Frame, Rgb};
use pixtuoid_core::state::ActivityState;
use pixtuoid_core::{AgentId, AgentSlot, GlobalDeskIndex, SceneState};
use pixtuoid_scene::frame_cache::{FrameCache, FrameKey};

fn key(id: AgentId, anim_name: Piece, frame_idx: usize, flip_x: bool) -> FrameKey {
    FrameKey {
        agent_id: id,
        anim_name,
        frame_idx,
        flip_x,
        glow_tint: None,
        burn: pixtuoid_scene::burn::BurnTier::Normal,
        density: pixtuoid_core::sprite::format::Density::ONE,
    }
}

fn dummy_frame(seed: u8) -> Frame {
    Frame::from_pixels(
        1,
        1,
        vec![Some(Rgb {
            r: seed,
            g: seed,
            b: seed,
        })],
    )
}

fn make_slot(id: AgentId) -> AgentSlot {
    let now = SystemTime::UNIX_EPOCH;
    AgentSlot {
        agent_id: id,
        source: Arc::from("claude-code"),
        session_id: Arc::from("s"),
        cwd: Arc::from(PathBuf::from("/x").as_path()),
        label: "x".into(),
        state: ActivityState::Idle,
        state_started_at: now,
        created_at: now,
        last_event_at: now,
        exiting_at: None,

        desk_index: GlobalDeskIndex(0),
        floor_idx: 0,
        tool_call_count: 0,
        active_ms: 0,
        unknown_cwd: false,
        parent_id: None,
        pid: None,
        model: None,
        effort: None,
        tokens_used: 0,
        last_usage: None,
    }
}

#[test]
fn get_or_make_caches_by_full_key() {
    use std::cell::Cell;

    let mut cache = FrameCache::new();
    let id = AgentId::from_transcript_path("/a.jsonl");
    let compute_calls = Cell::new(0u32);

    let f1 = cache
        .get_or_make(key(id, Piece::Walking, 0, false), || {
            compute_calls.set(compute_calls.get() + 1);
            dummy_frame(1)
        })
        .clone();
    assert_eq!(compute_calls.get(), 1);
    assert_eq!(f1.as_slice()[0], Some(Rgb { r: 1, g: 1, b: 1 }));

    let f2 = cache
        .get_or_make(key(id, Piece::Walking, 0, false), || {
            compute_calls.set(compute_calls.get() + 1);
            dummy_frame(99)
        })
        .clone();
    assert_eq!(
        compute_calls.get(),
        1,
        "second lookup with same key must not recompute"
    );
    assert_eq!(f2.as_slice()[0], Some(Rgb { r: 1, g: 1, b: 1 }));

    cache.get_or_make(key(id, Piece::Walking, 1, false), || {
        compute_calls.set(compute_calls.get() + 1);
        dummy_frame(2)
    });
    assert_eq!(compute_calls.get(), 2);

    cache.get_or_make(key(id, Piece::Walking, 0, true), || {
        compute_calls.set(compute_calls.get() + 1);
        dummy_frame(3)
    });
    assert_eq!(compute_calls.get(), 3);

    cache.get_or_make(key(id, Piece::Seated, 0, false), || {
        compute_calls.set(compute_calls.get() + 1);
        dummy_frame(4)
    });
    assert_eq!(compute_calls.get(), 4);

    assert_eq!(cache.len(), 4);
}

#[test]
fn evict_missing_drops_entries_for_absent_agents() {
    let mut cache = FrameCache::new();
    let kept = AgentId::from_transcript_path("/kept.jsonl");
    let gone = AgentId::from_transcript_path("/gone.jsonl");

    cache.get_or_make(key(kept, Piece::Walking, 0, false), || dummy_frame(1));
    cache.get_or_make(key(gone, Piece::Walking, 0, false), || dummy_frame(2));
    cache.get_or_make(key(gone, Piece::Seated, 0, false), || dummy_frame(3));
    assert_eq!(cache.len(), 3);

    let mut scene = SceneState::uniform(4);
    scene.agents.insert(kept, make_slot(kept));

    cache.evict_missing(&scene);

    assert_eq!(
        cache.len(),
        1,
        "two entries for the absent agent should be dropped"
    );
    let _ = cache.get_or_make(key(kept, Piece::Walking, 0, false), || {
        panic!("evict must not have dropped the kept agent's entry")
    });
}
