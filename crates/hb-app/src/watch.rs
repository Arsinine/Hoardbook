//! Snapshot auto-update — the filesystem-**watch** that keeps a published listing fresh (spec
//! §Collection Manager → Snapshot trigger; Decision #17). When a published collection's source tree
//! changes, the listing re-snapshots and re-publishes **automatically**, so followers stop trading
//! around stale snapshots — via a debounced fs-watch, **never a poll** (real hoards span many
//! SMB-mounted drives with thousands of folders; scanning them on a timer is a network clusterfuck).
//!
//! The design is split so the OS watcher is **never** in a unit test:
//! - [`watch_plan`] is **pure** — a batch of raw fs events + config + prior state → the set of
//!   collections to republish, applying debounce/coalesce, a per-collection cadence floor, and scope
//!   (published collections only). Fake-clock tested.
//! - [`plan_launch_rescan`] is **pure** — the bounded launch reconcile (F24): a concurrency cap, a
//!   total budget, ordered, with a progress signal — so 20 published collections don't become a
//!   20×30 s serial scan + 20 republish bursts on boot.
//! - [`PublishSink`] is the **injected** publish seam: the prod impl re-scans + fingerprint-diffs +
//!   re-publishes through the existing path; the test impl counts calls (no `RelayClient`).
//! - [`spawn_fs_watcher`] is the **thin** `notify` wrapper — the only place the OS watcher lives.
//!
//! **SMB honesty (Decision #17).** `notify` reports *local-mount* edits (inotify/FSEvents/RDCW).
//! It does **not** see edits another host makes server-side on an SMB/CIFS/NFS share — those
//! reconcile on app **launch** (the bounded re-scan below), on the **manual** "Regenerate" button,
//! or via the **opt-in** low-frequency reconcile poll (`snapshot_reconcile_poll`). The spec is the
//! citation (§Collection Manager → Snapshot trigger; Resolved Design Decision #17).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::commands::collection::{count_items, publish_collection_inner, rescan_listing};
use crate::identity_state::SharedIdentity;
use crate::net::SharedRelay;
use crate::store::DataStore;

/// Default per-collection republish cadence floor: a churning folder cannot publish (and therefore
/// emit a timing signal) more than once a minute.
pub const MIN_REPUBLISH_INTERVAL: Duration = Duration::from_secs(60);
/// Default debounce/coalesce window: many fs events in a short burst collapse to one decision.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_secs(2);
/// Default launch-reconcile concurrency cap (F24): at most this many collections re-scan at once,
/// so boot doesn't fan out a storm of parallel SMB walks.
pub const MAX_CONCURRENT_LAUNCH_SCANS: usize = 2;
/// Default launch-reconcile budget (F24): at most this many collections are checked at launch; the
/// rest defer to the watch / next launch (logged, never dropped).
pub const STARTUP_SCAN_BUDGET: usize = 12;
/// Default flush tick: how often the idle loop wakes to flush a pending (debounced) republish.
pub const WATCH_TICK: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Pure types
// ---------------------------------------------------------------------------

/// A raw filesystem change, normalized from a `notify` event: the changed path + when we observed
/// it (the debounce anchor).
#[derive(Debug, Clone)]
pub struct FsEvent {
    pub path: PathBuf,
    pub at: Instant,
}

/// A published collection the watch is responsible for: its slug + the absolute root it watches.
#[derive(Debug, Clone)]
pub struct WatchedCollection {
    pub slug: String,
    pub root: PathBuf,
}

/// Watch tuning (Decision #17 / F24). All durations + caps are configurable; the defaults are the
/// `*_` consts above.
#[derive(Debug, Clone)]
pub struct WatchCfg {
    pub debounce: Duration,
    pub min_republish_interval: Duration,
    pub watched: Vec<WatchedCollection>,
    pub max_concurrent_launch_scans: usize,
    pub startup_scan_budget: usize,
}

impl Default for WatchCfg {
    fn default() -> Self {
        Self {
            debounce: DEBOUNCE_WINDOW,
            min_republish_interval: MIN_REPUBLISH_INTERVAL,
            watched: Vec::new(),
            max_concurrent_launch_scans: MAX_CONCURRENT_LAUNCH_SCANS,
            startup_scan_budget: STARTUP_SCAN_BUDGET,
        }
    }
}

