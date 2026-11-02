//! Twins of tests/test_facts.py, plus the Rust-side additions (spans, waiting_since, the
//! line-type sniff, damaged lines).

use board::cache;
use board::claude::jobs::JobState;
use board::claude::registry;
use board::claude::transcript::{self, Span};
use board::facts::{Projects, Session, State, group};
use board::mask::mask_secrets;
use board::store;
use serde_json::{Value, json};

mod common;
use common::*;

fn claude(tmp: &TempDir) -> std::path::PathBuf {
    tmp.join("claude")
}

fn job_state(v: Value) -> State {
    JobState::from_value(&v).board_state()
}

#[test]
fn job_state_reads_tempo_not_state() {
    assert_eq!(job_state(json!({"state": "blocked", "tempo": "active"})), State::Running);
    assert_eq!(job_state(json!({"state": "done", "tempo": "blocked", "detail": "Approve rm?"})), State::NeedsYou);
    assert_eq!(job_state(json!({"state": "done", "tempo": "idle"})), State::Idle);
}

#[test]
fn freshly_resumed_job_is_idle_not_needs_you() {
    let st = json!({"state": "working", "tempo": "blocked", "detail": "(idle — send a prompt to start)"});
    assert_eq!(job_state(st), State::Idle);
}

#[test]
fn states_titles_and_summaries() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("aaaa1111-job", "/opt/plex").reply("from transcript").write(&c);
    job("aaaa1111", "aaaa1111-job", "/opt/plex").tempo("blocked").detail("Approve?").result("Plex updated.").write(&c);
    transcript("bbbb-inter", "/opt/plex").prompts(&["check logs"]).title("Log check").write(&c);
    transcript("cccc-ended", "/home/x").prompts(&["old question"]).reply("old answer").write(&c);
    let a = agents(json!([
        {"id": "aaaa1111", "kind": "background", "sessionId": "aaaa1111-job", "cwd": "/opt/plex",
         "name": "plex update", "state": "blocked"},
        {"kind": "interactive", "sessionId": "bbbb-inter", "cwd": "/opt/plex", "name": "host-e1", "pid": 1},
    ]));
    let s = collect(&tmp, &a);
    let job = &s["aaaa1111-job"];
    assert_eq!(job.state, State::NeedsYou);
    assert_eq!(job.job.as_deref(), Some("aaaa1111"));
    assert_eq!(job.title, "plex update");
    assert_eq!(job.summary.as_deref(), Some("Plex updated."));
    assert_eq!(s["bbbb-inter"].state, State::Elsewhere);
    assert_eq!(s["bbbb-inter"].title, "Log check"); // the transcript's title beats the host-made name
    let ended = &s["cccc-ended"];
    assert_eq!(ended.state, State::Ended);
    assert_eq!(ended.asked.as_deref(), Some("old question"));
    assert_eq!(ended.said.as_deref(), Some("old answer"));
    assert_eq!(ended.title, "old question");
}

#[test]
fn automation_and_promptless_sessions_are_dropped() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("sdk", "/opt").entry("sdk-py").write(&c);
    transcript("empty", "/opt").prompts(&[]).write(&c);
    transcript("real", "/opt").write(&c);
    assert_eq!(ids(&collect(&tmp, &[])), ["real"]);
}

#[test]
fn old_transcripts_are_skipped() {
    let tmp = TempDir::new();
    transcript("old", "/opt").ts(NOW - 40.0 * 86400.0).write(&claude(&tmp));
    assert!(collect(&tmp, &[]).is_empty());
}

#[test]
fn ended_copy_of_a_live_conversation_is_folded() {
    // Backgrounding copies the transcript; both copies keep the first message's uuid.
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("fore", "/opt/ha").root("root-1").ts(NOW - 3600.0).write(&c);
    transcript("back", "/opt/ha").root("root-1").write(&c);
    job("back0000", "back", "/opt/ha").write(&c);
    let a = agents(json!([{"id": "back0000", "kind": "background", "sessionId": "back", "cwd": "/opt/ha"}]));
    assert_eq!(ids(&collect(&tmp, &a)), ["back"]);
}

#[test]
fn transcript_a_job_resumed_is_folded() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("before", "/opt/b").ts(NOW - 3600.0).write(&c);
    transcript("after", "/opt/b").write(&c);
    job("after000", "after", "/opt/b").resume("before").write(&c);
    let a = agents(json!([{"id": "after000", "kind": "background", "sessionId": "after", "cwd": "/opt/b"}]));
    assert_eq!(ids(&collect(&tmp, &a)), ["after"]);
}

