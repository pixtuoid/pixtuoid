//! The checks on a download against the digest its release lists.

/// `bytes`' SHA-256 as a sums file lists it.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex::<sha2::Sha256>(bytes)
}

/// `bytes`' digest under `D` in lowercase hex: sha2's digest has no `LowerHex`.
pub fn digest_hex<D: sha2::Digest>(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    D::digest(bytes).iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

fn digests<'a>(sums: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> {
    sums.lines().filter_map(move |l| {
        let mut cols = l.split_whitespace();
        let digest = cols.next()?;
        let listed = cols.next()?;
        // A binary-mode line marks its name with one `*`, which jcode's
        // installer accepts (`scripts/install.sh:249` at v1.1.2).
        (listed.strip_prefix('*').unwrap_or(listed) == name).then_some(digest)
    })
}

/// Whether a release's `sums` file lists `name`.
pub fn lists(sums: &str, name: &str) -> bool {
    digests(sums, name).next().is_some()
}

/// Whether a release's `sums` file lists `bytes`' digest for `name`.
pub fn vouches(sums: &str, name: &str, bytes: &[u8]) -> bool {
    let digest = sha256_hex(bytes);
    digests(sums, name).any(|d| d == digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-2's "abc" vector, whose bytes below 0x10 catch a dropped pad.
    #[test]
    fn a_digest_reads_as_a_sums_file_lists_it() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_sums_file_vouches_for_its_own_line_alone() {
        let sums = format!(
            "{}  omp-linux-x64-baseline\n{}  omp-linux-x64\n{}  omp-darwin-arm64\n",
            sha256_hex(b"baseline"),
            sha256_hex(b"linux"),
            sha256_hex(b"darwin")
        );
        assert!(vouches(&sums, "omp-linux-x64", b"linux"));
        assert!(
            !vouches(&sums, "omp-linux-x64", b"darwin"),
            "another's digest"
        );
        assert!(
            !vouches(&sums, "omp-linux-x64", b"baseline"),
            "a longer name's digest"
        );
        assert!(!vouches(&sums, "omp-linux-x64", b"tampered"));
        assert!(!vouches(&sums, "omp-windows-x64.exe", b"linux"), "unlisted");
    }

    /// jcode's v1.1.2 lists its Linux asset alone.
    #[test]
    fn a_sums_file_lists_a_name_by_its_own_line_alone() {
        let sums = format!("{}  jcode-linux-x86_64.tar.gz\n", sha256_hex(b"linux"));
        assert!(lists(&sums, "jcode-linux-x86_64.tar.gz"));
        assert!(!lists(&sums, "jcode-macos-aarch64.tar.gz"), "unlisted");
        assert!(!lists(&sums, "jcode-linux-x86_64"), "a longer name's line");
    }

    #[test]
    fn a_binary_mode_line_lists_and_vouches_for_its_name() {
        let sums = format!("{} *jcode-linux-x86_64.tar.gz\n", sha256_hex(b"linux"));
        assert!(lists(&sums, "jcode-linux-x86_64.tar.gz"));
        assert!(vouches(&sums, "jcode-linux-x86_64.tar.gz", b"linux"));
        assert!(
            !lists(&sums, "*jcode-linux-x86_64.tar.gz"),
            "the marker is no part of the name"
        );
        let twice = format!("{} **jcode-linux-x86_64.tar.gz\n", sha256_hex(b"linux"));
        assert!(!lists(&twice, "jcode-linux-x86_64.tar.gz"), "one marker");
    }
}