/// Per-collection debounce + cadence state, threaded across `watch_plan` calls.
#[derive(Debug, Default, Clone)]
struct CollState {
    /// Most recent fs event time (the debounce anchor); `None` once a pending republish flushed.
    last_dirty: Option<Instant>,
    /// Last time this collection actually republished (the cadence-floor anchor).
    last_republish: Option<Instant>,
}

/// Opaque debounce/cadence state for the watch loop.
#[derive(Debug, Default)]
pub struct WatchState {
    per: HashMap<String, CollState>,
}

/// A decision to republish one collection now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepublishAction {
    pub slug: String,
}

/// The bounded launch-reconcile plan (F24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRescanPlan {
    /// Slugs to re-scan at launch, ordered, within the budget.
    pub to_scan: Vec<String>,
    /// Slugs deferred past the budget (handled by the watch / next launch — not dropped).
    pub deferred: Vec<String>,
    /// Concurrency cap the loop must honour.
    pub max_concurrent: usize,
    /// Total published count (the M in the "checking N of M" progress signal).
    pub total: usize,
}

/// Progress signal emitted while the launch reconcile runs ("Checking snapshots… N of M").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotProgress {
    pub checked: usize,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// Map a changed path to the published collection that owns it: the watched collection whose root
/// is the **longest** ancestor of the path (so nested roots resolve to the most specific). Returns
/// `None` for a path outside every watched root — the scope guard (an edit to an unpublished /
/// unwatched tree triggers nothing).
fn collection_for_path(cfg: &WatchCfg, path: &Path) -> Option<String> {
    cfg.watched
        .iter()
        .filter(|w| path.starts_with(&w.root))
        .max_by_key(|w| w.root.as_os_str().len())
        .map(|w| w.slug.clone())
}

/// **Pure** watch decision: fold a batch of raw fs events into the set of collections to republish
/// *now*, applying scope (published only), debounce/coalesce (a burst collapses to one; the quiet
/// window must elapse), and the per-collection cadence floor (no republish within
/// `min_republish_interval` of the last one — a churning folder can't spam relays). Called on each
/// fs-event batch and on each idle tick (an empty batch just advances time and flushes ready
/// pending collections). Deterministic: actions are returned slug-sorted.
pub fn watch_plan(
    events: &[FsEvent],
    cfg: &WatchCfg,
    state: &mut WatchState,
    now: Instant,
) -> Vec<RepublishAction> {
    // 1. Scope + mark dirty (anchor the debounce to the most recent event for the collection).
    for ev in events {
        if let Some(slug) = collection_for_path(cfg, &ev.path) {
            let st = state.per.entry(slug).or_default();
            st.last_dirty = Some(match st.last_dirty {
                Some(prev) if prev > ev.at => prev,
                _ => ev.at,
            });
        }
    }
    // 2. Flush every collection whose quiet window has elapsed AND whose cadence floor is satisfied.
    //    A collection blocked only by the floor stays pending (last_dirty kept) and flushes on a
    //    later tick once the floor passes — never dropped.
    let mut actions = Vec::new();
    for (slug, st) in state.per.iter_mut() {
        let Some(dirty) = st.last_dirty else { continue };
        let debounced = now.saturating_duration_since(dirty) >= cfg.debounce;
        let floor_ok = st
            .last_republish
            .is_none_or(|lr| now.saturating_duration_since(lr) >= cfg.min_republish_interval);
        if debounced && floor_ok {
            actions.push(RepublishAction { slug: slug.clone() });
            st.last_republish = Some(now);
            st.last_dirty = None;
        }
    }
    actions.sort_by(|a, b| a.slug.cmp(&b.slug));
    actions
}

/// **Pure** bounded launch reconcile (F24): order the published slugs deterministically, take the
/// first `startup_scan_budget` to scan now, defer the rest (handled by the watch / next launch), and
/// surface the concurrency cap + the total for the "checking N of M" progress signal. The budget and
/// cap clamp to ≥1 so a misconfig can't stall the reconcile entirely.
pub fn plan_launch_rescan(published_slugs: &[String], cfg: &WatchCfg) -> LaunchRescanPlan {
    let mut ordered: Vec<String> = published_slugs.to_vec();
    ordered.sort();
    let total = ordered.len();
    let budget = cfg.startup_scan_budget.max(1);
    let split = budget.min(total);
    let deferred = ordered.split_off(split);
    LaunchRescanPlan {
        to_scan: ordered,
        deferred,
        max_concurrent: cfg.max_concurrent_launch_scans.max(1),
        total,
    }
}

