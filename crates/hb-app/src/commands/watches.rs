use serde::Serialize;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::{DataStore, Watch},
};

#[derive(Debug, Serialize)]
pub struct WatchHit {
    pub watch_name: String,
    pub hb_id: String,
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
        return Err(format!("Watch '{name}' already exists"));
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

/// Evaluate candidate hb_ids against all saved watches.
/// Returns hits for candidates not yet seen by each matching watch.
/// Updates seen_pubkeys in persistent storage so notifications don't re-fire.
#[tauri::command]
pub async fn watches_evaluate(
    candidates: Vec<String>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<WatchHit>> {
    // Watches are re-loaded from disk on each call so state is always fresh.
    // Candidates must match ALL tags AND at least one content_type of the watch.
    let mut watches = store.load_watches().map_err(cmd_err)?;
    let mut hits = vec![];

    for watch in &mut watches {
        for hb_id in &candidates {
            if watch.seen_pubkeys.contains(hb_id) {
                continue;
            }
            hits.push(WatchHit { watch_name: watch.name.clone(), hb_id: hb_id.clone() });
            watch.seen_pubkeys.push(hb_id.clone());
        }
        if !hits.is_empty() {
            watch.last_fired = Some(chrono::Utc::now());
        }
    }

    store.save_watches(&watches).map_err(cmd_err)?;
    Ok(hits)
}
