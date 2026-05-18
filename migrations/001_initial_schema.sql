-- Initial schema for animus-subject-sqlite v0.1.0.
--
-- Subjects live in a single wide table. JSON-encoded columns
-- (labels, custom_fields, status_metadata, attachments) preserve the
-- Animus Subject schema without an explosion of join tables. Indexes
-- cover the SubjectFilter dimensions we expect to filter on most
-- frequently (kind, status, assignee, parent_id).

CREATE TABLE IF NOT EXISTS subjects (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    status TEXT NOT NULL,
    priority INTEGER,
    assignee TEXT,
    labels TEXT NOT NULL DEFAULT '[]',
    parent_id TEXT,
    custom_fields TEXT NOT NULL DEFAULT '{}',
    native_status TEXT,
    status_metadata TEXT,
    attachments TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_subjects_kind ON subjects(kind);
CREATE INDEX IF NOT EXISTS idx_subjects_status ON subjects(status);
CREATE INDEX IF NOT EXISTS idx_subjects_assignee ON subjects(assignee);
CREATE INDEX IF NOT EXISTS idx_subjects_parent_id ON subjects(parent_id);
