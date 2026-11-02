//! What the board does to sessions: the rules for what is allowed (and in plain words why not),
//! and the `claude` CLI commands that do it. Always through the CLI, never by editing `~/.claude`.
//!
//! Getting in: attach to a background session, or resume an ended one in the background first.
//! Inside an attached session, Ctrl+Z ends `claude attach` with code 0, and so does ← then Esc;
//! the session keeps running either way. So the board suspends itself, runs `claude attach`, and
//! takes the terminal back when it exits.
//!
//! New, stop, delete (checked on Claude Code 2.1.316):
//! - `claude --bg [--name N] [-- prompt]` prints `backgrounded · <id> · <name>`. With no prompt the
//!   session runs no turn and waits, like a resumed one.
//! - `claude stop <id>` keeps the conversation; the job reports state "stopped" and `claude attach`
//!   opens it again. Stopping twice also exits 0.
//! - `claude rm <id>` deletes the job (and its worktree when safe) but keeps the transcript. A
//!   second `rm` fails: "No job matching '<id>'".

use crate::facts::{Session, State};
use crate::proc::{self, Output, RunError};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

/// CLI calls that finish on their own get this long. `claude attach` gets no limit.
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// Why something can't be done, in plain words for a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refuse<T>(message: impl Into<String>) -> Result<T, Refused> {
    Err(Refused(message.into()))
}

/// How to get into a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Enter {
    /// A background session: `claude attach <job>`.
    Attach { job: String },
    /// An ended session: `claude --bg --resume <id>` in its folder, then attach.
    ResumeThenAttach { session_id: String, cwd: String },
}

pub fn plan_enter(s: &Session) -> Result<Enter, Refused> {
    if s.state == State::Elsewhere {
        return refuse(format!(
            "“{}” is open in another terminal. Leave it there, or send it to the background with /bg, and it \
             can be entered from here.",
            s.title
        ));
    }
    if let Some(job) = &s.job {
        return Ok(Enter::Attach { job: job.clone() });
    }
    if !Path::new(&s.cwd).is_dir() {
        return refuse(format!("“{}” can't be resumed: its folder {} no longer exists.", s.title, s.cwd));
    }
    Ok(Enter::ResumeThenAttach { session_id: s.session_id.clone(), cwd: s.cwd.clone() })
}

/// A checked request from the new-session form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSession {
    pub cwd: String,
    pub name: String,
    pub prompt: String,
}

/// Check what was typed in the new-session form: the folder trimmed, `~` expanded, made absolute
/// and required to exist; the name's whitespace collapsed; a name or a prompt required.
pub fn plan_new(folder: &str, name: &str, prompt: &str, home: &str) -> Result<NewSession, Refused> {
    let folder = folder.trim();
    if folder.is_empty() {
        return refuse("Say which folder the session should start in.");
    }
    let folder = if folder == "~" || folder.starts_with("~/") { format!("{home}{}", &folder[1..]) } else { folder.into() };
    let folder = crate::config::normalize(&crate::config::absolute(&folder));
    if !Path::new(&folder).is_dir() {
        return refuse(format!("There's no folder {folder}."));
    }
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let prompt = prompt.trim().to_string();
    if name.is_empty() && prompt.is_empty() {
        return refuse("Give the session a name or a first prompt.");
    }
    Ok(NewSession { cwd: folder, name, prompt })
}

/// The job to stop, or why not.
pub fn plan_stop(s: &Session) -> Result<String, Refused> {
    if s.state == State::Elsewhere {
        return refuse(format!("“{}” is open in another terminal; stop it there.", s.title));
    }
    let Some(job) = &s.job else {
        return refuse(format!("“{}” has already ended; there's nothing to stop.", s.title));
    };
    if s.stopped {
        return refuse(format!("“{}” is already stopped. Enter opens it again.", s.title));
    }
    Ok(job.clone())
}

