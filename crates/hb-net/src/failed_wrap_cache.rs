//! The bounded, FIFO-evicted negative cache for gift-wrap ids that fail to open (QURATOR-247).
//!
//! **One TYPE, deliberately many INSTANCES.** This is the shared *mechanism* — the hard cap +
//! oldest-first eviction an attacker-fed id set must not be allowed to drift between consumers —
//! lifted here from hb-app's DM poller (`commands/chat.rs`, audit #11) so that a consumer of the
//! multiplexed kind-1059 inbox (`{kinds:[1059], #p:[me]}` — DMs, join requests, invites, key
//! grants and private listings all land on one stream) has one bounding discipline to adopt. What is
//! deliberately NOT shared is the cache *contents*: each consumer holds its own static, because
//! their failure verdicts are not interchangeable — a valid chat DM fails `open_private_listing`'s
//! inner-kind pin, a valid private listing fails `open_key_grant`'s, so a shared entry set would let
//! one consumer's routine kind-pin refusal blacklist another consumer's perfectly good wrap (pinned
//! in `priv_browse` by `kind_pin_failures_do_not_cross_the_inner_kind_scopes`). Since QURATOR-296
//! the KEY COMPOSITION and the seen/record wrappers below are shared too — every hb-net consumer
//! calls them with its OWN static — because the key is a security-relevant construction (the
//! identity segment stops one node's failure verdict poisoning another's; the scope segment stops
//! one inner-kind blacklist leaking into another's) and two byte-identical copies meant a hardening
//! applied to one was silently absent from the other. The type itself stays key-agnostic
//! (`contains`/`insert` take caller-composed keys): the discipline that must not drift (the bound,
//! and now the key) is shared while the verdicts that must not leak (the entries) cannot.
//!
//! ⚠ **Coverage is NAMED here, never quantified — do not write "every consumer" again.** An
//! earlier draft of this doc said every consumer used this discipline; that was FALSE when written
//! and a review caught it (2026-09-19). A sentence like that states a security posture, so the
//! next audit reads it, believes the surface is closed, and stops looking.
//!
//! **Cached today:** this crate's `priv_browse` opens (both inner-kind scopes); `topic.rs`'s
//! join-request opens and the whole of `hb_core::topic::open_invite` for its invite opens
//! (QURATOR-294, widened by QURATOR-298); hb-app's `merge_wraps_into_cache` and `decode_dms`.
//!
//! ⚠ **One deliberate exclusion, and the reason generalises:** an invite's FULL verdict is NOT
//! cacheable, because the policy half (`hb_core::topic::redeem_opened_invite`) consults
//! `expected_topic_id`, `expected_issuer`, the caller's `now` and a mutable replay set — all caller
//! context. The same wrap legitimately fails a join for topic B and must still redeem for topic A.
//! So `topic.rs` caches the deterministic `open_invite` verdict and **never records a policy
//! refusal**. Before adding any consumer here, establish that ITS verdict is deterministic over
//! (identity keys, wrap bytes) alone; that property is what makes a negative cache safe, and it
//! does not transfer for free.
//!
//! ⚠ **Scope of "the whole of `open_invite`", stated precisely** (updated 2026-09-19 after a review
//! caught this paragraph still describing the pre-QURATOR-298 surface). Before 298 the invite scope
//! cached only the unwrap + inner-kind prefix; the schema/crypto tags, `InvitePayload` parse and
//! version-consistency checks sat past the gate inside the old monolith and were never cached. They
//! are cached now. **The one deterministic check still NOT cached** is the 32-byte `topic_key` hex
//! decode, which stays in the policy half so it runs before the replay-nonce insert — see the
//! residual note on `topic.rs`'s `TOPIC_FAILED_OPENS`. Keep this paragraph exact: the ⚠ above says
//! a posture sentence here is what the next audit trusts, and an understated one stops it looking.
//!
//! In-memory only, never persisted: it is a CPU-DoS backstop, not a correctness boundary — losing
//! it across a restart merely re-attempts each remembered wrap once, never loses a message.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

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

/// Compose the negative-cache key: (identity npub, scope, wrap id). `\x00` appears in none of
/// bech32 (npub), the scope tags, or hex (wrap id), so the join is collision-free. ONE shared
/// construction (QURATOR-296): this trio was lifted verbatim from `priv_browse` and `topic`,
/// whose copies were byte-identical — the identity segment stops one node's failure verdict
/// poisoning another's, the scope segment stops one inner-kind blacklist leaking into another's,
/// and sharing means a hardening here now applies to every consumer at once. The cache itself is
/// NOT passed here because no cache is touched — callers that only need the string may compose
/// directly; every hb-net consumer goes through [`failed_open_seen`]/[`record_failed_open`].
///
/// MUTATION (P-10, for the orchestrator to apply): in this fn (failed_wrap_cache.rs, the
/// `format!` in `failed_open_key`), drop the scope segment — change it to
/// `format!("{me_npub}\u{0}{wrap_id}")` — this must red BOTH
/// `kind_pin_failures_do_not_cross_the_inner_kind_scopes` (priv_browse.rs) AND
/// `invite_gate_failures_do_not_cross_into_the_join_request_scope` (topic.rs): the cross-file red
/// is the proof the extraction actually shares one construction.
pub(crate) fn failed_open_key(me_npub: &str, scope: &str, wrap_id: &str) -> String {
    format!("{me_npub}\u{0}{scope}\u{0}{wrap_id}")
}

/// O(1) "already failed to open under THIS identity and scope?" against the CALLER's cache —
/// checked before the open so a previously-failed wrap is not re-decrypted on every poll. The
/// cache is a parameter, never a static here: each consumer passes its OWN (see the module doc —
/// one mechanism, deliberately many instances, so one consumer's verdicts can never appear in
/// another's cache).
pub(crate) fn failed_open_seen(
    cache: &Mutex<FailedWrapCache>,
    me_npub: &str,
    scope: &str,
    id: &str,
) -> bool {
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&failed_open_key(me_npub, scope, id))
}

/// Record a (identity, scope, wrap id) that failed to open into the CALLER'S cache (bounded,
/// FIFO-evicted). Only a deterministic open verdict is ever recorded through this — never a
/// policy refusal; each consumer's own doc states why its verdict qualifies.
pub(crate) fn record_failed_open(
    cache: &Mutex<FailedWrapCache>,
    me_npub: &str,
    scope: &str,
    id: String,
) {
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(failed_open_key(me_npub, scope, &id));
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
