//! The one rule for what the watcher reads of a source's tree: which entries are
//! its transcripts ([`classify`]) and which bytes of one are its lines ([`lines`]).
//!
//! Compiled whenever `native` or `harness` is on, either alone being enough: the
//! production watcher is behind `native` and the offline drivers behind
//! `harness`, and the whole point is that both ask the same question. The
//! TRAVERSAL stays with each caller — the watcher's is async and retires a
//! vanished path's cursor, the harness's is a plain recursion.

use std::path::Path;

/// What the walker should do with one directory entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Entry {
    /// A real directory — descend.
    Recurse,
    /// A transcript this source watches.
    Take,
    /// A symlink of either kind. Its own variant because the watcher logs this
    /// case and only this case — folded into `Skip`, the caller has to re-ask
    /// `is_symlink()` to tell them apart.
    SkipSymlink,
    /// Not a transcript: wrong extension, or the source's filter declined it.
    Skip,
}

/// Classify one entry from its `symlink_metadata` and the source's own filter.
///
/// A symlinked entry is refused before anything else: a directory link would
/// recurse a loop or drag a foreign tree into this source's id space, and a file
/// link would double-count one transcript.
pub(crate) fn classify(
    meta: &std::fs::Metadata,
    path: &Path,
    admits: &dyn Fn(&Path) -> bool,
) -> Entry {
    if meta.file_type().is_symlink() {
        return Entry::SkipSymlink;
    }
    if meta.is_dir() {
        return Entry::Recurse;
    }
    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
        return Entry::Skip;
    }
    if !admits(path) {
        return Entry::Skip;
    }
    Entry::Take
}

/// A transcript body's lines: split on `\n`, empty ones dropped, and one that is
/// not UTF-8 an `Err` costing that line alone, never the rest of its file.
pub(crate) fn lines(body: &[u8]) -> impl Iterator<Item = Result<&str, std::str::Utf8Error>> {
    body.split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(std::str::from_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_of(p: &Path) -> std::fs::Metadata {
        std::fs::symlink_metadata(p).expect("symlink_metadata")
    }

    // Whole test Unix-gated: a Windows directory symlink needs a different API
    // and a privilege this suite does not assume, and gating only the CREATION
    // left the assertions claiming a platform they never ran on.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_skipped_whether_it_names_a_dir_or_a_file() {
        let d = tempfile::tempdir().expect("tempdir");
        let real_dir = d.path().join("d");
        let real_file = d.path().join("a.jsonl");
        std::fs::create_dir(&real_dir).expect("mkdir");
        std::fs::write(&real_file, "{}").expect("write");
        let (dir_link, file_link) = (d.path().join("dl"), d.path().join("fl.jsonl"));
        std::os::unix::fs::symlink(&real_dir, &dir_link).expect("symlink");
        std::os::unix::fs::symlink(&real_file, &file_link).expect("symlink");
        let yes = |_: &Path| true;
        assert_eq!(
            classify(&meta_of(&dir_link), &dir_link, &yes),
            Entry::SkipSymlink
        );
        assert_eq!(
            classify(&meta_of(&file_link), &file_link, &yes),
            Entry::SkipSymlink
        );
        assert_eq!(
            classify(&meta_of(&real_dir), &real_dir, &yes),
            Entry::Recurse
        );
        assert_eq!(
            classify(&meta_of(&real_file), &real_file, &yes),
            Entry::Take
        );
    }

    #[test]
    fn the_extension_and_the_sources_own_filter_both_gate_a_take() {
        let d = tempfile::tempdir().expect("tempdir");
        let txt = d.path().join("notes.txt");
        let jsonl = d.path().join("updates.jsonl");
        std::fs::write(&txt, "x").expect("write");
        std::fs::write(&jsonl, "{}").expect("write");
        let yes = |_: &Path| true;
        let no = |_: &Path| false;
        assert_eq!(classify(&meta_of(&txt), &txt, &yes), Entry::Skip);
        assert_eq!(classify(&meta_of(&jsonl), &jsonl, &yes), Entry::Take);
        assert_eq!(classify(&meta_of(&jsonl), &jsonl, &no), Entry::Skip);
    }
}
