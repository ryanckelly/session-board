//! Fixture `~/.claude` trees for the tests, built in temp folders like the Python tests' own.
//! Nothing here reads the machine's config or `~/.claude`: every path is passed explicitly.
#![allow(dead_code)] // each test file uses its own part of this

use board::claude::agents::Agent;
use board::facts::{self, Projects, Session, Sources};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const NOW: f64 = 1_793_500_000.0; // 2026-10-31

/// `path` as the board spells it ([`board::config::canon`]): itself on Unix, `C:/…` on Windows.
pub fn path_str(path: &Path) -> String {
    board::config::canon(&path.to_string_lossy())
}

/// `path`'s permission bits are `want` (`0o600`, `0o700`). Unix only: on Windows the board's
/// files inherit the profile folder's ACL instead, so there is nothing to check.
pub fn assert_mode(path: &Path, want: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, want, "{}", path.display());
    }
    #[cfg(not(unix))]
    let _ = (path, want);
}

/// A folder removed when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "board-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        // canonicalize() spells Windows paths `\\?\C:\…`; the board spells them `C:/…`.
        TempDir(PathBuf::from(path_str(&path.canonicalize().unwrap())))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, p: &str) -> PathBuf {
        PathBuf::from(path_str(&self.0.join(p)))
    }

    pub fn str(&self) -> String {
        path_str(&self.0)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn iso(epoch: f64) -> String {
    let t = chrono::DateTime::from_timestamp_micros((epoch * 1e6).round() as i64).unwrap();
    t.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}

/// One transcript, written the way Claude Code writes the lines the board reads.
pub struct Transcript<'a> {
    pub sid: &'a str,
    pub cwd: &'a str,
    pub prompts: Vec<&'a str>,
    pub reply: &'a str,
    pub entry: &'a str,
    pub ts: f64,
    pub root: Option<&'a str>,
    pub title: Option<&'a str>,
    pub cleared: bool,
    pub ai: Option<&'a str>,
    pub extra: Vec<Value>,
}

pub fn transcript<'a>(sid: &'a str, cwd: &'a str) -> Transcript<'a> {
    Transcript {
        sid,
        cwd,
        prompts: vec!["do the thing"],
        reply: "done it",
        entry: "cli",
        ts: NOW - 600.0,
        root: None,
        title: None,
        cleared: false,
        ai: None,
        extra: vec![],
    }
}

impl<'a> Transcript<'a> {
    pub fn prompts(mut self, p: &[&'a str]) -> Self {
        self.prompts = p.to_vec();
        self
    }
    pub fn reply(mut self, r: &'a str) -> Self {
        self.reply = r;
        self
    }
    pub fn entry(mut self, e: &'a str) -> Self {
        self.entry = e;
        self
    }
    pub fn ts(mut self, ts: f64) -> Self {
        self.ts = ts;
        self
    }
    pub fn root(mut self, r: &'a str) -> Self {
        self.root = Some(r);
        self
    }
    pub fn title(mut self, t: &'a str) -> Self {
        self.title = Some(t);
        self
    }
    pub fn ai(mut self, t: &'a str) -> Self {
        self.ai = Some(t);
        self
    }
    pub fn cleared(mut self) -> Self {
        self.cleared = true;
        self
    }
    pub fn extra(mut self, lines: Vec<Value>) -> Self {
        self.extra = lines;
        self
    }

    /// Write it under `claude/projects/<folder>/<sid>.jsonl`, with its mtime at `ts`.
    pub fn write(self, claude: &Path) -> PathBuf {
        // As Claude Code names it: every character but a letter or digit becomes `-`
        // (`/home/u/app` → `-home-u-app`, `C:\Users\u` → `C--Users-u`).
        let name: String = self.cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let folder = claude.join("projects").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let (sid, ts) = (self.sid, self.ts);
        let mut lines = Vec::new();
        if self.cleared {
            // What Claude Code writes first in a conversation started with /clear.
            lines.push(json!({"type": "attachment", "attachment": {"type": "hook_success", "hookName": "SessionStart:clear"},
                              "sessionId": sid, "timestamp": iso(ts - 1.0)}));
            lines.push(json!({"type": "system", "subtype": "local_command", "commandRun": {"command": "clear", "args": ""},
                              "sessionId": sid, "timestamp": iso(ts - 1.0)}));
        }
        if let Some(ai) = self.ai {
            lines.push(json!({"type": "ai-title", "aiTitle": ai, "sessionId": sid}));
        }
        if let Some(title) = self.title {
            lines.push(json!({"type": "custom-title", "customTitle": title, "sessionId": sid}));
        }
        for (i, p) in self.prompts.iter().enumerate() {
            let uuid = match self.root {
                Some(r) if i == 0 => r.to_string(),
                _ => format!("{sid}-u{i}"),
            };
            lines.push(json!({"type": "user", "uuid": uuid, "cwd": self.cwd, "entrypoint": self.entry,
                              "promptSource": "typed", "timestamp": iso(ts + i as f64),
                              "message": {"role": "user", "content": p}}));
            lines.push(json!({"type": "assistant", "cwd": self.cwd, "entrypoint": self.entry,
                              "timestamp": iso(ts + i as f64 + 0.5),
                              "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": "hm"},
                                                                           {"type": "text", "text": self.reply}]}}));
        }
        lines.extend(self.extra);
        let path = folder.join(format!("{sid}.jsonl"));
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        set_mtime(&path, ts);
        path
    }
}

