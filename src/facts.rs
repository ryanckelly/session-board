//! Facts for the board: which sessions exist, what state each is in, and a summary. Facts only,
//! no model calls. Sources (see `docs/data-sources.md`):
//!
//! - `claude agents --json --all`: background sessions and live interactive ones. If the CLI fails,
//!   the jobs folder and the live-process registry stand in for it.
//! - `jobs/<id>/state.json`: a background session's tempo and Claude Code's own summary.
//! - `daemon/roster.json`: the background workers that are running, to catch a job whose
//!   state.json still says "working" or "needs you" after its process went away.
//! - Transcripts under `projects/`: titles, the first prompt, the last reply, a pending tool call,
//!   the model and token use, activity spans, and ended sessions.

use crate::cache;
use crate::claude::agents::{self, Agent};
use crate::claude::jobs::JobState;
use crate::claude::roster;
pub use crate::claude::transcript::PendingTool;
use crate::claude::transcript::{FileTouch, Span, TranscriptInfo};
use crate::git::{self, GitInfo};
use crate::config::Config;
use crate::mask::{flat, mask_secrets};
use crate::store::{self, Deleted};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A session's state. The order is the board's sort order: what needs you first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum State {
    #[serde(rename = "needs you")]
    NeedsYou,
    #[serde(rename = "running")]
    Running,
    /// Open in a terminal, not the board's to enter.
    #[serde(rename = "elsewhere")]
    Elsewhere,
    #[serde(rename = "idle")]
    Idle,
    /// A background job whose state.json says it needs you or is working, while Claude Code has
    /// no process running it.
    #[serde(rename = "stale")]
    Stale,
    /// A transcript only.
    #[serde(rename = "ended")]
    Ended,
}

impl State {
    pub const ALL: [State; 6] =
        [State::NeedsYou, State::Running, State::Elsewhere, State::Idle, State::Stale, State::Ended];

    pub fn as_str(self) -> &'static str {
        match self {
            State::NeedsYou => "needs you",
            State::Running => "running",
            State::Elsewhere => "elsewhere",
            State::Idle => "idle",
            State::Stale => "stale",
            State::Ended => "ended",
        }
    }

    /// Position in the sort order, 0 = most urgent.
    pub fn rank(self) -> usize {
        self as usize
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of the board: a conversation, live or ended. Serialised whole for `board --dump` and
/// `--shot --data` (the 17 fields of `mockups/sample.json`, then `waiting_since` and `spans`);
/// `board --json` is a projection of it without transcript text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub project: String,
    pub state: State,
    /// Epoch seconds of the last activity.
    pub last: f64,
    /// Short id for `claude attach`, for background sessions.
    #[serde(default)]
    pub job: Option<String>,
    /// The first prompt you typed.
    #[serde(default)]
    pub asked: Option<String>,
    /// The latest prompt you typed, and how many you typed in all. Never in `--json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_asked: Option<String>,
    #[serde(default)]
    pub prompts: u32,
    /// Claude Code's own summary of a background session.
    #[serde(default)]
    pub summary: Option<String>,
    /// The last thing Claude said.
    #[serde(default)]
    pub said: Option<String>,
    /// A background session stopped with `claude stop`.
    #[serde(default)]
    pub stopped: bool,
    /// The uuid of the first message: the same for every copy of a conversation.
    #[serde(default)]
    pub root: Option<String>,
    /// What it needs from you (or last needed, when stale); secrets masked.
    #[serde(default)]
    pub waiting: Option<String>,
    /// When `waiting` is a tool call from the transcript: the tool and its argument (masked
    /// too), for the "Approve this tool call?" panels. Absent in older `--dump` files; never in
    /// `--json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_tool: Option<PendingTool>,
    /// For a stale session, the state its state.json still claims.
    #[serde(default)]
    pub was: Option<State>,
    /// The model of its last reply, e.g. `claude-opus-5-5`.
    #[serde(default)]
    pub model: Option<String>,
    /// Tokens in the context window at its last reply.
    #[serde(default)]
    pub context: u64,
    /// Output tokens over the whole conversation.
    #[serde(default)]
    pub output: u64,
    /// When it began waiting: the pending tool call's time, or the job's `updatedAt` when the
    /// wait comes from its state.json.
    #[serde(default)]
    pub waiting_since: Option<f64>,
    /// Activity spans from the transcript, oldest first.
    #[serde(default)]
    pub spans: Vec<Span>,
    /// Files it read or changed, most recent first. Never in `--json`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileTouch>,
    /// Where it is working now, when it moved after starting into a worktree, or from a
    /// workspace (`~`) into a repository. Its project and git state are this folder's; actions
    /// still run in `cwd`, where Claude Code keeps the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// The git state of its working folder now. Never in `--json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
}

