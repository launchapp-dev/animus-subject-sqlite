//! [`SqliteBackend`] — the `SubjectBackend` implementation for local
//! SQLite-owned subjects.
//!
//! Unlike upstream-API backends (Linear, Jira, GitHub Issues), this backend
//! IS the system of record. Created subjects, status changes, and assignee
//! updates are committed directly to the local database with no upstream
//! roundtrip. The [`SqliteBackend::watch`] stream is fed by an in-process
//! `tokio::sync::broadcast` channel so subscribers see every write.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use animus_plugin_protocol::{HealthCheckResult, HealthStatus};
use animus_subject_protocol::{
    BackendError, ChangeKind, CustomFieldKind, CustomFieldSpec, EventStream, Subject,
    SubjectBackend, SubjectChangedEvent, SubjectFilter, SubjectId, SubjectList, SubjectPatch,
    SubjectSchema, SubjectStatus,
};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::config::SqliteConfig;
use crate::id_gen;
use crate::migrations;
use crate::store::SqliteStore;

/// Capacity of the in-process `tokio::sync::broadcast` channel that backs
/// [`SqliteBackend::watch`]. Watchers that fall more than this many events
/// behind will lag and see a `BroadcastStreamRecvError::Lagged(_)` they get
/// silently dropped (see [`watch`](SqliteBackend::watch)).
const WATCH_CHANNEL_CAPACITY: usize = 256;

/// SQLite-backed subject backend plugin state.
#[derive(Debug, Clone)]
pub struct SqliteBackend {
    store: SqliteStore,
    config: SqliteConfig,
    events: Arc<broadcast::Sender<SubjectChangedEvent>>,
}

impl SqliteBackend {
    /// Open the configured SQLite database, run pending migrations (if
    /// [`SqliteConfig::auto_migrate`]), and return a ready backend.
    pub async fn new(config: SqliteConfig) -> anyhow::Result<Self> {
        if let Some(parent) = config.db_path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
        }

        let connect_options = SqliteConnectOptions::new()
            .filename(&config.db_path)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(connect_options)
            .await?;

        if config.auto_migrate {
            migrations::apply(&pool).await?;
        }

        Ok(Self::from_pool(pool, config))
    }

    /// Build a backend on top of an already-opened pool. Used by tests that
    /// want a `sqlite::memory:` pool without a filesystem dance.
    pub fn from_pool(pool: SqlitePool, config: SqliteConfig) -> Self {
        let (tx, _rx) = broadcast::channel(WATCH_CHANNEL_CAPACITY);
        Self {
            store: SqliteStore::new(pool),
            config,
            events: Arc::new(tx),
        }
    }

    /// Borrow the configured kinds list. Useful in tests + introspection.
    pub fn served_kinds(&self) -> &[String] {
        &self.config.kinds
    }

    /// Insert a brand-new subject. The caller may pre-fill an id (typically
    /// a `sqlite:<ULID>` produced by [`id_gen::new_subject_id`]); if the
    /// supplied id is empty or doesn't carry the `sqlite:` prefix we
    /// generate a fresh one.
    pub async fn create(&self, mut subject: Subject) -> Result<Subject, BackendError> {
        if !self.config.accepts_kind(&subject.kind) {
            return Err(BackendError::InvalidRequest(format!(
                "kind {:?} is not in this backend's served set {:?}",
                subject.kind, self.config.kinds
            )));
        }

        if subject.id.as_str().is_empty() || !id_gen::is_sqlite_id(&subject.id) {
            subject.id = id_gen::new_subject_id();
        }

        let now = Utc::now();
        // Honor caller-supplied timestamps when present, otherwise stamp.
        if subject.created_at.timestamp() == 0 {
            subject.created_at = now;
        }
        if subject.updated_at.timestamp() == 0 {
            subject.updated_at = now;
        }

        let stored = self.store.insert(&subject).await?;
        self.broadcast(ChangeKind::Created, stored.clone(), None, None);
        Ok(stored)
    }

    fn broadcast(
        &self,
        change_kind: ChangeKind,
        subject: Subject,
        previous_native_status: Option<String>,
        previous_dispatch_label: Option<String>,
    ) {
        let event = SubjectChangedEvent {
            id: subject.id.clone(),
            change_kind,
            subject,
            previous_native_status,
            previous_dispatch_label,
        };
        // `send` only errors when there are zero subscribers; that's a
        // normal state (the daemon may have no watchers attached), so we
        // intentionally ignore the result.
        let _ = self.events.send(event);
    }
}

#[async_trait]
impl SubjectBackend for SqliteBackend {
    async fn list(&self, filter: SubjectFilter) -> Result<SubjectList, BackendError> {
        let (subjects, next_cursor) = self.store.list(&filter).await?;
        Ok(SubjectList {
            subjects,
            next_cursor,
            fetched_at: Utc::now(),
        })
    }

