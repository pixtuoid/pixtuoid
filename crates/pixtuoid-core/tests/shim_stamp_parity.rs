//! Each key the shim stamps is the one the daemon reads. The shim's source is
//! included via `#[path]`, NOT a cargo dependency, because the hook crate must
//! stay free of pixtuoid-core (`pixtuoid-hook/src/keys.rs`).

#[path = "../../pixtuoid-hook/src/keys.rs"]
mod shim_keys;

use pixtuoid_core::source::decoder;

#[test]
fn the_shim_stamps_each_key_the_daemon_reads() {
    assert_eq!(
        shim_keys::SHIM_STAMP,
        pixtuoid_core::harness::merge::SHIM_STAMP
    );
    assert_eq!(shim_keys::SOURCE_STAMP, decoder::SOURCE_STAMP);
    assert_eq!(shim_keys::EVENT_STAMP, decoder::EVENT_STAMP);
    assert_eq!(shim_keys::PID_STAMP, decoder::PID_STAMP);
}