impl Session {
    /// A row with only its identity set; the rest empty. For tests and fixtures.
    pub fn new(session_id: &str, title: &str, cwd: &str, project: &str, state: State, last: f64) -> Session {
        Session {
            session_id: session_id.into(),
            title: title.into(),
            cwd: cwd.into(),
            project: project.into(),
            state,
            last,
            job: None,
            asked: None,
            last_asked: None,
            prompts: 0,
            summary: None,
            said: None,
            stopped: false,
            root: None,
            waiting: None,
            pending_tool: None,
            was: None,
            model: None,
            context: 0,
            output: 0,
            waiting_since: None,
            spans: Vec::new(),
            files: Vec::new(),
            workdir: None,
            git: None,
        }
    }

    /// Where it is working now: [`Session::workdir`], else `cwd`.
    pub fn folder(&self) -> &str {
        self.workdir.as_deref().unwrap_or(&self.cwd)
    }

    fn sort_key(&self) -> (usize, f64) {
        (self.state.rank(), -self.last)
    }
}

/// Everything the board shows at one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When it was collected, epoch seconds.
    pub now: f64,
    /// Counts up with each collect, so a view can tell a new snapshot from the one it has.
    #[serde(default)]
    pub generation: u64,
    /// Sorted by state, then most recent first.
    pub sessions: Vec<Session>,
}

impl Snapshot {
    pub fn get(&self, session_id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.session_id == session_id)
    }
}

// --- projects -------------------------------------------------------------------------------------

/// How folders are grouped into projects.
#[derive(Clone, Debug, PartialEq)]
pub struct Projects {
    /// A repo or folder directly under one of these is a project of its own (`~/work/api`),
    /// even when the workspace itself is a git repo.
    pub workspaces: Vec<String>,
    /// Sessions in throwaway folders under here are one project, "scratch".
    pub scratch: String,
}

impl Projects {
    /// The project a folder belongs to: the scratch folder, a git repo, a workspace's child, else
    /// the folder itself.
    pub fn project_of(&self, cwd: &str) -> String {
        self.resolver().project_of(cwd)
    }

    /// A resolver that remembers git roots for one collect.
    pub fn resolver(&self) -> ProjectResolver<'_> {
        ProjectResolver { rules: self, git_roots: HashMap::new() }
    }
}

pub struct ProjectResolver<'a> {
    rules: &'a Projects,
    git_roots: HashMap<String, Option<String>>,
}

