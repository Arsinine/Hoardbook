//! Public-relay survey — `RELAY_DEPLOY.md` §2 / Open Q#2 / `DISC5`.
//!
//! For each `--relay`, with a **fresh key**, probe four things the runbook asks about:
//!   1. does it **accept our kinds** (teaser 30117 / presence 11111 / listing 31111 / NIP-17 1059)?
//!   2. does it **admit a brand-new `npub`** (no web-of-trust write gate)?
//!   3. **short-term retention** — can the just-published event be fetched straight back?
//!   4. does it **require NIP-13 PoW**? (the `DISC5` rejection-without-PoW signal a real PoW-gated
//!      relay gives, which the ephemeral CI relay never does.)
//!
//! Each relay is surveyed **independently** (one fresh key, connected to that relay alone), so this
//! is not the cooperative-relay-set the L2 suites assume. It is **out-of-CI** (needs the public
//! network) and is always informational — it exits 0; a rejection is a *finding*, surfaced as a TAP
//! `not ok` line plus a per-relay summary, not a build failure.
//!
//! Run: `hb-it --survey --relay wss://relay.example [--relay …]`.

use std::time::Duration;

use hb_core::binding::KIND_PRESENCE;
use hb_core::event::{KIND_LISTING, KIND_TEASER};
use hb_core::Identity;
use hb_net::RelayClient;
use nostr::{EventBuilder, Filter, Kind, Tag};

use crate::tap::TestResult;