#[test]
fn two_ended_copies_keep_the_newest() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("older", "/opt/b").root("r").ts(NOW - 3600.0).write(&c);
    transcript("newer", "/opt/b").root("r").ts(NOW - 60.0).write(&c);
    assert_eq!(ids(&collect(&tmp, &[])), ["newer"]);
}

#[test]
fn live_sessions_are_never_folded() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("one", "/opt/b").root("r").write(&c);
    transcript("two", "/opt/b").root("r").write(&c);
    job("one00000", "one", "/opt/b").write(&c);
    job("two00000", "two", "/opt/b").write(&c);
    let a = agents(json!([{"id": "one00000", "kind": "background", "sessionId": "one", "cwd": "/opt/b"},
                          {"id": "two00000", "kind": "background", "sessionId": "two", "cwd": "/opt/b"}]));
    assert_eq!(ids(&collect(&tmp, &a)), ["one", "two"]);
}

#[test]
fn cache_is_reused_and_private() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("s1", "/opt").write(&c);
    collect(&tmp, &[]);
    let cache_path = tmp.join("cache.json");
    assert_mode(&cache_path, 0o600);
    let mut calls = 0;
    let got = cache::load_with(&c, &cache_path, 30, NOW, &mut |_| {
        calls += 1;
        None
    });
    assert_eq!(calls, 0); // unchanged file: served from the cache
    assert_eq!(got.len(), 1);
}

#[test]
fn the_cache_notices_a_changed_file() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("s1", "/opt").write(&c);
    collect(&tmp, &[]);
    transcript("s1", "/opt").prompts(&["a", "b"]).ts(NOW - 300.0).write(&c);
    let mut calls = 0;
    cache::load_with(&c, &tmp.join("cache.json"), 30, NOW, &mut |p| {
        calls += 1;
        transcript::parse_file(p).ok()
    });
    assert_eq!(calls, 1);
}

#[test]
fn a_damaged_or_foreign_cache_is_ignored() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("s1", "/opt").write(&c);
    std::fs::write(tmp.join("cache.json"), r#"{"version": 4, "files": {}}"#).unwrap(); // the Python board's
    assert_eq!(ids(&collect(&tmp, &[])), ["s1"]);
    std::fs::write(tmp.join("cache.json"), "not json").unwrap();
    assert_eq!(ids(&collect(&tmp, &[])), ["s1"]);
}

#[test]
fn project_of() {
    let tmp = TempDir::new();
    let home = tmp.join("home");
    let repo = home.join("board");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    let opt = tmp.join("opt");
    std::fs::create_dir_all(opt.join(".git")).unwrap();
    std::fs::create_dir_all(opt.join("plex/conf")).unwrap();
    let s = |p: std::path::PathBuf| path_str(&p);
    let ws = vec![s(opt.clone()), s(home.clone())];
    // The temp dir is itself under /tmp, so scratch is somewhere else here.
    let rules = Projects { workspaces: ws.clone(), scratch: "/nonexistent-scratch".into() };
    assert_eq!(rules.project_of(&s(repo.join("sub"))), s(repo.clone())); // a repo is one project
    assert_eq!(rules.project_of(&s(opt.join("plex/conf"))), s(opt.join("plex"))); // a workspace's child is its own
    assert_eq!(rules.project_of(&s(opt.clone())), s(opt.clone()));
    assert_eq!(rules.project_of(&s(home.clone())), s(home.clone()));
    // A linked worktree joins its main repository's project.
    let wt = repo.join(".claude/worktrees/fix");
    std::fs::create_dir_all(repo.join(".git/worktrees/fix")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", s(repo.join(".git/worktrees/fix")))).unwrap();
    assert_eq!(rules.project_of(&s(wt.join("src"))), s(repo.clone()));
    // So does one kept outside the repository, in the home folder, with a relative gitdir.
    let outside = home.join("board-fix");
    std::fs::create_dir_all(repo.join(".git/worktrees/board-fix")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join(".git"), "gitdir: ../board/.git/worktrees/board-fix\n").unwrap();
    assert_eq!(rules.project_of(&s(outside.clone())), s(repo.clone()));
    let with_tmp = Projects { workspaces: ws, scratch: "/tmp".into() };
    assert_eq!(with_tmp.project_of("/tmp/claude-1000/x"), "/tmp");
}

#[test]
fn group_puts_urgent_projects_first() {
    let sessions = vec![
        Session::new("a", "a", "/p1", "/p1", State::Ended, NOW),
        Session::new("b", "b", "/p2", "/p2", State::NeedsYou, NOW - 9999.0),
        Session::new("c", "c", "/p1", "/p1", State::Idle, NOW - 5.0),
    ];
    let groups = group(&sessions);
    assert_eq!(groups.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["/p2", "/p1"]);
    assert_eq!(groups[1].1.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(), ["c", "a"]);
}

#[test]
fn a_stopped_job_is_flagged() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("s1", "/opt/a").write(&c);
    job("j1", "s1", "/opt/a").tempo("idle").detail("stopped").state("stopped").write(&c);
    transcript("s2", "/opt/a").write(&c);
    job("j2", "s2", "/opt/a").write(&c);
    let a = agents(json!([{"id": "j1", "kind": "background", "sessionId": "s1", "cwd": "/opt/a"},
                          {"id": "j2", "kind": "background", "sessionId": "s2", "cwd": "/opt/a"}]));
    let s = collect(&tmp, &a);
    assert_eq!((s["s1"].state, s["s1"].stopped), (State::Idle, true));
    assert!(!s["s2"].stopped);
}