// ---------------------------------------------------------------------------
// The injected publish seam
// ---------------------------------------------------------------------------

/// The publish seam the watch loop drives. The prod impl ([`RelayPublishSink`]) re-scans,
/// fingerprint-diffs, and re-publishes through the existing path; a test impl counts calls so the
/// loop is exercised with no real `RelayClient`. Returns whether a republish actually happened
/// (`false` = fingerprint no-op).
#[async_trait::async_trait]
pub trait PublishSink: Send + Sync + 'static {
    async fn republish(&self, slug: &str) -> anyhow::Result<bool>;
}

/// The re-scan decision for one collection — pure of the network, so it is testable with only a
/// `DataStore` + a tempdir tree (no relay).
#[derive(Debug)]
pub(crate) enum RescanDecision {
    /// The tree changed; carries the freshly-scanned listing to publish.
    Changed(Vec<hb_core::DirectoryItem>),
    /// The re-scan hashed equal to the last published snapshot — a **no-op** (the storm guard).
    Unchanged,
    /// Not actionable (not published, or no persisted scan spec to re-scan from).
    Skipped(String),
}

/// Decide whether a published collection needs a republish: re-scan its source tree (filesystem
/// only), fingerprint it, and diff against the last-published fingerprint. `Unchanged` ⇒ zero relay
/// writes (the republish-storm + metadata-churn guard).
pub(crate) fn evaluate_rescan(slug: &str, store: &DataStore) -> Result<RescanDecision, String> {
    if !store.is_published(slug) {
        return Ok(RescanDecision::Skipped("not published".into()));
    }
    let Some(new_listing) = rescan_listing(slug, store)? else {
        return Ok(RescanDecision::Skipped("no scan spec (pre-M9 draft)".into()));
    };
    let new_fp = hb_core::snapshot_fingerprint(&new_listing);
    match store.load_snapshot_fingerprint(slug).map_err(|e| e.to_string())? {
        Some(prev) if hb_core::unchanged_since(&prev, &new_fp) => Ok(RescanDecision::Unchanged),
        _ => Ok(RescanDecision::Changed(new_listing)),
    }
}

/// Production sink: re-scan → fingerprint-diff → (if changed) update the draft + re-publish via the
/// **existing** `publish_collection_inner` path (which re-encrypts/re-signs, re-derives the teaser's
/// aggregated `content_types`, and records the new fingerprint). Reuses the publish path — never
/// forks it.
pub struct RelayPublishSink {
    pub store: DataStore,
    pub identity: SharedIdentity,
    pub relay: SharedRelay,
}

