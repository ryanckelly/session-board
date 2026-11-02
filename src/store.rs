//! The board's own small files: conversations deleted from the board (`deleted.json`) and
//! remembered choices (`prefs.json`). Both are shared with the Python board, in the same format.

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `data` as JSON, readable by you only (the cache holds transcript excerpts), atomically:
/// a temp file in the same folder renamed into place, so concurrent `board --json` runs never
/// see half a file and need no lock. On Unix the folder is 0700 and the file 0600; on Windows
/// both inherit the ACL of your profile folder, which already keeps other users out.
pub fn write_private(path: &Path, data: &impl Serialize) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(
        ".tmp-{}-{}-{nanos}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&tmp)?;
        f.write_all(&serde_json::to_vec(data).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn read_object(path: &Path) -> Option<Map<String, Value>> {
    match crate::claude::read_json(path)? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

// --- conversations deleted from the board -------------------------------------------------------

/// Session ids and conversation roots deleted from the board, with when (epoch seconds).
/// `claude rm` deletes a background session but keeps its transcript, which would bring it
/// straight back as an ended row; the board hides it instead.
pub type Deleted = HashMap<String, f64>;

pub fn load_deleted(path: &Path) -> Deleted {
    read_object(path)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.as_f64().map(|t| (k, t)))
        .collect()
}

/// Hide a deleted session's conversation, every copy of it, from the ended rows. Entries go after
/// `days + 1` days, when Claude Code has cleared the transcripts anyway.
pub fn forget(session_id: &str, root: Option<&str>, path: &Path, days: u32, now: f64) -> std::io::Result<()> {
    let horizon = now - f64::from(days + 1) * 86400.0;
    let mut data: Deleted = load_deleted(path).into_iter().filter(|(_, at)| *at > horizon).collect();
    for key in [Some(session_id), root].into_iter().flatten().filter(|k| !k.is_empty()) {
        data.insert(key.to_string(), now);
    }
    // Sorted, so the file doesn't churn between writes.
    let sorted: std::collections::BTreeMap<_, _> = data.into_iter().collect();
    write_private(path, &sorted)
}

/// Hidden, unless the conversation was used again after it was deleted.
pub fn is_deleted(deleted: &Deleted, session_id: &str, root: Option<&str>, last: f64) -> bool {
    let at = |k: &str| deleted.get(k).copied().unwrap_or(0.0);
    let at = at(session_id).max(root.map(at).unwrap_or(0.0));
    at > 0.0 && last <= at
}

// --- remembered choices -------------------------------------------------------------------------

/// The board's remembered choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    /// Show ended sessions (hidden at first).
    pub show_ended: bool,
    /// Columns' metadata strip (shown at first).
    pub show_columns: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { show_ended: false, show_columns: true }
    }
}

pub fn load_prefs(path: &Path) -> Prefs {
    let m = read_object(path).unwrap_or_default();
    Prefs {
        show_ended: crate::claude::truthy(m.get("show_ended")),
        show_columns: m.get("show_columns") != Some(&Value::Bool(false)),
    }
}

/// Remember `prefs`, keeping any other keys in the file. A choice not remembered is no reason
/// to stop, so failures are ignored.
pub fn save_prefs(prefs: Prefs, path: &Path) {
    let mut m = read_object(path).unwrap_or_default();
    m.insert("show_ended".into(), prefs.show_ended.into());
    m.insert("show_columns".into(), prefs.show_columns.into());
    let _ = write_private(path, &m);
}
