//! iroh-based P2P file transfer.
//!
//! Protocol (`/hoardbook/xfer/1`):
//!   Client → Server  [u32-LE request-len] [JSON XferRequest]
//!   Server → Client  [u8 status: 0=ok 1=error]
//!     ok    → [u64-LE file-size] [file bytes]
//!     error → [u32-LE msg-len]  [UTF-8 error message]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, atomic::{AtomicU32, AtomicU64, Ordering}};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSetBuilder};
use iroh::{Endpoint, EndpointAddr};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};

use crate::store::DataStore;

// ---------------------------------------------------------------------------
// Download registry — tracks in-flight downloads and their cancel tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgressEvent {
    pub id: u64,
    pub filename: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub status: DownloadStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Active,
    Done,
    Cancelled,
    Error,
}

pub struct DownloadRegistry {
    next_id: AtomicU64,
    cancels: Mutex<HashMap<u64, oneshot::Sender<()>>>,
    /// Counts concurrent active server-side downloads for enforcing download_limit.
    active_downloads: AtomicU32,
}

impl DownloadRegistry {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            cancels: Mutex::new(HashMap::new()),
            active_downloads: AtomicU32::new(0),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Increment the active-download counter and return the new count.
    pub fn acquire_slot(&self) -> u32 {
        self.active_downloads.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Decrement the active-download counter.
    pub fn release_slot(&self) {
        self.active_downloads.fetch_sub(1, Ordering::Relaxed);
    }

    pub async fn register(&self, id: u64) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.cancels.lock().await.insert(id, tx);
        rx
    }

    pub async fn cancel(&self, id: u64) -> bool {
        if let Some(tx) = self.cancels.lock().await.remove(&id) {
            tx.send(()).is_ok()
        } else {
            false
        }
    }

    pub async fn remove(&self, id: u64) {
        self.cancels.lock().await.remove(&id);
    }
}

pub type SharedDownloadRegistry = Arc<DownloadRegistry>;

pub const XFER_ALPN: &[u8] = b"/hoardbook/xfer/1";

#[derive(Serialize, Deserialize)]
struct XferRequest {
    slug: String,
    path: String,
    // NOTE: there is intentionally no requester-identity field here. The server
    // authorizes `require_follow` against `conn.remote_id()` — the iroh-authenticated
    // remote endpoint id (== the requester's hb_id) — never a self-claimed JSON value.
}

// ---------------------------------------------------------------------------
// Server — runs as a background task on the local iroh endpoint
// ---------------------------------------------------------------------------

