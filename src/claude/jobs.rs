//! A background session's `jobs/<id>/state.json`: its tempo and Claude Code's own summary.

use super::{epoch, path_field, read_json, string, text};
use crate::facts::State;
use serde_json::Value;
use std::path::Path;

/// A session resumed with `claude --bg --resume` and no prompt reports tempo "blocked" with this
/// detail. It is waiting for a first prompt, not for you, so it counts as idle.
const IDLE_PLACEHOLDER: &str = "(idle";

/// The fields of `state.json` the board reads. Empty strings count as absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobState {
    pub state: Option<String>,
    /// Where status lives: blocked (needs you), active (working), idle.
    pub tempo: Option<String>,
    /// A one-line status.
    pub detail: Option<String>,
    /// What it waits for, in Claude Code's words.
    pub needs: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub updated_at: f64,
    /// The transcript the job is on now (after a /clear), or the one it was resumed from.
    pub resume_session_id: Option<String>,
    pub name: Option<String>,
    /// `output.result`: Claude Code's summary of the finished work.
    pub result: Option<String>,
}

impl JobState {
    pub fn from_value(v: &Value) -> JobState {
        JobState {
            state: string(v, "state"),
            tempo: string(v, "tempo"),
            detail: string(v, "detail"),
            needs: string(v, "needs"),
            session_id: string(v, "sessionId"),
            cwd: path_field(v, "cwd"),
            updated_at: epoch(v.get("updatedAt")),
            resume_session_id: string(v, "resumeSessionId"),
            name: string(v, "name"),
            result: v.get("output").and_then(|o| text(o, "result")).map(String::from),
        }
    }

    /// The job's `state.json`, or an empty state if it is missing or damaged.
    pub fn read(claude_dir: &Path, job: &str) -> JobState {
        read_json(&claude_dir.join("jobs").join(job).join("state.json"))
            .filter(Value::is_object)
            .map(|v| JobState::from_value(&v))
            .unwrap_or_default()
    }

    fn idle_placeholder(&self) -> bool {
        self.detail.as_deref().is_some_and(|d| d.starts_with(IDLE_PLACEHOLDER))
    }

    /// The board state Claude Code reports. Status lives in `tempo`, not `state`: a job mid-turn
    /// reports state "blocked" with tempo "active".
    pub fn board_state(&self) -> State {
        let tempo = self.tempo.as_deref();
        if (tempo == Some("blocked") || self.state.as_deref() == Some("needs_input")) && !self.idle_placeholder() {
            State::NeedsYou
        } else if tempo == Some("active") {
            State::Running
        } else {
            State::Idle
        }
    }

    /// Stopped with `claude stop`; `claude attach` opens it again.
    pub fn stopped(&self) -> bool {
        self.state.as_deref() == Some("stopped")
    }

    /// Claude Code's own summary: the result, else the status line unless it is the idle
    /// placeholder.
    pub fn summary(&self) -> Option<String> {
        self.result.clone().or_else(|| self.detail.clone().filter(|_| !self.idle_placeholder()))
    }
}
