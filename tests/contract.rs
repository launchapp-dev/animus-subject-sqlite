//! Contract tests for the SQLite `SubjectBackend` implementation.
//!
//! Each test stands up a fresh in-memory SQLite pool, runs migrations,
//! wires up a [`SqliteBackend`], and exercises one trait method end-to-end.
//! No network, no fixtures — sqlite IS the store, so the test bodies are
//! about as direct as they get.

use std::collections::BTreeMap;
use std::time::Duration;

use animus_plugin_protocol::HealthStatus;
use animus_subject_protocol::{
    Subject, SubjectBackend, SubjectFilter, SubjectId, SubjectPatch, SubjectStatus,
};
use animus_subject_sqlite::backend::SqliteBackend;
use animus_subject_sqlite::config::SqliteConfig;
use animus_subject_sqlite::migrations;
use chrono::Utc;
use futures::StreamExt;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tempfile::TempDir;

async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("sqlite memory pool");
    migrations::apply(&pool).await.expect("migrations");
    pool
}

async fn backend(kinds: Vec<&str>) -> SqliteBackend {
    let pool = memory_pool().await;
    let config =
        SqliteConfig::new("sqlite::memory:").with_kinds(kinds.into_iter().map(String::from));
    SqliteBackend::from_pool(pool, config)
}

fn sample_subject(kind: &str, title: &str) -> Subject {
    Subject {
        id: SubjectId::new(""),
        kind: kind.to_string(),
        title: title.to_string(),
        description: Some("body text".to_string()),
        status: SubjectStatus::Ready,
        priority: Some(2),
        assignee: Some("alice@example.com".to_string()),
        labels: vec!["backend".into(), "v0.1.0".into()],
        parent: None,
        children: vec![],
        url: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        custom: BTreeMap::new(),
        native_status: None,
        status_metadata: Value::Null,
        attachments: vec![],
    }
}

#[tokio::test]
async fn creates_subject_and_round_trips_via_get() {
    let backend = backend(vec!["task"]).await;
    let created = backend
        .create(sample_subject("task", "Investigate flaky test"))
        .await
        .expect("create");

    assert!(created.id.as_str().starts_with("sqlite:"));
    assert_eq!(created.title, "Investigate flaky test");
    assert_eq!(created.status, SubjectStatus::Ready);
    assert_eq!(created.priority, Some(2));
    assert_eq!(created.assignee.as_deref(), Some("alice@example.com"));
    assert_eq!(created.labels.len(), 2);

    let fetched = backend.get(&created.id).await.expect("get");
    assert_eq!(fetched, created);
}

#[tokio::test]
async fn lists_subjects_with_status_filter() {
    let backend = backend(vec!["task"]).await;
    let ready = backend
        .create(sample_subject("task", "Ready task"))
        .await
        .unwrap();
    let in_progress = {
        let mut s = sample_subject("task", "In-progress task");
        s.status = SubjectStatus::InProgress;
        backend.create(s).await.unwrap()
    };

    let page = backend
        .list(SubjectFilter {
            status: vec![SubjectStatus::InProgress],
            ..Default::default()
        })
        .await
        .expect("list");

    assert_eq!(page.subjects.len(), 1);
    assert_eq!(page.subjects[0].id, in_progress.id);
    assert_ne!(page.subjects[0].id, ready.id);
}

#[tokio::test]
async fn lists_subjects_with_kind_filter() {
    let backend = backend(vec!["task", "issue", "incident"]).await;
    let task = backend.create(sample_subject("task", "T1")).await.unwrap();
    let issue = backend.create(sample_subject("issue", "I1")).await.unwrap();
    let _incident = backend
        .create(sample_subject("incident", "INC1"))
        .await
        .unwrap();

    let page = backend
        .list(SubjectFilter {
            kind: vec!["task".into(), "issue".into()],
            ..Default::default()
        })
        .await
        .expect("list");

    assert_eq!(page.subjects.len(), 2);
    let ids: Vec<&str> = page.subjects.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&task.id.as_str()));
    assert!(ids.contains(&issue.id.as_str()));
}

#[tokio::test]
async fn updates_status_and_native_status() {
    let backend = backend(vec!["task"]).await;
    let created = backend.create(sample_subject("task", "T1")).await.unwrap();

    let mut custom = BTreeMap::new();
    custom.insert("native_status".into(), json!("In Review"));

    let updated = backend
        .update(
            &created.id,
            SubjectPatch {
                status: Some(SubjectStatus::InProgress),
                custom,
                ..Default::default()
            },
        )
        .await
        .expect("update");

    assert_eq!(updated.status, SubjectStatus::InProgress);
    assert_eq!(updated.native_status.as_deref(), Some("In Review"));
    assert!(
        updated.updated_at >= created.updated_at,
        "updated_at should advance"
    );
}