/// Inner handler for a single xfer request/response. Extracted with generic
/// stream bounds so it can be tested without real QUIC networking.
pub(crate) async fn handle_xfer_stream(
    mut send: impl tokio::io::AsyncWrite + Unpin,
    mut recv: impl tokio::io::AsyncRead + Unpin,
    remote_hb_id: &str,
    store: &DataStore,
    registry: &SharedDownloadRegistry,
) -> Result<()> {
    // Read request
    let req_len = recv.read_u32_le().await.context("read req len")?;
    if req_len > 64 * 1024 {
        return Err(anyhow!("request too large"));
    }
    let mut req_bytes = vec![0u8; req_len as usize];
    recv.read_exact(&mut req_bytes).await.context("read req")?;
    let req: XferRequest = serde_json::from_slice(&req_bytes).context("parse request")?;

    // M7: validate the remote-supplied slug before it reaches any filesystem path.
    if !crate::commands::collection::is_valid_slug(&req.slug) {
        return send_error(&mut send, "Invalid collection slug").await;
    }

    // Load share settings
    let settings = store
        .load_share_settings(&req.slug)
        .context("load share settings")?
        .unwrap_or_default();

    if !settings.enabled {
        return send_error(&mut send, "Sharing is disabled for this collection").await;
    }

    // H17: enforce require_follow against the authenticated remote identity, not a
    // self-claimed request field.
    if settings.require_follow {
        let contacts = store.list_contacts().unwrap_or_default();
        if !contacts.iter().any(|c| c.hb_id == remote_hb_id) {
            return send_error(&mut send, "This collection is restricted to followers only").await;
        }
    }

    // Enforce download_limit
    if let Some(limit) = settings.download_limit {
        let current = registry.acquire_slot();
        if current > limit {
            registry.release_slot();
            return send_error(&mut send, "Download limit reached — try again later").await;
        }
    }
    // RAII guard to decrement on exit
    let _slot_guard = if settings.download_limit.is_some() {
        Some(DownloadSlotGuard { registry: registry.clone() })
    } else {
        None
    };

    if !is_allowed_path(&req.path, &settings.allowed_paths) {
        return send_error(&mut send, "File is not in the allowed download paths").await;
    }

    let root = match settings.root_path {
        Some(p) => p,
        None => return send_error(&mut send, "Collection root path not configured on sharer's end").await,
    };

    // Build and validate path (prevent traversal)
    let rel = Path::new(&req.path);
    if rel.is_absolute()
        || rel.components().any(|c| c == std::path::Component::ParentDir)
    {
        return send_error(&mut send, "Invalid file path").await;
    }
    let file_path = Path::new(&root).join(rel);

    if !file_path.is_file() {
        return send_error(&mut send, "File not found").await;
    }

    // M8: defense-in-depth against symlink escape. The `..`/absolute check above
    // operates on the unresolved path; resolve symlinks and confirm the real target
    // still lives under the (also-resolved) root. canonicalize() also normalizes
    // Windows UNC `\\?\` prefixes on both sides, so starts_with compares like-for-like.
    let canon_root = tokio::fs::canonicalize(&root)
        .await
        .context("canonicalize share root")?;
    match tokio::fs::canonicalize(&file_path).await {
        Ok(canon_file) if canon_file.starts_with(&canon_root) => {}
        _ => return send_error(&mut send, "Invalid file path").await,
    }

    // Stream file
    let file = tokio::fs::File::open(&file_path).await.context("open file")?;
    let file_size = file.metadata().await.context("metadata")?.len();

    send.write_u8(0).await.context("write ok status")?;
    send.write_u64_le(file_size).await.context("write file size")?;

    let mut reader = tokio::io::BufReader::new(file);

    if let Some(kbps) = settings.speed_cap_kbps {
        throttled_copy(&mut reader, &mut send, kbps as u64 * 1024).await.context("throttled stream")?;
    } else {
        tokio::io::copy(&mut reader, &mut send).await.context("stream file")?;
    }

    send.shutdown().await.context("shutdown send")?;
    Ok(())
}

pub(crate) async fn handle_xfer_connection(
    conn: iroh::endpoint::Connection,
    store: DataStore,
    registry: SharedDownloadRegistry,
) -> Result<()> {
    // The iroh-authenticated identity of the remote peer (from its TLS cert).
    // This == the requester's hb_id and is the ONLY trustworthy requester identity.
    let remote_hb_id = hb_core::hb_id_encode(conn.remote_id().as_bytes());
    let (send, recv) = conn.accept_bi().await.context("accept_bi")?;
    let res = handle_xfer_stream(send, recv, &remote_hb_id, &store, &registry).await;
    // Hold the connection open until the client has read the response. Small error
    // responses (e.g. the require_follow denial) otherwise race a CONNECTION_CLOSE on
    // fast links and are seen as a truncated read. See conn::drain_connection.
    crate::conn::drain_connection(&conn).await;
    res
}

// ---------------------------------------------------------------------------
// Path matching — glob-aware
// ---------------------------------------------------------------------------