#[test]
fn a_deleted_conversation_stays_hidden_until_it_is_used_again() {
    // `claude rm` keeps the transcript; both copies of the conversation would come back as ended.
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("copy", "/opt/a").root("R").ts(NOW - 900.0).write(&c);
    transcript("gone", "/opt/a").root("R").ts(NOW - 600.0).write(&c);
    transcript("other", "/opt/a").ts(NOW - 600.0).write(&c);
    store::forget("gone", Some("R"), &tmp.join("deleted.json"), 30, NOW - 100.0).unwrap();
    assert_eq!(ids(&collect(&tmp, &[])), ["other"]);
    transcript("gone", "/opt/a").root("R").ts(NOW - 50.0).write(&c); // resumed and used since
    assert_eq!(ids(&collect(&tmp, &[])), ["gone", "other"]);
}

#[test]
fn a_deleted_live_session_still_shows() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("back", "/opt/a").root("R").ts(NOW - 900.0).write(&c);
    job("j1", "back", "/opt/a").write(&c);
    store::forget("back", Some("R"), &tmp.join("deleted.json"), 30, NOW).unwrap();
    let a = agents(json!([{"id": "j1", "kind": "background", "sessionId": "back", "cwd": "/opt/a"}]));
    assert_eq!(ids(&collect(&tmp, &a)), ["back"]);
}

#[test]
fn forget_drops_entries_older_than_the_transcripts() {
    let tmp = TempDir::new();
    let path = tmp.join("state/deleted.json");
    store::forget("old", None, &path, 30, NOW - 40.0 * 86400.0).unwrap();
    store::forget("new", Some("R"), &path, 30, NOW).unwrap();
    let got = store::load_deleted(&path);
    assert_eq!(got.len(), 2);
    assert_eq!((got["new"], got["R"]), (NOW, NOW));
    assert_mode(&path, 0o600);
    assert_mode(&tmp.join("state"), 0o700);
}

