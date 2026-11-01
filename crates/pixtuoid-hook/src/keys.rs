//! The keys the shim stamps on an envelope, and the one source name it branches
//! on, in their own file so `pixtuoid-core/tests/shim_stamp_parity.rs` can
//! `#[path]`-include them and pin each to core's: the shim stays free of
//! pixtuoid-core.

pub(crate) const SHIM_STAMP: &str = "_shim_ts_ms";
pub(crate) const SOURCE_STAMP: &str = "_pixtuoid_source";
pub(crate) const EVENT_STAMP: &str = "_pixtuoid_event";
pub(crate) const PID_STAMP: &str = "_pid";
pub(crate) const JCODE_SOURCE: &str = "jcode";
