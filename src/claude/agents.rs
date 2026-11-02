//! The live sessions: `claude agents --json --all`, or, when the CLI fails or hangs, the same list
//! rebuilt from the jobs folder and the live-process registry.

use super::{glob, path_field, read_json, registry, string};
use crate::proc;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// How long `claude agents` gets before the files stand in for it.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// One entry of `claude agents --json --all`. Newer fields (`state`, `status`, `startedAt`) are
/// ignored on purpose: a job's status comes from its `state.json` tempo.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Agent {
    /// The job's short id, for background sessions.
    pub id: Option<String>,
    /// `background` or `interactive`.
    pub kind: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
}

impl Agent {
    pub fn from_value(v: &Value) -> Option<Agent> {
        v.is_object().then(|| Agent {
            id: string(v, "id"),
            kind: string(v, "kind"),
            session_id: string(v, "sessionId"),
            cwd: path_field(v, "cwd"),
            name: string(v, "name"),
        })
    }

    pub fn is_background(&self) -> bool {
        self.kind.as_deref() == Some("background")
    }

    pub fn is_interactive(&self) -> bool {
        self.kind.as_deref() == Some("interactive")
    }
}

/// Parse the CLI's JSON: a list of agents, or `None` if it isn't one.
pub fn parse(json: &str) -> Option<Vec<Agent>> {
    let v: Value = serde_json::from_str(json).ok()?;
    Some(v.as_array()?.iter().filter_map(Agent::from_value).collect())
}

/// `claude agents --json --all`, started so it runs while transcripts are parsed.
pub struct Pending(Option<proc::Running>);

pub fn start(claude: &[String]) -> Pending {
    let argv: Vec<String> = claude.iter().cloned().chain(["agents", "--json", "--all"].map(String::from)).collect();
    Pending(proc::spawn(&argv, None, false, false).ok())
}

impl Pending {
    /// The CLI's list, or `None` if it failed, hung past `timeout` or printed something else.
    pub fn finish(self, timeout: Duration) -> Option<Vec<Agent>> {
        let out = self.0?.wait(timeout).ok()?;
        if out.code != Some(0) {
            return None;
        }
        parse(&out.stdout)
    }
}

/// What `claude agents --json --all` reports, rebuilt from files, for when the CLI fails:
/// every job under `jobs/` and every live interactive session in `sessions/`.
pub fn from_files(claude_dir: &Path) -> Vec<Agent> {
    let mut out = Vec::new();
    for dir in glob(&claude_dir.join("jobs"), "") {
        let Some(st) = read_json(&dir.join("state.json")).filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
        else {
            continue;
        };
        out.push(Agent {
            id: dir.file_name().map(|n| n.to_string_lossy().into_owned()),
            kind: Some("background".into()),
            session_id: string(&st, "sessionId"),
            cwd: path_field(&st, "cwd"),
            name: string(&st, "name"),
        });
    }
    for path in glob(&claude_dir.join("sessions"), ".json") {
        let Some(r) = read_json(&path) else { continue };
        if r.get("kind").and_then(Value::as_str) == Some("interactive")
            && registry::alive(r.get("pid"), r.get("procStart"))
        {
            out.push(Agent {
                id: None,
                kind: Some("interactive".into()),
                session_id: string(&r, "sessionId"),
                cwd: path_field(&r, "cwd"),
                name: string(&r, "name"),
            });
        }
    }
    out
}