impl ProjectResolver<'_> {
    /// The project of a session launched in `cwd` that is now working in `workdir`. Claude Code
    /// moves a session's folder with every `cd`, so the move counts only when it plainly changed
    /// what the session works on: it is now in a linked worktree (it joins that repository), or
    /// it was launched from a workspace itself (`~`, `/opt`) and is now in a git repository.
    /// Also says whether the move counted.
    pub fn project_for(&mut self, cwd: &str, workdir: Option<&str>) -> (String, bool) {
        if let Some(w) = workdir {
            let in_worktree = Path::new(&self.plain_root(w)).join(".git").is_file();
            let from_workspace = self.rules.workspaces.iter().any(|ws| ws == cwd);
            if in_worktree || (from_workspace && self.git_root(w).is_some_and(|r| !self.rules.workspaces.contains(&r))) {
                return (self.project_of(w), true);
            }
        }
        (self.project_of(cwd), false)
    }

    /// The nearest folder at or above `path` holding `.git`, a worktree's own folder included.
    fn plain_root(&self, path: &str) -> String {
        Path::new(path)
            .ancestors()
            .find(|d| !d.as_os_str().is_empty() && d.join(".git").exists())
            .map(|d| crate::config::canon(&d.to_string_lossy()))
            .unwrap_or_else(|| "/nonexistent".into())
    }

    pub fn project_of(&mut self, cwd: &str) -> String {
        if cwd.is_empty() {
            return "?".into();
        }
        let scratch = &self.rules.scratch;
        if cwd == scratch || cwd.starts_with(&format!("{scratch}/")) {
            return scratch.clone();
        }
        let root = self.git_root(cwd);
        if let Some(r) = &root
            && !self.rules.workspaces.contains(r)
        {
            return r.clone();
        }
        let mut workspaces: Vec<&String> = self.rules.workspaces.iter().collect();
        workspaces.sort_by_key(|w| std::cmp::Reverse(w.len()));
        for w in workspaces {
            if cwd == w {
                return w.clone();
            }
            if let Some(rest) = cwd.strip_prefix(&format!("{w}/")) {
                let child = rest.split('/').next().unwrap_or("");
                return crate::config::canon(&Path::new(w).join(child).to_string_lossy());
            }
        }
        root.unwrap_or_else(|| cwd.to_string())
    }

    /// The nearest folder at or above `path` holding `.git`. For a linked worktree (`.git` is a
    /// file, `gitdir: <repo>/.git/worktrees/<name>`), the main repository's folder, so a
    /// session in a worktree joins its repository's project.
    fn git_root(&mut self, path: &str) -> Option<String> {
        if let Some(hit) = self.git_roots.get(path) {
            return hit.clone();
        }
        let mut dir = Some(Path::new(path));
        let mut found = None;
        while let Some(d) = dir.filter(|d| !d.as_os_str().is_empty() && *d != Path::new("/")) {
            let dot = d.join(".git");
            if dot.is_file() {
                found = Some(main_repo(&dot).unwrap_or_else(|| crate::config::canon(&d.to_string_lossy())));
                break;
            }
            if dot.exists() {
                found = Some(crate::config::canon(&d.to_string_lossy()));
                break;
            }
            dir = d.parent();
        }
        self.git_roots.insert(path.to_string(), found.clone());
        found
    }
}

