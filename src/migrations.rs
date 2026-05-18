//! Embedded migration runner.
//!
//! Migrations ship as inline SQL strings (one per `&str` in [`MIGRATIONS`]).
//! On startup the runner inspects a `schema_version` table — creating it on
//! first launch — and applies any migration whose ordinal exceeds the
//! recorded version. Migrations are applied inside a transaction so a
//! mid-flight failure leaves the database at its prior version.

use anyhow::{Context, Result};
use sqlx::sqlite::SqlitePool;
use sqlx::Executor;

/// Ordered list of migration SQL bodies. Index `i` is migration version
/// `i + 1`. Always append; never edit a migration that has shipped.
pub const MIGRATIONS: &[&str] = &[include_str!("../migrations/001_initial_schema.sql")];

/// Apply every migration whose ordinal is newer than the recorded
/// `schema_version`. No-op if the database is already current.
pub async fn apply(pool: &SqlitePool) -> Result<()> {
    // The version table lives outside [`MIGRATIONS`] so the runner can
    // bootstrap a fresh database without a chicken-and-egg dependency.
    pool.execute(
        r#"
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )
    .await
    .context("creating schema_version table")?;

    let current: Option<i64> = sqlx::query_scalar("SELECT MAX(version) FROM schema_version")
        .fetch_one(pool)
        .await
        .context("reading current schema version")?;
    let current = current.unwrap_or(0) as usize;

    for (idx, body) in MIGRATIONS.iter().enumerate() {
        let version = (idx + 1) as i64;
        if (version as usize) <= current {
            continue;
        }

        let mut tx = pool
            .begin()
            .await
            .with_context(|| format!("beginning migration {version}"))?;

        tx.execute(*body)
            .await
            .with_context(|| format!("executing migration {version}"))?;

        sqlx::query("INSERT INTO schema_version (version) VALUES (?)")
            .bind(version)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("recording migration {version}"))?;

        tx.commit()
            .await
            .with_context(|| format!("committing migration {version}"))?;

        tracing::info!(
            target: "animus_subject_sqlite",
            version,
            "applied migration"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn apply_is_idempotent() {
        let pool = pool().await;
        apply(&pool).await.unwrap();
        // Second apply must not error or double-insert.
        apply(&pool).await.unwrap();

        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }

    #[tokio::test]
    async fn apply_creates_subjects_table() {
        let pool = pool().await;
        apply(&pool).await.unwrap();

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'subjects'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "subjects table should exist after migration");
    }
}
