//! QURATOR-332 slice A — the background enumeration queue for never-enumerated contacts.
//!
//! Owner rulings 2026-09-24:
//! 1. A contact added but never had its listings enumerated is **Pending**, not an honest empty —
//!    the old stub default (`Fetched`) made Browse render a false "No public collections".
//! 2. Never-enumerated contacts are enumerated in the background, **QUEUED AND DRAGGED OUT OVER
//!    A LONG PERIOD, NEVER A BURST** — one contact per [`ENUMERATION_SPACING`].
//! 3. This relay-side read is **not gated by the discovery-auto-fetch opt-in** — it is the same
//!    read a manual refresh click performs, on the same relays, with no publish, no DM and no ask
//!    to the peer (`resolve_peer`'s keyless arm only FETCHES the peer's author-pinned listing
//!    events and presence; nothing is ever sent to the peer — verified in browse.rs).
//!
//! The loop is deliberately thin (fetch_driver's `poll_once` pattern): the decisions live in
//! testable pure functions — [`heal_pending_never_enumerated`], [`pick_next_pending`],
//! [`mark_failed_retry`] — and one resolve step that reuses the production path
//! ([`crate::commands::browse::refresh_contact_inner`]) so the queue and the click can never
//! drift apart. The loop's own glue is pinned live by slice C (a WAN integration row).

use crate::commands::browse::refresh_contact_inner;
use crate::error::cmd_err;
use crate::identity_state::SharedIdentity;
use crate::net::SharedRelay;
use crate::store::{CachedPeer, ContactSource, DataStore, ListingsStatus};

/// One contact per 30 s — owner ruling 2026-09-24: never-enumerated contacts are enumerated in
/// the background, "QUEUED AND DRAGGED OUT OVER A LONG PERIOD, NEVER A BURST". At the roster cap
/// (`MAX_AUTO_ADDED_ROSTER_CONTACTS` = 500, topics.rs) a full backlog drains in ≈4 h — the point.
const ENUMERATION_SPACING: std::time::Duration = std::time::Duration::from_secs(30);

/// Nothing Pending: re-poll the store for newly-created stubs (topic joins, request accepts) at a
/// leisurely cadence — there is nothing urgent about "no one has joined yet".
const IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// One-time heal (QURATOR-332 slice A), run at the loop's start: stored contacts stamped
/// `Fetched` by the OLD stub default — a `Topic`-sourced, keyless, empty-collections contact that
/// was never actually enumerated — are indistinguishable from a never-enumerated stub, so they
/// are re-marked `Pending` (err toward NOT a confident negative; the queue then classifies them
/// honestly). Returns the number healed. Idempotent: once healed, the contact is `Pending`, and
/// the queue's resolve rewrites it to an earned state.
///
/// Scope is deliberately the Topic stub path only (the brief's exact condition): a `Manual`
/// keyless empty `Fetched` contact predating this change is left alone — a manual add was
/// deliberate, and the user can refresh it with a click; the heal must not rewrite records the
/// ruling did not cover. Idempotent and pure-ish over the store; no network.
pub(crate) fn heal_pending_never_enumerated(store: &DataStore) -> Result<usize, String> {
    let mut healed = 0usize;
    for peer in store.list_contacts().map_err(cmd_err)? {
        let legacy_stub_default = peer.source == ContactSource::Topic
            && peer.browse_key_hex.is_none()
            && peer.collections.is_empty()
            && peer.listings_state == ListingsStatus::Fetched;
        if !legacy_stub_default {
            continue;
        }
        let hash = CachedPeer::pubkey_hash(&peer.npub);
        let mut fixed = peer;
        fixed.listings_state = ListingsStatus::Pending;
        store.save_contact(&hash, &fixed).map_err(cmd_err)?;
        healed += 1;
    }
    Ok(healed)
}

/// Pick the next contact to enumerate. Deterministic — oldest `last_fetched` first, npub as the
/// tie-break so equal stamps never reschedule on `read_dir` order — and Pending-only: a contact
/// that already carries an earned classification (`Fetched`/`Sealed`/`FetchFailed`) is never
/// re-picked, and a `Pending` one always is.
pub(crate) fn pick_next_pending(contacts: &[CachedPeer]) -> Option<&CachedPeer> {
    contacts
        .iter()
        .filter(|p| p.listings_state == ListingsStatus::Pending)
        .min_by(|a, b| (a.last_fetched, a.npub.as_str()).cmp(&(b.last_fetched, b.npub.as_str())))
}