/// The main repository of a linked worktree, from its `.git` file.
fn main_repo(dot_git: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    // A relative gitdir (`worktree.useRelativePaths`) is relative to the worktree; `..` is
    // resolved by name, not through links.
    let mut resolved = PathBuf::new();
    for c in dot_git.parent()?.join(gitdir).components() {
        match c {
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    let gitdir = resolved;
    // <repo>/.git/worktrees/<name>
    let worktrees = gitdir.parent().filter(|p| p.file_name().is_some_and(|n| n == "worktrees"))?;
    let repo_git = worktrees.parent().filter(|p| p.file_name().is_some_and(|n| n == ".git"))?;
    Some(crate::config::canon(&repo_git.parent()?.to_string_lossy()))
}

/// Sessions by project: the project with the most urgent session first, then the most recent.
pub fn group(sessions: &[Session]) -> Vec<(String, Vec<&Session>)> {
    let mut groups: Vec<(String, Vec<&Session>)> = Vec::new();
    for s in sessions {
        match groups.iter_mut().find(|(p, _)| *p == s.project) {
            Some((_, items)) => items.push(s),
            None => groups.push((s.project.clone(), vec![s])),
        }
    }
    for (_, items) in &mut groups {
        items.sort_by(|a, b| cmp_key(a.sort_key(), b.sort_key()));
    }
    let key = |items: &Vec<&Session>| {
        let rank = items.iter().map(|s| s.state.rank()).min().unwrap_or(usize::MAX);
        let latest = items.iter().map(|s| s.last).fold(f64::MIN, f64::max);
        (rank, -latest)
    };
    groups.sort_by(|a, b| cmp_key(key(&a.1), key(&b.1)));
    groups
}

fn cmp_key(a: (usize, f64), b: (usize, f64)) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
}

// --- collecting ----------------------------------------------------------------------------------

/// Where the facts come from, with every path explicit so tests never touch the real `~/.claude`.
#[derive(Clone, Debug)]
pub struct Sources {
    pub claude_dir: PathBuf,
    pub cache_path: PathBuf,
    pub deleted_path: PathBuf,
    /// The folder for sessions that name none.
    pub home: String,
    /// How far back ended sessions go.
    pub days: u32,
    pub projects: Projects,
    /// The claude CLI, split into words.
    pub claude: Vec<String>,
}

impl Sources {
    pub fn from_config(c: &Config) -> Sources {
        Sources {
            claude_dir: c.claude_dir.clone().into(),
            cache_path: c.cache_path(),
            deleted_path: c.deleted_path(),
            home: c.home.clone(),
            days: c.days,
            projects: Projects { workspaces: c.workspaces.clone(), scratch: c.scratch.clone() },
            claude: c.claude_argv(),
        }
    }
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Every session the board shows. Starts `claude agents` first so it runs while transcripts
/// are parsed, then waits for it up to [`agents::TIMEOUT`], falling back to the files.
pub fn collect(src: &Sources, generation: u64) -> Snapshot {
    let now = now();
    let cli = agents::start(&src.claude);
    let transcripts = cache::load_transcripts(&src.claude_dir, &src.cache_path, src.days, now);
    let deleted = store::load_deleted(&src.deleted_path);
    let agents = cli.finish(agents::TIMEOUT).unwrap_or_else(|| agents::from_files(&src.claude_dir));
    let mut sessions = build(src, &transcripts, &agents, &deleted);
    let repos = git::read_all(sessions.iter().map(Session::folder));
    for s in &mut sessions {
        s.git = repos.get(s.folder()).cloned();
    }
    Snapshot { now, generation, sessions }
}

/// [`collect`] with the agents given rather than asked for, at a given time. For tests.
pub fn collect_with(src: &Sources, agents: &[Agent], now: f64) -> Vec<Session> {
    let transcripts = cache::load_transcripts(&src.claude_dir, &src.cache_path, src.days, now);
    let deleted = store::load_deleted(&src.deleted_path);
    build(src, &transcripts, agents, &deleted)
}

/// Rows from transcripts (newest first), agents and the deleted list.
fn build(src: &Sources, transcripts: &[(String, TranscriptInfo)], agents: &[Agent], deleted: &Deleted) -> Vec<Session> {
    let by_id: HashMap<&str, &TranscriptInfo> = transcripts.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let empty = TranscriptInfo::default();
    let workers = roster::live_workers(&src.claude_dir);
    let mut projects = src.projects.resolver();
    let mut out: Vec<Session> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    // Transcripts a background session continues from, and a job's conversation from before /clear.
    let mut resumed: HashSet<String> = HashSet::new();
    let mut before_clear: HashSet<String> = HashSet::new();
    let interactive: HashSet<&str> =
        agents.iter().filter(|a| a.is_interactive()).filter_map(|a| a.session_id.as_deref()).collect();

    for a in agents.iter().filter(|a| a.is_background()) {
        let (Some(first_sid), Some(job)) = (&a.session_id, &a.id) else { continue };
        let st = JobState::read(&src.claude_dir, job);
        let mut sid = first_sid.clone();
        if let Some(current) = &st.resume_session_id {
            resumed.insert(current.clone());
            if *current != sid && by_id.get(current.as_str()).is_some_and(|t| t.cleared) {
                // After a /clear the job keeps its first session id; resumeSessionId names the
                // conversation it is on now, and the first one is an ended conversation of its own.
                before_clear.insert(sid);
                sid = current.clone();
            }
        }
        if interactive.contains(sid.as_str()) {
            continue; // the conversation went on in a terminal (`claude --continue`); that row wins
        }
        let t = by_id.get(sid.as_str()).copied().unwrap_or(&empty);
        let cwd = pick([&a.cwd, &st.cwd, &t.cwd]).unwrap_or_else(|| src.home.clone());
        let mut state = st.board_state();
        let stopped = st.stopped();
        let mut was = None;
        let claims = matches!(state, State::NeedsYou | State::Running);
        if claims && !stopped && workers.as_ref().is_some_and(|w| !w.contains(job)) {
            was = Some(state);
            state = State::Stale;
        }
        let (waiting, waiting_since, pending_tool) = if was.unwrap_or(state) == State::NeedsYou {
            let needs = mask_secrets(&flat(st.needs.as_deref().unwrap_or("")));
            if !needs.is_empty() {
                (Some(needs), (st.updated_at > 0.0).then_some(st.updated_at), None)
            } else {
                (t.pending.clone(), t.pending_since, t.pending_tool.clone())
            }
        } else {
            (None, None, None)
        };
        let title = pick([&a.name, &st.name, &t.custom, &t.ai]).unwrap_or_else(|| t.first_line());
        let mut s = row(&sid, title, cwd, state, t.last.max(st.updated_at), t, &mut projects);
        s.job = Some(job.clone());
        // While a turn starts, Claude Code's status is often just the prompt it was given: that's
        // not a summary, and the prompt is shown already.
        s.summary = st.summary().filter(|m| !echoes(m, s.last_asked.as_deref()));
        s.stopped = stopped;
        s.waiting = waiting;
        s.waiting_since = waiting_since;
        s.pending_tool = pending_tool;
        s.was = was;
        // Two jobs on one conversation: one row, the later job's, as a map would have it.
        match out.iter().position(|x| x.session_id == sid) {
            Some(i) => out[i] = s,
            None => out.push(s),
        }
        taken.insert(sid);
    }

    for a in agents.iter().filter(|a| a.is_interactive()) {
        let Some(sid) = &a.session_id else { continue };
        if taken.contains(sid) {
            continue;
        }
        let Some(t) = by_id.get(sid.as_str()).filter(|t| t.typed > 0) else {
            continue; // no prompt yet
        };
        let cwd = pick([&a.cwd, &t.cwd]).unwrap_or_else(|| src.home.clone());
        // A conversation begun with /clear carries the session's old name as its own; the
        // terminal's name for it is the better title then.
        let title = a
            .name
            .clone()
            .filter(|_| t.cleared)
            .or_else(|| t.own_title().map(String::from))
            .or_else(|| a.name.clone())
            .unwrap_or_else(|| t.first_line());
        out.push(row(sid, title, cwd, State::Elsewhere, t.last, t, &mut projects));
        taken.insert(sid.clone());
    }

    let live_roots: HashSet<&str> = taken.iter().filter_map(|sid| by_id.get(sid.as_str())?.root.as_deref()).collect();
    let mut ended_roots: HashSet<&str> = HashSet::new();
    for (sid, t) in transcripts {
        if taken.contains(sid) || t.entry.as_deref() != Some("cli") || t.typed == 0 {
            continue; // automation (plugins, headless runs), or opened and closed without a prompt
        }
        let root = t.root.as_deref();
        if resumed.contains(sid) || root.is_some_and(|r| live_roots.contains(r) || ended_roots.contains(r)) {
            continue; // an older copy of a conversation already on the board
        }
        if store::is_deleted(deleted, sid, root, t.last) {
            continue; // deleted from the board; `claude rm` keeps the transcript
        }
        if let Some(r) = root {
            ended_roots.insert(r);
        }
        let title = if before_clear.contains(sid) {
            // The job's name most likely came from this conversation, its first after the job began.
            let name = pick([&t.custom, &t.agent, &t.ai]).unwrap_or_else(|| t.first_line());
            format!("{name} (before /clear)")
        } else {
            t.own_title().map(String::from).unwrap_or_else(|| t.first_line())
        };
        let cwd = t.cwd.clone().unwrap_or_else(|| src.home.clone());
        out.push(row(sid, title, cwd, State::Ended, t.last, t, &mut projects));
    }

    // Stable: equal keys keep the order above (jobs, terminals, then ended newest first).
    out.sort_by(|a, b| cmp_key(a.sort_key(), b.sort_key()));
    out
}

/// A row with what every kind takes from its transcript.
fn row(sid: &str, title: String, cwd: String, state: State, last: f64, t: &TranscriptInfo, projects: &mut ProjectResolver) -> Session {
    // Where it works now, when that differs from where it was launched and still exists.
    // Kept only when it moved what the session works on (see `project_for`), so its project,
    // folder and git state agree.
    let workdir = t.cwd_last.clone().filter(|w| *w != cwd && Path::new(w).is_dir());
    let (project, moved) = projects.project_for(&cwd, workdir.as_deref());
    let workdir = workdir.filter(|_| moved);
    Session {
        workdir,
        asked: t.asked.clone(),
        last_asked: t.last_asked.clone(),
        prompts: t.typed,
        said: t.said.clone(),
        root: t.root.clone(),
        model: t.model.clone(),
        context: t.context,
        output: t.output,
        spans: t.spans.clone(),
        files: t.files.clone(),
        ..Session::new(sid, &title, &cwd, &project, state, last)
    }
}

/// Whether `status` only repeats `prompt` (possibly cut short, with or without an ellipsis).
fn echoes(status: &str, prompt: Option<&str>) -> bool {
    let Some(prompt) = prompt else { return false };
    let norm = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let (status, prompt) = (norm(status), norm(prompt));
    let status = status.trim_end_matches(['…', '.']).trim_end();
    !status.is_empty() && (prompt.starts_with(status) || status.starts_with(prompt.as_str()))
}

/// The first value that is set.
fn pick<const N: usize>(candidates: [&Option<String>; N]) -> Option<String> {
    candidates.into_iter().find_map(|c| c.clone())
}
