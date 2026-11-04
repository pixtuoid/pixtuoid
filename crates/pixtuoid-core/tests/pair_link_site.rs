//! A pairing link opens a page the crate's site serves, for a phone without the
//! App: its host is the crate's homepage and its path a page in `site/public`.

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use pixtuoid_core::remote::PairLink;

#[test]
fn a_link_opens_a_page_the_site_serves() {
    let link = PairLink {
        host: "studio.local".into(),
        port: 47_820,
        spki: "A".repeat(64),
        code: "A".repeat(64),
        expires: UNIX_EPOCH + Duration::from_secs(1_800_000_000),
    }
    .to_string();
    let (page, _fields) = link.split_once('#').expect("its fields ride the fragment");
    let path = page
        .strip_prefix(concat!(env!("CARGO_PKG_HOMEPAGE"), "/"))
        .expect("the link is on the site");
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../site/public")
        .join(path)
        .join("index.html");
    assert!(file.is_file(), "{file:?}");
}