#[test]
fn a_damaged_deleted_file_is_ignored() {
    let tmp = TempDir::new();
    let path = tmp.join("deleted.json");
    std::fs::write(&path, r#"{"a": "soon", "b": 5}"#).unwrap();
    let got = store::load_deleted(&path);
    assert_eq!((got.len(), got["b"]), (1, 5.0));
    std::fs::write(&path, "not json").unwrap();
    assert!(store::load_deleted(&path).is_empty());
}

// --- /clear in a background session ------------------------------------------------------------

/// Job j1 began as conversation "first"; /clear moved it on to "second", which carries the job's
/// name as an inherited title. The job still reports sessionId "first".
fn cleared_job(tmp: &TempDir) -> Vec<Value> {
    let c = claude(tmp);
    transcript("first", "/opt/a").prompts(&["what is rust for"]).ai("Rust uses").ts(NOW - 900.0).write(&c);
    transcript("second", "/opt/a").prompts(&["read the handoff"]).ai("Rust uses").cleared().ts(NOW - 300.0).write(&c);
    job("j1", "first", "/opt/a").resume("second").name("Rust uses").write(&c);
    vec![json!({"id": "j1", "kind": "background", "sessionId": "first", "cwd": "/opt/a", "name": "Rust uses"})]
}

#[test]
fn a_cleared_job_shows_its_current_conversation() {
    let tmp = TempDir::new();
    let a = agents(Value::Array(cleared_job(&tmp)));
    let s = collect(&tmp, &a);
    assert_eq!(ids(&s), ["first", "second"]);
    let second = &s["second"];
    assert_eq!(
        (second.job.as_deref(), second.title.as_str(), second.asked.as_deref()),
        (Some("j1"), "Rust uses", Some("read the handoff"))
    );
    assert_eq!((s["first"].state, s["first"].title.as_str()), (State::Ended, "Rust uses (before /clear)"));
}

#[test]
fn a_cleared_conversation_continued_in_a_terminal_shows_once() {
    let tmp = TempDir::new();
    let mut list = cleared_job(&tmp);
    list.push(json!({"kind": "interactive", "sessionId": "second", "cwd": "/opt/a", "name": "Rust uses"}));
    let s = collect(&tmp, &agents(Value::Array(list)));
    assert_eq!(ids(&s), ["first", "second"]);
    let second = &s["second"];
    assert_eq!((second.state, second.job.as_deref(), second.title.as_str()), (State::Elsewhere, None, "Rust uses"));
}

#[test]
fn a_background_copy_is_not_mistaken_for_a_clear() {
    // `claude --bg --resume` of a running session makes a copy; resumeSessionId names the original.
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("orig", "/opt/a").root("R").ts(NOW - 900.0).write(&c);
    transcript("copy", "/opt/a").root("R").ts(NOW - 300.0).write(&c);
    job("j1", "copy", "/opt/a").resume("orig").write(&c);
    let s = collect(&tmp, &agents(json!([{"id": "j1", "kind": "background", "sessionId": "copy", "cwd": "/opt/a"}])));
    assert_eq!(ids(&s), ["copy"]);
    assert_eq!(s["copy"].job.as_deref(), Some("j1"));
}

#[test]
fn an_ended_conversation_started_with_clear_is_titled_by_its_first_prompt() {
    let tmp = TempDir::new();
    transcript("x", "/opt/a").prompts(&["fix the cert"]).ai("Old session name").cleared().write(&claude(&tmp));
    assert_eq!(collect(&tmp, &[])["x"].title, "fix the cert");
}

// --- stale jobs --------------------------------------------------------------------------------

/// Claude Code's list of running background workers. A worker given as alive gets this test's own
/// pid and start time; one given as dead gets a pid that can't exist.
fn write_roster(tmp: &TempDir, workers: &[(&str, bool)]) {
    let dir = claude(tmp).join("daemon");
    std::fs::create_dir_all(&dir).unwrap();
    // macOS has no start time to read (`alive` checks the pid alone there), so it stays empty.
    let start = registry::own_start_time().unwrap_or_default();
    let entries: serde_json::Map<String, Value> = workers
        .iter()
        .map(|(job, alive)| {
            let pid = if *alive { std::process::id() } else { (1 << 22) + 7 };
            (job.to_string(), json!({"pid": pid, "procStart": start}))
        })
        .collect();
    std::fs::write(dir.join("roster.json"), json!({"proto": 1, "supervisorPid": 1, "workers": entries}).to_string()).unwrap();
}

fn stale_jobs(tmp: &TempDir) -> Vec<board::claude::agents::Agent> {
    let c = claude(tmp);
    transcript("ask", "/opt/a").write(&c);
    job("askjob", "ask", "/opt/a").tempo("blocked").detail("Approve?").needs("approve rm -rf build").write(&c);
    transcript("busy", "/opt/b").write(&c);
    job("busyjob", "busy", "/opt/b").tempo("active").write(&c);
    transcript("rest", "/opt/c").write(&c);
    job("restjob", "rest", "/opt/c").tempo("idle").write(&c);
    transcript("halt", "/opt/d").write(&c);
    job("haltjob", "halt", "/opt/d").tempo("idle").state("stopped").write(&c);
    agents(json!([background("askjob", "ask"), background("busyjob", "busy"),
                  background("restjob", "rest"), background("haltjob", "halt")]))
}

#[test]
fn a_job_with_no_process_is_stale_not_needing_you() {
    let tmp = TempDir::new();
    let a = stale_jobs(&tmp);
    write_roster(&tmp, &[("busyjob", false)]); // listed, but its process is gone
    let s = collect(&tmp, &a);
    assert_eq!((s["ask"].state, s["ask"].was), (State::Stale, Some(State::NeedsYou)));
    assert_eq!((s["busy"].state, s["busy"].was), (State::Stale, Some(State::Running)));
    assert_eq!((s["rest"].state, s["halt"].state), (State::Idle, State::Idle)); // nothing claimed, nothing stale
    assert_eq!(s["ask"].waiting.as_deref(), Some("approve rm -rf build")); // what it last needed, kept for the row
}

#[test]
fn a_stopped_job_is_never_stale() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("halt", "/opt/d").write(&c);
    job("haltjob", "halt", "/opt/d").tempo("active").state("stopped").write(&c);
    write_roster(&tmp, &[]);
    let s = collect(&tmp, &agents(json!([background("haltjob", "halt")])));
    assert_eq!((s["halt"].state, s["halt"].stopped), (State::Running, true));
}

