//! Local SQLite subject backend plugin for Animus.
//!
//! This crate is consumed by `src/main.rs` (the stdio plugin binary) and by
//! `tests/contract.rs`. It exposes:
//!
//! - [`config::SqliteConfig`] — environment-driven configuration
//! - [`store::SqliteStore`] — low-level CRUD against the database
//! - [`backend::SqliteBackend`] — the `SubjectBackend` implementation
//! - [`migrations`] — embedded migration runner
//! - [`id_gen`] — ULID-based subject id generator
//!
//! Unlike upstream-API-backed backends (Linear, Jira, GitHub Issues), this
//! plugin OWNS the data. Subjects are stored in a local SQLite database and
//! every CRUD operation completes locally — no upstream roundtrip, no API
//! tokens, no rate limits. Watch streams are driven by an in-process
//! broadcast channel, so callers see updates immediately after `update()` /
//! `create()` returns.

pub mod backend;
pub mod config;
pub mod id_gen;
pub mod migrations;
pub mod store;
