//! QURATOR-347 slice A — the `search_titles` Tauri command: a thin synchronous read over the
//! in-memory title index ([`crate::title_index`]). No network, no crypto — §5 does not fire.

use std::collections::HashSet;

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
///
/// `authors` scopes the search to those npubs (owner, v0.24.0 devtest: Browse searches the
/// selected contact, or the selected group's members). Absent = every contact. An optional
/// argument, so an older UI that never sends it keeps the unscoped search.
#[tauri::command]
pub fn search_titles(
    query: String,
    limit: Option<usize>,
    authors: Option<Vec<String>>,
    store: State<'_, DataStore>,
) -> Result<TitleSearchResult, String> {
    let my = crate::commands::people::my_similarity_inputs(&store);
    let scope: Option<HashSet<String>> = authors.map(|a| a.into_iter().collect());
    Ok(title_index::search_titles(&query, limit, &my, scope.as_ref()))
}