#[test]
fn a_job_with_a_live_worker_keeps_its_state() {
    let tmp = TempDir::new();
    let a = stale_jobs(&tmp);
    write_roster(&tmp, &[("askjob", true), ("busyjob", true)]);
    let s = collect(&tmp, &a);
    assert_eq!((s["ask"].state, s["ask"].was), (State::NeedsYou, None));
    assert_eq!(s["busy"].state, State::Running);
}

#[test]
fn without_a_roster_nothing_is_called_stale() {
    let tmp = TempDir::new();
    let a = stale_jobs(&tmp);
    let s = collect(&tmp, &a);
    assert_eq!((s["ask"].state, s["busy"].state), (State::NeedsYou, State::Running));
}

#[test]
fn a_roster_without_workers_is_unknown() {
    let tmp = TempDir::new();
    let a = stale_jobs(&tmp);
    let dir = claude(&tmp).join("daemon");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("roster.json"), r#"{"proto": 1, "workers": []}"#).unwrap();
    assert_eq!(collect(&tmp, &a)["busy"].state, State::Running);
}

// --- what a session is waiting on ----------------------------------------------------------------

fn tool_use(tid: &str, name: &str, args: Value, ts: f64) -> Value {
    json!({"type": "assistant", "timestamp": iso(ts), "message": {
        "id": format!("msg-{tid}"), "role": "assistant",
        "content": [{"type": "tool_use", "id": tid, "name": name, "input": args}]}})
}

fn tool_result(tid: &str, ts: f64) -> Value {
    json!({"type": "user", "timestamp": iso(ts), "message": {
        "role": "user", "content": [{"type": "tool_result", "tool_use_id": tid, "content": "ok"}]}})
}

fn one_job(tmp: &TempDir, extra: Vec<Value>, tempo: &str, needs: Option<&str>, detail: &str) -> Session {
    let c = claude(tmp);
    transcript("j", "/opt/a").extra(extra).write(&c);
    let mut j = job("jj", "j", "/opt/a").tempo(tempo).detail(detail);
    if let Some(n) = needs {
        j = j.needs(n);
    }
    j.write(&c);
    collect(tmp, &agents(json!([background("jj", "j")]))).remove("j").unwrap()
}

#[test]
fn waiting_on_prefers_the_jobs_own_words() {
    let tmp = TempDir::new();
    let s = one_job(&tmp, vec![tool_use("t1", "Bash", json!({"command": "make"}), NOW - 100.0)], "blocked",
                    Some("Reply yes   to commit"), "");
    assert_eq!(s.waiting.as_deref(), Some("Reply yes to commit"));
    assert_eq!(s.pending_tool, None, "the job's words aren't a tool call");
    assert_eq!(s.waiting_since, Some(NOW - 60.0)); // the job's updatedAt
}

#[test]
fn waiting_on_falls_back_to_the_pending_tool_call() {
    let tmp = TempDir::new();
    let s = one_job(
        &tmp,
        vec![
            tool_use("t1", "Read", json!({"file_path": "/opt/a/x"}), NOW - 100.0),
            tool_result("t1", NOW - 90.0),
            tool_use("t2", "Bash", json!({"command": "curl -H 'X: 1' https://api?x=1\nAPI_KEY=sk-123456 deploy"}), NOW - 80.0),
        ],
        "blocked",
        None,
        "Approve?",
    );
    assert_eq!(s.waiting.as_deref(), Some("Bash: curl -H 'X: 1' https://api?x=1 API_KEY=… deploy"));
    assert_eq!(s.waiting_since, Some(NOW - 80.0)); // when the tool call was made
    let tool = s.pending_tool.expect("a tool call, structured");
    assert_eq!((tool.name.as_str(), tool.arg.as_str()), ("Bash", "curl -H 'X: 1' https://api?x=1 API_KEY=… deploy"));
}

