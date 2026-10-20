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

/// Whether a release's `sums` file lists `bytes`' digest for `name`.
pub fn vouches(sums: &str, name: &str, bytes: &[u8]) -> bool {
    let digest = sha256_hex(bytes);
    sums.lines().any(|l| {
        let mut cols = l.split_whitespace();
        cols.next() == Some(digest.as_str()) && cols.next() == Some(name)
    })
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
}
