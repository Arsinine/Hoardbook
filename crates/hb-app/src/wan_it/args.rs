//! Argument parsing for `hb-wan-it` — pure helpers (`flag_value`, `collect_relays`) reused from the
//! retired `hb-p2p-it` harness, plus the WAN-specific relay-set validators (the P3 flood-guard and
//! the public-only guard). Pure so they unit-test without a process.

/// The first positional command word (`serve` / `probe` / `canary`), or `None` if absent.
pub fn command(args: &[String]) -> Option<&str> {
    args.first().map(String::as_str)
}

/// The value following `name` in `args`, if present. Mirrors the retired harness's helper exactly
/// (including the shared lifetime — the borrow ties to whichever input actually owns it).
pub fn flag_value<'a>(args: &'a [String], name: &'a str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Every value following a repeated `--relay` flag (trimmed of trailing slashes). Mirrors the retired
/// harness's helper; the harness passes its relay set explicitly rather than reading Settings.
pub fn collect_relays(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--relay" {
            if let Some(v) = args.get(i + 1) {
                out.push(v.trim_end_matches('/').to_string());
            }
        }
        i += 1;
    }
    out
}

/// Every value following a repeated `--flood-relay` flag (P3: the cap-displacement targets, which
/// the relay-citizenship ruling restricts to explicitly-passed VPS strfry URLs).
pub fn collect_flood_relays(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--flood-relay" {
            if let Some(v) = args.get(i + 1) {
                out.push(v.trim_end_matches('/').to_string());
            }
        }
        i += 1;
    }
    out
}

/// P3 flood-guard (relay citizenship, M16 standing ruling + M20 W6 §W6): a flood-shaped row may
/// NEVER run against anything other than the explicitly-passed `--flood-relay` URLs. The probe's
/// `--relay` set is the read set; if it contains any URL that is NOT also a `--flood-relay`, the row
/// must refuse to run rather than flood a public relay. Returns the offending URLs so the refusal
/// diagnostic names them.
///
/// Concretely: `--relay` ∩ public-defaults is forbidden for P3. The mechanical check is "every
/// `--relay` used by P3 must appear in the `--flood-relay` allowlist" — so the operator MUST pass the
/// VPS strfry URLs as both, and any public relay in `--relay` is rejected. This is stricter and
/// simpler than matching against a hardcoded default list: it fails closed if the operator forgets to
/// pass `--flood-relay`, and it never relies on the harness's copy of the default list being current.
pub fn flood_guard_violations(read_relays: &[String], flood_relays: &[String]) -> Vec<String> {
    let flood: std::collections::HashSet<&String> = flood_relays.iter().collect();
    read_relays.iter().filter(|r| !flood.contains(r)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flag_value_finds_first() {
        let a = args(&["--data-dir", "/tmp/x", "--relay", "wss://a"]);
        assert_eq!(flag_value(&a, "--data-dir"), Some("/tmp/x"));
        assert_eq!(flag_value(&a, "--relay"), Some("wss://a"));
        assert_eq!(flag_value(&a, "--missing"), None);
    }

    #[test]
    fn collect_relays_gathers_all_and_trims_trailing_slash() {
        let a = args(&["--relay", "wss://a/", "--relay", "wss://b/", "--relay", "wss://c"]);
        assert_eq!(collect_relays(&a), vec!["wss://a", "wss://b", "wss://c"]);
    }

    #[test]
    fn collect_flood_relays_is_separate_from_read_relays() {
        let a = args(&[
            "--relay", "wss://read-only",
            "--flood-relay", "ws://vps1:7777/",
            "--flood-relay", "ws://vps2:7777",
        ]);
        assert_eq!(collect_flood_relays(&a), vec!["ws://vps1:7777", "ws://vps2:7777"]);
        assert_eq!(collect_relays(&a), vec!["wss://read-only"]);
    }

    #[test]
    fn flood_guard_passes_when_read_relays_subset_of_flood_relays() {
        // The intended P3 shape: --relay and --flood-relay both name the VPS strfry set.
        let read = vec!["ws://198.51.100.1:7777".to_string(), "ws://198.51.100.2:7777".to_string()];
        let flood = read.clone();
        assert!(flood_guard_violations(&read, &flood).is_empty(), "VPS set passes when both agree");
    }

    #[test]
    fn flood_guard_flags_any_read_relay_not_in_flood_allowlist() {
        // The forbidden shape: a public default in --relay but not --flood-relay.
        let read = vec!["wss://relay.damus.io".to_string(), "ws://198.51.100.1:7777".to_string()];
        let flood = vec!["ws://198.51.100.1:7777".to_string()];
        let bad = flood_guard_violations(&read, &flood);
        assert_eq!(bad, vec!["wss://relay.damus.io".to_string()]);
    }

    #[test]
    fn flood_guard_fails_closed_when_no_flood_relays_passed() {
        // Forgetting --flood-relay must refuse (never silently fall back to the read set).
        let read = vec!["wss://relay.damus.io".to_string()];
        assert_eq!(flood_guard_violations(&read, &[]), read);
    }

    #[test]
    fn command_reads_first_positional() {
        assert_eq!(command(&args(&["serve", "--data-dir", "/x"])), Some("serve"));
        assert_eq!(command(&[]), None);
    }
}
