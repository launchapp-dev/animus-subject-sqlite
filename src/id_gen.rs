//! Subject id generation.
//!
//! We use [ULIDs](https://github.com/ulid/spec) instead of UUIDv4 for two
//! reasons:
//!
//! 1. **Sortable.** ULIDs are time-prefixed and 26-char Crockford-base32
//!    encoded, so `ORDER BY id` returns subjects in creation order without
//!    a separate `created_at` index.
//! 2. **URL- and shell-safe.** No hyphens, no slashes, no padding — they
//!    drop into ids, filenames, and command-line args without escaping.
//!
//! Generated ids always carry the `sqlite:` prefix so the daemon can
//! dispatch the matching backend from the id alone, matching the convention
//! used by every other Animus subject backend.

use animus_subject_protocol::SubjectId;
use ulid::Ulid;

/// Id prefix every sqlite-owned subject carries.
pub const ID_PREFIX: &str = "sqlite:";

/// Generate a fresh subject id of the form `sqlite:<ULID>`.
pub fn new_subject_id() -> SubjectId {
    SubjectId::new(format!("{ID_PREFIX}{}", Ulid::new()))
}

/// True if `id` is recognizable as a sqlite-backend subject id.
pub fn is_sqlite_id(id: &SubjectId) -> bool {
    id.as_str().starts_with(ID_PREFIX)
}

/// Strip the `sqlite:` prefix from `id`, returning the bare ULID portion.
/// Returns `None` if the prefix is missing — callers should treat that as
/// an `InvalidRequest` at the trait boundary.
pub fn strip_prefix(id: &SubjectId) -> Option<&str> {
    id.as_str().strip_prefix(ID_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_subject_id_has_prefix_and_ulid_length() {
        let id = new_subject_id();
        let raw = id.as_str();
        assert!(raw.starts_with(ID_PREFIX));
        // sqlite: + 26 ULID chars
        assert_eq!(raw.len(), ID_PREFIX.len() + 26);
    }

    #[test]
    fn new_ids_sort_by_creation_order() {
        let a = new_subject_id();
        // ULID monotonicity within the same millisecond is best-effort; we
        // just need timestamps to differ for the sort assertion to be
        // meaningful. Sleep 2ms.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = new_subject_id();
        assert!(a.as_str() < b.as_str(), "{a} should sort before {b}");
    }

    #[test]
    fn is_sqlite_id_and_strip_prefix_round_trip() {
        let id = new_subject_id();
        assert!(is_sqlite_id(&id));
        assert!(strip_prefix(&id).is_some());

        let foreign = SubjectId::new("linear:ENG-1");
        assert!(!is_sqlite_id(&foreign));
        assert!(strip_prefix(&foreign).is_none());
    }
}
