//! Q7 — the stranger-DM Request inbox (M13 Part B; owner ruling): a message from someone who is not
//! a saved contact never touches the main inbox. It lands in a separate, quarantined **Request**
//! bucket (the message-requests pattern) — seen only when the user looks — bucketed by sender. Until
//! accepted, no reply is possible (enforced in the UI: `canReply` gates the composer). This module is
//! the pure data shape + merge logic + the `DataStore` persistence; `commands/chat.rs` owns the
//! classification (`classify_dms`) and the Tauri command surface.
//!
//! Persisted directly under the app's base dir (`dm_requests.json` / `dm_declined.json` /
//! `dm_blocked.json` / `announce_times.json`), so `DataStore::wipe()`'s "remove everything under
//! base_dir()" sweep (audit I-11) covers all four automatically — no wipe-list entry needed here.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::store::{read_json_lenient, write_json, DataStore};

/// OWNER-RATIFICATION DEFAULT (Q7): the maximum number of distinct stranger senders held in the
/// Request inbox at once. Beyond this, the least-recently-active bucket (by `last_message_at`) is
/// LRU-evicted to admit a new sender — bounds the inbox against an unbounded stranger flood.
pub const REQUEST_SENDER_CAP: usize = 64;

/// OWNER-RATIFICATION DEFAULT (Q7): the maximum number of messages kept in a single sender's Request
/// bucket. The oldest message is evicted first — bounds one spammy stranger's bucket from growing
/// unbounded even while the sender count stays under [`REQUEST_SENDER_CAP`].
pub const REQUEST_MSGS_PER_SENDER: usize = 50;

/// OWNER-RATIFICATION DEFAULT (Q7): the maximum number of remembered declines. A decline is permanent
/// (see `dm_request_decline` in `commands/chat.rs` for why), but the remembered set itself is bounded
/// — LRU-evicted by `declined_at` — so it can't grow forever.
pub const DECLINED_CAP: usize = 1024;

/// One stranger sender's quarantined Request bucket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DmRequestBucket {
    pub npub: String,
    pub first_seen: u64,
    pub last_message_at: u64,
    pub messages: Vec<RequestMessage>,
}

/// One quarantined message inside a Request bucket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestMessage {
    /// The gift-wrap event id (hex) — the dedup key. A relay re-delivering the same wrap collapses to
    /// one copy rather than duplicating the message.
    pub wrap_id: String,
    pub content: String,
    /// RFC3339 timestamp from the inner rumor (the real send time).
    pub sent_at: String,
}

/// Merge freshly-classified stranger messages into the existing Request buckets (pure — no I/O).
/// Dedups by `wrap_id`, caps each bucket at [`REQUEST_MSGS_PER_SENDER`] (oldest evicted first), and
/// caps the sender count at [`REQUEST_SENDER_CAP`] (the stalest bucket by `last_message_at` evicted
/// first).
pub fn merge_into_requests(
    existing: Vec<DmRequestBucket>,
    incoming: Vec<(String, RequestMessage)>,
    now: u64,
) -> Vec<DmRequestBucket> {
    let mut by_npub: HashMap<String, DmRequestBucket> =
        existing.into_iter().map(|b| (b.npub.clone(), b)).collect();

    for (npub, msg) in incoming {
        let bucket = by_npub.entry(npub.clone()).or_insert_with(|| DmRequestBucket {
            npub,
            first_seen: now,
            last_message_at: now,
            messages: Vec::new(),
        });
        if bucket.messages.iter().any(|m| m.wrap_id == msg.wrap_id) {
            continue; // a re-delivered wrap collapses — no duplicate, no bumped activity
        }
        bucket.messages.push(msg);
        bucket.last_message_at = now;
        if bucket.messages.len() > REQUEST_MSGS_PER_SENDER {
            let excess = bucket.messages.len() - REQUEST_MSGS_PER_SENDER;
            bucket.messages.drain(0..excess);
        }
    }

    let mut buckets: Vec<DmRequestBucket> = by_npub.into_values().collect();
    if buckets.len() > REQUEST_SENDER_CAP {
        buckets.sort_by_key(|b| std::cmp::Reverse(b.last_message_at));
        buckets.truncate(REQUEST_SENDER_CAP);
    }
    buckets
}

