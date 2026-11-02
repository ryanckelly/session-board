//! Twins of tests/test_config.py.

use board::config::{self, Config, ConfigError};
use std::collections::HashMap;

mod common;
use common::TempDir;

/// `SESSION_BOARD_WORKSPACES` is separated like PATH: `:` on Unix, `;` on Windows.
const SEP: &str = if cfg!(windows) { ";" } else { ":" };

fn load(tmp: &TempDir, env: &[(&str, &str)], toml: Option<&str>) -> Result<Config, ConfigError> {
    let path = tmp.join("config.toml");
    if let Some(t) = toml {
        std::fs::write(&path, t).unwrap();
    }
    let mut map: HashMap<String, String> =
        [("HOME", "/home/u"), ("SESSION_BOARD_CONFIG", path.to_str().unwrap())].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    map.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    config::load(&map)
}

#[test]
fn defaults() {
    let tmp = TempDir::new();
    let c = load(&tmp, &[], None).unwrap();
    assert_eq!(c.workspaces, ["/home/u"]);
    // No $TMPDIR in this environment: /tmp on Unix, the system temp folder on Windows.
    let tmp_default = if cfg!(windows) { common::path_str(&std::env::temp_dir()).trim_end_matches('/').to_string() } else { "/tmp".into() };
    assert_eq!(c.scratch, tmp_default);
    assert_eq!((c.days, c.poll, c.claude.as_str()), (30, 10.0, "claude"));
    assert_eq!(c.claude_dir, "/home/u/.claude");
    assert_eq!(c.cache_dir, "/home/u/.cache/session-board");
    assert_eq!(c.state_dir, "/home/u/.local/state/session-board");
}

#[test]
fn scratch_defaults_to_tmpdir_when_set() {
    // As on macOS, where the temp dir is under /var/folders.
    let tmp = TempDir::new();
    let c = load(&tmp, &[("TMPDIR", "/var/folders/xy/T/")], None).unwrap();
    assert_eq!(c.scratch, "/var/folders/xy/T");
}

#[test]
fn the_file_sets_values_and_expands_home() {
    let tmp = TempDir::new();
    let c = load(&tmp, &[], Some("workspaces = [\"~\", \"/opt\", \"~/work/\"]\nscratch = \"~/scratch\"\ndays = 14\n")).unwrap();
    assert_eq!(c.workspaces, ["/home/u", "/opt", "/home/u/work"]);
    assert_eq!(c.scratch, "/home/u/scratch");
    assert_eq!(c.days, 14);
    assert!(c.source["workspaces"].ends_with("config.toml"));
    assert_eq!(c.source["poll"], "default");
}

#[test]
fn environment_wins_over_the_file() {
    let tmp = TempDir::new();
    let c = load(
        &tmp,
        &[
            ("SESSION_BOARD_WORKSPACES", &format!("/srv{SEP}~/code")),
            ("SESSION_BOARD_POLL", "2.5"),
            ("SESSION_BOARD_CLAUDE", "/usr/local/bin/claude"),
            ("CLAUDE_CONFIG_DIR", "~/.claude-work"),
            ("XDG_STATE_HOME", "/state"),
        ],
        Some("workspaces = [\"/opt\"]\npoll = 30\n"),
    )
    .unwrap();
    assert_eq!(c.workspaces, ["/srv", "/home/u/code"]);
    assert_eq!(c.poll, 2.5);
    assert_eq!(c.claude, "/usr/local/bin/claude");
    assert_eq!(c.claude_dir, "/home/u/.claude-work");
    assert_eq!(c.state_dir, "/state/session-board");
    assert_eq!(c.source["workspaces"], "$SESSION_BOARD_WORKSPACES");
}

#[test]
fn workspaces_expand_variables_and_dedupe() {
    let tmp = TempDir::new();
    let list = "$HOME:/opt/:/opt:${HOME}/x/..: :$NOPE/a".replace(':', SEP);
    let c = load(&tmp, &[("SESSION_BOARD_WORKSPACES", &list)], None).unwrap();
    assert_eq!(c.workspaces, ["/home/u", "/opt", c.workspaces[2].as_str()]);
    assert!(c.workspaces[2].ends_with("$NOPE/a")); // unknown variables stay as typed
}

#[test]
fn a_bad_value_stops_with_a_message() {
    for toml in ["days = 0\n", "poll = \"soon\"\n", "days = [\n"] {
        let tmp = TempDir::new();
        let e = load(&tmp, &[], Some(toml)).unwrap_err();
        assert!(e.to_string().contains("session-board"), "{toml:?}: {e}");
    }
}

#[test]
fn a_bad_value_names_its_source() {
    let tmp = TempDir::new();
    let e = load(&tmp, &[("SESSION_BOARD_POLL", "soon")], None).unwrap_err();
    assert_eq!(e.to_string(), "session-board: poll must be a positive number, not 'soon' (from $SESSION_BOARD_POLL)");
}

#[test]
fn describe_names_each_source() {
    let tmp = TempDir::new();
    let text = config::describe(&load(&tmp, &[("SESSION_BOARD_DAYS", "7")], None).unwrap());
    assert!(text.contains("not found; defaults apply"));
    assert!(text.contains("days        7    [$SESSION_BOARD_DAYS]"));
}

#[test]
fn claude_is_split_like_a_shell() {
    let tmp = TempDir::new();
    let c = load(&tmp, &[("SESSION_BOARD_CLAUDE", "ssh 'my box' claude")], None).unwrap();
    assert_eq!(c.claude_argv(), ["ssh", "my box", "claude"]);
}