#[test]
fn a_question_is_shown_as_its_text() {
    let tmp = TempDir::new();
    let q = tool_use("q1", "AskUserQuestion", json!({"questions": [{"question": "Which repo?", "options": []}]}), NOW - 100.0);
    let s = one_job(&tmp, vec![q], "blocked", None, "");
    assert_eq!(s.waiting.as_deref(), Some("Which repo?"));
    assert_eq!(s.pending_tool, None, "a question isn't a tool call");
}

#[test]
fn a_session_not_waiting_has_no_waiting_line() {
    let tmp = TempDir::new();
    let s = one_job(&tmp, vec![tool_use("t1", "Bash", json!({"command": "ls"}), NOW - 100.0)], "active", None, "");
    assert_eq!((s.waiting, s.waiting_since), (None, None));
}

#[test]
fn a_new_prompt_clears_an_unanswered_tool_call() {
    let tmp = TempDir::new();
    let path = transcript("j", "/opt/a")
        .extra(vec![
            tool_use("t1", "Bash", json!({"command": "ls"}), NOW - 100.0),
            json!({"type": "user", "promptSource": "typed", "timestamp": iso(NOW - 50.0),
                   "message": {"role": "user", "content": "never mind"}}),
        ])
        .write(tmp.path());
    assert_eq!(transcript::parse_file(&path).unwrap().pending, None);
}

#[test]
fn a_queued_prompt_clears_it_too() {
    let tmp = TempDir::new();
    let path = transcript("j", "/opt/a")
        .extra(vec![
            tool_use("t1", "Bash", json!({"command": "ls"}), NOW - 100.0),
            json!({"type": "user", "promptSource": "queued", "timestamp": iso(NOW - 50.0),
                   "message": {"role": "user", "content": "/compact"}}),
        ])
        .write(tmp.path());
    let t = transcript::parse_file(&path).unwrap();
    assert_eq!((t.pending, t.typed), (None, 1)); // a slash command isn't a typed prompt
}