/// Record `npub` as declined at `now` (pure — no I/O), replacing any existing entry for the same
/// sender and LRU-evicting the stalest declined entry (by `declined_at`) when over [`DECLINED_CAP`].
pub fn record_declined(existing: Vec<(String, u64)>, npub: String, now: u64) -> Vec<(String, u64)> {
    let mut map: HashMap<String, u64> = existing.into_iter().collect();
    map.insert(npub, now);
    let mut entries: Vec<(String, u64)> = map.into_iter().collect();
    if entries.len() > DECLINED_CAP {
        entries.sort_by_key(|(_, declined_at)| std::cmp::Reverse(*declined_at));
        entries.truncate(DECLINED_CAP);
    }
    entries
}

// ---------------------------------------------------------------------------
// DataStore persistence — mirrors the Group/Watch/StoredTopic pattern in store.rs
// ---------------------------------------------------------------------------

impl DataStore {
    pub fn dm_requests_path(&self) -> PathBuf {
        self.base_dir().join("dm_requests.json")
    }

    pub fn load_dm_requests(&self) -> Result<Vec<DmRequestBucket>> {
        Ok(read_json_lenient::<Vec<DmRequestBucket>>(&self.dm_requests_path())
            .context("loading dm requests")?
            .unwrap_or_default())
    }

    pub fn save_dm_requests(&self, buckets: &[DmRequestBucket]) -> Result<()> {
        write_json(&self.dm_requests_path(), buckets).context("saving dm requests")
    }

    pub fn dm_declined_path(&self) -> PathBuf {
        self.base_dir().join("dm_declined.json")
    }

    pub fn load_dm_declined(&self) -> Result<Vec<(String, u64)>> {
        Ok(read_json_lenient::<Vec<(String, u64)>>(&self.dm_declined_path())
            .context("loading dm declined")?
            .unwrap_or_default())
    }

    pub fn save_dm_declined(&self, declined: &[(String, u64)]) -> Result<()> {
        write_json(&self.dm_declined_path(), declined).context("saving dm declined")
    }

    pub fn dm_blocked_path(&self) -> PathBuf {
        self.base_dir().join("dm_blocked.json")
    }

    pub fn load_dm_blocked(&self) -> Result<Vec<String>> {
        Ok(read_json_lenient::<Vec<String>>(&self.dm_blocked_path())
            .context("loading dm blocked")?
            .unwrap_or_default())
    }

    pub fn save_dm_blocked(&self, blocked: &[String]) -> Result<()> {
        write_json(&self.dm_blocked_path(), blocked).context("saving dm blocked")
    }

    pub fn announce_times_path(&self) -> PathBuf {
        self.base_dir().join("announce_times.json")
    }

    pub fn load_announce_times(&self) -> Result<HashMap<String, u64>> {
        Ok(read_json_lenient::<HashMap<String, u64>>(&self.announce_times_path())
            .context("loading announce times")?
            .unwrap_or_default())
    }