    async fn get(&self, id: &SubjectId) -> Result<Subject, BackendError> {
        if !id_gen::is_sqlite_id(id) {
            return Err(BackendError::InvalidRequest(format!(
                "subject id {id:?} is not a sqlite id (expected `sqlite:<id>` prefix)"
            )));
        }
        self.store.get(id).await
    }

    async fn update(&self, id: &SubjectId, patch: SubjectPatch) -> Result<Subject, BackendError> {
        if !id_gen::is_sqlite_id(id) {
            return Err(BackendError::InvalidRequest(format!(
                "subject id {id:?} is not a sqlite id (expected `sqlite:<id>` prefix)"
            )));
        }

        // Read-modify-write. The pool only ever connects to a single file
        // on a single host, so contention is local; we don't bother with
        // optimistic concurrency tokens at v0.1.0.
        let current = self.store.get(id).await?;
        let previous_native_status = current.native_status.clone();
        let new_status = patch.status.unwrap_or(current.status);

        // Status changed? Track it explicitly so we can emit the right
        // ChangeKind below.
        let status_changed = patch.status.is_some() && patch.status != Some(current.status);

        let new_assignee: Option<String> = match patch.assignee {
            Some(Some(a)) => Some(a),
            Some(None) => None,
            None => current.assignee.clone(),
        };

        let mut new_labels = current.labels.clone();
        new_labels.retain(|l| !patch.labels_remove.contains(l));
        for label in &patch.labels_add {
            if !new_labels.contains(label) {
                new_labels.push(label.clone());
            }
        }

        // `comment` v0.1.0 semantics: replace the body. Backends like
        // Linear treat `comment` as an activity-log entry; sqlite has no
        // separate comment stream, so the closest equivalent is updating
        // the description field.
        let new_body = match &patch.comment {
            Some(text) => Some(text.clone()),
            None => current.description.clone(),
        };

        // Merge custom fields: a `null` value clears the key, anything
        // else upserts.
        let mut new_custom: BTreeMap<String, Value> = current.custom.clone();
        let mut new_native_status = current.native_status.clone();
        for (key, value) in &patch.custom {
            match key.as_str() {
                "native_status" => {
                    new_native_status = match value {
                        Value::Null => None,
                        Value::String(s) => Some(s.clone()),
                        other => Some(other.to_string()),
                    };
                }
                _ => {
                    if value.is_null() {
                        new_custom.remove(key);
                    } else {
                        new_custom.insert(key.clone(), value.clone());
                    }
                }
            }
        }

        let updated = self
            .store
            .update_row(
                id,
                new_status,
                new_assignee.as_deref(),
                &new_labels,
                new_body.as_deref(),
                &new_custom,
                new_native_status.as_deref(),
                Utc::now(),
            )
            .await?;

        let change_kind = if status_changed {
            ChangeKind::StatusChanged
        } else {
            ChangeKind::Updated
        };
        self.broadcast(change_kind, updated.clone(), previous_native_status, None);
        Ok(updated)
    }

    async fn watch(&self) -> Option<EventStream> {
        let rx = self.events.subscribe();
        let stream = BroadcastStream::new(rx).filter_map(|item| async move {
            // `BroadcastStreamRecvError::Lagged` events are dropped — the
            // daemon treats lag as a recoverable hint and will re-list on
            // the next tick. Errors-shaped-as-Ok-events would surprise
            // upstream consumers.
            item.ok()
        });
        Some(Box::pin(stream) as Pin<Box<_>>)
    }

    fn schema(&self) -> SubjectSchema {
        SubjectSchema {
            kinds: self.config.kinds.clone(),
            status_values: vec![
                SubjectStatus::Ready,
                SubjectStatus::InProgress,
                SubjectStatus::Blocked,
                SubjectStatus::Done,
                SubjectStatus::Cancelled,
            ],
            supports_watch: true,
            supports_create: true,
            supports_pagination: true,
            native_status_values: vec![],
            status_dispatch_hints: vec![],
            custom_fields: vec![
                CustomFieldSpec {
                    key: "priority".to_string(),
                    kind: CustomFieldKind::Number,
                    values: None,
                },
                CustomFieldSpec {
                    key: "native_status".to_string(),
                    kind: CustomFieldKind::String,
                    values: None,
                },
            ],
        }
    }

    async fn health(&self) -> Result<HealthCheckResult, BackendError> {
        match self.store.ping().await {
            Ok(()) => Ok(HealthCheckResult {
                status: HealthStatus::Healthy,
                uptime_ms: None,
                memory_usage_bytes: None,
                last_error: None,
            }),
            Err(e) => Ok(HealthCheckResult {
                status: HealthStatus::Unhealthy,
                uptime_ms: None,
                memory_usage_bytes: None,
                last_error: Some(e.to_string()),
            }),
        }
    }
}