#[test]
fn mask_secrets_() {
    let m = mask_secrets;
    assert_eq!(m("API_KEY=abc123 curl x"), "API_KEY=… curl x");
    assert_eq!(m("mysql --password=hunter2 -u root"), "mysql --password=… -u root");
    assert_eq!(m("git clone https://ryan:ghp_abcd@github.com/x"), "git clone https://ryan:…@github.com/x");
    assert!(!m(r#"curl -H "Authorization: Bearer sk-ant-abcdefgh12345" x"#).contains("sk-ant-abcdefgh12345"));
    assert_eq!(m(&format!("echo {}", "a1".repeat(20))), "echo …");
    assert_eq!(m("docker compose restart homeassistant"), "docker compose restart homeassistant");
}

// --- model and tokens ----------------------------------------------------------------------------

fn reply(mid: &str, model: &str, inp: u64, cache: u64, out: u64, ts: f64) -> Value {
    json!({"type": "assistant", "timestamp": iso(ts), "message": {
        "id": mid, "model": model, "role": "assistant", "content": [{"type": "text", "text": "x"}],
        "usage": {"input_tokens": inp, "cache_creation_input_tokens": 0, "cache_read_input_tokens": cache,
                  "output_tokens": out}}})
}

#[test]
fn model_context_and_output_tokens() {
    let tmp = TempDir::new();
    let path = transcript("j", "/opt/a")
        .extra(vec![
            reply("m1", "claude-opus-5-5", 10, 40_000, 300, NOW - 30.0),
            reply("m1", "claude-opus-5-5", 10, 40_000, 900, NOW - 29.0), // the same reply, a later block
            reply("m2", "claude-sonnet-5-5", 5, 182_000, 100, NOW - 20.0),
            reply("m3", "<synthetic>", 0, 0, 0, NOW - 10.0), // Claude Code's own notices
        ])
        .write(tmp.path());
    let t = transcript::parse_file(&path).unwrap();
    assert_eq!((t.model.as_deref(), t.context, t.output), (Some("claude-sonnet-5-5"), 182_005, 1000));
}

#[test]
#[cfg(unix)]
fn liveness_without_proc_falls_back_to_the_pid() {
    let alive = |pid: Value, start: Value| registry::alive_with(Some(&pid), Some(&start), false);
    assert!(alive(json!(std::process::id()), json!("anything")));
    assert!(!alive(json!((1 << 22) + 7), json!("x")));
    assert!(!alive(json!("nope"), json!("x")) && !alive(json!(0), json!("x")));
}

#[test]
#[cfg(any(target_os = "linux", windows))] // where a process's start time can be read
fn liveness_checks_the_start_time() {
    let start = registry::own_start_time().unwrap();
    let pid = json!(std::process::id());
    assert!(registry::alive(Some(&pid), Some(&json!(start))));
    assert!(!registry::alive(Some(&pid), Some(&json!("1"))));
}

// --- additions: parsing robustness, sidechains, spans -------------------------------------------

#[test]
fn damaged_lines_and_bad_utf8_are_skipped_not_fatal() {
    let tmp = TempDir::new();
    let path = transcript("j", "/opt/a").reply("fine").write(tmp.path());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{not json\n");
    bytes.extend_from_slice(b"{\"type\":\"assistant\",\"timestamp\":\"2026-10-31T00:00:00Z\",\"message\":{\"content\":\"caf\xe9 ok\"}}\n");
    bytes.extend_from_slice(b"{\"type\":\"api-request\",\"payload\":\"x\"}\n[1,2]\n");
    std::fs::write(&path, bytes).unwrap();
    let t = transcript::parse_file(&path).unwrap();
    assert_eq!(t.said.as_deref(), Some("caf\u{fffd} ok"));
    assert_eq!(t.typed, 1);
}

#[test]
fn sidechain_entries_are_ignored() {
    let tmp = TempDir::new();
    let mut side = tool_use("s1", "Bash", json!({"command": "ls"}), NOW - 10.0);
    side["isSidechain"] = json!(true);
    let path = transcript("j", "/opt/a").reply("main reply").extra(vec![side]).write(tmp.path());
    let t = transcript::parse_file(&path).unwrap();
    assert_eq!((t.pending, t.said.as_deref()), (None, Some("main reply")));
}

#[test]
fn root_is_the_first_user_entry_of_any_kind() {
    let tmp = TempDir::new();
    let c = tmp.join("c");
    std::fs::create_dir_all(&c).unwrap();
    let path = c.join("x.jsonl");
    let lines = [
        json!({"type": "user", "uuid": "first-tool-result", "message": {"content": [{"type": "tool_result", "tool_use_id": "z"}]}}),
        json!({"type": "user", "uuid": "typed", "promptSource": "typed", "message": {"content": "hi"}}),
    ];
    std::fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    assert_eq!(transcript::parse_file(&path).unwrap().root.as_deref(), Some("first-tool-result"));
}

#[test]
fn asked_and_said_are_cut_at_1200_characters() {
    let tmp = TempDir::new();
    let long = "é".repeat(1500);
    let path = transcript("j", "/opt/a").prompts(&[&long]).reply(&long).write(tmp.path());
    let t = transcript::parse_file(&path).unwrap();
    assert_eq!(t.asked.unwrap().chars().count(), 1200);
    assert_eq!(t.said.unwrap().chars().count(), 1200);
}

#[test]
fn spans_split_on_ten_minute_gaps_and_bridge_tool_runs() {
    let tmp = TempDir::new();
    let base = NOW - 7200.0;
    let path = transcript("j", "/opt/a")
        .ts(base) // prompt at base, reply at base + 0.5
        .extra(vec![
            tool_use("t1", "Bash", json!({"command": "make test"}), base + 60.0),
            tool_result("t1", base + 60.0 + 1500.0), // a 25-minute test run: still one span
            reply("m1", "claude-opus-5-5", 1, 1, 1, base + 1600.0),
            reply("m2", "claude-opus-5-5", 1, 1, 1, base + 4000.0), // 40 minutes later: a new span
        ])
        .write(tmp.path());
    let t = transcript::parse_file(&path).unwrap();
    assert_eq!(t.spans, vec![Span { start: base, end: base + 1600.0 }, Span { start: base + 4000.0, end: base + 4000.0 }]);
}

#[test]
fn every_row_kind_carries_its_spans() {
    let tmp = TempDir::new();
    transcript("x", "/opt/a").write(&claude(&tmp));
    let s = collect(&tmp, &[]);
    assert_eq!(s["x"].spans, vec![Span { start: NOW - 600.0, end: NOW - 599.5 }]);
}

#[test]
fn a_job_with_no_transcript_still_shows() {
    let tmp = TempDir::new();
    job("j1", "nosuch", "/opt/a").write(&claude(&tmp));
    let s = collect(&tmp, &agents(json!([background("j1", "nosuch")])));
    assert_eq!((s["nosuch"].title.as_str(), s["nosuch"].last), ("(untitled)", NOW - 60.0));
}

#[test]
fn rows_sort_by_state_then_most_recent() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("e1", "/opt/a").ts(NOW - 100.0).write(&c);
    transcript("e2", "/opt/a").ts(NOW - 50.0).write(&c);
    transcript("busy", "/opt/b").ts(NOW - 5000.0).write(&c);
    job("bj", "busy", "/opt/b").tempo("active").write(&c);
    let rows = board::facts::collect_with(&sources(&tmp), &agents(json!([background("bj", "busy")])), NOW);
    assert_eq!(rows.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(), ["busy", "e2", "e1"]);
}

#[test]
fn the_latest_prompt_is_kept_with_the_first() {
    let tmp = TempDir::new();
    let path = transcript("j", "/opt/a").prompts(&["fix the cert", "/compact", "now renew it"]).write(tmp.path());
    let t = transcript::parse_file(&path).unwrap();
    assert_eq!(t.asked.as_deref(), Some("fix the cert"));
    assert_eq!(t.last_asked.as_deref(), Some("now renew it")); // slash commands aren't prompts
    assert_eq!(t.typed, 2);
}

#[test]
fn a_status_that_only_repeats_the_prompt_is_not_a_summary() {
    let tmp = TempDir::new();
    let c = claude(&tmp);
    transcript("j", "/opt/a").prompts(&["first", "read the screenshot and fix the alignment please"]).write(&c);
    job("jj", "j", "/opt/a").tempo("active").detail("read the screenshot and fix the…").write(&c);
    transcript("k", "/opt/a").prompts(&["fix the notebook"]).write(&c);
    job("kk", "k", "/opt/a").tempo("active").detail("Rewriting the retention query").write(&c);
    let s = collect(&tmp, &agents(json!([background("jj", "j"), background("kk", "k")])));
    assert_eq!(s["j"].summary, None);
    assert_eq!(s["k"].summary.as_deref(), Some("Rewriting the retention query"));
}

#[test]
fn a_session_that_moved_into_a_worktree_is_grouped_by_where_it_works() {
    let tmp = TempDir::new();
    let home = tmp.join("home");
    let repo = home.join("app");
    let wt = repo.join(".claude/worktrees/fix");
    std::fs::create_dir_all(repo.join(".git/worktrees/fix")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", repo.join(".git/worktrees/fix").display())).unwrap();
    let (home_s, wt_s) = (path_str(&home), path_str(&wt));
    let moved = json!({"type": "user", "cwd": wt_s, "timestamp": iso(NOW - 500.0),
                       "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]},
                       "toolUseResult": {"filePath": format!("{wt_s}/src/a.rs"), "structuredPatch": [{"lines": ["+x"]}]}});
    transcript("s1", &home_s).extra(vec![moved]).write(&tmp.join("claude"));
    let s = &collect(&tmp, &[])["s1"];
    assert_eq!(s.cwd, home_s, "actions run where the conversation is kept");
    assert_eq!(s.workdir.as_deref(), Some(wt_s.as_str()));
    assert_eq!(s.project, path_str(&repo));
    assert_eq!(s.files.len(), 1);
    assert_eq!((s.files[0].added, s.files[0].edits), (1, 1));
}

#[test]
fn a_cd_elsewhere_does_not_move_a_session_to_another_project() {
    let tmp = TempDir::new();
    let home = tmp.join("home");
    let repo = home.join("app");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let scratch = repo.join("notes");
    std::fs::create_dir_all(&scratch).unwrap();
    let other = home.join("plain/dir");
    std::fs::create_dir_all(&other).unwrap();
    let s = |p: &std::path::Path| path_str(p);
    let at = |cwd: String| json!({"type": "user", "cwd": cwd, "timestamp": iso(NOW - 500.0),
                                  "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]}});
    // Launched in a repo, then a command ran in a plain folder: stays with the repo.
    transcript("s1", &s(&repo)).extra(vec![at(s(&other))]).write(&tmp.join("claude"));
    // Launched in the home workspace, then moved into a repo: joins the repo.
    transcript("s2", &s(&home)).extra(vec![at(s(&scratch))]).write(&tmp.join("claude"));
    // Launched in the home workspace, then into a plain folder: stays put.
    transcript("s3", &s(&home)).extra(vec![at(s(&other))]).write(&tmp.join("claude"));
    let mut src = sources(&tmp);
    src.projects.workspaces = vec![s(&home)];
    let got: std::collections::HashMap<String, Session> =
        board::facts::collect_with(&src, &[], NOW).into_iter().map(|x| (x.session_id.clone(), x)).collect();
    assert_eq!(got["s1"].project, s(&repo));
    assert_eq!(got["s1"].workdir, None, "a passing cd isn't where it works");
    assert_eq!(got["s2"].project, s(&repo));
    assert_eq!(got["s3"].project, s(&home));
}