    /// Persist the per-topic last-announce-at map, pruning entries whose cooldown has already
    /// expired (`announce_cooldown_remaining(..) == 0`) — the map never accumulates topics nobody has
    /// broadcast to recently.
    pub fn save_announce_times(&self, times: &HashMap<String, u64>, now: u64) -> Result<()> {
        let pruned: HashMap<String, u64> = times
            .iter()
            .filter(|(_, &last)| hb_core::announce_cooldown_remaining(Some(last), now) > 0)
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        write_json(&self.announce_times_path(), &pruned).context("saving announce times")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(wrap_id: &str) -> RequestMessage {
        RequestMessage { wrap_id: wrap_id.into(), content: "hi".into(), sent_at: "2026-01-01T00:00:00Z".into() }
    }

    #[test]
    fn merge_into_requests_dedups_by_wrap_id_and_caps_messages() {
        let sender = "npub1stranger".to_string();
        let mut buckets = vec![];
        let base = 1_700_000_000u64;
        for i in 0..51u64 {
            buckets = merge_into_requests(buckets, vec![(sender.clone(), msg(&format!("wrap{i}")))], base + i);
        }
        let bucket = buckets.iter().find(|b| b.npub == sender).unwrap();
        assert_eq!(bucket.messages.len(), REQUEST_MSGS_PER_SENDER, "capped at the per-sender message cap");
        assert_eq!(bucket.messages.first().unwrap().wrap_id, "wrap1", "the 51st message evicted the oldest (wrap0)");
        assert_eq!(bucket.messages.last().unwrap().wrap_id, "wrap50");

        // Re-delivering an already-seen wrap collapses — no growth, no duplicate.
        let redelivered = msg("wrap50");
        let after = merge_into_requests(buckets, vec![(sender.clone(), redelivered)], base + 100);
        let bucket = after.iter().find(|b| b.npub == sender).unwrap();
        assert_eq!(bucket.messages.len(), REQUEST_MSGS_PER_SENDER, "a re-delivered wrap does not grow the bucket");
    }

    #[test]
    fn sender_cap_lru_evicts_oldest_activity_bucket() {
        let mut buckets = vec![];
        for i in 0..REQUEST_SENDER_CAP {
            let npub = format!("npub{i}");
            buckets = merge_into_requests(buckets, vec![(npub, msg(&format!("w{i}")))], i as u64);
        }
        assert_eq!(buckets.len(), REQUEST_SENDER_CAP);

        let newcomer = "npub_new".to_string();
        let after = merge_into_requests(
            buckets,
            vec![(newcomer.clone(), msg("wnew"))],
            REQUEST_SENDER_CAP as u64 + 100,
        );
        assert_eq!(after.len(), REQUEST_SENDER_CAP, "the sender count stays capped");
        assert!(after.iter().any(|b| b.npub == newcomer), "the newcomer is admitted");
        assert!(!after.iter().any(|b| b.npub == "npub0"), "the stalest bucket (npub0) is LRU-evicted");
    }

    #[test]
    fn record_declined_replaces_and_lru_evicts_over_cap() {
        let declined = record_declined(vec![], "npub1a".into(), 1);
        let declined = record_declined(declined, "npub1a".into(), 2); // re-decline updates, not duplicates
        assert_eq!(declined.len(), 1);
        assert_eq!(declined[0], ("npub1a".to_string(), 2));
    }

    #[test]
    fn quarantine_and_announce_files_land_under_base_dir_and_are_wipe_covered() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store
            .save_dm_requests(&[DmRequestBucket {
                npub: "npub1x".into(),
                first_seen: 0,
                last_message_at: 0,
                messages: vec![msg("w1")],
            }])
            .unwrap();
        store.save_dm_declined(&[("npub1y".into(), 1)]).unwrap();
        store.save_dm_blocked(&["npub1z".into()]).unwrap();
        let mut times = HashMap::new();
        times.insert("films".to_string(), 1_000u64);
        store.save_announce_times(&times, 1_000).unwrap();

        for name in ["dm_requests.json", "dm_declined.json", "dm_blocked.json", "announce_times.json"] {
            assert!(store.base_dir().join(name).exists(), "{name} must live directly under base_dir");
        }

        store.wipe().unwrap();
        for name in ["dm_requests.json", "dm_declined.json", "dm_blocked.json", "announce_times.json"] {
            assert!(!store.base_dir().join(name).exists(), "{name} must be removed by wipe()");
        }
    }
}
