//! Per-agent cache of recolored sprite frames.
//!
//! A recolored frame is stable for as long as its palette inputs are: hair/skin
//! are `agent_id`-seeded (fixed for the agent's lifetime), and the OUTFIT is
//! keyed on the agent's cwd — mutable mid-lifetime via a cwd backfill, which
//! `FrameCache::note_outfit_seed` detects. With that one invalidation, caching
//! is safe.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::sprite::{Frame, Rgb};
use pixtuoid_core::{AgentId, SceneState};

/// Cache identity for one recolored frame — every input that changes the output
/// pixels is part of the key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub agent_id: AgentId,
    pub anim_name: crate::pack::Piece,
    pub frame_idx: usize,
    pub flip_x: bool,
    pub glow_tint: Option<Rgb>,
    /// Burn tier keys the recolor too (ember hair) — a tier flip mid-life
    /// simply misses to a fresh entry, evicted with the agent.
    pub burn: crate::burn::BurnTier,
    /// The grid the recolored art is authored on — 1 for the base sprite, `N`
    /// for its `@Nx` variant: the same frame at two densities is two images.
    pub density: crate::sprite::format::Density,
}

#[derive(Debug, Default)]
pub struct FrameCache {
    entries: HashMap<FrameKey, Frame>,
    /// Last-seen outfit-determining seed per agent.
    outfit_seeds: HashMap<AgentId, u64>,
}

impl FrameCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_make<F: FnOnce() -> Frame>(&mut self, key: FrameKey, compute: F) -> &Frame {
        self.entries.entry(key).or_insert_with(compute)
    }

    /// Record the outfit-determining seed for `id`, before `get_or_make`. A seed
    /// CHANGE (the cwd backfill) drops the agent's cached frames — otherwise
    /// already-cached poses keep the stale outfit for the agent's lifetime while
    /// new poses render the healed one. Callers pass the exact seed the palette
    /// derives (`sim::outfit_seed_for`).
    pub(crate) fn note_outfit_seed(&mut self, id: AgentId, seed: u64) {
        match self.outfit_seeds.entry(id) {
            Entry::Occupied(mut e) => {
                if *e.get() != seed {
                    e.insert(seed);
                    self.entries.retain(|k, _| k.agent_id != id);
                }
            }
            Entry::Vacant(v) => {
                v.insert(seed);
            }
        }
    }

    pub fn evict_missing(&mut self, scene: &SceneState) {
        self.entries
            .retain(|k, _| scene.agents.contains_key(&k.agent_id));
        self.outfit_seeds
            .retain(|id, _| scene.agents.contains_key(id));
    }

    /// Test-only inspection seam; `#[doc(hidden)]` because the cache is opaque
    /// to consumers.
    #[doc(hidden)]
    #[expect(
        clippy::len_without_is_empty,
        reason = "an inspection seam, not a collection"
    )]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_frame() -> Frame {
        Frame::from_pixels(1, 1, vec![None])
    }

    fn key_for(id: AgentId) -> FrameKey {
        FrameKey {
            agent_id: id,
            anim_name: crate::pack::Piece::Standing,
            frame_idx: 0,
            flip_x: false,
            glow_tint: None,
            burn: crate::burn::BurnTier::Normal,
            density: crate::sprite::format::Density::ONE,
        }
    }

    fn key() -> FrameKey {
        key_for(AgentId::from_transcript_path("/fc/a.jsonl"))
    }

    #[test]
    fn new_cache_is_empty_then_populated_after_get_or_make() {
        let mut cache = FrameCache::new();
        assert_eq!(cache.len(), 0);

        let _ = cache.get_or_make(key(), dummy_frame);
        assert_eq!(cache.len(), 1);

        let mut computed_again = false;
        let _ = cache.get_or_make(key(), || {
            computed_again = true;
            dummy_frame()
        });
        assert!(!computed_again, "cached key must not recompute");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn outfit_seed_change_drops_only_that_agents_entries() {
        let a = AgentId::from_transcript_path("/fc/a.jsonl");
        let b = AgentId::from_transcript_path("/fc/b.jsonl");
        let mut cache = FrameCache::new();
        cache.note_outfit_seed(a, 1);
        cache.note_outfit_seed(b, 7);
        let _ = cache.get_or_make(key_for(a), dummy_frame);
        let _ = cache.get_or_make(key_for(b), dummy_frame);
        assert_eq!(cache.len(), 2);

        cache.note_outfit_seed(a, 1);
        let mut recomputed = false;
        let _ = cache.get_or_make(key_for(a), || {
            recomputed = true;
            dummy_frame()
        });
        assert!(!recomputed, "an unchanged outfit seed must not evict");

        cache.note_outfit_seed(a, 2);
        let mut recomputed = false;
        let _ = cache.get_or_make(key_for(a), || {
            recomputed = true;
            dummy_frame()
        });
        assert!(
            recomputed,
            "a changed outfit seed must drop the agent's stale-outfit frames"
        );
        let mut b_recomputed = false;
        let _ = cache.get_or_make(key_for(b), || {
            b_recomputed = true;
            dummy_frame()
        });
        assert!(!b_recomputed, "another agent's cache must be untouched");
    }
}
