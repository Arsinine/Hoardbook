use anyhow::Result;
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const EXPIRY_DAYS: i64 = 30;
const SECS_PER_DAY: i64 = 86_400;

// ---------------------------------------------------------------------------
// Connect & migrate
// ---------------------------------------------------------------------------

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true);

    let pool = SqlitePool::connect_with(opts).await?;
    Ok(pool)
}

pub async fn migrate(pool: &SqlitePool) -> Result<()> {
    // Drop legacy tables that are no longer part of the relay's remit.
    // The relay no longer caches profiles or collections — those are served
    // peer-to-peer by each node's iroh endpoint.
    let _ = sqlx::query("DROP TABLE IF EXISTS documents").execute(pool).await;
    let _ = sqlx::query("DROP TABLE IF EXISTS collections").execute(pool).await;
    let _ = sqlx::query("DROP TABLE IF EXISTS channel_messages").execute(pool).await;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS heartbeats (
            pubkey    TEXT    PRIMARY KEY,
            last_seen INTEGER NOT NULL,
            node_addr TEXT
        );

        CREATE TABLE IF NOT EXISTS messages (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            from_key   TEXT    NOT NULL,
            to_key     TEXT    NOT NULL,
            envelope   TEXT    NOT NULL,
            sent_at    TEXT    NOT NULL,
            stored_at  INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            UNIQUE(from_key, sent_at)
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_messages_to ON messages(to_key, sent_at DESC)",
    )
    .execute(pool)
    .await?;

    // Legacy column cleanup — safe to ignore if already absent.
    let _ = sqlx::query("ALTER TABLE heartbeats DROP COLUMN listed").execute(pool).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn expiry_secs() -> i64 {
    now_secs() + EXPIRY_DAYS * SECS_PER_DAY
}

// ---------------------------------------------------------------------------
// Heartbeats
// ---------------------------------------------------------------------------

pub async fn upsert_heartbeat(
    pool: &SqlitePool,
    pubkey: &str,
    node_addr: Option<&str>,
) -> Result<()> {
    let now = now_secs();

    sqlx::query(
        r#"
        INSERT INTO heartbeats (pubkey, last_seen, node_addr)
        VALUES (?, ?, ?)
        ON CONFLICT(pubkey) DO UPDATE SET
            last_seen = excluded.last_seen,
            node_addr = excluded.node_addr
        "#,
    )
    .bind(pubkey)
    .bind(now)
    .bind(node_addr)
    .execute(pool)
    .await?;

    Ok(())
}

/// Returns `(last_seen_unix, node_addr)` or `None` if no heartbeat exists.
pub async fn get_heartbeat(
    pool: &SqlitePool,
    pubkey: &str,
) -> Result<Option<(i64, Option<String>)>> {
    let row: Option<(i64, Option<String>)> =
        sqlx::query_as("SELECT last_seen, node_addr FROM heartbeats WHERE pubkey = ?")
            .bind(pubkey)
            .fetch_optional(pool)
            .await?;

    Ok(row)
}

// ---------------------------------------------------------------------------
// DM Messages
// ---------------------------------------------------------------------------

pub const MAX_MESSAGES_PER_RECIPIENT: i64 = 500;
/// Per-(sender, recipient) cap — stops one sender from monopolizing a recipient's
/// mailbox under the global cap (M6).
pub const MAX_MESSAGES_PER_PAIR: i64 = 50;
/// Per-sender cap across all recipients — stops one sender from flooding many
/// distinct mailboxes (M6).
pub const MAX_MESSAGES_PER_SENDER: i64 = 200;

pub async fn insert_message(
    pool: &SqlitePool,
    from_key: &str,
    to_key: &str,
    sent_at: &str,
    envelope_json: &str,
) -> Result<()> {
    let now = now_secs();
    let expires = expiry_secs();

    sqlx::query(
        r#"
        INSERT OR IGNORE INTO messages (from_key, to_key, envelope, sent_at, stored_at, expires_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(from_key)
    .bind(to_key)
    .bind(envelope_json)
    .bind(sent_at)
    .bind(now)
    .bind(expires)
    .execute(pool)
    .await?;

    Ok(())
}

/// Counts non-expired messages addressed to `to_key`.
pub async fn count_messages_for(pool: &SqlitePool, to_key: &str) -> Result<i64> {
    let now = now_secs();
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM messages WHERE to_key = ? AND expires_at > ?",
    )
    .bind(to_key)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Counts non-expired messages from `from_key` to `to_key` (per-pair cap, M6).
pub async fn count_messages_from_to(pool: &SqlitePool, from_key: &str, to_key: &str) -> Result<i64> {
    let now = now_secs();
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM messages WHERE from_key = ? AND to_key = ? AND expires_at > ?",
    )
    .bind(from_key)
    .bind(to_key)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Counts non-expired messages sent by `from_key` to any recipient (per-sender cap, M6).
pub async fn count_messages_from(pool: &SqlitePool, from_key: &str) -> Result<i64> {
    let now = now_secs();
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM messages WHERE from_key = ? AND expires_at > ?",
    )
    .bind(from_key)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Returns the 100 most recent non-expired messages for `to_key`, oldest first.
pub async fn get_messages_for(pool: &SqlitePool, to_key: &str) -> Result<Vec<String>> {
    let now = now_secs();
    let rows: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT envelope FROM (
            SELECT envelope, sent_at FROM messages
            WHERE to_key = ? AND expires_at > ?
            ORDER BY sent_at DESC
            LIMIT 100
        ) ORDER BY sent_at ASC
        "#,
    )
    .bind(to_key)
    .bind(now)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(env,)| env).collect())
}

