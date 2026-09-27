//! QURATOR-347 slice A — the `search_titles` Tauri command: a thin synchronous read over the
//! in-memory title index ([`crate::title_index`]). No network, no crypto — §5 does not fire.

use tauri::State;

use crate::store::DataStore;
use crate::title_index::{self, TitleSearchResult};

/// Search the title index. The query is normalised with the ONE `normalize_title`
/// (QURATOR-344), so the index, the holder counts and the similarity holder sort (slice B)
/// can never disagree about whether two names are the same title.
///
/// Hard caps, documented: at most 50 titles and 20 holders per title per query; `limit` may
/// lower the title cap, never raise it. `TitleSearchResult.truncated` is true when more
/// matching titles existed than were returned. The honest limit the UI copy must state:
/// **holder counts cover only people you can read**.
///
/// Slice B: holders are ranked by similarity to MY inputs — built here, ONCE per query, from
/// the store (my published public titles + my interests/collection tags), never per holder.
#[tauri::command]
pub fn search_titles(
    query: String,
    limit: Option<usize>,
    store: State<'_, DataStore>,
) -> Result<TitleSearchResult, String> {
    let my = crate::commands::people::my_similarity_inputs(&store);
    Ok(title_index::search_titles(&query, limit, &my))
}
