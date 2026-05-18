# animus-subject-sqlite

A local SQLite subject backend plugin for [Animus](https://github.com/launchapp-dev/animus-cli).

> **Status:** Under construction — landing in Animus v0.4.0.

## What this is

Animus v0.4.0 makes subjects (units of dispatchable work) pluggable. Most
subject backends are thin wrappers over an external system of record
(Linear, Jira, GitHub Issues, Notion). This one is different: **`animus-subject-sqlite`
owns the data**. Tasks, issues, and any other subject kinds you configure
live in a local SQLite database the plugin reads and writes directly. No
upstream API, no API tokens, no rate limits.

Use it when you want Animus to be the system of record for your work, or
when you don't yet have an external tracker and don't want to commit to
one. Solo developers, small teams, throwaway prototypes, and CI bots
without a Linear seat all benefit.

## Differences from external-API backends

| Capability | External-API backends | `animus-subject-sqlite` |
|------------|-----------------------|-------------------------|
| `supports_create` | `false` (creates happen upstream) | `true` |
| `supports_watch`  | typically `false` (polling) | `true` (in-process broadcast) |
| `supports_pagination` | `true` | `true` (keyset on `id`) |
| Latency | upstream API roundtrip | local sqlite query |
| Subject id prefix | `linear:`, `jira:`, ... | `sqlite:<ULID>` |

## Install

```bash
animus plugin install launchapp-dev/animus-subject-sqlite
```

## Configure

```bash
export ANIMUS_SQLITE_DB_PATH=./.animus/subjects/sqlite.db
export ANIMUS_SQLITE_KINDS=task,issue
animus daemon start
```

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ANIMUS_SQLITE_DB_PATH` | `./.animus/subjects/sqlite.db` | Path to the SQLite file. Parent directories are created on first use. |
| `ANIMUS_SQLITE_KINDS` | `task` | Comma-separated subject kinds this backend serves. Used to validate `create()` requests and to populate `schema().kinds`. |
| `ANIMUS_SQLITE_AUTO_MIGRATE` | `true` | Run embedded migrations on startup. Set to `false` if you manage the schema externally. |

## Workflow YAML

```yaml
# .animus/workflows/standard.yaml
subjects:
  local:
    plugin: animus-subject-sqlite
    config:
      db_path: ./.animus/subjects/sqlite.db
      kinds:   [task, issue]

workflows:
  - id: triage-and-impl
    subject_type: local
    phases: [...]
```

## ID convention

Subjects created by this backend carry ids of the form
`sqlite:<ULID>` — for example `sqlite:01HVK0YQX2N3TJ4R8DRVK0KAVH`. ULIDs are:

- **time-prefixed and lexicographically sortable**, so `ORDER BY id` walks
  rows in creation order without a separate `created_at` index;
- **URL- and shell-safe** (Crockford-base32, no hyphens), so ids drop
  into filenames, CLI args, and HTTP paths without escaping.

Callers may also pass a pre-allocated id when creating a subject as long
as it starts with `sqlite:`; the plugin replaces ids missing that prefix
with a freshly generated ULID.

## Schema

Subjects live in a single wide table with JSON-encoded columns for
the list-typed fields. See `migrations/001_initial_schema.sql`.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `TEXT PRIMARY KEY` | `sqlite:<ULID>` |
| `kind` | `TEXT NOT NULL` | indexed |
| `title` | `TEXT NOT NULL` | |
| `body` | `TEXT` | description / long-form text |
| `status` | `TEXT NOT NULL` | indexed; one of `ready`, `in-progress`, `blocked`, `done`, `cancelled` |
| `priority` | `INTEGER` | 0..=4 scale |
| `assignee` | `TEXT` | indexed |
| `labels` | `TEXT NOT NULL DEFAULT '[]'` | JSON array |
| `parent_id` | `TEXT` | indexed |
| `custom_fields` | `TEXT NOT NULL DEFAULT '{}'` | JSON object |
| `native_status` | `TEXT` | backend-raw status string |
| `status_metadata` | `TEXT` | optional JSON object |
| `attachments` | `TEXT` | optional JSON array |
| `created_at` | `TEXT NOT NULL` | RFC3339 |
| `updated_at` | `TEXT NOT NULL` | RFC3339 |

A `schema_version` table records the latest applied migration ordinal so
re-running the plugin on an existing database is a no-op.

## Watch

`subject/watch` is backed by an in-process `tokio::sync::broadcast`
channel. Every `create()` and `update()` emits a `SubjectChangedEvent` to
all live subscribers before returning. Subscribers that fall more than
256 events behind are silently lagged off; the daemon recovers by
re-listing on its next tick.

## Soft deletes

There is no `delete` method on the `SubjectBackend` trait. To remove a
subject from active dispatch, update its `status` to `Cancelled` — the
default `list()` query still returns it, but a workflow YAML filter that
excludes `Cancelled` (which is what most workflows ship by default) will
treat the row as gone.

## Design

The subject backend plugin protocol is defined in the Animus core repo:

- **Protocol design:** [`docs/architecture/subject-backend-plugins.md`](https://github.com/launchapp-dev/animus-cli/blob/main/docs/architecture/subject-backend-plugins.md)
- **Naming contract:** [`docs/architecture/naming-contract.md`](https://github.com/launchapp-dev/animus-cli/blob/main/docs/architecture/naming-contract.md)
- **Repository name:** `animus-subject-sqlite`
- **Crate name:** `animus-subject-sqlite`
- **Binary name:** `animus-subject-sqlite`

## License

MIT — see [LICENSE](LICENSE).
