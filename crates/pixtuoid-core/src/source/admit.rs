//! The one rule for what the watcher reads of a source's tree: which entries are
//! its transcripts (`classify`) and which bytes of one are its lines ([`lines`]).
//!
//! `lines` is in every build, since the tail checkers read it; `classify` and
//! `whole` whenever `native` or `harness` is on, either alone being enough: the
//! production watcher is behind `native` and the offline drivers behind
//! `harness`, and the whole point is that both ask the same question. The
//! TRAVERSAL stays with each caller — the watcher's is async and retires a
//! vanished path's cursor, the harness's is a plain recursion.

#[cfg(any(feature = "native", feature = "harness"))]
use std::path::Path;
use std::str::Utf8Error;

/// What the walker should do with one directory entry.
#[cfg(any(feature = "native", feature = "harness"))]
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
#[cfg(any(feature = "native", feature = "harness"))]
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

/// `span`'s whole lines: everything through its last `\n`. What follows is a line
/// still being written, which a reader with a cursor holds back until its `\n`
/// lands.
#[cfg(any(feature = "native", feature = "harness"))]
pub(crate) fn whole(span: &[u8]) -> &[u8] {
    span.iter()
        .rposition(|&b| b == b'\n')
        .map_or(&[], |i| &span[..=i])
}

/// `span`'s lines: split on `\n`, empty ones dropped, and one that is not UTF-8
/// an `Err` costing that line alone, never the rest of its file.
pub(crate) fn lines(span: &[u8]) -> impl Iterator<Item = Result<&str, Utf8Error>> {
    span.split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(std::str::from_utf8)
}

/// [`lines`] of a window read back from mid-file: its first, partial line dropped.
#[cfg(feature = "native")]
pub(crate) fn window_lines(window: &[u8]) -> impl Iterator<Item = Result<&str, Utf8Error>> {
    lines(
        window
            .iter()
            .position(|&b| b == b'\n')
            .map_or(&[], |i| &window[i + 1..]),
    )
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

    #[test]
    fn whole_holds_back_an_unterminated_last_line() {
        assert_eq!(whole(b"{\"a\":1}\n{\"b\":2}"), b"{\"a\":1}\n");
        assert_eq!(whole(b"{\"b\":2}"), b"");
        let got: Vec<_> = lines(whole(b"{\"a\":1}\n\n\xff\n{\"b\":2}")).collect();
        assert_eq!(got.len(), 2, "got {got:?}");
        assert_eq!(got[0], Ok("{\"a\":1}"));
        assert!(got[1].is_err(), "the non-UTF-8 line is its own Err");
    }

    #[cfg(feature = "native")]
    #[test]
    fn a_window_drops_its_partial_first_line_and_only_that() {
        let got: Vec<_> = window_lines(b"rt\"}\n{\"a\":1}\n{\"b\":2}").collect();
        assert_eq!(got, [Ok("{\"a\":1}"), Ok("{\"b\":2}")]);
        let on_a_newline: Vec<_> = window_lines(b"\n{\"a\":1}\n").collect();
        assert_eq!(on_a_newline, [Ok("{\"a\":1}")], "no real line is lost");
    }
}
