//! Parsed transcripts, cached by file size and modification time. Parsing every transcript from
//! the last month reads hundreds of megabytes; between polls usually one file has changed.

use crate::claude::transcript::{self, TranscriptInfo};
use crate::claude::glob;
use crate::store::write_private;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// A file's modification time in nanoseconds since the epoch (0 if the platform can't say).
/// The same number `st_mtime * 1e9 + st_mtime_nsec` gives on Unix, so caches carry over.
fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

/// Bump when [`TranscriptInfo`] or its parsing changes; a cache of another version is dropped.
const VERSION: u32 = 5;

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    version: u32,
    files: BTreeMap<String, Cached>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Cached {
    /// (mtime in ns, size): a file with the same signature isn't parsed again.
    sig: (i128, u64),
    info: TranscriptInfo,
}

/// Transcripts modified in the last `days` days, by session id, newest first.
pub fn load_transcripts(claude_dir: &Path, cache_path: &Path, days: u32, now: f64) -> Vec<(String, TranscriptInfo)> {
    load_with(claude_dir, cache_path, days, now, &mut |p| transcript::parse_file(p).ok())
}

/// [`load_transcripts`] with the parser passed in, so tests can see which files get parsed.
pub fn load_with(
    claude_dir: &Path,
    cache_path: &Path,
    days: u32,
    now: f64,
    parse: &mut dyn FnMut(&Path) -> Option<TranscriptInfo>,
) -> Vec<(String, TranscriptInfo)> {
    let old = std::fs::read(cache_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
        .filter(|c| c.version == VERSION)
        .unwrap_or_default()
        .files;
    let horizon = now - f64::from(days) * 86400.0;
    let mut files = BTreeMap::new();
    let mut parsed_any = false;
    for folder in glob(&claude_dir.join("projects"), "") {
        for path in glob(&folder, ".jsonl") {
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let mtime_ns = mtime_ns(&meta);
            if !meta.is_file() || (mtime_ns as f64) / 1e9 < horizon {
                continue;
            }
            let sig = (mtime_ns, meta.len());
            let key = path.to_string_lossy().into_owned();
            match old.get(&key) {
                Some(hit) if hit.sig == sig => {
                    files.insert(key, hit.clone());
                }
                _ => {
                    if let Some(info) = parse(&path) {
                        files.insert(key, Cached { sig, info });
                        parsed_any = true;
                    }
                }
            }
        }
    }
    // Rewritten when a file was parsed, and when one dropped out of the window or vanished.
    if parsed_any || !files.keys().eq(old.keys()) {
        let _ = write_private(cache_path, &CacheFile { version: VERSION, files: files.clone() });
    }
    let mut out: Vec<(String, TranscriptInfo)> = files
        .into_iter()
        .map(|(path, c)| {
            let sid = Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            (sid, c.info)
        })
        .collect();
    // Newest first; ties by id, so the order never depends on the file system.
    out.sort_by(|a, b| b.1.last.total_cmp(&a.1.last).then_with(|| a.0.cmp(&b.0)));
    // The same session id in two project folders: keep one, the newest.
    let mut seen = std::collections::HashSet::new();
    out.retain(|(sid, _)| seen.insert(sid.clone()));
    out
}