/// Failure bookkeeping for a resolve that ERRORED: nothing was classified, so the contact stays
/// `Pending` — but its `last_fetched` is bumped to now, which in the oldest-first order moves it
/// to the BACK of the queue. A failing peer is therefore retried only after every other Pending
/// contact has had its turn (or, if it is the only Pending one, once per [`ENUMERATION_SPACING`]
/// — one read per spacing tick, never a hot loop). Persisted, so the deferral also survives a
/// restart. (The in-memory skip-set alternative was rejected: the loop deliberately owns no
/// cross-iteration state, and a per-function static is never shared — CLAUDE.md §6/§9.)
pub(crate) fn mark_failed_retry(peer: &mut CachedPeer) {
    peer.last_fetched = chrono::Utc::now();
    // The state is deliberately untouched: Pending stays Pending — nothing was classified.
}

/// Classify ONE Pending contact through the production resolve path. Returns `Some((npub,
/// status))` with the earned classification, or `None` when there is no identity yet (fresh
/// install), no Pending contact, or the resolve errored (the contact stays `Pending`, deferred
/// via [`mark_failed_retry`]).
pub(crate) async fn enumerate_next_pending(
    store: &DataStore,
    identity: &SharedIdentity,
    relay: &SharedRelay,
) -> Option<(String, ListingsStatus)> {
    // Per-iteration identity read — a fresh install has no identity when the loop starts, and a
    // one-shot snapshot would leave it dead for the whole session (fetch_driver's reason, verbatim).
    let me = {
        let guard = identity.read().await;
        match guard.as_ref() {
            Some(id) => id.identity.clone(),
            None => return None,
        }
    };
    let contacts = match store.list_contacts() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "enumeration queue: cannot list contacts this tick");
            return None;
        }
    };
    let npub = pick_next_pending(&contacts)?.npub.clone();
    match refresh_contact_inner(&npub, &me, store, relay).await {
        Ok(updated) => Some((npub, updated.listings_state)),
        Err(e) => {
            tracing::warn!(npub = %npub, error = %e, "enumeration queue: resolve failed; deferred");
            // Failed resolve policy: leave it Pending, move it to the back of the queue.
            let hash = CachedPeer::pubkey_hash(&npub);
            if let Ok(Some(mut peer)) = store.load_contact(&hash) {
                mark_failed_retry(&mut peer);
                if let Err(e) = store.save_contact(&hash, &peer) {
                    tracing::warn!(error = %e, "enumeration queue: failed to defer the retry");
                }
            }
            None
        }
    }
}

