//! Low-level CRUD against the SQLite database.
//!
//! [`SqliteStore`] is the only place that talks SQL. The public
//! [`crate::backend::SqliteBackend`] sits one layer above it and translates
//! between `Subject` / `SubjectPatch` / `SubjectFilter` and the database's
//! wide-row representation.

use std::collections::BTreeMap;

use animus_subject_protocol::{
    BackendError, Subject, SubjectAttachment, SubjectFilter, SubjectId, SubjectStatus,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;

/// Hard upper bound on the page size a single `list` call returns.
/// Backends are free to clamp `SubjectFilter::limit`; we pick 500 as a
/// safety net so a misbehaving caller can't tilt the daemon by requesting
/// a million-row page.
pub const MAX_LIMIT: u32 = 500;

/// Default page size when [`SubjectFilter::limit`] is unset.
pub const DEFAULT_LIMIT: u32 = 50;

/// Thin wrapper around an `SqlitePool` exposing the row-level operations
/// the backend needs.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Wrap an already-opened pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool (used by `health()` for a connectivity ping).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Insert a subject. Returns the stored row re-read from the database so
    /// the caller observes the same shape it would see from a later `get`.
    pub async fn insert(&self, subject: &Subject) -> Result<Subject, BackendError> {
        let labels =
            serde_json::to_string(&subject.labels).map_err(|e| BackendError::Other(e.into()))?;
        let custom =
            serde_json::to_string(&subject.custom).map_err(|e| BackendError::Other(e.into()))?;
        let status_metadata = if subject.status_metadata.is_null() {
            None
        } else {
            Some(
                serde_json::to_string(&subject.status_metadata)
                    .map_err(|e| BackendError::Other(e.into()))?,
            )
        };
        let attachments = if subject.attachments.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&subject.attachments)
                    .map_err(|e| BackendError::Other(e.into()))?,
            )
        };

        sqlx::query(
            r#"
            INSERT INTO subjects (
                id, kind, title, body, status, priority, assignee,
                labels, parent_id, custom_fields, native_status,
                status_metadata, attachments, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(subject.id.as_str())
        .bind(&subject.kind)
        .bind(&subject.title)
        .bind(subject.description.as_deref())
        .bind(status_to_str(subject.status))
        .bind(subject.priority.map(i64::from))
        .bind(subject.assignee.as_deref())
        .bind(&labels)
        .bind(subject.parent.as_ref().map(|p| p.as_str().to_string()))
        .bind(&custom)
        .bind(subject.native_status.as_deref())
        .bind(status_metadata)
        .bind(attachments)
        .bind(subject.created_at.to_rfc3339())
        .bind(subject.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_err)?;

        self.get(&subject.id).await
    }

    /// Fetch a subject by id.
    pub async fn get(&self, id: &SubjectId) -> Result<Subject, BackendError> {
        let row = sqlx::query(
            r#"
            SELECT id, kind, title, body, status, priority, assignee,
                   labels, parent_id, custom_fields, native_status,
                   status_metadata, attachments, created_at, updated_at
            FROM subjects
            WHERE id = ?
            "#,
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;

        match row {
            Some(row) => row_to_subject(&row),
            None => Err(BackendError::NotFound(id.to_string())),
        }
    }

    /// List subjects matching `filter`. Returns the page plus an optional
    /// opaque cursor (the last row's id) when more rows remain. Pagination
    /// is keyset on the `id` column, which is ULID-sortable.
    pub async fn list(
        &self,
        filter: &SubjectFilter,
    ) -> Result<(Vec<Subject>, Option<String>), BackendError> {
        let limit = filter.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as i64;
        // Fetch one extra row so we know whether there's a next page
        // without a separate `COUNT(*)` round trip.
        let probe_limit = limit + 1;

        let mut sql = String::from(
            r#"
            SELECT id, kind, title, body, status, priority, assignee,
                   labels, parent_id, custom_fields, native_status,
                   status_metadata, attachments, created_at, updated_at
            FROM subjects
            WHERE 1 = 1
            "#,
        );

        // Build bind groups inline so we keep param order in sync.
        let mut binds: Vec<BindValue> = Vec::new();

        if !filter.kind.is_empty() {
            sql.push_str(" AND kind IN (");
            for (i, kind) in filter.kind.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push('?');
                binds.push(BindValue::Text(kind.clone()));
            }
            sql.push(')');
        }

        if !filter.status.is_empty() {
            sql.push_str(" AND status IN (");
            for (i, status) in filter.status.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push('?');
                binds.push(BindValue::Text(status_to_str(*status).to_string()));
            }
            sql.push(')');
        }

        if !filter.assignee.is_empty() {
            sql.push_str(" AND assignee IN (");
            for (i, assignee) in filter.assignee.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push('?');
                binds.push(BindValue::Text(assignee.clone()));
            }
            sql.push(')');
        }

        if let Some(updated_since) = filter.updated_since {
            sql.push_str(" AND updated_at >= ?");
            binds.push(BindValue::Text(updated_since.to_rfc3339()));
        }

        if let Some(native_status) = &filter.native_status {
            sql.push_str(" AND native_status = ?");
            binds.push(BindValue::Text(native_status.clone()));
        }

        if let Some(cursor) = &filter.cursor {
            sql.push_str(" AND id > ?");
            binds.push(BindValue::Text(cursor.clone()));
        }

        sql.push_str(" ORDER BY id ASC LIMIT ?");
        binds.push(BindValue::Int(probe_limit));

        let mut query = sqlx::query(&sql);
        for bind in &binds {
            query = match bind {
                BindValue::Text(s) => query.bind(s),
                BindValue::Int(i) => query.bind(*i),
            };
        }

        let rows = query.fetch_all(&self.pool).await.map_err(map_sqlx_err)?;

        let has_more = rows.len() as i64 > limit;
        let take = rows.len().min(limit as usize);

        let mut subjects = Vec::with_capacity(take);
        for row in rows.iter().take(take) {
            let subject = row_to_subject(row)?;

            // Post-SQL filters for dimensions stored inside JSON columns or
            // requiring set-membership semantics we don't model in the
            // SELECT. These are deliberately last so the cursor still points
            // at the last SQL-visible row.
            if !filter.labels_any.is_empty()
                && !subject.labels.iter().any(|l| filter.labels_any.contains(l))
            {
                continue;
            }
            if !filter.labels_all.is_empty()
                && !filter
                    .labels_all
                    .iter()
                    .all(|need| subject.labels.contains(need))
            {
                continue;
            }
            if let Some(kind_needed) = &filter.has_attachment_kind {
                if !subject.attachments.iter().any(|a| &a.kind == kind_needed) {
                    continue;
                }
            }

            subjects.push(subject);
        }

        let next_cursor = if has_more {
            subjects.last().map(|s| s.id.as_str().to_string())
        } else {
            None
        };

        Ok((subjects, next_cursor))
    }

    /// Apply a row update. Returns the refreshed subject.
    ///
    /// Caller (the backend) is responsible for building the new field
    /// values from the existing row + patch; we just write them.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_row(
        &self,
        id: &SubjectId,
        status: SubjectStatus,
        assignee: Option<&str>,
        labels: &[String],
        body: Option<&str>,
        custom_fields: &BTreeMap<String, Value>,
        native_status: Option<&str>,
        updated_at: DateTime<Utc>,
    ) -> Result<Subject, BackendError> {
        let labels_json =
            serde_json::to_string(labels).map_err(|e| BackendError::Other(e.into()))?;
        let custom_json =
            serde_json::to_string(custom_fields).map_err(|e| BackendError::Other(e.into()))?;

        let rows = sqlx::query(
            r#"
            UPDATE subjects
            SET status = ?,
                assignee = ?,
                labels = ?,
                body = ?,
                custom_fields = ?,
                native_status = ?,
                updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(status_to_str(status))
        .bind(assignee)
        .bind(&labels_json)
        .bind(body)
        .bind(&custom_json)
        .bind(native_status)
        .bind(updated_at.to_rfc3339())
        .bind(id.as_str())
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();

        if rows == 0 {
            return Err(BackendError::NotFound(id.to_string()));
        }

        self.get(id).await
    }

    /// True if the database connection is usable. Driven by `health()`.
    pub async fn ping(&self) -> Result<(), BackendError> {
        sqlx::query("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }
}

enum BindValue {
    Text(String),
    Int(i64),
}

fn row_to_subject(row: &SqliteRow) -> Result<Subject, BackendError> {
    let id: String = row.try_get("id").map_err(map_sqlx_err)?;
    let kind: String = row.try_get("kind").map_err(map_sqlx_err)?;
    let title: String = row.try_get("title").map_err(map_sqlx_err)?;
    let description: Option<String> = row.try_get("body").map_err(map_sqlx_err)?;
    let status_str: String = row.try_get("status").map_err(map_sqlx_err)?;
    let priority: Option<i64> = row.try_get("priority").map_err(map_sqlx_err)?;
    let assignee: Option<String> = row.try_get("assignee").map_err(map_sqlx_err)?;
    let labels_raw: String = row.try_get("labels").map_err(map_sqlx_err)?;
    let parent: Option<String> = row.try_get("parent_id").map_err(map_sqlx_err)?;
    let custom_raw: String = row.try_get("custom_fields").map_err(map_sqlx_err)?;
    let native_status: Option<String> = row.try_get("native_status").map_err(map_sqlx_err)?;
    let status_meta_raw: Option<String> = row.try_get("status_metadata").map_err(map_sqlx_err)?;
    let attachments_raw: Option<String> = row.try_get("attachments").map_err(map_sqlx_err)?;
    let created_at_raw: String = row.try_get("created_at").map_err(map_sqlx_err)?;
    let updated_at_raw: String = row.try_get("updated_at").map_err(map_sqlx_err)?;

    let status = status_from_str(&status_str)?;
    let labels: Vec<String> = serde_json::from_str(&labels_raw)
        .map_err(|e| BackendError::Other(anyhow::anyhow!("invalid labels JSON: {e}")))?;
    let custom: BTreeMap<String, Value> = serde_json::from_str(&custom_raw)
        .map_err(|e| BackendError::Other(anyhow::anyhow!("invalid custom_fields JSON: {e}")))?;
    let status_metadata: Value = status_meta_raw
        .map(|raw| {
            serde_json::from_str(&raw).map_err(|e| {
                BackendError::Other(anyhow::anyhow!("invalid status_metadata JSON: {e}"))
            })
        })
        .transpose()?
        .unwrap_or(Value::Null);
    let attachments: Vec<SubjectAttachment> = attachments_raw
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|e| BackendError::Other(anyhow::anyhow!("invalid attachments JSON: {e}")))
        })
        .transpose()?
        .unwrap_or_default();

    let created_at = parse_ts(&created_at_raw)?;
    let updated_at = parse_ts(&updated_at_raw)?;

    Ok(Subject {
        id: SubjectId::new(id),
        kind,
        title,
        description,
        status,
        priority: priority.and_then(|n| u8::try_from(n).ok()),
        assignee,
        labels,
        parent: parent.map(SubjectId::new),
        children: vec![],
        url: None,
        created_at,
        updated_at,
        custom,
        native_status,
        status_metadata,
        attachments,
    })
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>, BackendError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| BackendError::Other(anyhow::anyhow!("invalid timestamp {raw:?}: {e}")))
}

/// Stable string form used in the `status` column. Kept hand-rolled rather
/// than serde-derived so a future rename of the wire-form serde tag can't
/// silently rewrite every database we ship.
pub(crate) fn status_to_str(status: SubjectStatus) -> &'static str {
    match status {
        SubjectStatus::Ready => "ready",
        SubjectStatus::InProgress => "in-progress",
        SubjectStatus::Blocked => "blocked",
        SubjectStatus::Done => "done",
        SubjectStatus::Cancelled => "cancelled",
    }
}

fn status_from_str(raw: &str) -> Result<SubjectStatus, BackendError> {
    match raw {
        "ready" => Ok(SubjectStatus::Ready),
        "in-progress" => Ok(SubjectStatus::InProgress),
        "blocked" => Ok(SubjectStatus::Blocked),
        "done" => Ok(SubjectStatus::Done),
        "cancelled" => Ok(SubjectStatus::Cancelled),
        other => Err(BackendError::Other(anyhow::anyhow!(
            "unrecognized status value {other:?} in database"
        ))),
    }
}

fn map_sqlx_err<E: std::fmt::Display>(error: E) -> BackendError {
    BackendError::Unavailable(error.to_string())
}