#[async_trait::async_trait]
impl PublishSink for RelayPublishSink {
    async fn republish(&self, slug: &str) -> anyhow::Result<bool> {
        let (id, browse_key) = {
            let guard = self.identity.read().await;
            let app = guard
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no identity loaded; skipping snapshot republish"))?;
            (app.identity.clone(), app.browse_key)
        };
        match evaluate_rescan(slug, &self.store).map_err(|e| anyhow::anyhow!(e))? {
            RescanDecision::Unchanged => Ok(false),
            RescanDecision::Skipped(reason) => {
                tracing::debug!("snapshot republish of '{slug}' skipped: {reason}");
                Ok(false)
            }
            RescanDecision::Changed(new_listing) => {
                // Update the draft's tree (item_count + last_updated) before publishing; the publish
                // path then encrypts/signs/publishes it and saves the new fingerprint.
                if let Some(mut col) = self.store.load_collection_draft(slug)? {
                    col.item_count = count_items(&new_listing);
                    col.listing = new_listing;
                    col.last_updated = chrono::Utc::now();
                    self.store.save_collection_draft(&col)?;
                }
                publish_collection_inner(slug, &self.store, &id, &browse_key, &self.relay)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?;
                Ok(true)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// Build the watched-collection set from the store: every published collection with a persisted
/// scan spec, keyed to its root. The watch's scope + the OS watcher's roots both derive from this.
pub fn watched_from_store(store: &DataStore) -> Vec<WatchedCollection> {
    let mut out = Vec::new();
    for slug in store.list_published_slugs().unwrap_or_default() {
        if let Ok(Some(spec)) = store.load_scan_spec(&slug) {
            if !spec.root.is_empty() {
                out.push(WatchedCollection { slug, root: PathBuf::from(spec.root) });
            }
        }
    }
    out
}

/// The snapshot watch loop — a sibling of `run_presence_loop`, owning exactly one watcher per app
/// (single-instance, M8). Does the bounded launch reconcile first (F24, with progress), then folds
/// fs events through `watch_plan` on each batch + idle tick, republishing via the injected `sink`.
/// `wakeups` counts loop iterations so the L4 idle guard can assert the loop doesn't busy-spin.
#[allow(clippy::too_many_arguments)]
pub async fn run_watch_loop(
    store: DataStore,
    cfg: WatchCfg,
    sink: Arc<dyn PublishSink>,
    mut fs_rx: mpsc::Receiver<FsEvent>,
    mut cancel_rx: watch::Receiver<bool>,
    tick: Duration,
    progress: Option<mpsc::UnboundedSender<SnapshotProgress>>,
    wakeups: Arc<AtomicU64>,
) {
    // 1. Bounded launch reconcile (catches edits made while closed + server-side SMB edits the watch
    //    missed). Each re-scan no-ops via the fingerprint guard unless the tree actually changed.
    let published = store.list_published_slugs().unwrap_or_default();
    let plan = plan_launch_rescan(&published, &cfg);
    if !plan.to_scan.is_empty() {
        tracing::info!(
            "snapshot launch reconcile: checking {} of {} published ({} deferred to the watch)",
            plan.to_scan.len(),
            plan.total,
            plan.deferred.len()
        );
    }
    let mut checked = 0usize;
    for chunk in plan.to_scan.chunks(plan.max_concurrent) {
        let mut handles = Vec::new();
        for slug in chunk {
            let sink = Arc::clone(&sink);
            let slug = slug.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = sink.republish(&slug).await {
                    tracing::debug!("launch reconcile of '{slug}' failed: {e:#}");
                }
            }));
        }
        for h in handles {
            let _ = h.await;
            checked += 1;
            if let Some(p) = &progress {
                let _ = p.send(SnapshotProgress { checked, total: plan.total });
            }
        }
        if *cancel_rx.borrow() {
            return;
        }
    }

    // 2. Steady state: fold fs events + idle ticks through the pure planner.
    let mut state = WatchState::default();
    let mut fs_open = true;
    loop {
        wakeups.fetch_add(1, Ordering::Relaxed);
        let mut batch: Vec<FsEvent> = Vec::new();
        tokio::select! {
            maybe = fs_rx.recv(), if fs_open => {
                match maybe {
                    Some(ev) => {
                        batch.push(ev);
                        // Coalesce the rest of an in-flight burst without waiting.
                        while let Ok(ev) = fs_rx.try_recv() {
                            batch.push(ev);
                        }
                    }
                    // The watcher was dropped (auto-update off / shutdown): stop polling the closed
                    // channel so the loop idles on the tick instead of busy-looping on None.
                    None => fs_open = false,
                }
            }
            _ = tokio::time::sleep(tick) => {}
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    tracing::debug!("watch loop cancelled");
                    break;
                }
            }
        }
        for action in watch_plan(&batch, &cfg, &mut state, Instant::now()) {
            // `watch_plan` optimistically cleared the pending flag + advanced the cadence floor when
            // it emitted this action. A *transient* publish failure here is therefore not a silent
            // permanent drop: the persisted snapshot fingerprint only advances on a **successful**
            // publish (`publish_collection_inner`), so the changed-but-unpublished tree is re-detected
            // (as `Changed`) by the next fs event and by the launch reconcile — eventual consistency,
            // with the floor preventing a hammer-the-down-relay retry storm (chorus: Codex).
            if let Err(e) = sink.republish(&action.slug).await {
                tracing::debug!("watch republish of '{}' failed: {e:#}", action.slug);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The thin notify wrapper (the only place the OS watcher lives)
// ---------------------------------------------------------------------------

/// Start a recursive `notify` watcher over `roots`, forwarding each changed path into `tx` as an
/// [`FsEvent`]. The returned watcher must be kept alive (dropping it stops watching). **Thin by
/// design** — this is the sole OS-watcher touchpoint, kept out of every unit test; the decision
/// logic lives in the pure [`watch_plan`].
///
/// **SMB/network-mount blind spot (Decision #17, spec §Collection Manager → Snapshot trigger):** the
/// underlying inotify/FSEvents/ReadDirectoryChangesW only sees edits made through *this* host's view
/// of the mount. A change another host writes server-side on an SMB/CIFS/NFS share raises **no**
/// local event here — those reconcile on launch (the bounded re-scan), on manual "Regenerate", or
/// via the opt-in `snapshot_reconcile_poll`.
pub fn spawn_fs_watcher(
    roots: &[PathBuf],
    tx: mpsc::Sender<FsEvent>,
) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // Access-only events (reads/opens) are not content changes — ignore them so a browse of
            // the share doesn't trigger a needless re-scan (the fingerprint would no-op anyway, but
            // skipping here avoids the work).
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                // blocking_send is correct from notify's own (non-tokio) callback thread; if the
                // bounded channel is full the watcher thread briefly backpressures, which is fine.
                let _ = tx.blocking_send(FsEvent { path, at: Instant::now() });
            }
        },
        Config::default(),
    )?;
    for root in roots {
        // Best-effort per root: an unmounted/missing root is skipped (the launch re-scan + manual
        // refresh still cover it), never fatal to the whole watcher.
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::debug!("could not watch {}: {e}", root.display());
        }
    }
    Ok(watcher)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure watch_plan (fake clock via Instant offsets) ------------------

    fn cfg_with(roots: &[(&str, &str)]) -> WatchCfg {
        WatchCfg {
            debounce: Duration::from_secs(2),
            min_republish_interval: Duration::from_secs(60),
            watched: roots
                .iter()
                .map(|(slug, root)| WatchedCollection { slug: slug.to_string(), root: PathBuf::from(root) })
                .collect(),
            ..Default::default()
        }
    }

    fn ev(root: &str, child: &str, at: Instant) -> FsEvent {
        FsEvent { path: PathBuf::from(root).join(child), at }
    }

    #[test]
    fn single_edit_yields_one_republish_after_debounce() {
        let cfg = cfg_with(&[("films", "/mnt/films")]);
        let mut st = WatchState::default();
        let t0 = Instant::now();
        // The edit alone (debounce not yet elapsed) → no action.
        let immediate = watch_plan(&[ev("/mnt/films", "a.mkv", t0)], &cfg, &mut st, t0);
        assert!(immediate.is_empty(), "must wait out the debounce window");
        // A tick past the debounce → exactly one republish.
        let after = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(3));
        assert_eq!(after, vec![RepublishAction { slug: "films".into() }]);
        // And it does not fire again on a later idle tick (pending cleared).
        let again = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(5));
        assert!(again.is_empty(), "a flushed republish does not repeat");
    }

    #[test]
    fn burst_within_debounce_coalesces_to_one() {
        let cfg = cfg_with(&[("films", "/mnt/films")]);
        let mut st = WatchState::default();
        let t0 = Instant::now();
        // A burst of many edits inside the debounce window.
        let burst: Vec<FsEvent> = (0..10)
            .map(|i| ev("/mnt/films", &format!("f{i}.mkv"), t0 + Duration::from_millis(i * 100)))
            .collect();
        assert!(watch_plan(&burst, &cfg, &mut st, t0 + Duration::from_millis(900)).is_empty());
        // After the burst settles, exactly one republish — not ten.
        let after = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(4));
        assert_eq!(after.len(), 1, "a burst coalesces to a single republish");
    }

    #[test]
    fn two_collections_edited_yield_two_republishes() {
        let cfg = cfg_with(&[("films", "/mnt/films"), ("music", "/mnt/music")]);
        let mut st = WatchState::default();
        let t0 = Instant::now();
        let evs = vec![ev("/mnt/films", "a.mkv", t0), ev("/mnt/music", "b.flac", t0)];
        assert!(watch_plan(&evs, &cfg, &mut st, t0).is_empty());
        let after = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(3));
        assert_eq!(after, vec![
            RepublishAction { slug: "films".into() },
            RepublishAction { slug: "music".into() },
        ]);
    }

    #[test]
    fn rapid_churn_is_floored_to_one_per_interval() {
        let cfg = cfg_with(&[("films", "/mnt/films")]);
        let mut st = WatchState::default();
        let t0 = Instant::now();
        // First edit → flushes at t0+3s (debounce elapsed, no prior republish).
        watch_plan(&[ev("/mnt/films", "a.mkv", t0)], &cfg, &mut st, t0);
        let first = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(3));
        assert_eq!(first.len(), 1, "first republish fires");
        // More churn shortly after: debounce elapses again, but the 60 s cadence floor blocks it.
        watch_plan(&[ev("/mnt/films", "b.mkv", t0 + Duration::from_secs(4))], &cfg, &mut st, t0 + Duration::from_secs(4));
        let blocked = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(8));
        assert!(blocked.is_empty(), "the cadence floor blocks a second republish within 60s");
        // Once the floor passes, the still-pending change flushes (never dropped).
        let later = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(65));
        assert_eq!(later.len(), 1, "the deferred republish flushes after the floor passes");
    }

    #[test]
    fn edit_to_unpublished_collection_is_ignored() {
        // Scope guard: an event under a path no watched collection owns triggers nothing.
        let cfg = cfg_with(&[("films", "/mnt/films")]);
        let mut st = WatchState::default();
        let t0 = Instant::now();
        watch_plan(&[ev("/mnt/private", "secret.txt", t0)], &cfg, &mut st, t0);
        let after = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(3));
        assert!(after.is_empty(), "a change outside every watched root is out of scope");
    }

    #[test]
    fn nested_roots_resolve_to_the_most_specific() {
        // A path under both /mnt and /mnt/films belongs to the longer (more specific) root.
        let cfg = cfg_with(&[("all", "/mnt"), ("films", "/mnt/films")]);
        let mut st = WatchState::default();
        let t0 = Instant::now();
        watch_plan(&[ev("/mnt/films", "a.mkv", t0)], &cfg, &mut st, t0);
        let after = watch_plan(&[], &cfg, &mut st, t0 + Duration::from_secs(3));
        assert_eq!(after, vec![RepublishAction { slug: "films".into() }], "longest-prefix wins");
    }

    // ---- plan_launch_rescan (F24) ------------------------------------------

    #[test]
    fn launch_rescan_caps_budget_and_concurrency_defers_the_rest() {
        let cfg = WatchCfg {
            startup_scan_budget: 12,
            max_concurrent_launch_scans: 2,
            ..Default::default()
        };
        let slugs: Vec<String> = (0..20).map(|i| format!("col-{i:02}")).collect();
        let plan = plan_launch_rescan(&slugs, &cfg);
        assert_eq!(plan.total, 20, "M (for 'N of M') is the full published count");
        assert_eq!(plan.to_scan.len(), 12, "at most the budget is scanned at launch");
        assert_eq!(plan.deferred.len(), 8, "the over-budget collections defer, not dropped");
        assert_eq!(plan.max_concurrent, 2, "the concurrency cap is honoured");
        // Nothing is lost: to_scan ∪ deferred == all slugs.
        let mut union = plan.to_scan.clone();
        union.extend(plan.deferred.clone());
        union.sort();
        let mut all = slugs.clone();
        all.sort();
        assert_eq!(union, all, "every published collection is either scanned or deferred");
    }

    #[test]
    fn launch_rescan_under_budget_scans_all_with_nothing_deferred() {
        let cfg = WatchCfg { startup_scan_budget: 12, ..Default::default() };
        let slugs: Vec<String> = (0..3).map(|i| format!("c{i}")).collect();
        let plan = plan_launch_rescan(&slugs, &cfg);
        assert_eq!(plan.to_scan.len(), 3);
        assert!(plan.deferred.is_empty());
        assert_eq!(plan.total, 3);
    }

    // ---- run_watch_loop wiring (injected fake sink; no RelayClient) --------

    /// A fake sink that records every slug it was asked to republish.
    struct CountingSink {
        calls: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl PublishSink for CountingSink {
        async fn republish(&self, slug: &str) -> anyhow::Result<bool> {
            self.calls.lock().unwrap().push(slug.to_string());
            Ok(true)
        }
    }

    fn fast_cfg(roots: &[(&str, &str)]) -> WatchCfg {
        WatchCfg {
            debounce: Duration::from_millis(10),
            min_republish_interval: Duration::from_millis(10),
            watched: roots
                .iter()
                .map(|(s, r)| WatchedCollection { slug: s.to_string(), root: PathBuf::from(r) })
                .collect(),
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_loop_republishes_on_fs_event_via_injected_sink() {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::new(CountingSink { calls: Arc::clone(&calls) });
        let (fs_tx, fs_rx) = mpsc::channel(64);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());

        let handle = tokio::spawn(run_watch_loop(
            store,
            fast_cfg(&[("films", "/mnt/films")]),
            sink,
            fs_rx,
            cancel_rx,
            Duration::from_millis(20),
            None,
            Arc::new(AtomicU64::new(0)),
        ));

        fs_tx.send(FsEvent { path: PathBuf::from("/mnt/films/a.mkv"), at: Instant::now() }).await.unwrap();
        // Give the debounce + a tick time to flush.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        let got = calls.lock().unwrap();
        assert_eq!(&*got, &["films".to_string()], "one fs event → one republish through the sink");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_loop_launch_rescan_checks_published_and_emits_progress() {
        // Two published collections on disk → the launch reconcile republishes each + emits "N of M".
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        for slug in ["a", "b"] {
            let col = hb_core::types::Collection {
                slug: slug.into(),
                path_alias: slug.into(),
                description: None,
                item_count: 0,
                est_size: None,
                content_types: vec![],
                tags: vec![],
                languages: vec![],
                visibility: hb_core::types::Visibility::Public,
                sorted: false,
                last_updated: chrono::Utc::now(),
                listing: vec![],
            };
            store.save_collection_draft(&col).unwrap();
            store.save_published(slug, "{}").unwrap();
            store.save_scan_spec(slug, &crate::store::ScanSpec { root: dir.path().to_string_lossy().into(), include: vec![], exclude: vec![], ..Default::default() }).unwrap();
        }
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::new(CountingSink { calls: Arc::clone(&calls) });
        let (_fs_tx, fs_rx) = mpsc::channel(8);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (prog_tx, mut prog_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run_watch_loop(
            store,
            WatchCfg::default(),
            sink,
            fs_rx,
            cancel_rx,
            Duration::from_millis(50),
            Some(prog_tx),
            Arc::new(AtomicU64::new(0)),
        ));
        tokio::time::sleep(Duration::from_millis(120)).await;
        let _ = cancel_tx.send(true);
        let _ = handle.await;

        let got = calls.lock().unwrap();
        assert_eq!(got.len(), 2, "both published collections checked at launch");
        // Progress reached "2 of 2".
        let mut last = None;
        while let Ok(p) = prog_rx.try_recv() {
            last = Some(p);
        }
        assert_eq!(last, Some(SnapshotProgress { checked: 2, total: 2 }), "progress completes at N of M");
    }

    // ---- L4 idle guard: the loop must not busy-spin -------------------------

    /// Run a future for `window` while a metronome counts its own cooperative ticks; return the
    /// metronome count and the loop's wakeup count. An *idle* loop yields between ticks so the
    /// metronome runs freely and the loop wakes ~`window/tick` times; a *busy* loop wakes far more.
    async fn measure_idle<F>(make_loop: F, window: Duration, tick: Duration) -> (u64, u64)
    where
        F: FnOnce(watch::Receiver<bool>, Arc<AtomicU64>) -> tokio::task::JoinHandle<()>,
    {
        let wakeups = Arc::new(AtomicU64::new(0));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let handle = make_loop(cancel_rx, Arc::clone(&wakeups));
        let start = Instant::now();
        let mut metronome = 0u64;
        while start.elapsed() < window {
            tokio::time::sleep(tick).await;
            metronome += 1;
        }
        let _ = cancel_tx.send(true);
        let _ = handle.await;
        (metronome, wakeups.load(Ordering::Relaxed))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_loop_idles_under_budget() {
        // With no fs events, the loop wakes only on its tick — wakeups/sec stays under budget.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let window = Duration::from_millis(300);
        let tick = Duration::from_millis(50);
        let (_meter, wakeups) = measure_idle(
            move |cancel_rx, wk| {
                let (_fs_tx, fs_rx) = mpsc::channel(8);
                tokio::spawn(run_watch_loop(
                    store,
                    WatchCfg::default(),
                    Arc::new(CountingSink { calls }),
                    fs_rx,
                    cancel_rx,
                    tick,
                    None,
                    wk,
                ))
            },
            window,
            tick,
        )
        .await;
        // Budget: an idle loop wakes ~window/tick (~6) plus slack. 100 wakeups in 300ms (333/s) is a
        // generous ceiling a correct loop never approaches but a busy-spin blows past.
        assert!(wakeups < 100, "idle watch loop woke {wakeups} times in 300ms — busy-spinning?");
    }

    /// A deliberately-spinning loop: it yields (so cancel can fire) but never sleeps, re-polling as
    /// fast as the scheduler allows — the 2026-06-07 GUI-loop-spin pathology. The guard must flag it.
    async fn spinning_fixture(cancel_rx: watch::Receiver<bool>, wakeups: Arc<AtomicU64>) {
        loop {
            wakeups.fetch_add(1, Ordering::Relaxed);
            if *cancel_rx.borrow() {
                break;
            }
            tokio::task::yield_now().await; // hot re-poll, no sleep
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spinning_loop_fails_the_idle_guard() {
        // Proof the guard bites: the spinning fixture blows past the same budget the idle loop meets.
        let window = Duration::from_millis(300);
        let tick = Duration::from_millis(50);
        let (_meter, wakeups) = measure_idle(
            |cancel_rx, wk| tokio::spawn(spinning_fixture(cancel_rx, wk)),
            window,
            tick,
        )
        .await;
        assert!(wakeups > 100, "a spinning loop must exceed the idle budget (woke {wakeups}) — guard is theatre otherwise");
    }

    // ---- evaluate_rescan: the fingerprint no-op storm guard (filesystem; no relay) ----

    fn write_tree(root: &Path) {
        std::fs::write(root.join("a.mkv"), b"x").unwrap();
        std::fs::write(root.join("b.mkv"), b"x").unwrap();
    }

    fn setup_published_collection(store: &DataStore, slug: &str, root: &Path) {
        // Scan the tree, save it as the published draft, persist the scan spec + the fingerprint
        // baseline — i.e. the on-disk state a real publish leaves, without touching the network.
        let (listing, _) = crate::commands::collection::scan_selective(
            root,
            &crate::commands::collection::IncludeSet::new(vec![]),
            &globset::GlobSetBuilder::new().build().unwrap(),
        )
        .unwrap();
        let col = hb_core::types::Collection {
            slug: slug.into(),
            path_alias: slug.into(),
            description: None,
            item_count: 0,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec![],
            languages: vec![],
            visibility: hb_core::types::Visibility::Public,
            sorted: false,
            last_updated: chrono::Utc::now(),
            listing: listing.clone(),
        };
        store.save_collection_draft(&col).unwrap();
        store.save_published(slug, "{}").unwrap();
        store
            .save_scan_spec(slug, &crate::store::ScanSpec { root: root.to_string_lossy().into(), include: vec![], exclude: vec![], ..Default::default() })
            .unwrap();
        store.save_snapshot_fingerprint(slug, &hb_core::snapshot_fingerprint(&listing)).unwrap();
    }

    #[test]
    fn evaluate_rescan_is_a_no_op_when_tree_unchanged() {
        let work = tempfile::tempdir().unwrap();
        write_tree(work.path());
        let data = tempfile::tempdir().unwrap();
        let store = DataStore::new(data.path().to_path_buf());
        setup_published_collection(&store, "films", work.path());
        // Re-scan with no filesystem change → Unchanged (zero relay writes).
        assert!(matches!(evaluate_rescan("films", &store).unwrap(), RescanDecision::Unchanged));
    }

    #[test]
    fn evaluate_rescan_detects_a_real_change() {
        let work = tempfile::tempdir().unwrap();
        write_tree(work.path());
        let data = tempfile::tempdir().unwrap();
        let store = DataStore::new(data.path().to_path_buf());
        setup_published_collection(&store, "films", work.path());
        // Add a file → the fingerprint differs → Changed (carrying the new tree).
        std::fs::write(work.path().join("c.mkv"), b"x").unwrap();
        match evaluate_rescan("films", &store).unwrap() {
            RescanDecision::Changed(listing) => {
                assert!(listing.iter().any(|i| i.name == "c.mkv"), "the new file is in the re-scanned tree");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_rescan_skips_unpublished_collection() {
        let data = tempfile::tempdir().unwrap();
        let store = DataStore::new(data.path().to_path_buf());
        match evaluate_rescan("ghost", &store).unwrap() {
            RescanDecision::Skipped(_) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
