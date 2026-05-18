//! Environment-driven configuration for the SQLite subject backend.
//!
//! All fields are populated from environment variables so the plugin can be
//! launched as a stdio child process without command-line argument plumbing.
//! Loading is lenient: every field has a sensible default and the only way
//! to make [`SqliteConfig::from_env`] fail is to supply an outright invalid
//! value (e.g. `ANIMUS_SQLITE_KINDS=""`).

use std::path::PathBuf;

use anyhow::{anyhow, Result};

/// Environment variable holding the SQLite database path.
pub const ENV_DB_PATH: &str = "ANIMUS_SQLITE_DB_PATH";

/// Environment variable holding the comma-separated list of subject kinds
/// this backend serves. Defaults to `task`.
pub const ENV_KINDS: &str = "ANIMUS_SQLITE_KINDS";

/// Environment variable toggling startup migrations. Defaults to `true`.
pub const ENV_AUTO_MIGRATE: &str = "ANIMUS_SQLITE_AUTO_MIGRATE";

/// Default path (relative to the current working directory) used when
/// [`ENV_DB_PATH`] is not set. Mirrors `<project_root>/.animus/subjects/sqlite.db`
/// when the daemon launches the plugin with the project root as its cwd.
pub const DEFAULT_DB_PATH: &str = ".animus/subjects/sqlite.db";

/// Default subject kind served when [`ENV_KINDS`] is unset.
pub const DEFAULT_KIND: &str = "task";

/// Runtime configuration for the SQLite subject backend.
#[derive(Debug, Clone)]
pub struct SqliteConfig {
    /// Filesystem path to the SQLite database. The file is created on
    /// first use if it does not exist.
    pub db_path: PathBuf,

    /// Subject kinds this backend serves. Created subjects with a kind
    /// outside this list are rejected with `BackendError::InvalidRequest`.
    /// Non-empty by construction (defaults to `[DEFAULT_KIND]`).
    pub kinds: Vec<String>,

    /// Whether to run embedded migrations on startup.
    pub auto_migrate: bool,
}

impl SqliteConfig {
    /// Read the configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let db_path = std::env::var(ENV_DB_PATH)
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DB_PATH));

        let kinds = std::env::var(ENV_KINDS)
            .ok()
            .filter(|s| !s.is_empty())
            .map(|raw| parse_kinds(&raw))
            .transpose()?
            .unwrap_or_else(|| vec![DEFAULT_KIND.to_string()]);

        let auto_migrate = std::env::var(ENV_AUTO_MIGRATE)
            .ok()
            .filter(|s| !s.is_empty())
            .map(|raw| parse_bool(&raw))
            .transpose()?
            .unwrap_or(true);

        Ok(Self {
            db_path,
            kinds,
            auto_migrate,
        })
    }

    /// In-memory builder used by tests + embedders.
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            kinds: vec![DEFAULT_KIND.to_string()],
            auto_migrate: true,
        }
    }

    /// Override the served kind list. Empty input is rejected so the
    /// backend always serves at least one kind.
    pub fn with_kinds<I, S>(mut self, kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let kinds: Vec<String> = kinds.into_iter().map(Into::into).collect();
        if !kinds.is_empty() {
            self.kinds = kinds;
        }
        self
    }

    /// Override the auto-migrate flag.
    pub fn with_auto_migrate(mut self, auto_migrate: bool) -> Self {
        self.auto_migrate = auto_migrate;
        self
    }

    /// Whether `kind` is in the served set.
    pub fn accepts_kind(&self, kind: &str) -> bool {
        self.kinds.iter().any(|k| k == kind)
    }
}

fn parse_kinds(raw: &str) -> Result<Vec<String>> {
    let kinds: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if kinds.is_empty() {
        return Err(anyhow!(
            "ANIMUS_SQLITE_KINDS must be a non-empty comma-separated list"
        ));
    }
    Ok(kinds)
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(anyhow!(
            "ANIMUS_SQLITE_AUTO_MIGRATE must be a boolean ({other:?})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_yields_default_kind() {
        let cfg = SqliteConfig::new("/tmp/test.db");
        assert_eq!(cfg.kinds, vec![DEFAULT_KIND.to_string()]);
        assert!(cfg.auto_migrate);
        assert!(cfg.accepts_kind("task"));
        assert!(!cfg.accepts_kind("issue"));
    }

    #[test]
    fn with_kinds_overrides_default() {
        let cfg = SqliteConfig::new("/tmp/test.db").with_kinds(["task", "issue"]);
        assert_eq!(cfg.kinds, vec!["task".to_string(), "issue".to_string()]);
        assert!(cfg.accepts_kind("issue"));
    }

    #[test]
    fn with_kinds_ignores_empty_input() {
        let cfg = SqliteConfig::new("/tmp/test.db").with_kinds(Vec::<String>::new());
        assert_eq!(cfg.kinds, vec![DEFAULT_KIND.to_string()]);
    }

    #[test]
    fn parse_kinds_trims_whitespace_and_drops_empties() {
        let parsed = parse_kinds(" task , issue ,, custom-thing").unwrap();
        assert_eq!(
            parsed,
            vec![
                "task".to_string(),
                "issue".to_string(),
                "custom-thing".to_string()
            ]
        );
    }

    #[test]
    fn parse_kinds_rejects_all_whitespace() {
        assert!(parse_kinds("  ,  , ").is_err());
    }

    #[test]
    fn parse_bool_accepts_canonical_forms() {
        assert!(parse_bool("true").unwrap());
        assert!(parse_bool("1").unwrap());
        assert!(parse_bool("YES").unwrap());
        assert!(!parse_bool("false").unwrap());
        assert!(!parse_bool("0").unwrap());
        assert!(parse_bool("not-a-bool").is_err());
    }
}
