//! QURATOR-347 slice A — the `search_titles` Tauri command: a thin synchronous read over the
//! in-memory title index ([`crate::title_index`]). No network, no crypto — §5 does not fire.

use crate::title_index::{self, TitleSearchResult};

/// Search the title index. The query is normalised with the ONE `normalize_title`
/// (QURATOR-344), so the index, the holder counts and (P3b) the similarity sort can never
/// disagree about whether two names are the same title.
///
/// Hard caps, documented: at most 50 titles and 20 holders per title per query; `limit` may
/// lower the title cap, never raise it. `TitleSearchResult.truncated` is true when more
/// matching titles existed than were returned. The honest limit the UI copy must state:
/// **holder counts cover only people you can read**.
#[tauri::command]
pub fn search_titles(query: String, limit: Option<usize>) -> Result<TitleSearchResult, String> {
    Ok(title_index::search_titles(&query, limit))
}