/// The job to delete, or why not. `days` is how long Claude Code keeps transcripts.
pub fn plan_delete(s: &Session, days: u32) -> Result<String, Refused> {
    if s.state == State::Elsewhere {
        return refuse(format!(
            "“{}” is open in another terminal. Only background sessions can be deleted from here.",
            s.title
        ));
    }
    let Some(job) = &s.job else {
        return refuse(format!(
            "“{}” has ended and is only a saved conversation. Claude Code can't delete one conversation on its \
             own (only a whole project's data), so it stays until Claude Code clears it after {days} days.",
            s.title
        ));
    };
    Ok(job.clone())
}

/// The confirm text for stopping a session that is working right now (others stop at once).
pub fn stop_confirm(s: &Session) -> Option<String> {
    (s.state == State::Running).then(|| {
        format!(
            "“{}” is working right now. Stop it?\n\nIts conversation is kept; Enter opens it again.",
            s.title
        )
    })
}

/// The confirm text for deleting a session; it always asks.
pub fn delete_confirm(s: &Session, days: u32) -> String {
    let running = if s.state == State::Running { " It's working right now; deleting stops it." } else { "" };
    format!(
        "Delete “{}”?{running}\n\nThis removes the background session, and its worktree if it has one. It can't \
         be undone. The board stops showing the conversation; Claude Code keeps its transcript until it clears \
         old ones ({days} days).",
        s.title
    )
}

/// Arguments for `claude` that start a new background session. The prompt goes after `--`, so
/// one starting with `-` isn't read as an option.
pub fn new_argv(req: &NewSession) -> Vec<String> {
    let mut argv = vec!["--bg".to_string()];
    if !req.name.is_empty() {
        argv.extend(["--name".into(), req.name.clone()]);
    }
    if !req.prompt.is_empty() {
        argv.extend(["--".into(), req.prompt.clone()]);
    }
    argv
}

// --- running the CLI ----------------------------------------------------------------------------

/// Runs a full command line in a folder. The real one has a [`TIMEOUT`]; tests use a fake.
pub trait Runner {
    fn run(&self, argv: &[String], cwd: &str) -> Result<Output, RunError>;
}

/// Runs commands for real.
pub struct SystemRunner;

impl Runner for SystemRunner {
    /// Only the `claude --bg` family gets the board's stdin; `stop` and `rm` run on a thread
    /// while the board reads keys, so they get none and can't take a keypress.
    fn run(&self, argv: &[String], cwd: &str) -> Result<Output, RunError> {
        let bg = argv.iter().any(|a| a == "--bg");
        proc::spawn(argv, Some(Path::new(cwd)), bg, true)?.wait(TIMEOUT)
    }
}

static BACKGROUNDED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"backgrounded · ([0-9a-f]{6,})").unwrap());

/// The `claude` CLI, as configured, run through a [`Runner`].
pub struct Cli<R: Runner = SystemRunner> {
    /// The claude command split into words (`["claude"]`, or `["ssh", "box", "claude"]`).
    pub claude: Vec<String>,
    /// Where commands that act on a job by id run.
    pub home: String,
    pub runner: R,
}

impl<R: Runner> Cli<R> {
    /// Run a command that finishes on its own; its output, or why it failed.
    fn call(&self, args: &[String], cwd: &str, failed: &str) -> Result<String, Refused> {
        let argv: Vec<String> = self.claude.iter().chain(args).cloned().collect();
        let out = self.runner.run(&argv, cwd).map_err(|e| Refused(format!("{failed}: {e}")))?;
        if out.code != Some(0) {
            let said = if out.stderr.is_empty() { &out.stdout } else { &out.stderr };
            let said = said.trim();
            let why = if said.is_empty() { exit_text(out.code) } else { said.to_string() };
            return refuse(format!("{failed}: {why}"));
        }
        Ok(out.stdout)
    }