// ---------------------------------------------------------------------------
// Expiry
// ---------------------------------------------------------------------------

/// Delete expired messages. Heartbeat rows never expire. Run hourly.
pub async fn expire_messages(pool: &SqlitePool) -> Result<()> {
    let now = now_secs();
    let msgs = sqlx::query("DELETE FROM messages WHERE expires_at <= ?")
        .bind(now)
        .execute(pool)
        .await?
        .rows_affected();

    if msgs > 0 {
        tracing::info!("expired {msgs} messages");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Returns the count of distinct peers the relay has seen heartbeat from.
pub async fn count_stored_peers(pool: &SqlitePool) -> Result<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM heartbeats").fetch_one(pool).await?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn in_memory_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn migration_idempotent() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        // Run twice — must not error on second call.
        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap();

        // Verify the expected tables exist.
        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let names: Vec<&str> = tables.iter().map(|(n,)| n.as_str()).collect();
        assert!(names.contains(&"heartbeats"), "heartbeats table must exist");
        assert!(names.contains(&"messages"), "messages table must exist");
        assert!(!names.contains(&"documents"), "documents table must be absent");
        assert!(!names.contains(&"collections"), "collections table must be absent");
    }

    #[tokio::test]
    async fn message_dedup_silently_ignored() {
        let pool = in_memory_pool().await;
        insert_message(&pool, "alice", "bob", "2026-04-22T12:00:00Z", "envelope_v1")
            .await
            .unwrap();
        insert_message(&pool, "alice", "bob", "2026-04-22T12:00:00Z", "envelope_v2")
            .await
            .unwrap();

        let msgs = get_messages_for(&pool, "bob").await.unwrap();
        assert_eq!(msgs.len(), 1, "duplicate (from, sent_at) must be silently dropped");
        assert_eq!(msgs[0], "envelope_v1", "first write wins");
    }

    #[tokio::test]
    async fn expire_removes_old_messages() {
        let pool = in_memory_pool().await;
        // Insert a message that is already past its expiry by manipulating expires_at directly.
        sqlx::query(
            "INSERT INTO messages (from_key, to_key, envelope, sent_at, stored_at, expires_at) \
             VALUES ('a', 'b', 'env', '2020-01-01T00:00:00Z', 0, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Also insert a fresh message.
        insert_message(&pool, "a", "b", "2026-06-01T00:00:00Z", "fresh")
            .await
            .unwrap();

        expire_messages(&pool).await.unwrap();

        let msgs = get_messages_for(&pool, "b").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "fresh");
    }

    #[tokio::test]
    async fn heartbeat_not_expired() {
        let pool = in_memory_pool().await;
        upsert_heartbeat(&pool, "pk1", Some("addr1")).await.unwrap();

        expire_messages(&pool).await.unwrap();

        let hb = get_heartbeat(&pool, "pk1").await.unwrap();
        assert!(hb.is_some(), "heartbeat must survive expire_messages");
    }

    #[tokio::test]
    async fn count_messages_non_expired_only() {
        let pool = in_memory_pool().await;
        // One expired message.
        sqlx::query(
            "INSERT INTO messages (from_key, to_key, envelope, sent_at, stored_at, expires_at) \
             VALUES ('a', 'b', 'old', '2020-01-01T00:00:00Z', 0, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // One fresh message.
        insert_message(&pool, "a", "b", "2026-06-01T00:00:00Z", "fresh")
            .await
            .unwrap();

        let count = count_messages_for(&pool, "b").await.unwrap();
        assert_eq!(count, 1, "count must exclude expired messages");
    }

    #[tokio::test]
    async fn heartbeat_and_message_roundtrip() {
        let pool = in_memory_pool().await;

        upsert_heartbeat(&pool, "pk1", Some("iroh://addr1")).await.unwrap();
        insert_message(&pool, "pk1", "pk2", "2026-06-01T00:00:00Z", "hello")
            .await
            .unwrap();

        let hb = get_heartbeat(&pool, "pk1").await.unwrap().unwrap();
        assert_eq!(hb.1.as_deref(), Some("iroh://addr1"));

        let msgs = get_messages_for(&pool, "pk2").await.unwrap();
        assert_eq!(msgs, ["hello"]);
    }

    #[tokio::test]
    async fn insert_message_deduplicates_same_sender_and_timestamp() {
        let pool = in_memory_pool().await;
        insert_message(&pool, "alice", "bob", "2026-04-22T12:00:00Z", "envelope_v1")
            .await
            .unwrap();
        insert_message(&pool, "alice", "bob", "2026-04-22T12:00:00Z", "envelope_v2")
            .await
            .unwrap();

        let msgs = get_messages_for(&pool, "bob").await.unwrap();
        assert_eq!(msgs.len(), 1, "duplicate (from, sent_at) must be silently dropped");
        assert_eq!(msgs[0], "envelope_v1", "first write wins");
    }

    #[tokio::test]
    async fn get_messages_returns_chronological_order() {
        let pool = in_memory_pool().await;
        insert_message(&pool, "s", "me", "2026-04-22T12:00:02Z", "third").await.unwrap();
        insert_message(&pool, "s", "me", "2026-04-22T12:00:00Z", "first").await.unwrap();
        insert_message(&pool, "s", "me", "2026-04-22T12:00:01Z", "second").await.unwrap();

        let msgs = get_messages_for(&pool, "me").await.unwrap();
        assert_eq!(msgs, ["first", "second", "third"]);
    }

    #[tokio::test]
    async fn get_messages_caps_at_100_most_recent() {
        let pool = in_memory_pool().await;
        for i in 0u32..150 {
            let sent_at = format!(
                "2026-04-22T{:02}:{:02}:{:02}Z",
                i / 3600,
                (i / 60) % 60,
                i % 60
            );
            insert_message(&pool, "s", "r", &sent_at, &format!("env{i}"))
                .await
                .unwrap();
        }
        let msgs = get_messages_for(&pool, "r").await.unwrap();
        assert_eq!(msgs.len(), 100, "must cap at 100");
        assert_eq!(msgs[0], "env50", "should start from the 51st message (0-indexed)");
        assert_eq!(msgs[99], "env149", "should end with the newest");
    }

    #[tokio::test]
    async fn count_messages_for_reflects_actual_count() {
        let pool = in_memory_pool().await;
        assert_eq!(count_messages_for(&pool, "bob").await.unwrap(), 0);

        insert_message(&pool, "alice", "bob", "2026-04-22T12:00:00Z", "e1")
            .await
            .unwrap();
        insert_message(&pool, "carol", "bob", "2026-04-22T12:00:01Z", "e2")
            .await
            .unwrap();

        assert_eq!(count_messages_for(&pool, "bob").await.unwrap(), 2);
        assert_eq!(count_messages_for(&pool, "alice").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn mailbox_cap_constant_matches_handler_expectation() {
        assert_eq!(MAX_MESSAGES_PER_RECIPIENT, 500);
    }

    #[tokio::test]
    async fn upsert_heartbeat_stores_and_retrieves() {
        let pool = in_memory_pool().await;
        upsert_heartbeat(&pool, "pk1", None).await.unwrap();
        upsert_heartbeat(&pool, "pk2", Some("addr_b")).await.unwrap();
        let hb = get_heartbeat(&pool, "pk1").await.unwrap();
        assert!(hb.is_some());
        let hb2 = get_heartbeat(&pool, "pk2").await.unwrap();
        assert_eq!(hb2.unwrap().1.as_deref(), Some("addr_b"));
    }
}