/// The driver loop. Spawned once at startup; runs for the process lifetime.
///
/// Deliberately thin: it owns the cadence (heal once, then classify one Pending contact per
/// [`ENUMERATION_SPACING`], idling at [`IDLE_INTERVAL`] when the queue is empty) and delegates
/// every decision to [`enumerate_next_pending`].
pub(crate) async fn run_enumeration_queue_loop(
    store: DataStore,
    identity: SharedIdentity,
    relay: SharedRelay,
) {
    // One-time heal, idempotent — a failure is hygiene-level: log it and try again next start.
    match heal_pending_never_enumerated(&store) {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            healed = n,
            "enumeration queue: re-marked never-enumerated legacy stubs as Pending"
        ),
        Err(e) => tracing::warn!(error = %e, "enumeration queue: heal failed; retries next start"),
    }
    tracing::info!(
        spacing_secs = ENUMERATION_SPACING.as_secs(),
        idle_secs = IDLE_INTERVAL.as_secs(),
        "enumeration queue: loop started (one never-enumerated contact per spacing; never a burst)"
    );
    loop {
        match enumerate_next_pending(&store, &identity, &relay).await {
            Some((npub, state)) => {
                tracing::debug!(npub = %npub, ?state, "enumeration queue: classified");
                tokio::time::sleep(ENUMERATION_SPACING).await;
            }
            None => tokio::time::sleep(IDLE_INTERVAL).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::browse::PeerCollection;
    use hb_core::types::{Collection, Visibility};

    fn dir_store() -> (tempfile::TempDir, DataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    /// Minimal CachedPeer fixture: never-enumerated topic stub unless overridden.
    fn stub(npub: &str, source: ContactSource) -> CachedPeer {
        CachedPeer {
            npub: npub.to_string(),
            source,
            browse_key_hex: None,
            petname: None,
            profile: None,
            collections: vec![],
            listings_state: ListingsStatus::Pending,
            online: false,
            last_fetched: chrono::Utc::now(),
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
        }
    }

    /// A realistic failure shape to heal: the OLD stub — Topic, keyless, empty, `Fetched` by the
    /// old default. Exactly what `upsert_topic_contact` wrote before QURATOR-332 slice A.
    fn legacy_stub(npub: &str, last_fetched: chrono::DateTime<chrono::Utc>) -> CachedPeer {
        let mut p = stub(npub, ContactSource::Topic);
        p.listings_state = ListingsStatus::Fetched;
        p.last_fetched = last_fetched;
        p
    }

    /// A contact already in the queue — the post-heal / post-QURATOR-332 stub shape.
    fn pending_stub(npub: &str, last_fetched: chrono::DateTime<chrono::Utc>) -> CachedPeer {
        let mut p = legacy_stub(npub, last_fetched);
        p.listings_state = ListingsStatus::Pending;
        p
    }

    fn one_collection() -> Vec<PeerCollection> {
        vec![PeerCollection {
            collection: Collection {
                slug: "films".into(),
                path_alias: "films".into(),
                description: None,
                item_count: 3,
                est_size: None,
                content_types: vec![],
                tags: vec![],
                languages: vec![],
                visibility: Visibility::Public,
                sorted: false,
                last_updated: chrono::Utc::now(),
                listing: vec![],
            },
            parts_total: None,
            parts_present: None,
            truncated: None,
            total_items: None,
            oversized: None,
            snapshot_fingerprint: None,
            manifest_imported_at: None,
            teaser_event_id: None,
        }]
    }

    fn load(store: &DataStore, npub: &str) -> CachedPeer {
        store
            .load_contact(&CachedPeer::pubkey_hash(npub))
            .unwrap()
            .unwrap_or_else(|| panic!("contact {npub} missing"))
    }

    /// QURATOR-332 slice A heal — the never-enumerated legacy stub (`Fetched` from the old
    /// default, never actually enumerated) must come out of the heal as `Pending`, so the queue
    /// classifies it and Browse never shows a false confident "No public collections".
    // P-10 mutation: in `heal_pending_never_enumerated` (crates/hb-app/src/enumeration_queue.rs),
    // change `fixed.listings_state = ListingsStatus::Pending;` to
    // `ListingsStatus::Fetched` (a no-op write) — this test reds (the contact stays Fetched).
    #[test]
    fn heal_remarks_topic_keyless_empty_fetched_as_pending() {
        let (_dir, store) = dir_store();
        store
            .save_contact(&CachedPeer::pubkey_hash("hb1_a"), &legacy_stub("hb1_a", chrono::Utc::now()))
            .unwrap();
        assert_eq!(heal_pending_never_enumerated(&store).unwrap(), 1);
        assert_eq!(
            load(&store, "hb1_a").listings_state,
            ListingsStatus::Pending
        );
    }

    /// The heal must NOT rewrite records the ruling did not cover: a Manual contact (a deliberate
    /// add), a keyed contact (its `collections` are authoritative), and a Topic contact whose
    /// cache holds collections (its `Fetched` was earned by a real enumeration) all keep their
    /// stored state and their key/data — the heal touches none of them.
    // P-10 mutation: in `heal_pending_never_enumerated`, delete the
    // `&& peer.browse_key_hex.is_none()` conjunct — the keyed-Topic fixture is then wrongly
    // healed and the `browse_key_hex` assertion below reds (its key survived the heal).
    #[test]
    fn heal_leaves_manual_keyed_and_cached_contacts_untouched() {
        let (_dir, store) = dir_store();

        // (1) Manual + keyless + empty + Fetched — the same shape as (3) but Manual-sourced.
        let mut manual = legacy_stub("hb1_manual", chrono::Utc::now());
        manual.source = ContactSource::Manual;
        // (2) Topic but KEYED — a pasted full share code; its cache is authoritative.
        let mut keyed = legacy_stub("hb1_keyed", chrono::Utc::now());
        keyed.browse_key_hex = Some("deadbeef".into());
        // (3) Topic + keyless but WITH collections — an earned honest empty.
        let mut cached = legacy_stub("hb1_cached", chrono::Utc::now());
        cached.collections = one_collection();

        for p in [&manual, &keyed, &cached] {
            store.save_contact(&CachedPeer::pubkey_hash(&p.npub), p).unwrap();
        }
        assert_eq!(heal_pending_never_enumerated(&store).unwrap(), 0);

        for p in [&manual, &keyed, &cached] {
            let after = load(&store, &p.npub);
            assert_eq!(
                after.listings_state,
                ListingsStatus::Fetched,
                "{}: a record outside the heal's scope keeps its earned state",
                p.npub
            );
            assert_eq!(
                after.collections.len(),
                p.collections.len(),
                "{}: data untouched",
                p.npub
            );
        }
        assert_eq!(
            load(&store, "hb1_keyed").browse_key_hex.as_deref(),
            Some("deadbeef"),
            "a keyed contact's key is never touched by the heal"
        );
    }

    /// Idempotence: the second pass finds nothing left to heal — the heal runs once at the loop's
    /// start and must not keep rewriting (or misclassifying) on every startup.
    // P-10 mutation: in `heal_pending_never_enumerated`, change the guard
    // `if !legacy_stub_default { continue; }` to `if legacy_stub_default { continue; }` (invert
    // the guard) — every fixture is then rewritten each pass and the second-run `0` reds.
    #[test]
    fn heal_is_idempotent() {
        let (_dir, store) = dir_store();
        store
            .save_contact(&CachedPeer::pubkey_hash("hb1_a"), &legacy_stub("hb1_a", chrono::Utc::now()))
            .unwrap();
        assert_eq!(heal_pending_never_enumerated(&store).unwrap(), 1);
        assert_eq!(heal_pending_never_enumerated(&store).unwrap(), 0);
    }

    /// The picker is Pending-only and deterministic (oldest `last_fetched` first): classified
    /// contacts are never re-picked, and among Pending ones the oldest wins.
    // P-10 mutation: in `pick_next_pending`, change the filter to
    // `p.listings_state == ListingsStatus::Fetched` — the picker then selects a CLASSIFIED
    // contact and the `picked.npub == "hb1_old"` assertion reds.
    #[test]
    fn pick_next_pending_returns_only_pending_oldest_first() {
        let old = chrono::Utc::now() - chrono::Duration::hours(2);
        let mid = chrono::Utc::now() - chrono::Duration::hours(1);
        let contacts = vec![
            {
                let mut p = legacy_stub("hb1_fetched", mid);
                p.listings_state = ListingsStatus::Fetched; // classified — never re-picked
                p
            },
            pending_stub("hb1_old", old),   // oldest Pending — must win
            pending_stub("hb1_new", mid),   // newer Pending
            {
                let mut p = legacy_stub("hb1_sealed", old);
                p.listings_state = ListingsStatus::Sealed; // older but classified — skipped
                p
            },
        ];
        let picked = pick_next_pending(&contacts).expect("a Pending contact is picked");
        assert_eq!(picked.npub, "hb1_old", "oldest PENDING contact wins, classified ones skipped");

        // No Pending at all ⇒ nothing to do.
        let done: Vec<CachedPeer> = contacts
            .iter()
            .map(|p| {
                let mut q = p.clone();
                if q.listings_state == ListingsStatus::Pending {
                    q.listings_state = ListingsStatus::Fetched;
                }
                q
            })
            .collect();
        assert!(pick_next_pending(&done).is_none(), "empty queue is None");
    }

    /// A failed resolve is not re-picked immediately: `mark_failed_retry` bumps `last_fetched`,
    /// which in the oldest-first order sends the failing peer to the BACK of the queue while
    /// other Pending contacts get their turn. (If it is the ONLY Pending contact it is retried
    /// once per `ENUMERATION_SPACING` — one read per spacing tick, never a hot loop; the
    /// glue that applies this on the resolve's Err arm is pinned live by slice C's WAN row.)
    // P-10 mutation: in `mark_failed_retry`, delete `peer.last_fetched = chrono::Utc::now();` —
    // the failing peer keeps its old stamp, is re-picked immediately, and the `picked.npub ==
    // "hb1_next"` assertion reds.
    #[test]
    fn mark_failed_retry_defers_a_failed_peer_to_the_back_of_the_queue() {
        let old = chrono::Utc::now() - chrono::Duration::hours(2);
        let mut failed = pending_stub("hb1_failed", old);
        let next = pending_stub("hb1_next", chrono::Utc::now() - chrono::Duration::minutes(5));
        let mut contacts = vec![failed.clone(), next.clone()];

        assert_eq!(pick_next_pending(&contacts).unwrap().npub, "hb1_failed");
        mark_failed_retry(&mut failed);
        contacts[0] = failed.clone();
        assert_eq!(
            pick_next_pending(&contacts).unwrap().npub,
            "hb1_next",
            "the failed peer rotates to the back; another Pending contact gets its turn"
        );
        assert_eq!(
            failed.listings_state,
            ListingsStatus::Pending,
            "nothing was classified — the state stays Pending"
        );
    }

    /// Structural guard (QURATOR-332 slice A): BOTH consumers resolve through the ONE shared
    /// production path — `refresh_contact`'s command body and the queue's
    /// `enumerate_next_pending` — so a future edit cannot fork the resolve/save sequence into a
    /// hand-copied drift pair. Call forms only, comment-stripped, and sliced so the inner fn's
    /// own DEFINITION line can never satisfy the needle (CLAUDE.md §9 P-12 / string-literal
    /// rules). The loop's cadence glue around these calls is pinned live by slice C (a WAN row).
    // P-10 mutation: in `refresh_contact` (crates/hb-app/src/commands/browse.rs), delete the
    // `refresh_contact_inner(&npub, &me, &store, &relay)` call and re-inline the old body
    // (`let hash = CachedPeer::pubkey_hash(&npub); ... resolve_peer(...)` sequence) — the first
    // assert reds with 0 occurrences in the command body. Symmetrically, re-pointing the queue's
    // call at its own resolve/save copy reds the second.
    #[test]
    fn refresh_contact_and_queue_share_one_resolve_path() {
        let strip = |s: &str| -> String {
            s.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        // Test modules are excluded first so this guard's own needle strings can never
        // satisfy a scan (P-12: the guard must not count itself).
        let browse_src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/commands/browse.rs"
        ))
        .unwrap();
        let queue_src =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/enumeration_queue.rs"))
                .unwrap();
        let browse = strip(&browse_src).split("#[cfg(test)]").next().unwrap().to_string();
        let queue = strip(&queue_src).split("#[cfg(test)]").next().unwrap().to_string();

        // The command delegates: its body calls the shared inner fn and contains none of the
        // resolve machinery it used to re-implement.
        let cmd_start = browse
            .find("pub async fn refresh_contact(")
            .expect("refresh_contact must exist");
        let inner_start = browse
            .find("pub(crate) async fn refresh_contact_inner(")
            .expect("refresh_contact_inner must follow the command in browse.rs");
        assert!(
            inner_start > cmd_start,
            "the inner fn must be defined after the command for this slice to be sound"
        );
        let cmd_body = &browse[cmd_start..inner_start];
        assert_eq!(
            cmd_body.matches("refresh_contact_inner(").count(),
            1,
            "the refresh click resolves through the ONE shared path"
        );
        assert!(
            !cmd_body.contains("resolve_peer("),
            "the command must not re-implement the resolve sequence beside the shared path"
        );

        // The queue drives the same path.
        let q_start = queue
            .find("pub(crate) async fn enumerate_next_pending(")
            .expect("enumerate_next_pending must exist");
        let q_end = queue
            .find("pub(crate) async fn run_enumeration_queue_loop(")
            .expect("the loop must follow enumerate_next_pending in enumeration_queue.rs");
        assert!(q_end > q_start, "loop must be defined after enumerate for the slice");
        let q_body = &queue[q_start..q_end];
        assert_eq!(
            q_body.matches("refresh_contact_inner(").count(),
            1,
            "the queue resolves through the ONE shared path"
        );
    }
}