#[tokio::test]
async fn update_returns_refreshed_subject() {
    let backend = backend(vec!["task"]).await;
    let created = backend.create(sample_subject("task", "T1")).await.unwrap();

    let updated = backend
        .update(
            &created.id,
            SubjectPatch {
                assignee: Some(Some("bob@example.com".into())),
                labels_add: vec!["urgent".into()],
                labels_remove: vec!["v0.1.0".into()],
                ..Default::default()
            },
        )
        .await
        .expect("update");

    assert_eq!(updated.assignee.as_deref(), Some("bob@example.com"));
    assert!(updated.labels.contains(&"urgent".to_string()));
    assert!(!updated.labels.contains(&"v0.1.0".to_string()));

    // And the next get matches what update returned.
    let refetched = backend.get(&created.id).await.unwrap();
    assert_eq!(refetched, updated);
}

#[tokio::test]
async fn delete_via_status_set_to_cancelled() {
    let backend = backend(vec!["task"]).await;
    let created = backend.create(sample_subject("task", "T1")).await.unwrap();

    let updated = backend
        .update(
            &created.id,
            SubjectPatch {
                status: Some(SubjectStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.status, SubjectStatus::Cancelled);

    // Default list (no status filter) still returns the row — cancellation
    // is a soft delete that callers can filter against explicitly.
    let all = backend.list(SubjectFilter::default()).await.unwrap();
    assert_eq!(all.subjects.len(), 1);

    // Excluding cancelled drops it from the page.
    let active = backend
        .list(SubjectFilter {
            status: vec![
                SubjectStatus::Ready,
                SubjectStatus::InProgress,
                SubjectStatus::Blocked,
                SubjectStatus::Done,
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(active.subjects.is_empty());
}

#[tokio::test]
async fn watch_emits_event_on_create() {
    let backend = backend(vec!["task"]).await;
    let mut stream = backend.watch().await.expect("watch stream");

    let created = backend.create(sample_subject("task", "T1")).await.unwrap();

    let event = tokio::time::timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timeout waiting for event")
        .expect("stream closed");
    assert_eq!(event.id, created.id);
    assert!(matches!(
        event.change_kind,
        animus_subject_protocol::ChangeKind::Created
    ));
}

#[tokio::test]
async fn watch_emits_event_on_update() {
    let backend = backend(vec!["task"]).await;
    let created = backend.create(sample_subject("task", "T1")).await.unwrap();

    let mut stream = backend.watch().await.expect("watch stream");

    backend
        .update(
            &created.id,
            SubjectPatch {
                status: Some(SubjectStatus::InProgress),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("timeout waiting for event")
        .expect("stream closed");
    assert_eq!(event.id, created.id);
    assert!(matches!(
        event.change_kind,
        animus_subject_protocol::ChangeKind::StatusChanged
    ));
    assert_eq!(event.subject.status, SubjectStatus::InProgress);
}

#[tokio::test]
async fn schema_advertises_supports_create_true() {
    let backend = backend(vec!["task", "issue"]).await;
    let schema = backend.schema();
    assert!(schema.supports_create);
    assert!(schema.supports_watch);
    assert!(schema.supports_pagination);
    assert_eq!(schema.kinds, vec!["task".to_string(), "issue".to_string()]);
}

#[tokio::test]
async fn health_unhealthy_when_db_unreachable() {
    // Point the backend at a directory the OS won't let us open as a sqlite
    // file (the path is a directory). `SqliteBackend::new` will fail to
    // connect; we drive `health()` after force-constructing a backend on
    // a pool that we then close.
    let dir = TempDir::new().expect("tempdir");
    let bogus_path = dir.path().to_path_buf();

    // Constructing the backend on a directory path should fail at pool
    // creation; that's the real-world "unreachable" signal.
    let result = SqliteBackend::new(SqliteConfig::new(bogus_path).with_auto_migrate(false)).await;
    assert!(
        result.is_err(),
        "constructing backend pointed at a directory must fail"
    );

    // For an explicit `health()` round trip we tear down a pool out from
    // under a built backend and confirm health flips to Unhealthy.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    migrations::apply(&pool).await.unwrap();
    let backend = SqliteBackend::from_pool(pool.clone(), SqliteConfig::new("sqlite::memory:"));
    pool.close().await;

    let health = backend.health().await.expect("health call");
    assert_eq!(health.status, HealthStatus::Unhealthy);
    assert!(
        health.last_error.is_some(),
        "unhealthy result should carry an error string"
    );
}

#[tokio::test]
async fn pagination_returns_cursor_when_more_rows_remain() {
    let backend = backend(vec!["task"]).await;
    for i in 0..5 {
        let mut s = sample_subject("task", &format!("T{i}"));
        s.title = format!("Subject {i}");
        backend.create(s).await.unwrap();
    }

    let first = backend
        .list(SubjectFilter {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(first.subjects.len(), 2);
    assert!(first.next_cursor.is_some());

    let second = backend
        .list(SubjectFilter {
            limit: Some(2),
            cursor: first.next_cursor.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(second.subjects.len(), 2);
    assert!(second.next_cursor.is_some());

    let third = backend
        .list(SubjectFilter {
            limit: Some(2),
            cursor: second.next_cursor.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(third.subjects.len(), 1);
    assert!(third.next_cursor.is_none());
}
