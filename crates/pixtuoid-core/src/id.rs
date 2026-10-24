use serde::{Deserialize, Serialize};
use std::fmt;

/// An opaque, source-namespaced session identifier — a 64-bit FNV-1a hash of
/// `(source, opaque_id)` (see [`AgentId::from_parts`]), so two sources never
/// collide on the same opaque id.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgentId(u64);

/// splitmix64 finalizer. FNV-1a (used by [`AgentId::from_parts`]) doesn't
/// avalanche the mid/high bits for short, similar inputs — desk-adjacent ids
/// collide to a couple of buckets — so the personality slicers finalize the raw
/// id through this before taking a bit window. Not cryptographic.
#[doc(hidden)]
pub fn splitmix64(z: u64) -> u64 {
    let z = (z ^ (z >> 30)).wrapping_mul(SPLITMIX64_M1);
    let z = (z ^ (z >> 27)).wrapping_mul(SPLITMIX64_M2);
    z ^ (z >> 31)
}

#[doc(hidden)]
pub const SPLITMIX64_M1: u64 = 0xbf58_476d_1ce4_e5b9;
#[doc(hidden)]
pub const SPLITMIX64_M2: u64 = 0x94d0_49bb_1331_11eb;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `words`, each folded in whole: the bytes of a string, or wider
/// words where a caller hashes numbers.
#[doc(hidden)]
pub fn fnv1a(words: impl IntoIterator<Item = u64>) -> u64 {
    words
        .into_iter()
        .fold(FNV_OFFSET_BASIS, |h, w| (h ^ w).wrapping_mul(FNV_PRIME))
}

/// Canonical form of a path STRING before it is used as an identity key
/// (an `AgentId` transcript-path key, or the palette's cwd outfit key).
/// Identity on Unix. On Windows: `\`→`/` + lowercase — CC emits backslash
/// paths in hook payloads but mixes `\`/`/` forms of the same file
/// internally, and NTFS is case-insensitive; without folding, the hook key
/// and the watcher key hash to two different AgentIds and every session
/// renders as TWO sprites.
///
/// Lives HERE (the identity-keying module), not in `source::decoder`: the
/// `pixtuoid-scene` palette shares this one identity-key definition, and the
/// render layer must not depend on the decoder layer for it.
#[doc(hidden)]
pub fn normalize_path_key(s: &str) -> String {
    normalize_key_inner(cfg!(windows), s)
}

/// Pure core, separated so the Windows arm is unit-testable on any platform.
fn normalize_key_inner(windows: bool, s: &str) -> String {
    if !windows {
        return s.to_string();
    }
    // Strip the `\\?\` verbatim prefix before folding, so a verbatim-prefixed path
    // (what `std::fs::canonicalize` returns on Windows) keys the same as its plain
    // form — otherwise `\\?\C:\X` folds to `//?/c:/x` and never coalesces with
    // `C:\X` (#197). `\\?\UNC\server\share` denotes `\\server\share`.
    let stripped = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    };
    stripped.replace('\\', "/").to_lowercase()
}

impl AgentId {
    /// Test/example opaque-id factory — `from_parts("claude-code",
    /// normalize_path_key(path))`. **Not** production CC keying, which keys on
    /// the session UUID via `cc_id_from_path`; do not call this in decode paths.
    #[doc(hidden)]
    pub fn from_transcript_path(path: &str) -> Self {
        Self::from_parts("claude-code", &normalize_path_key(path))
    }

    /// Source-agnostic factory. `source` matches `Source::name()`; `opaque_id`
    /// is whatever that source uses to uniquely identify a session. The pair is
    /// hashed, so two sources sharing an `opaque_id` still produce distinct ids.
    pub fn from_parts(source: &str, opaque_id: &str) -> Self {
        let bytes = source
            .bytes()
            // Domain separator, so ("a", "bc") can't collide with ("ab", "c").
            .chain([0xff])
            .chain(opaque_id.bytes());
        AgentId(fnv1a(bytes.map(u64::from)))
    }

    /// The underlying 64-bit hash (finalize through [`splitmix64`] before
    /// taking a bit window).
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_differs_per_path() {
        let a = AgentId::from_transcript_path("/Users/me/.claude/projects/x/abc.jsonl");
        let b = AgentId::from_transcript_path("/Users/me/.claude/projects/x/def.jsonl");
        assert_ne!(a, b);
    }

    #[test]
    fn agent_id_displays_as_hex() {
        let id = AgentId::from_transcript_path("x");
        assert_eq!(format!("{id}").len(), 16);
    }

    #[test]
    fn from_parts_distinguishes_source_and_opaque() {
        let cc = AgentId::from_parts("claude-code", "session-123");
        let cx = AgentId::from_parts("codex", "session-123");
        assert_ne!(cc, cx);
    }

    #[test]
    fn from_parts_has_domain_separator() {
        let a = AgentId::from_parts("a", "bc");
        let b = AgentId::from_parts("ab", "c");
        assert_ne!(a, b);
    }

    #[test]
    fn normalize_path_key_is_identity_on_unix() {
        assert_eq!(
            normalize_key_inner(false, "/Users/Me/.claude/projects/X/s.jsonl"),
            "/Users/Me/.claude/projects/X/s.jsonl"
        );
    }

    #[test]
    fn normalize_path_key_folds_separators_and_case_on_windows() {
        let a = normalize_key_inner(true, r"C:\Users\Me\.claude\projects\X\s.jsonl");
        assert_eq!(a, "c:/users/me/.claude/projects/x/s.jsonl");
        assert_eq!(
            normalize_key_inner(true, r"C:\Users\Me\x\s.jsonl"),
            normalize_key_inner(true, "C:/users/me/X/s.jsonl")
        );
    }

    #[test]
    fn normalize_path_key_strips_verbatim_prefix_on_windows() {
        assert_eq!(
            normalize_key_inner(true, r"\\?\C:\Foo\Bar.jsonl"),
            normalize_key_inner(true, r"C:\Foo\Bar.jsonl")
        );
        assert_eq!(normalize_key_inner(true, r"\\?\C:\Foo"), "c:/foo");
        // \\?\UNC\server\share denotes \\server\share — they must coalesce.
        assert_eq!(
            normalize_key_inner(true, r"\\?\UNC\srv\share\s.jsonl"),
            normalize_key_inner(true, r"\\srv\share\s.jsonl")
        );
    }

    #[test]
    fn normalize_path_key_verbatim_prefix_is_inert_on_unix() {
        assert_eq!(normalize_key_inner(false, r"\\?\C:\Foo"), r"\\?\C:\Foo");
    }
}
