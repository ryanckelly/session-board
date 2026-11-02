//! The `board` binary end to end, against a fixture tree, with the environment cleared so the
//! machine's own config and `~/.claude` are never read.

use board::facts::Snapshot;
use board::proc::{self, RunError};
use serde_json::{Value, json};
use std::process::Command;
use std::time::Duration;

mod common;
use common::*;

/// Asked, said, waiting and summary text the fixture puts in its transcripts and job.
const SECRETS: [&str; 4] = ["ASKED-the-private-question", "SAID-the-private-answer", "NEEDS-approval-text", "SUMMARY-of-work"];

fn fixture() -> TempDir {
    let tmp = TempDir::new();
    let c = tmp.join("claude");
    let work = tmp.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let cwd = work.to_string_lossy().into_owned();
    let now = board::facts::now();
    transcript("job-sid", &cwd).prompts(&[SECRETS[0]]).reply(SECRETS[1]).ts(now - 400.0).write(&c);
    job("jobid001", "job-sid", &cwd).tempo("blocked").needs(SECRETS[2]).result(SECRETS[3]).name("the job").write(&c);
    // Titled, since an untitled row is titled by its first prompt's first line.
    transcript("ended-sid", &cwd).prompts(&[SECRETS[0]]).reply(SECRETS[1]).title("ended one").ts(now - 7300.0).write(&c);
    tmp
}

fn board(tmp: &TempDir, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_board"))
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path())
        .env("TMPDIR", tmp.join("scratch"))
        .env("SESSION_BOARD_CONFIG", tmp.join("no-such-config.toml"))
        .env("SESSION_BOARD_CLAUDE", "/nonexistent/claude") // the CLI fails: the files stand in
        .env("CLAUDE_CONFIG_DIR", tmp.join("claude"))
        .env("XDG_CACHE_HOME", tmp.join("cache"))
        .env("XDG_STATE_HOME", tmp.join("state"))
        .output()
        .unwrap();
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (text(out.stdout), text(out.stderr), out.status.code().unwrap_or(-1))
}

#[test]
fn json_has_exactly_the_twelve_fields_and_no_transcript_text() {
    let tmp = fixture();
    let (out, err, code) = board(&tmp, &["--json"]);
    assert_eq!(code, 0, "{err}");
    for secret in SECRETS {
        assert!(!out.contains(secret), "--json leaks {secret}");
    }
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.len(), 2);
    let keys: Vec<&str> = rows[0].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["session_id", "job", "title", "state", "stopped", "was", "project", "cwd", "last", "model", "context", "output"]
    );
    assert_eq!((&rows[0]["session_id"], &rows[0]["state"], &rows[0]["job"]), (&json!("job-sid"), &json!("needs you"), &json!("jobid001")));
    assert_eq!((&rows[1]["state"], &rows[1]["job"], &rows[1]["was"]), (&json!("ended"), &Value::Null, &Value::Null));
    assert!(rows[0]["last"].is_f64());
}

#[test]
fn the_cache_lands_in_its_own_private_file() {
    let tmp = fixture();
    board(&tmp, &["--json"]);
    let path = tmp.join("cache/session-board/transcripts-rs.json");
    assert!(path.is_file());
    assert_mode(&path, 0o600);
    assert_mode(path.parent().unwrap(), 0o700);
}

#[test]
fn list_prints_projects_rows_and_what_a_row_needs() {
    let tmp = fixture();
    let (out, _, code) = board(&tmp, &["--list"]);
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "work  (~/work)");
    assert_eq!(lines[1], "  ◆ the job  — needs you, 6m  [jobid001]");
    assert_eq!(lines[2], format!("      needs: {}", SECRETS[2]));
    assert_eq!(lines[3], "  ○ ended one  — ended, 2h");
}

#[test]
fn dump_is_the_full_snapshot() {
    let tmp = fixture();
    let (out, _, code) = board(&tmp, &["--dump"]);
    assert_eq!(code, 0);
    let snap: Snapshot = serde_json::from_str(&out).unwrap();
    let job = snap.get("job-sid").unwrap();
    assert_eq!(job.summary.as_deref(), Some(SECRETS[3]));
    assert_eq!(job.waiting.as_deref(), Some(SECRETS[2]));
    assert!(job.waiting_since.is_some());
    assert_eq!(job.spans.len(), 1);
}

#[test]
fn config_help_and_bad_options() {
    let tmp = TempDir::new();
    let (out, _, code) = board(&tmp, &["--config"]);
    assert_eq!(code, 0);
    assert!(out.contains("(not found; defaults apply)"));
    assert!(out.contains(&format!("  scratch     {}    [default]", tmp.join("scratch").display())));
    let (out, _, code) = board(&tmp, &["--help"]);
    assert_eq!(code, 0);
    for word in ["needs you", "elsewhere", "stale", "ended", "Ctrl+K", "--json"] {
        assert!(out.contains(word), "--help lacks {word}");
    }
    let (_, err, code) = board(&tmp, &["--frobnicate"]);
    assert_eq!(code, 2);
    assert!(err.contains("--frobnicate"));
}

#[test]
fn a_bad_config_stops_with_a_message() {
    let tmp = TempDir::new();
    std::fs::write(tmp.join("no-such-config.toml"), "days = 0\n").unwrap();
    let (_, err, code) = board(&tmp, &["--json"]);
    assert_eq!(code, 1);
    assert!(err.starts_with("session-board: days must be a positive number, not 0"), "{err}");
}

#[test]
fn the_sample_fixture_reads_as_a_snapshot() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.json")).unwrap();
    let snap: Snapshot = serde_json::from_str(&text).unwrap();
    assert!(!snap.sessions.is_empty());
    assert!(snap.sessions.iter().any(|s| s.state == board::facts::State::NeedsYou));
}

#[test]
fn a_command_past_its_deadline_is_killed() {
    let started = std::time::Instant::now();
    let r = proc::run(&["sleep".into(), "5".into()], None, Duration::from_millis(200));
    assert!(matches!(r, Err(RunError::Timeout(_))));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_command_s_output_and_exit_code_are_kept() {
    let argv = ["sh", "-c", "echo out; echo err >&2; exit 3"].map(String::from);
    let out = proc::run(&argv, Some(std::path::Path::new("/")), Duration::from_secs(5)).unwrap();
    assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (Some(3), "out\n", "err\n"));
}