fn is_allowed_path(path: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }

    // Build a glob set from the allowed patterns.
    // Fall back to simple prefix matching if a pattern is not valid glob syntax.
    let mut builder = GlobSetBuilder::new();
    let mut plain_prefixes: Vec<&str> = vec![];

    for pat in allowed {
        let trimmed = pat.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.ends_with('/') {
            plain_prefixes.push(trimmed);
        } else {
            match Glob::new(trimmed) {
                Ok(g) => { builder.add(g); }
                Err(_) => plain_prefixes.push(trimmed),
            }
        }
    }

    // Check plain prefix matches
    for prefix in &plain_prefixes {
        if path.starts_with(prefix) {
            return true;
        }
    }

    // Check glob matches
    if let Ok(set) = builder.build() {
        if set.is_match(path) {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Rate-limited copy
// ---------------------------------------------------------------------------

async fn throttled_copy(
    reader: &mut (impl AsyncReadExt + Unpin),
    writer: &mut (impl AsyncWriteExt + Unpin),
    bytes_per_sec: u64,
) -> Result<()> {
    const CHUNK: usize = 65_536; // 64 KB
    let mut buf = vec![0u8; CHUNK];

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        let start = tokio::time::Instant::now();
        writer.write_all(&buf[..n]).await?;

        // Sleep to hit the target rate.
        let budget = Duration::from_secs_f64(n as f64 / bytes_per_sec as f64);
        let elapsed = start.elapsed();
        if budget > elapsed {
            tokio::time::sleep(budget - elapsed).await;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Download slot RAII guard
// ---------------------------------------------------------------------------

struct DownloadSlotGuard {
    registry: SharedDownloadRegistry,
}

impl Drop for DownloadSlotGuard {
    fn drop(&mut self) {
        self.registry.release_slot();
    }
}

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

async fn send_error(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    msg: &str,
) -> Result<()> {
    let bytes = msg.as_bytes();
    send.write_u8(1).await.context("write error status")?;
    send.write_u32_le(bytes.len() as u32).await.context("write error len")?;
    send.write_all(bytes).await.context("write error msg")?;
    send.shutdown().await.context("shutdown")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Client — called from the request_download command
// ---------------------------------------------------------------------------

/// Check that `peer_addr.id` matches `expected_peer_hb_id` before connecting.
/// Extracted as a pure function so it can be unit-tested without a live endpoint.
pub(crate) fn verify_peer_identity(peer_addr: &EndpointAddr, expected_peer_hb_id: &str) -> Result<()> {
    let expected_id = iroh::EndpointId::from_bytes(&hb_core::hb_id_decode(expected_peer_hb_id)?)
        .context("expected peer hb_id is not a valid endpoint key")?;
    if peer_addr.id != expected_id {
        return Err(anyhow!(
            "Peer address does not match the expected identity — refusing to connect (the relay may be lying)."
        ));
    }
    Ok(())
}

/// Connect to a peer and download a single file, emitting progress events.
/// Respects cancellation via the registry. Returns bytes written.
#[allow(clippy::too_many_arguments)]
pub async fn download_file(
    endpoint: &Endpoint,
    peer_addr_json: &str,
    expected_peer_hb_id: &str,
    slug: &str,
    path: &str,
    save_path: &str,
    expected_sha256: Option<String>,
    download_id: u64,
    registry: SharedDownloadRegistry,
    app: AppHandle,
) -> Result<u64> {
    // Thin Tauri wrapper: forward every progress event to the webview. All the real
    // work lives in `download_file_inner`, which is Tauri-free so the integration
    // harness (hb-p2p-it) can drive the exact same streaming / integrity-check /
    // cancellation path without an AppHandle.
    download_file_inner(
        endpoint,
        peer_addr_json,
        expected_peer_hb_id,
        slug,
        path,
        save_path,
        expected_sha256,
        download_id,
        registry,
        move |ev| { let _ = app.emit("download:progress", ev); },
    )
    .await
}

/// Tauri-free core of [`download_file`]. `on_progress` receives every progress
/// event; the command wrapper forwards them to the webview, while tests and the
/// `hb-p2p-it` harness pass a no-op or logging closure.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_file_inner(
    endpoint: &Endpoint,
    peer_addr_json: &str,
    expected_peer_hb_id: &str,
    slug: &str,
    path: &str,
    save_path: &str,
    expected_sha256: Option<String>,
    download_id: u64,
    registry: SharedDownloadRegistry,
    on_progress: impl Fn(DownloadProgressEvent),
) -> Result<u64> {
    let filename = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string();

    let mut cancel_rx = registry.register(download_id).await;

    let emit_progress = |bytes_done: u64, bytes_total: u64, bps: u64, status: DownloadStatus, error: Option<String>| {
        on_progress(DownloadProgressEvent {
            id: download_id,
            filename: filename.clone(),
            bytes_done,
            bytes_total,
            bytes_per_sec: bps,
            status,
            error,
        });
    };

    emit_progress(0, 0, 0, DownloadStatus::Active, None);

    let peer_addr: EndpointAddr =
        serde_json::from_str(peer_addr_json).context("parse peer EndpointAddr")?;

    // H2: verify peer identity before opening any QUIC connection; see `verify_peer_identity`.
    if let Err(e) = verify_peer_identity(&peer_addr, expected_peer_hb_id) {
        let msg = e.to_string();
        emit_progress(0, 0, 0, DownloadStatus::Error, Some(msg.clone()));
        registry.remove(download_id).await;
        return Err(e);
    }

    let conn = endpoint
        .connect(peer_addr, XFER_ALPN)
        .await
        .context("connect to peer")?;

    let (mut send, mut recv) = conn.open_bi().await.context("open_bi")?;

    let req = XferRequest {
        slug: slug.to_string(),
        path: path.to_string(),
    };
    let req_bytes = serde_json::to_vec(&req).context("serialize request")?;
    send.write_u32_le(req_bytes.len() as u32).await.context("write req len")?;
    send.write_all(&req_bytes).await.context("write req")?;
    send.shutdown().await.context("shutdown send")?;

    let status = recv.read_u8().await.context("read status")?;
    if status != 0 {
        let err_len = recv.read_u32_le().await.context("read err len")?;
        let mut err_bytes = vec![0u8; err_len as usize];
        recv.read_exact(&mut err_bytes).await.context("read err")?;
        let msg = String::from_utf8_lossy(&err_bytes).into_owned();
        emit_progress(0, 0, 0, DownloadStatus::Error, Some(msg.clone()));
        registry.remove(download_id).await;
        return Err(anyhow!(msg));
    }

    let file_size = recv.read_u64_le().await.context("read file size")?;
    emit_progress(0, file_size, 0, DownloadStatus::Active, None);

    if let Some(parent) = Path::new(save_path).parent() {
        tokio::fs::create_dir_all(parent).await.context("create dirs")?;
    }
    let out = tokio::fs::File::create(save_path).await.context("create output file")?;
    let mut writer = tokio::io::BufWriter::new(out);

    // Chunked copy with progress emission every ~250 KB, cancel check per chunk.
    const CHUNK: usize = 256 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut bytes_done: u64 = 0;
    let start = Instant::now();
    let mut last_emit = Instant::now();

    loop {
        // Cancel check
        if cancel_rx.try_recv().is_ok() {
            drop(writer);
            let _ = tokio::fs::remove_file(save_path).await;
            emit_progress(bytes_done, file_size, 0, DownloadStatus::Cancelled, None);
            registry.remove(download_id).await;
            conn.close(0u32.into(), b"cancelled");
            return Err(anyhow!("Download cancelled"));
        }

        let remaining = (file_size - bytes_done) as usize;
        if remaining == 0 { break; }
        let to_read = remaining.min(CHUNK);
        // Quinn-style read returns Option<usize> — None means stream ended.
        let n = match recv.read(&mut buf[..to_read]).await.context("read chunk")? {
            Some(n) => n,
            None => break,
        };
        if n == 0 { break; }

        writer.write_all(&buf[..n]).await.context("write chunk")?;
        bytes_done += n as u64;

        // Emit every 250 ms
        if last_emit.elapsed().as_millis() >= 250 {
            let elapsed_secs = start.elapsed().as_secs_f64().max(0.001);
            let bps = (bytes_done as f64 / elapsed_secs) as u64;
            emit_progress(bytes_done, file_size, bps, DownloadStatus::Active, None);
            last_emit = Instant::now();
        }
    }

    writer.flush().await.context("flush")?;

    if let Some(ref expected) = expected_sha256 {
        let written = tokio::fs::read(save_path).await.context("re-read for integrity check")?;
        if let Err(e) = verify_hash(&written, expected) {
            let _ = tokio::fs::remove_file(save_path).await;
            let msg = format!("Integrity check failed: {e}");
            emit_progress(bytes_done, file_size, 0, DownloadStatus::Error, Some(msg.clone()));
            registry.remove(download_id).await;
            conn.close(0u32.into(), b"hash-mismatch");
            return Err(anyhow!(msg));
        }
    }

    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    let bps = (bytes_done as f64 / elapsed) as u64;
    emit_progress(bytes_done, file_size, bps, DownloadStatus::Done, None);
    registry.remove(download_id).await;
    conn.close(0u32.into(), b"");
    Ok(bytes_done)
}

// ---------------------------------------------------------------------------
// Integrity helpers
// ---------------------------------------------------------------------------

pub(crate) fn sha256_bytes(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// Return `Ok(())` if `data` hashes to `expected_hex`; `Err` otherwise.
pub(crate) fn verify_hash(data: &[u8], expected_hex: &str) -> anyhow::Result<()> {
    let actual = sha256_bytes(data);
    anyhow::ensure!(
        actual == expected_hex,
        "SHA256 mismatch: got {actual}, expected {expected_hex}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_pattern_matches_extension() {
        assert!(is_allowed_path("Movies/Akira.mkv", &["**/*.mkv".to_string()]));
        assert!(is_allowed_path("Season 1/E01.mkv", &["Season 1/**".to_string()]));
        assert!(!is_allowed_path("Season 2/E01.mkv", &["Season 1/**".to_string()]));
        assert!(is_allowed_path("anything", &[]));
    }

    #[test]
    fn prefix_matching_still_works() {
        assert!(is_allowed_path("Movies/foo.mp4", &["Movies/".to_string()]));
        assert!(!is_allowed_path("Other/foo.mp4", &["Movies/".to_string()]));
    }

    #[test]
    fn sha256_bytes_known_content() {
        use sha2::{Digest, Sha256};
        let expected = hex::encode(Sha256::digest(b"hello world"));
        assert_eq!(sha256_bytes(b"hello world"), expected);
    }

    #[test]
    fn sha256_bytes_empty() {
        use sha2::{Digest, Sha256};
        let expected = hex::encode(Sha256::digest(b""));
        assert_eq!(sha256_bytes(b""), expected);
    }

    #[test]
    fn verify_hash_accepts_matching_content() {
        let data = b"some file content";
        let hash = sha256_bytes(data);
        assert!(verify_hash(data, &hash).is_ok());
    }

    #[test]
    fn verify_hash_rejects_corrupted_content() {
        let hash = sha256_bytes(b"original");
        let result = verify_hash(b"corrupted", &hash);
        assert!(result.is_err(), "mismatched hash must return Err");
    }

    #[test]
    fn verify_hash_rejects_garbage_hex() {
        let result = verify_hash(b"data", "not-even-hex");
        assert!(result.is_err());
    }

    // ── H2: verify_peer_identity ──────────────────────────────────────────────

    #[test]
    fn verify_peer_id_accepts_matching_identity() {
        use std::collections::BTreeSet;
        let kp = hb_core::HoardbookKeypair::generate();
        let bytes = hb_core::hb_id_decode(&kp.hb_id()).unwrap();
        let id = iroh::EndpointId::from_bytes(&bytes).unwrap();
        let peer_addr = EndpointAddr { id, addrs: BTreeSet::new() };
        assert!(verify_peer_identity(&peer_addr, &kp.hb_id()).is_ok());
    }

    #[test]
    fn verify_peer_id_rejects_mismatched_identity() {
        use std::collections::BTreeSet;
        let kp    = hb_core::HoardbookKeypair::generate();
        let other = hb_core::HoardbookKeypair::generate();
        let bytes = hb_core::hb_id_decode(&kp.hb_id()).unwrap();
        let id = iroh::EndpointId::from_bytes(&bytes).unwrap();
        let peer_addr = EndpointAddr { id, addrs: BTreeSet::new() };
        // peer_addr.id is kp's key, but we claim it belongs to other — must reject.
        let err = verify_peer_identity(&peer_addr, &other.hb_id()).unwrap_err();
        assert!(err.to_string().contains("does not match"), "got: {err}");
    }

    // ── handle_xfer_stream integration tests ─────────────────────────────────
    // Use tokio::io::duplex() to exercise the full framing without real QUIC.

    #[tokio::test]
    async fn xfer_stream_invalid_slug_rejected() {
        use crate::store::DataStore;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let registry = std::sync::Arc::new(DownloadRegistry::new());

        let (srv, cli) = tokio::io::duplex(64 * 1024);
        let (mut cli_r, mut cli_w) = tokio::io::split(cli);
        let (srv_r, srv_w) = tokio::io::split(srv);
        tokio::spawn(async move {
            let _ = handle_xfer_stream(srv_w, srv_r, "any-id", &store, &registry).await;
        });

        let req = serde_json::json!({"slug": "../etc/passwd", "path": "file.txt"});
        let req_b = serde_json::to_vec(&req).unwrap();
        cli_w.write_u32_le(req_b.len() as u32).await.unwrap();
        cli_w.write_all(&req_b).await.unwrap();
        cli_w.shutdown().await.unwrap();

        let status = cli_r.read_u8().await.unwrap();
        assert_eq!(status, 1);
        let elen = cli_r.read_u32_le().await.unwrap();
        let mut ebuf = vec![0u8; elen as usize];
        cli_r.read_exact(&mut ebuf).await.unwrap();
        let msg = String::from_utf8(ebuf).unwrap();
        assert!(msg.contains("Invalid collection slug"), "got: {msg}");
    }

    #[tokio::test]
    async fn xfer_stream_require_follow_blocks_stranger() {
        use crate::store::{DataStore, ShareSettings};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store.save_share_settings("col", &ShareSettings {
            enabled: true, require_follow: true,
            root_path: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        }).unwrap();

        let stranger = hb_core::HoardbookKeypair::generate();
        let remote_id = stranger.hb_id();

        let registry = std::sync::Arc::new(DownloadRegistry::new());
        let (srv, cli) = tokio::io::duplex(64 * 1024);
        let (mut cli_r, mut cli_w) = tokio::io::split(cli);
        let (srv_r, srv_w) = tokio::io::split(srv);
        let s2 = store.clone(); let id2 = remote_id.clone();
        tokio::spawn(async move {
            let _ = handle_xfer_stream(srv_w, srv_r, &id2, &s2, &registry).await;
        });

        let req_b = serde_json::to_vec(&serde_json::json!({"slug":"col","path":"f.txt"})).unwrap();
        cli_w.write_u32_le(req_b.len() as u32).await.unwrap();
        cli_w.write_all(&req_b).await.unwrap();
        cli_w.shutdown().await.unwrap();

        let status = cli_r.read_u8().await.unwrap();
        assert_eq!(status, 1);
        let elen = cli_r.read_u32_le().await.unwrap();
        let mut ebuf = vec![0u8; elen as usize];
        cli_r.read_exact(&mut ebuf).await.unwrap();
        let msg = String::from_utf8(ebuf).unwrap();
        assert!(msg.contains("restricted to followers"), "got: {msg}");
    }

    #[tokio::test]
    async fn xfer_stream_require_follow_allows_contact() {
        use crate::store::{CachedPeer, DataStore, ShareSettings};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        store.save_share_settings("col", &ShareSettings {
            enabled: true, require_follow: true,
            root_path: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        }).unwrap();

        let peer_kp = hb_core::HoardbookKeypair::generate();
        let remote_id = peer_kp.hb_id();
        store.save_contact(
            &CachedPeer::pubkey_hash(&remote_id),
            &CachedPeer {
                hb_id: remote_id.clone(), profile: None, collections: vec![],
                online: false, node_addr: None, last_fetched: chrono::Utc::now(),
                last_seen_at: None, local_tags: vec![],
            },
        ).unwrap();

        let registry = std::sync::Arc::new(DownloadRegistry::new());
        let (srv, cli) = tokio::io::duplex(64 * 1024);
        let (mut cli_r, mut cli_w) = tokio::io::split(cli);
        let (srv_r, srv_w) = tokio::io::split(srv);
        let s2 = store.clone(); let id2 = remote_id.clone();
        tokio::spawn(async move {
            let _ = handle_xfer_stream(srv_w, srv_r, &id2, &s2, &registry).await;
        });

        let req_b = serde_json::to_vec(&serde_json::json!({"slug":"col","path":"f.txt"})).unwrap();
        cli_w.write_u32_le(req_b.len() as u32).await.unwrap();
        cli_w.write_all(&req_b).await.unwrap();
        cli_w.shutdown().await.unwrap();

        let status = cli_r.read_u8().await.unwrap();
        assert_eq!(status, 1, "expect a non-follower error, not success");
        let elen = cli_r.read_u32_le().await.unwrap();
        let mut ebuf = vec![0u8; elen as usize];
        cli_r.read_exact(&mut ebuf).await.unwrap();
        let msg = String::from_utf8(ebuf).unwrap();
        // Contact passed require_follow; next failure is "File not found", not the follower gate.
        assert!(!msg.contains("restricted to followers"),
            "contact must pass require_follow check; got: {msg}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn xfer_stream_symlink_escape_rejected() {
        use std::os::unix::fs::symlink;
        use crate::store::{DataStore, ShareSettings};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let outer = tempfile::tempdir().unwrap(); // outside the share root
        let root  = tempfile::tempdir().unwrap(); // share root
        let db    = tempfile::tempdir().unwrap(); // DataStore base

        let secret = outer.path().join("secret.txt");
        std::fs::write(&secret, b"secret content").unwrap();
        // Symlink inside root pointing outside
        symlink(&secret, root.path().join("escape.txt")).unwrap();

        let store = DataStore::new(db.path().to_path_buf());
        store.save_share_settings("col", &ShareSettings {
            enabled: true,
            root_path: Some(root.path().to_str().unwrap().to_string()),
            ..Default::default()
        }).unwrap();

        let registry = std::sync::Arc::new(DownloadRegistry::new());
        let (srv, cli) = tokio::io::duplex(64 * 1024);
        let (mut cli_r, mut cli_w) = tokio::io::split(cli);
        let (srv_r, srv_w) = tokio::io::split(srv);
        tokio::spawn(async move {
            let _ = handle_xfer_stream(srv_w, srv_r, "any-id", &store, &registry).await;
        });

        let req_b = serde_json::to_vec(
            &serde_json::json!({"slug":"col","path":"escape.txt"})
        ).unwrap();
        cli_w.write_u32_le(req_b.len() as u32).await.unwrap();
        cli_w.write_all(&req_b).await.unwrap();
        cli_w.shutdown().await.unwrap();

        let status = cli_r.read_u8().await.unwrap();
        assert_eq!(status, 1, "expect error for symlink escape");
        let elen = cli_r.read_u32_le().await.unwrap();
        let mut ebuf = vec![0u8; elen as usize];
        cli_r.read_exact(&mut ebuf).await.unwrap();
        let msg = String::from_utf8(ebuf).unwrap();
        assert!(msg.contains("Invalid file path"), "symlink escape must be rejected; got: {msg}");
    }
}
