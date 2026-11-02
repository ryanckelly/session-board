//! Claude Code's own files and CLI, read-only. These formats are internal to Claude Code and can
//! change with any update; `docs/data-sources.md` records what was verified and when.
//!
//! Everything here is lenient: a missing field, a value of the wrong type, a line that doesn't
//! parse or a file that has gone is treated as absent, never as an error.

pub mod agents;
pub mod jobs;
pub mod registry;
pub mod roster;
pub mod transcript;

use serde_json::Value;
use std::path::{Path, PathBuf};

/// A JSON file's contents, or `None` if it is missing or not JSON.
pub(crate) fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Epoch seconds from an ISO 8601 timestamp (`2026-10-31T12:00:00.123Z`); 0 if it isn't one.
/// A timestamp with no offset is local time, as Python's `datetime.timestamp()` reads it.
pub fn epoch(stamp: Option<&Value>) -> f64 {
    let Some(s) = stamp.and_then(Value::as_str) else { return 0.0 };
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return t.timestamp_micros() as f64 / 1e6;
    }
    let s = s.trim();
    let naive = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| chrono::NaiveDateTime::parse_from_str(s, f).ok())
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0))
        });
    naive
        .and_then(|n| n.and_local_timezone(chrono::Local).earliest())
        .map(|t| t.timestamp_micros() as f64 / 1e6)
        .unwrap_or(0.0)
}

/// Python truthiness, which the formats were first read with: null, false, 0, "", [] and {} are
/// all "not there".
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// A non-empty string field.
pub(crate) fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// [`text`], owned.
pub(crate) fn string(v: &Value, key: &str) -> Option<String> {
    text(v, key).map(String::from)
}

/// A path field, in the board's spelling ([`crate::config::canon`]).
pub(crate) fn path_field(v: &Value, key: &str) -> Option<String> {
    text(v, key).map(crate::config::canon)
}

/// Entries of `dir` matching `*<suffix>`, skipping hidden names as a shell glob does, sorted.
pub(crate) fn glob(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && name.ends_with(suffix)
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn epoch_reads_iso_with_z_and_fractions() {
        assert_eq!(epoch(Some(&json!("2026-10-31T00:00:00Z"))), 1_793_404_800.0);
        assert_eq!(epoch(Some(&json!("2026-10-31T00:00:00.5+00:00"))), 1_793_404_800.5);
        assert_eq!(epoch(Some(&json!("not a time"))), 0.0);
        assert_eq!(epoch(Some(&json!(12))), 0.0);
        assert_eq!(epoch(None), 0.0);
    }
}
