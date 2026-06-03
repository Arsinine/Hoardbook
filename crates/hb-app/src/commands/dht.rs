use serde::Serialize;
use tauri::State;

use crate::{
    error::CmdResult,
    store::DataStore,
    SharedRelay,
};
use hb_core::types::Profile;

#[derive(Debug, Serialize)]
pub struct DhtResult {
    pub hb_id: String,
    pub profile: Option<Profile>,
    pub online: bool,
}

/// Search the DHT for peers announcing `tags` and/or `content_types`.
///
/// Phase 1 stub: returns an empty list. Real mainline BEP 5 DHT integration
/// is deferred to Phase 2. The command signature and frontend wiring are
/// established here so no UI changes are needed when the real search lands.
#[tauri::command]
pub async fn dht_search(
    tags: Vec<String>,
    content_types: Vec<String>,
    _store: State<'_, DataStore>,
    _relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<DhtResult>> {
    let _ = (tags, content_types); // will be used by real impl
    Ok(vec![])
}

/// Begin announcing this node's tags + content_types on the DHT.
/// Phase 1 stub — no-op until mainline DHT crate is wired.
#[tauri::command]
pub async fn dht_start_announce() -> CmdResult<()> {
    Ok(())
}

/// Stop announcing on the DHT.
/// Phase 1 stub — no-op until mainline DHT crate is wired.
#[tauri::command]
pub async fn dht_stop_announce() -> CmdResult<()> {
    Ok(())
}
