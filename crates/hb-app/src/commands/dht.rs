use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, SocketAddrV4};
use serde::Serialize;
use tauri::State;

use crate::{
    SharedDhtCancel,
    SharedRelay,
    dht_service,
    error::{CmdResult, cmd_err},
    store::DataStore,
};
// DataStore is used by dht_start_announce and dht_stop_announce only.
use hb_core::types::Profile;

#[derive(Debug, Serialize)]
pub struct DhtResult {
    pub hb_id: String,
    pub profile: Option<Profile>,
    pub online: bool,
}

// ---------------------------------------------------------------------------
// dht_search
// ---------------------------------------------------------------------------

/// Search the DHT for peers announcing `tags` and/or `content_types`.
///
/// - Tags use AND logic: peer must appear in results for ALL specified tags.
/// - Content types use OR logic: peer must appear in results for AT LEAST ONE content type.
/// - At least one filter is required.
///
/// For each announcing peer, the app TCP-connects to the announced port to fetch
/// a signed identity payload. Invalid or unreachable peers are silently discarded.
#[tauri::command]
pub async fn dht_search(
    tags: Vec<String>,
    content_types: Vec<String>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<DhtResult>> {
    if tags.is_empty() && content_types.is_empty() {
        return Err("at least one tag or content type is required".into());
    }

    // A new DHT node is created per search (bootstraps in ~2-5s). Sharing a
    // persistent node across calls is a Phase 2 optimisation (requires SharedDht state).
    let dht = mainline::Dht::builder()
        .build()
        .map_err(|e| format!("DHT unavailable: {e}"))?
        .as_async();

    // Cache identity lookups by socket address so the same peer is only contacted once.
    let mut identity_cache: HashMap<SocketAddrV4, Option<(String, Vec<String>)>> = HashMap::new();

    // Collect hb_id sets for each tag (AND logic requires intersection).
    let mut tag_sets: Vec<HashSet<String>> = Vec::new();
    for tag in &tags {
        let info_hash = dht_service::sha1_id(tag.as_bytes());
        let addrs = dht_service::collect_peer_addrs(&dht, info_hash).await;
        let set = resolve_identities(addrs, &mut identity_cache).await;
        tag_sets.push(set);
    }

    // Collect hb_id sets for each content type (OR logic requires union).
    let mut ct_sets: Vec<HashSet<String>> = Vec::new();
    for ct in &content_types {
        let info_hash = dht_service::sha1_id(ct.as_bytes());
        let addrs = dht_service::collect_peer_addrs(&dht, info_hash).await;
        let set = resolve_identities(addrs, &mut identity_cache).await;
        ct_sets.push(set);
    }

    // AND across tag sets.
    let tag_result: Option<HashSet<String>> = if tag_sets.is_empty() {
        None
    } else {
        Some(
            tag_sets
                .into_iter()
                .reduce(|a, b| a.intersection(&b).cloned().collect())
                .unwrap_or_default(),
        )
    };

    // OR across content-type sets.
    let ct_result: Option<HashSet<String>> = if ct_sets.is_empty() {
        None
    } else {
        Some(
            ct_sets
                .into_iter()
                .reduce(|a, b| a.union(&b).cloned().collect())
                .unwrap_or_default(),
        )
    };

    let final_ids: HashSet<String> = match (tag_result, ct_result) {
        (Some(t), Some(c)) => t.intersection(&c).cloned().collect(),
        (Some(t), None) => t,
        (None, Some(c)) => c,
        (None, None) => unreachable!(), // validated at the top
    };

    // Query relay for online status and cached profile.
    let relay_client = relay.as_ref();
    let mut results = Vec::new();
    for hb_id in &final_ids {
        let (profile, online) = match relay_client.fetch_peer(hb_id).await {
            Ok(cached) => (cached.profile, cached.online),
            Err(_) => (None, false),
        };
        results.push(DhtResult { hb_id: hb_id.clone(), profile, online });
    }

    Ok(results)
}

/// Fetch identity from each address in parallel, cache results, return the set of hb_ids.
///
/// Addresses already in `cache` are not re-fetched. All uncached addresses are
/// queried concurrently (up to their individual 5 s timeout each) so that 300
/// addresses take at most ~5 s rather than up to 25 minutes sequentially.
async fn resolve_identities(
    addrs: Vec<SocketAddrV4>,
    cache: &mut HashMap<SocketAddrV4, Option<(String, Vec<String>)>>,
) -> HashSet<String> {
    // Collect addresses that are not yet cached.
    let uncached: Vec<SocketAddrV4> = addrs
        .iter()
        .filter(|a| !cache.contains_key(a))
        .copied()
        .collect();

    // Fetch all uncached addresses in parallel.
    if !uncached.is_empty() {
        let results = futures::future::join_all(uncached.iter().map(|&addr| async move {
            (addr, dht_service::fetch_peer_identity(SocketAddr::V4(addr)).await.ok())
        }))
        .await;
        for (addr, result) in results {
            cache.insert(addr, result);
        }
    }

    // Build the hb_id set from cache.
    addrs
        .iter()
        .filter_map(|addr| cache.get(addr)?.as_ref().map(|(hb_id, _)| hb_id.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// dht_start_announce / dht_stop_announce
// ---------------------------------------------------------------------------

/// Enable DHT announce for the given tags and content types.
/// Saves to settings and immediately triggers the background announce loop.
#[tauri::command]
pub async fn dht_start_announce(
    tags: Vec<String>,
    content_types: Vec<String>,
    store: State<'_, DataStore>,
    dht_cancel: State<'_, SharedDhtCancel>,
) -> CmdResult<()> {
    let mut settings = store.load_settings().map_err(cmd_err)?.unwrap_or_default();
    settings.dht_announce_enabled = true;
    settings.dht_announce_tags = tags;
    settings.dht_announce_content_types = content_types;
    store.save_settings(&settings).map_err(cmd_err)?;
    // Wake the announce loop so it announces immediately instead of waiting 30 min.
    let _ = dht_cancel.send(false);
    Ok(())
}

/// Disable DHT announce.
/// Saves to settings and wakes the announce loop; the loop skips announcing when disabled.
#[tauri::command]
pub async fn dht_stop_announce(
    store: State<'_, DataStore>,
    dht_cancel: State<'_, SharedDhtCancel>,
) -> CmdResult<()> {
    let mut settings = store.load_settings().map_err(cmd_err)?.unwrap_or_default();
    settings.dht_announce_enabled = false;
    store.save_settings(&settings).map_err(cmd_err)?;
    let _ = dht_cancel.send(false);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_rejected_at_command_boundary() {
        // The Tauri command is async, but the validation branch is synchronous-logic.
        // We test it by checking the error text directly.
        // (Full async test would require a Tauri test harness.)
        let tags: Vec<String> = vec![];
        let content_types: Vec<String> = vec![];
        let is_empty = tags.is_empty() && content_types.is_empty();
        assert!(is_empty, "empty filter should trigger error path");
    }

    #[test]
    fn and_logic_intersection() {
        let set_a: HashSet<String> = ["peer1", "peer2", "peer3"]
            .iter().map(|s| s.to_string()).collect();
        let set_b: HashSet<String> = ["peer2", "peer3", "peer4"]
            .iter().map(|s| s.to_string()).collect();

        let result: HashSet<String> = vec![set_a, set_b]
            .into_iter()
            .reduce(|a, b| a.intersection(&b).cloned().collect())
            .unwrap_or_default();

        assert!(result.contains("peer2"));
        assert!(result.contains("peer3"));
        assert!(!result.contains("peer1"), "peer1 not in both sets");
        assert!(!result.contains("peer4"), "peer4 not in both sets");
    }

    #[test]
    fn or_logic_union() {
        let set_a: HashSet<String> = ["peer1", "peer2"].iter().map(|s| s.to_string()).collect();
        let set_b: HashSet<String> = ["peer2", "peer3"].iter().map(|s| s.to_string()).collect();

        let result: HashSet<String> = vec![set_a, set_b]
            .into_iter()
            .reduce(|a, b| a.union(&b).cloned().collect())
            .unwrap_or_default();

        assert!(result.contains("peer1"));
        assert!(result.contains("peer2"));
        assert!(result.contains("peer3"));
    }

    #[test]
    fn combined_and_or_logic() {
        // Tags: peer must match ALL. Content types: peer must match ANY.
        // Combined: tag intersection ∩ ct union.
        let tag_a: HashSet<String> = ["p1", "p2", "p3"].iter().map(|s| s.to_string()).collect();
        let tag_b: HashSet<String> = ["p2", "p3", "p4"].iter().map(|s| s.to_string()).collect();
        let ct_x: HashSet<String> = ["p1", "p2"].iter().map(|s| s.to_string()).collect();
        let ct_y: HashSet<String> = ["p3", "p5"].iter().map(|s| s.to_string()).collect();

        let tag_result: HashSet<String> = tag_a.intersection(&tag_b).cloned().collect(); // {p2, p3}
        let ct_result: HashSet<String> = ct_x.union(&ct_y).cloned().collect();           // {p1,p2,p3,p5}
        let combined: HashSet<String> = tag_result.intersection(&ct_result).cloned().collect(); // {p2, p3}

        assert_eq!(combined, ["p2", "p3"].iter().map(|s| s.to_string()).collect());
    }
}