    /// Bring an ended session back as an idle background session and return its short id. It
    /// runs no turn: `claude --bg --resume` without a prompt waits for one.
    pub fn resume_in_background(&self, session_id: &str, cwd: &str) -> Result<String, Refused> {
        let args = ["--bg", "--resume", session_id].map(String::from);
        let out = self.call(&args, cwd, "Couldn't resume the session")?;
        // The short id is the session id's first 8 characters, should the output change.
        Ok(short_id(&out).unwrap_or_else(|| session_id.chars().take(8).collect()))
    }

    /// Start a background session and return its short id.
    pub fn start_in_background(&self, req: &NewSession) -> Result<String, Refused> {
        let out = self.call(&new_argv(req), &req.cwd, "Couldn't start the session")?;
        short_id(&out).ok_or_else(|| {
            let shown = if out.trim().is_empty() { "(no output)" } else { out.trim() };
            Refused(format!("Started a session but couldn't read its id from: {shown}"))
        })
    }

    pub fn stop(&self, job: &str) -> Result<(), Refused> {
        self.call(&["stop".into(), job.into()], &self.home, "Couldn't stop the session").map(drop)
    }

    /// `claude rm`. The caller also forgets the conversation (`store::forget`), since the
    /// transcript stays and would show again as ended.
    pub fn delete(&self, job: &str) -> Result<(), Refused> {
        self.call(&["rm".into(), job.into()], &self.home, "Couldn't delete the session").map(drop)
    }

    /// The command line for `claude attach` and the folder to run it in. It owns the terminal
    /// until it exits, so the caller runs it with inherited stdio and no timeout.
    pub fn attach_argv(&self, job: &str) -> (Vec<String>, String) {
        let argv = self.claude.iter().cloned().chain(["attach".into(), job.into()]).collect();
        (argv, self.home.clone())
    }
}

fn short_id(out: &str) -> Option<String> {
    BACKGROUNDED.captures(out).map(|c| c[1].to_string())
}

fn exit_text(code: Option<i32>) -> String {
    match code {
        Some(c) => format!("exit code {c}"),
        None => "killed by a signal".into(),
    }
}

// --- where the cursor and the form start -------------------------------------------------------

/// What the cursor rests on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cursor {
    Session(String),
    Project(String),
}

/// Where the cursor goes when a session's row goes away: the next session in its project, else
/// the one before, else the project. `rows` is what is on screen: each project with its session
/// ids in order. `None` if the session isn't shown.
pub fn after_delete(rows: &[(String, Vec<String>)], session_id: &str) -> Option<Cursor> {
    let (project, ids) = rows.iter().find(|(_, ids)| ids.iter().any(|id| id == session_id))?;
    let i = ids.iter().position(|id| id == session_id)?;
    let next = ids.get(i + 1).or_else(|| i.checked_sub(1).and_then(|j| ids.get(j)));
    Some(match next {
        Some(id) => Cursor::Session(id.clone()),
        None => Cursor::Project(project.clone()),
    })
}

/// The folder the new-session form offers: the highlighted session's folder if it still exists,
/// else its project's (or the highlighted project's). The scratch project's own folder is the
/// temp dir itself, so it offers its most recent session's folder instead. Else home.
pub fn default_folder(cursor: Option<&Cursor>, sessions: &[Session], scratch: &str, home: &str) -> String {
    let is_dir = |p: &str| Path::new(p).is_dir();
    let project = match cursor {
        Some(Cursor::Session(id)) => {
            let Some(s) = sessions.iter().find(|s| s.session_id == *id) else { return home.into() };
            if is_dir(&s.cwd) {
                return s.cwd.clone();
            }
            s.project.clone()
        }
        Some(Cursor::Project(p)) => p.clone(),
        None => return home.into(),
    };
    if project == scratch {
        let mut mine: Vec<&Session> = sessions.iter().filter(|t| t.project == scratch && t.cwd != scratch).collect();
        mine.sort_by(|a, b| b.last.total_cmp(&a.last));
        if let Some(t) = mine.into_iter().find(|t| is_dir(&t.cwd)) {
            return t.cwd.clone();
        }
    }
    if is_dir(&project) { project } else { home.into() }
}