pub fn set_mtime(path: &Path, ts: f64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(ts);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

/// A background job's `state.json`.
pub struct Job<'a> {
    pub job: &'a str,
    pub sid: &'a str,
    pub cwd: &'a str,
    pub tempo: &'a str,
    pub detail: &'a str,
    pub result: Option<&'a str>,
    pub resume: Option<&'a str>,
    pub name: Option<&'a str>,
    pub state: &'a str,
    pub needs: Option<&'a str>,
}

pub fn job<'a>(job: &'a str, sid: &'a str, cwd: &'a str) -> Job<'a> {
    Job { job, sid, cwd, tempo: "idle", detail: "", result: None, resume: None, name: None, state: "done", needs: None }
}

impl<'a> Job<'a> {
    pub fn tempo(mut self, t: &'a str) -> Self {
        self.tempo = t;
        self
    }
    pub fn detail(mut self, d: &'a str) -> Self {
        self.detail = d;
        self
    }
    pub fn result(mut self, r: &'a str) -> Self {
        self.result = Some(r);
        self
    }
    pub fn resume(mut self, r: &'a str) -> Self {
        self.resume = Some(r);
        self
    }
    pub fn name(mut self, n: &'a str) -> Self {
        self.name = Some(n);
        self
    }
    pub fn state(mut self, s: &'a str) -> Self {
        self.state = s;
        self
    }
    pub fn needs(mut self, n: &'a str) -> Self {
        self.needs = Some(n);
        self
    }

    pub fn write(self, claude: &Path) {
        let dir = claude.join("jobs").join(self.job);
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = json!({"state": self.state, "tempo": self.tempo, "detail": self.detail, "sessionId": self.sid,
                            "cwd": self.cwd, "updatedAt": iso(NOW - 60.0),
                            "resumeSessionId": self.resume.unwrap_or(self.sid), "name": self.name});
        if let Some(r) = self.result {
            st["output"] = json!({"result": r});
        }
        if let Some(n) = self.needs {
            st["needs"] = json!(n);
        }
        std::fs::write(dir.join("state.json"), st.to_string()).unwrap();
    }
}

/// Agents as `claude agents --json` would list them.
pub fn agents(list: Value) -> Vec<Agent> {
    list.as_array().unwrap().iter().filter_map(Agent::from_value).collect()
}

pub fn background(job: &str, sid: &str) -> Value {
    json!({"id": job, "kind": "background", "sessionId": sid})
}

/// Sources for a fixture tree: `<tmp>/claude`, the cache and deleted list beside it.
pub fn sources(tmp: &TempDir) -> Sources {
    Sources {
        claude_dir: tmp.join("claude"),
        cache_path: tmp.join("cache.json"),
        deleted_path: tmp.join("deleted.json"),
        home: "/home/u".into(),
        days: 30,
        projects: Projects { workspaces: vec!["/home/u".into()], scratch: "/nonexistent-scratch".into() },
        claude: vec!["claude".into()],
    }
}

pub fn collect(tmp: &TempDir, agents: &[Agent]) -> HashMap<String, Session> {
    facts::collect_with(&sources(tmp), agents, NOW).into_iter().map(|s| (s.session_id.clone(), s)).collect()
}

pub fn ids(sessions: &HashMap<String, Session>) -> Vec<&str> {
    let mut v: Vec<&str> = sessions.keys().map(String::as_str).collect();
    v.sort();
    v
}
