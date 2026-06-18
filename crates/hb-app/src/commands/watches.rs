use std::collections::HashSet;
use serde::Serialize;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::{DataStore, Watch},
};

#[derive(Debug, Serialize)]
pub struct WatchHit {
    pub watch_name: String,
    pub npub: String,
}

#[tauri::command]
pub async fn watches_get(store: State<'_, DataStore>) -> CmdResult<Vec<Watch>> {
    store.load_watches().map_err(cmd_err)
}

#[tauri::command]
pub async fn watches_create(
    name: String,
    tags: Vec<String>,
    content_types: Vec<String>,
    store: State<'_, DataStore>,
) -> CmdResult<Watch> {
    let mut watches = store.load_watches().map_err(cmd_err)?;
    if watches.iter().any(|w| w.name == name) {
        return Err(format!("watch '{name}' already exists"));
    }
    let watch = Watch { name, tags, content_types, last_fired: None, seen_pubkeys: vec![] };
    watches.push(watch.clone());
    store.save_watches(&watches).map_err(cmd_err)?;
    Ok(watch)
}

#[tauri::command]
pub async fn watches_delete(name: String, store: State<'_, DataStore>) -> CmdResult<()> {
    let mut watches = store.load_watches().map_err(cmd_err)?;
    watches.retain(|w| w.name != name);
    store.save_watches(&watches).map_err(cmd_err)
}

/// Evaluate `candidates` (npubs from a Nostr tag search) against all saved watches.
///
/// A candidate fires a watch when it is:
/// - NOT already in the user's contact list, and
/// - NOT already in the watch's `seen_pubkeys` (not previously notified).
///
/// Fires are recorded in `seen_pubkeys` so the same peer never fires again for
/// that watch. `last_fired` is updated only for watches that produced at least
/// one new hit this call.
///
/// Tag/content-type matching is the caller's responsibility: pass only candidates
/// that were returned by a Nostr tag search using the watch's own filter criteria.
#[tauri::command]
pub async fn watches_evaluate(
    candidates: Vec<String>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<WatchHit>> {
    let mut watches = store.load_watches().map_err(cmd_err)?;

    let contacts = store.list_contacts().map_err(cmd_err)?;
    let contact_npubs: HashSet<String> = contacts.iter().map(|c| c.npub.clone()).collect();

    let mut hits: Vec<WatchHit> = Vec::new();

    for watch in &mut watches {
        // Build a set for O(1) lookup instead of O(n) Vec::contains on every candidate.
        let seen: HashSet<String> = watch.seen_pubkeys.iter().cloned().collect();
        let before = hits.len();
        for npub in &candidates {
            if contact_npubs.contains(npub) {
                continue; // already a known contact — not a discovery
            }
            if seen.contains(npub) {
                continue; // already notified for this watch
            }
            hits.push(WatchHit { watch_name: watch.name.clone(), npub: npub.clone() });
            watch.seen_pubkeys.push(npub.clone());
        }
        if hits.len() > before {
            watch.last_fired = Some(chrono::Utc::now());
        }
    }

    store.save_watches(&watches).map_err(cmd_err)?;
    Ok(hits)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DataStore;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> DataStore {
        DataStore::new(PathBuf::from(dir.path()))
    }

    fn run_evaluate(
        watches: &mut [Watch],
        candidates: &[&str],
        contacts: &HashSet<String>,
    ) -> Vec<WatchHit> {
        let mut hits = Vec::new();
        for watch in watches.iter_mut() {
            let seen: HashSet<String> = watch.seen_pubkeys.iter().cloned().collect();
            let before = hits.len();
            for &npub in candidates {
                if contacts.contains(npub) { continue; }
                if seen.contains(npub) { continue; }  // O(1) HashSet lookup
                hits.push(WatchHit { watch_name: watch.name.clone(), npub: npub.to_string() });
                watch.seen_pubkeys.push(npub.to_string());
            }
            if hits.len() > before {
                watch.last_fired = Some(chrono::Utc::now());
            }
        }
        hits
    }

    #[test]
    fn watch_fires_new_peer() {
        let mut watches = vec![Watch {
            name: "test-watch".into(),
            tags: vec!["nature".into()],
            content_types: vec![],
            last_fired: None,
            seen_pubkeys: vec![],
        }];
        let contacts = HashSet::new();
        let hits = run_evaluate(&mut watches, &["hb1_newpeer"], &contacts);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].npub, "hb1_newpeer");
        assert_eq!(hits[0].watch_name, "test-watch");
    }

    #[test]
    fn watch_silent_known_contact() {
        let mut watches = vec![Watch {
            name: "test-watch".into(),
            tags: vec![],
            content_types: vec![],
            last_fired: None,
            seen_pubkeys: vec![],
        }];
        let mut contacts = HashSet::new();
        contacts.insert("hb1_contact".to_string());
        let hits = run_evaluate(&mut watches, &["hb1_contact"], &contacts);
        assert!(hits.is_empty(), "contacts must not trigger watch notifications");
    }

    #[test]
    fn watch_silent_dismissed() {
        let mut watches = vec![Watch {
            name: "test-watch".into(),
            tags: vec![],
            content_types: vec![],
            last_fired: None,
            seen_pubkeys: vec!["hb1_dismissed".to_string()],
        }];
        let contacts = HashSet::new();
        let hits = run_evaluate(&mut watches, &["hb1_dismissed"], &contacts);
        assert!(hits.is_empty(), "already-seen peer must not re-fire the watch");
    }

    #[test]
    fn watch_fires_independently() {
        let mut watches = vec![
            Watch {
                name: "watch-a".into(), tags: vec![], content_types: vec![],
                last_fired: None, seen_pubkeys: vec![],
            },
            Watch {
                name: "watch-b".into(), tags: vec![], content_types: vec![],
                last_fired: None, seen_pubkeys: vec!["hb1_peer".to_string()], // already seen in B
            },
        ];
        let contacts = HashSet::new();
        let hits = run_evaluate(&mut watches, &["hb1_peer"], &contacts);
        // Only watch-a fires; watch-b already saw this peer.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].watch_name, "watch-a");
    }

    #[test]
    fn watch_persists() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let watch = Watch {
            name: "persistent".into(),
            tags: vec!["tag1".into()],
            content_types: vec!["video".into()],
            last_fired: None,
            seen_pubkeys: vec![],
        };
        store.save_watches(std::slice::from_ref(&watch)).unwrap();

        let loaded = store.load_watches().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "persistent");
        assert_eq!(loaded[0].tags, vec!["tag1"]);
        assert_eq!(loaded[0].content_types, vec!["video"]);
    }

    #[test]
    fn last_fired_only_updated_for_watch_that_had_hits() {
        let mut watches = vec![
            Watch {
                name: "watch-hits".into(), tags: vec![], content_types: vec![],
                last_fired: None, seen_pubkeys: vec![],
            },
            Watch {
                name: "watch-no-hits".into(), tags: vec![], content_types: vec![],
                last_fired: None, seen_pubkeys: vec!["hb1_peer".to_string()], // already dismissed
            },
        ];
        let contacts = HashSet::new();
        run_evaluate(&mut watches, &["hb1_peer"], &contacts);

        assert!(watches[0].last_fired.is_some(), "watch with hit should have last_fired set");
        assert!(watches[1].last_fired.is_none(), "watch without hit must not have last_fired set");
    }
}
