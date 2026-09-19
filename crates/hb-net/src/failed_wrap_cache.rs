//! The bounded, FIFO-evicted negative cache for gift-wrap ids that fail to open (QURATOR-247).
//!
//! **One TYPE, deliberately many INSTANCES.** This is the shared *mechanism* — the hard cap +
//! oldest-first eviction an attacker-fed id set must not be allowed to drift between consumers —
//! lifted here from hb-app's DM poller (`commands/chat.rs`, audit #11) so that a consumer of the
//! multiplexed kind-1059 inbox (`{kinds:[1059], #p:[me]}` — DMs, join requests, invites, key
//! grants and private listings all land on one stream) has one bounding discipline to adopt. What is
//! deliberately NOT shared is the cache *contents*: each consumer composes its own keys and holds
//! its own static, because their failure verdicts are not interchangeable — a valid chat DM fails
//! `open_private_listing`'s inner-kind pin, a valid private listing fails `open_key_grant`'s, so a
//! shared entry set would let one consumer's routine kind-pin refusal blacklist another
//! consumer's perfectly good wrap (pinned in `priv_browse` by
//! `kind_pin_failures_do_not_cross_the_inner_kind_scopes`). Key-agnostic on purpose: callers own
//! key composition, so the discipline that must not drift (the bound) is shared and the verdicts
//! that must not leak (the entries) cannot.
//!
//! ⚠ **Coverage is NAMED here, never quantified — do not write "every consumer" again.** An
//! earlier draft of this doc said every consumer used this discipline; that was FALSE when written
//! and a review caught it (2026-09-19). A sentence like that states a security posture, so the
//! next audit reads it, believes the surface is closed, and stops looking.
//!
//! **Cached today:** this crate's `priv_browse` opens (both inner-kind scopes); `topic.rs`'s
//! join-request opens and the *deterministic prefix* of its invite opens (QURATOR-294); hb-app's
//! `merge_wraps_into_cache` and `decode_dms`.
//!
//! ⚠ **One deliberate exclusion, and the reason generalises:** an invite's FULL verdict is NOT
//! cacheable, because `hb_core::topic::redeem_invite` consults `expected_topic_id`,
//! `expected_issuer`, the caller's `now` and a mutable replay set — all caller context. The same
//! wrap legitimately fails a join for topic B and must still redeem for topic A. So `topic.rs`
//! caches only the open/kind-pin prefix and **never records a policy refusal**. Before adding any
//! consumer here, establish that ITS verdict is deterministic over (identity keys, wrap bytes)
//! alone; that property is what makes a negative cache safe, and it does not transfer for free.
//!
//! In-memory only, never persisted: it is a CPU-DoS backstop, not a correctness boundary — losing
//! it across a restart merely re-attempts each remembered wrap once, never loses a message.

use std::collections::{HashSet, VecDeque};

/// Cap on remembered failed-open wrap ids (the negative cache). A wrap that fails to open is
/// remembered so a repeated poll of the shared kind-1059 inbox does not re-run the full open
/// (NIP-44 ECDH + AES-GCM decrypt + Schnorr verify) on it — without the cache, one cheap signed
/// junk wrap re-buys that crypto work on every poll for as long as the relay serves it. Bounded
/// and FIFO-evicted because it is fed by attacker-controlled ids; 4,096 ids ≈ 256 KiB, and a
/// flood beyond that is bounded by the fetch limit + relay bandwidth rather than our memory.
/// The original DM-path number (audit #11), kept: same physical inbox, same traffic profile, no
/// basis for a different bound here.
pub const MAX_FAILED_WRAPS: usize = 4_096;

/// Bounded, FIFO negative cache of gift-wrap ids that failed to open (`set` for O(1) membership,
/// `order` for oldest-first eviction).
pub struct FailedWrapCache {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl FailedWrapCache {
    pub fn new() -> Self {
        FailedWrapCache { order: VecDeque::new(), set: HashSet::new() }
    }
    /// O(1) membership check on a caller-composed key.
    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
    /// Remember a key, evicting the OLDEST entry first once the cap is hit.
    pub fn insert(&mut self, id: String) {
        if self.set.insert(id.clone()) {
            self.order.push_back(id);
            if self.order.len() > MAX_FAILED_WRAPS {
                if let Some(evicted) = self.order.pop_front() {
                    self.set.remove(&evicted);
                }
            }
        }
    }
    /// Number of remembered keys (the bound's observability seam).
    pub fn len(&self) -> usize {
        self.set.len()
    }
    /// Convenience twin of [`len`](Self::len) (keeps `clippy::len_without_is_empty` happy).
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

impl Default for FailedWrapCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// audit #11 / QURATOR-247: the negative cache is fed by attacker-controlled ids, so it must
    /// be a hard bound (an unbounded one just moves the DoS to memory). FIFO: oldest evicted
    /// first. This is the test that lived at `commands/chat.rs`'s
    /// `failed_wrap_cache_is_bounded_and_fifo_evicts` — the type it drives moved here, and the
    /// chat.rs twin now exercises this implementation through its import.
    ///
    /// MUTATION (P-10, for the orchestrator to apply): in `insert`, delete the eviction block
    /// (`if self.order.len() > MAX_FAILED_WRAPS { … }`) — the 4,097th insert grows past the cap
    /// and this reds on both `len()` asserts.
    #[test]
    fn bounded_and_fifo_evicts() {
        let mut c = FailedWrapCache::new();
        for i in 0..MAX_FAILED_WRAPS {
            c.insert(format!("id{i}"));
        }
        assert_eq!(c.len(), MAX_FAILED_WRAPS, "the cache holds exactly the cap");
        assert!(c.contains("id0"), "the first-inserted id survives at exactly the cap");
        c.insert("overflow".into());
        assert_eq!(c.len(), MAX_FAILED_WRAPS, "the cap is a hard bound, never exceeded");
        assert!(!c.contains("id0"), "FIFO: the oldest entry is evicted first");
        assert!(c.contains("overflow"), "the newest entry is retained");
    }
}