const KIND_GIFT_WRAP: u16 = 1059; // NIP-17 gift wrap (nostr `Kind::GiftWrap`)
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The four kinds Hoardbook publishes, surveyed for acceptance.
fn survey_kinds() -> [(&'static str, u16); 4] {
    [
        ("teaser(30117)", KIND_TEASER),
        ("presence(11111)", KIND_PRESENCE),
        ("listing(31111)", KIND_LISTING),
        ("giftwrap(1059)", KIND_GIFT_WRAP),
    ]
}

/// How a relay's publish-rejection reason classifies. Pure (unit-tested) so the survey's
/// interpretation of an `OK: false` message is deterministic and reviewable.
#[derive(Debug, PartialEq, Eq)]
pub enum RejectKind {
    PowRequired,
    WotGated,
    RateLimited,
    Other,
}

/// Classify a relay's machine-readable rejection reason (NIP-01 `OK` message / `["OK",id,false,reason]`).
///
/// Order matters because `blocked:` is a *generic* NIP-20 rejection prefix that many relays put in
/// front of any reason (`blocked: pow: …`, `blocked: rate-limited`, `blocked: not in whitelist`):
///   1. **PoW first** — the most specific, actionable finding (mine and retry).
///   2. **Rate next** — so a `blocked: rate-limited` is not swallowed by the WoT arm's bare `blocked`.
///   3. **WoT last** — `blocked` here is the catch for an unspecified gate (chorus: Codex + Gemini).
pub fn classify_reject(reason: &str) -> RejectKind {
    let m = reason.to_ascii_lowercase();
    if m.contains("pow") || m.contains("difficulty") || m.contains("proof of work") {
        RejectKind::PowRequired
    } else if m.contains("rate") || m.contains("too many") || m.contains("slow down") || m.contains("too fast")
    {
        RejectKind::RateLimited
    } else if m.contains("whitelist")
        || m.contains("not in")
        || m.contains("not allowed")
        || m.contains("restricted")
        || m.contains("blocked")
    {
        RejectKind::WotGated
    } else {
        RejectKind::Other
    }
}

/// Survey every relay independently. Returns one `TestResult` per (relay, kind) probe plus a
/// per-relay connect line; the caller prints TAP + a summary. Always informational.
pub async fn run(relays: &[String]) -> Vec<TestResult> {
    let mut out = Vec::new();
    for relay in relays {
        eprintln!("\n== survey {relay} ==");
        // A FRESH key per relay: if a write is accepted, the relay admits brand-new npubs (no WoT gate).
        let id = Identity::generate();
        let client = match RelayClient::connect(&id, std::slice::from_ref(relay), CONNECT_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                out.push(TestResult::fail(format!("{relay}: connect"), format!("{e}")));
                eprintln!("[survey] {relay}: connect failed: {e}");
                continue;
            }
        };

        let mut accepted = 0u8;
        let mut pow_required = false;
        let mut wot_gated = false;
        for (label, kind) in survey_kinds() {
            // Minimal event of the kind; parameterized-replaceable kinds (3xxxx) need a `d` tag.
            // Gift wrap (1059) gets a base64-shaped placeholder rather than `{}` so a relay that
            // *validates* NIP-17 content doesn't false-reject a kind it actually stores (chorus:
            // Gemini + opencode). It is NOT a valid sealed payload — content-strict relays are rare.
            let content = if kind == KIND_GIFT_WRAP {
                "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5Cg=="
            } else {
                "{}"
            };
            let mut builder = EventBuilder::new(Kind::from_u16(kind), content);
            if kind >= 30_000 {
                builder = builder.tag(Tag::identifier("hb-survey"));
            }
            let ev = match id.sign(builder) {
                Ok(e) => e,
                Err(e) => {
                    out.push(TestResult::fail(format!("{relay} {label}: sign"), format!("{e}")));
                    continue;
                }
            };
            match client.publish(&ev).await {
                Ok(_) => {
                    accepted += 1;
                    // Retention: the fresh key published exactly this kind, so author+kind refetches it.
                    let filter = Filter::new().author(id.public_key()).kind(Kind::from_u16(kind));
                    let retained = client
                        .fetch(filter, FETCH_TIMEOUT)
                        .await
                        .map(|v| !v.is_empty())
                        .unwrap_or(false);
                    if retained {
                        out.push(TestResult::ok(format!("{relay} {label}: accepted + retained")));
                    } else {
                        // Accepted the write but the refetch returned nothing. Do NOT report this as
                        // ok — the survey would then claim a retention it never observed (chorus:
                        // Codex). Surface it as a finding; the process still exits 0 (informational).
                        out.push(TestResult::fail(
                            format!("{relay} {label}: accepted but NOT retained"),
                            "publish was OK'd but the author+kind refetch returned nothing within the \
                             timeout — a retention gap or indexing lag",
                        ));
                    }
                }
                Err(e) => {
                    let reason = format!("{e}");
                    let class = classify_reject(&reason);
                    match class {
                        RejectKind::PowRequired => pow_required = true,
                        RejectKind::WotGated => wot_gated = true,
                        _ => {}
                    }
                    out.push(TestResult::fail(
                        format!("{relay} {label}: rejected [{class:?}]"),
                        reason,
                    ));
                }
            }
        }
        eprintln!(
            "[survey] {relay}: {accepted}/4 kinds accepted · new-key admitted: {} · pow_required: {pow_required}",
            !wot_gated && accepted > 0
        );
        client.disconnect().await;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow_wins_over_blocked_prefix() {
        // strfry-style NIP-13 reject names both "blocked" and "pow"; PoW must win.
        assert_eq!(classify_reject("blocked: pow: need 28 bits"), RejectKind::PowRequired);
        assert_eq!(classify_reject("min difficulty 20 not met"), RejectKind::PowRequired);
        assert_eq!(classify_reject("proof of work required"), RejectKind::PowRequired);
    }

    #[test]
    fn wot_gate_classified() {
        assert_eq!(classify_reject("blocked: pubkey not in whitelist"), RejectKind::WotGated);
        assert_eq!(classify_reject("restricted: not allowed to write"), RejectKind::WotGated);
    }

    #[test]
    fn rate_limit_classified() {
        assert_eq!(classify_reject("rate-limited: slow down"), RejectKind::RateLimited);
        assert_eq!(classify_reject("error: too many requests"), RejectKind::RateLimited);
    }

    #[test]
    fn rate_wins_over_blocked_prefix() {
        // `blocked:` is a generic NIP-20 prefix; a rate-limit behind it must NOT read as WoT-gated.
        assert_eq!(classify_reject("blocked: rate-limited, slow down"), RejectKind::RateLimited);
        assert_eq!(classify_reject("blocked: too many events"), RejectKind::RateLimited);
    }

    #[test]
    fn unknown_is_other() {
        assert_eq!(classify_reject("invalid: bad signature"), RejectKind::Other);
        assert_eq!(classify_reject(""), RejectKind::Other);
    }
}
