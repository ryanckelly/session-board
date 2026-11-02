//! The git state of a session's folder, for the Git card: branch, upstream, uncommitted changes,
//! worktrees and the last commit. Read-only: every command runs with `--no-optional-locks`, so
//! even `git status` doesn't refresh the index, and nothing is ever fetched (how current the
//! upstream is shows as the time of the last fetch, from `FETCH_HEAD`).
//!
//! Folders are read in parallel, and each answer is kept for [`TTL`], so a board polling every
//! few seconds runs git at most that often per folder.

use crate::proc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long one folder's answer is reused.
pub const TTL: Duration = Duration::from_secs(20);
/// Each git command gets this long.
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitInfo {
    /// The top of the working tree the folder is in.
    pub root: String,
    /// The branch checked out; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Its upstream, e.g. `origin/main`.
    pub upstream: Option<String>,
    /// Commits ahead of and behind the upstream, as of the last fetch.
    pub ahead: u32,
    pub behind: u32,
    /// Files with staged changes, unstaged changes, untracked, and in conflict.
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicts: u32,
    /// Working trees of the repository, the main one included.
    pub worktrees: u32,
    /// When the repository last fetched (`FETCH_HEAD`'s mtime), epoch seconds.
    pub fetched: Option<i64>,
    /// The last commit on HEAD: when, and its subject.
    pub commit_at: Option<i64>,
    pub commit: Option<String>,
}

impl GitInfo {
    /// Uncommitted files of any kind.
    pub fn dirty(&self) -> u32 {
        self.staged + self.modified + self.untracked + self.conflicts
    }
}

/// `git status --porcelain=v2 --branch` into `info`.
pub fn parse_status(out: &str, info: &mut GitInfo) {
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            info.branch = (rest != "(detached)").then(|| rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            info.upstream = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            for part in rest.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    info.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    info.behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            // `1 XY …`: X is the index (staged), Y the working tree.
            let xy: Vec<char> = line[2..].chars().take(2).collect();
            if xy.first().is_some_and(|c| *c != '.') {
                info.staged += 1;
            }
            if xy.get(1).is_some_and(|c| *c != '.') {
                info.modified += 1;
            }
        } else if line.starts_with("u ") {
            info.conflicts += 1;
        } else if line.starts_with("? ") {
            info.untracked += 1;
        }
    }
}

/// Read the git state of `cwd`; `None` when it isn't in a git working tree (or git is missing).
pub fn read(cwd: &str) -> Option<GitInfo> {
    let dir = Path::new(cwd);
    if !dir.is_dir() {
        return None;
    }
    let git = |args: &[&str]| -> Option<String> {
        let mut argv = vec!["git".to_string(), "--no-optional-locks".to_string()];
        argv.extend(args.iter().map(|a| a.to_string()));
        let out = proc::spawn(&argv, Some(dir), false, false).ok()?.wait(TIMEOUT).ok()?;
        (out.code == Some(0)).then_some(out.stdout)
    };
    let top = git(&["rev-parse", "--show-toplevel", "--git-common-dir"])?;
    let mut lines = top.lines();
    let mut info = GitInfo { root: lines.next()?.to_string(), ..GitInfo::default() };
    let common = lines.next().map(|c| dir.join(c));
    let status = git(&["status", "--porcelain=v2", "--branch", "--untracked-files=normal"])?;
    parse_status(&status, &mut info);
    if let Some(log) = git(&["log", "-1", "--format=%ct%x1f%s"]) {
        let (at, subject) = log.trim_end().split_once('\x1f').unwrap_or(("", ""));
        info.commit_at = at.parse().ok();
        info.commit = (!subject.is_empty()).then(|| crate::mask::mask_secrets(subject));
    }
    info.worktrees = git(&["worktree", "list", "--porcelain"])
        .map(|w| w.lines().filter(|l| l.starts_with("worktree ")).count() as u32)
        .unwrap_or(1);
    info.fetched = common
        .and_then(|c| std::fs::metadata(c.join("FETCH_HEAD")).ok()?.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    Some(info)
}

type Cache = HashMap<String, (Instant, Option<GitInfo>)>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The git state of each of `folders`, read in parallel, reusing answers younger than [`TTL`].
pub fn read_all<'a>(folders: impl IntoIterator<Item = &'a str>) -> HashMap<String, GitInfo> {
    let now = Instant::now();
    let mut want: Vec<String> = folders.into_iter().map(String::from).collect();
    want.sort();
    want.dedup();
    let fresh: Vec<String> = {
        let c = cache().lock().unwrap_or_else(|p| p.into_inner());
        want.iter().filter(|f| !c.get(*f).is_some_and(|(at, _)| now.duration_since(*at) < TTL)).cloned().collect()
    };
    let read: Vec<(String, Option<GitInfo>)> = std::thread::scope(|s| {
        let handles: Vec<_> = fresh
            .chunks(fresh.len().div_ceil(8).max(1))
            .map(|chunk| s.spawn(move || chunk.iter().map(|f| (f.clone(), read(f))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut c = cache().lock().unwrap_or_else(|p| p.into_inner());
    for (f, info) in read {
        c.insert(f, (now, info));
    }
    c.retain(|_, (at, _)| now.duration_since(*at) < TTL * 3);
    want.iter().filter_map(|f| Some((f.clone(), c.get(f)?.1.clone()?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_counted() {
        let out = "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -1\n\
                   1 M. N... 100644 100644 100644 a b src/a.rs\n1 .M N... 100644 100644 100644 a b src/b.rs\n\
                   1 MM N... 100644 100644 100644 a b src/c.rs\n2 R. N... 100644 100644 100644 a b R100 n o\n\
                   u UU N... 1 2 3 4 a b c d x.rs\n? new.txt\n? other.txt\n";
        let mut g = GitInfo::default();
        parse_status(out, &mut g);
        assert_eq!(g.branch.as_deref(), Some("main"));
        assert_eq!(g.upstream.as_deref(), Some("origin/main"));
        assert_eq!((g.ahead, g.behind), (2, 1));
        assert_eq!((g.staged, g.modified, g.untracked, g.conflicts), (3, 2, 2, 1));
        let mut d = GitInfo::default();
        parse_status("# branch.head (detached)\n", &mut d);
        assert_eq!(d.branch, None);
    }

    #[test]
    fn a_real_repository_is_read() {
        let tmp = std::env::temp_dir().join(format!("sb-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&tmp)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(tmp.join("a.txt"), "a").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "First commit"]);
        std::fs::write(tmp.join("a.txt"), "b").unwrap();
        std::fs::write(tmp.join("new.txt"), "n").unwrap();
        let g = read(tmp.to_str().unwrap()).expect("a repo");
        assert_eq!(g.branch.as_deref(), Some("main"));
        assert_eq!((g.modified, g.untracked, g.worktrees), (1, 1, 1));
        assert_eq!(g.commit.as_deref(), Some("First commit"));
        assert!(g.upstream.is_none() && g.fetched.is_none());
        std::fs::remove_dir_all(&tmp).unwrap();
        assert!(read(tmp.to_str().unwrap()).is_none());
    }
}
