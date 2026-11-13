//! What a new version means at boot: whether its popup shows.

use semver::Version;

pub(crate) struct BootDecision {
    pub(crate) should_show_popup: bool,
    pub(crate) should_persist: bool,
}

/// Decide whether the version popup should fire on boot and whether to persist
/// `last_seen_version`. Persist also happens on a first-time install and on an
/// UNPARSEABLE recorded version — overwrite to recover, else a corrupted or
/// hand-edited value silently disables the popup forever.
pub(crate) fn boot_decision(current: &Version, last_seen: Option<&str>) -> BootDecision {
    let previous = last_seen.and_then(|v| Version::parse(v).ok());
    let should_show_popup = previous.as_ref().is_some_and(|p| current > p);
    BootDecision {
        should_show_popup,
        should_persist: should_show_popup || previous.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_newer_version(current: &str, last_seen: &str) -> bool {
        boot_decision(&v(current), Some(last_seen)).should_show_popup
    }

    #[test]
    fn prerelease_newer_than_older_release() {
        assert!(is_newer_version("0.5.0-alpha", "0.4.0"));
    }

    #[test]
    fn release_newer_than_prerelease_of_same_version() {
        assert!(is_newer_version("0.5.0", "0.5.0-rc1"));
        assert!(!is_newer_version("0.5.0-rc1", "0.5.0"));
    }

    #[test]
    fn a_later_release_candidate_is_newer() {
        assert!(is_newer_version("0.5.0-rc.10", "0.5.0-rc.2"));
        assert!(!is_newer_version("0.5.0-rc.2", "0.5.0-rc.10"));
    }

    // `v0.4.0` is the git-tag spelling — the corruption a hand-edit actually hits.
    #[test]
    fn boot_decision_overwrites_corrupted_last_seen() {
        for corrupted in ["v0.4.0", "0.5", "garbage", ""] {
            let d = boot_decision(&v("0.4.1"), Some(corrupted));
            assert!(
                !d.should_show_popup,
                "can't show popup when comparison fails: {corrupted:?}"
            );
            assert!(
                d.should_persist,
                "corrupted last_seen must be overwritten to recover: {corrupted:?}"
            );
        }
    }

    #[test]
    fn boot_decision_first_run_persists_silently() {
        let d = boot_decision(&v("0.4.1"), None);
        assert!(!d.should_show_popup);
        assert!(d.should_persist);
    }

    #[test]
    fn boot_decision_upgrade_shows_popup_and_persists() {
        let d = boot_decision(&v("0.4.0"), Some("0.3.0"));
        assert!(d.should_show_popup);
        assert!(d.should_persist);
    }

    #[test]
    fn boot_decision_same_version_no_action() {
        let d = boot_decision(&v("0.4.0"), Some("0.4.0"));
        assert!(!d.should_show_popup);
        assert!(!d.should_persist);
    }

    #[test]
    fn boot_decision_downgrade_no_action() {
        let d = boot_decision(&v("0.3.0"), Some("0.4.0"));
        assert!(!d.should_show_popup);
        assert!(!d.should_persist);
    }

    fn v(s: &str) -> Version {
        Version::parse(s).expect("a test version")
    }
}
