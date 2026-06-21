//! Pure update-decision logic for the Obsidian deferred-install pattern (spec §Auto-updater threat
//! model). These are the **CI-testable** half of the updater: version-change detection and the
//! visible-after notice gate.
//!
//! The actual `download()` / `install()` over a real signed release is the **I/O boundary**
//! (`commands::update`) and is **not** exercised here — minisign verification, the staged download,
//! and the on-quit apply need a published signed artifact + a real OS installer (decision #7/#8).

/// Has the running version changed since the one last seen? **Exact string equality** — not semver
/// parsing. The writer normalizes `last_seen` to the running-version string on first write, so
/// there is no `"1.0"`-vs-`"1.0.0"` fork. A first-ever launch (`last_seen` empty) is **not** a
/// change — there is no prior version to have updated *from*.
pub fn version_changed(last_seen: &str, current: &str) -> bool {
    !last_seen.is_empty() && last_seen != current
}

/// Should the post-update "now running vX.Y — what's new" notice fire? Fires once per version
/// change: the caller persists `last_seen = current` after showing it, so it does not re-fire.
pub fn should_show_update_notice(last_seen: &str, current: &str) -> bool {
    version_changed(last_seen, current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_change_since_last_seen_detected() {
        assert!(version_changed("0.9.0", "1.0.0"));
        assert!(should_show_update_notice("0.9.0", "1.0.0"));
    }

    #[test]
    fn same_version_is_no_update() {
        assert!(!version_changed("1.0.0", "1.0.0"));
        assert!(!should_show_update_notice("1.0.0", "1.0.0"));
    }

    #[test]
    fn first_launch_is_not_an_update_notice() {
        // Empty last_seen = a fresh install, not an update — no "now on vX" notice.
        assert!(!version_changed("", "1.0.0"));
        assert!(!should_show_update_notice("", "1.0.0"));
    }

    #[test]
    fn post_update_notice_fires_once_per_version_change() {
        let current = "1.0.0";
        let mut last_seen = "0.9.0".to_string();
        assert!(should_show_update_notice(&last_seen, current), "fires on the version change");
        // Caller persists last_seen = current after showing it.
        last_seen = current.to_string();
        assert!(!should_show_update_notice(&last_seen, current), "does not re-fire on the next launch");
    }
}
