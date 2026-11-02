//! The app's actions end to end on a `TestBackend`, with a fake runner in place of the claude
//! CLI: stop (confirming only while running), delete (always confirming, then forgetting the
//! conversation and moving the cursor), refusals, the new-session form, the pending job landing
//! the cursor, Finder's Ctrl+K menu, and Esc closing every overlay.

use board::actions::{Cli, Cursor, Enter, Runner};
use board::facts::{Snapshot, State};
use board::proc::{Output, RunError};
use board::shot;
use board::tui::app::{App, Effect, Task, ViewId};
use board::tui::overlays::{FOLDER, NAME, Overlay};
use board::tui::theme::Theme;
use ratatui::buffer::Buffer;
use std::cell::RefCell;

mod common;
use common::TempDir;

/// Records each call and answers with a canned output.
struct Fake {
    out: Output,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Runner for &Fake {
    fn run(&self, argv: &[String], _cwd: &str) -> Result<Output, RunError> {
        self.calls.borrow_mut().push(argv.to_vec());
        Ok(self.out.clone())
    }
}

fn fake(stdout: &str, code: i32, stderr: &str) -> Fake {
    Fake { out: Output { code: Some(code), stdout: stdout.into(), stderr: stderr.into() }, calls: RefCell::new(vec![]) }
}

impl Fake {
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|a| a.join(" ")).collect()
    }
}

fn cli(f: &Fake) -> Cli<&Fake> {
    Cli { claude: vec!["claude".into()], home: "/home/u".into(), runner: f }
}

fn sample() -> Snapshot {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn app() -> App {
    App::offline(sample(), "/home/u", "/tmp", Theme::default())
}

/// Press keys by name, drawing at 160x45 before each as the real loop does.
fn press(app: &mut App, keys: &str) -> Buffer {
    let keys: Vec<_> = keys.split(',').filter(|k| !k.is_empty()).map(|k| shot::key(k).unwrap()).collect();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 45)).unwrap();
    for k in keys {
        terminal.draw(|f| app.draw(f)).unwrap();
        app.handle_key(k);
    }
    terminal.draw(|f| app.draw(f)).unwrap();
    terminal.backend().buffer().clone()
}

/// Type text, a key per char (`,` is fine here).
fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char(c),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
    }
}

fn screen(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
        .collect()
}

fn toasted(app: &App, words: &str) -> bool {
    app.toasts().any(|t| t.text.contains(words))
}

fn on(app: &mut App, id: &str) {
    app.cursor = Some(Cursor::Session(id.into()));
}

// --- stop ----------------------------------------------------------------------------------------

#[test]
fn stop_runs_at_once_when_idle_and_asks_only_while_running() {
    let f = fake("", 0, "");
    let mut a = app();
    on(&mut a, "a2"); // idle
    press(&mut a, "s");
    assert!(a.overlay.is_none(), "an idle session stops without asking");
    assert!(toasted(&a, "Stopping “Upgrade the ORM to v3”…"));
    let rest = a.run_tasks(&cli(&f));
    assert_eq!(f.calls(), ["claude stop b2c3d4e5"]);
    assert!(toasted(&a, "Stopped “Upgrade the ORM to v3”."));
    assert_eq!(rest, [Effect::Refresh]);

    on(&mut a, "i1"); // running
    let buf = press(&mut a, "s");
    assert!(matches!(a.overlay, Some(Overlay::Confirm(_))));
    assert!(screen(&buf).contains("$ claude stop c3d4e5f6"), "the confirm shows the command");
    press(&mut a, "n");
    assert!(a.overlay.is_none());
    assert!(a.run_tasks(&cli(&f)).is_empty());
    assert_eq!(f.calls().len(), 1, "no is no");
    press(&mut a, "s,y");
    a.run_tasks(&cli(&f));
    assert_eq!(f.calls()[1], "claude stop c3d4e5f6");
}

#[test]
fn a_failed_command_toasts_its_stderr() {
    let f = fake("", 1, "No job matching 'b2c3d4e5'\n");
    let mut a = app();
    on(&mut a, "a2");
    press(&mut a, "s");
    a.run_tasks(&cli(&f));
    assert!(toasted(&a, "Couldn't stop the session: No job matching 'b2c3d4e5'"));
    assert!(a.toasts().any(|t| t.tone == board::tui::app::Tone::Error));
}

#[test]
fn refusals_toast_and_run_nothing() {
    let mut a = app();
    a.prefs.show_ended = true;
    for (id, key, words) in [
        ("w1", "s", "another terminal"),
        ("w1", "x", "another terminal"),
        ("a3", "s", "already ended"),
        ("a3", "x", "after 30 days"),
    ] {
        on(&mut a, id);
        press(&mut a, key);
        assert!(a.overlay.is_none(), "{id} {key}");
        assert!(toasted(&a, words), "{id} {key}: {words}");
        assert!(a.take_effects().is_empty());
    }
    let mut b = app();
    let mut s = sample();
    s.sessions.iter_mut().find(|s| s.session_id == "a2").unwrap().stopped = true;
    b.on_collect(Ok(s));
    on(&mut b, "a2");
    press(&mut b, "s");
    assert!(toasted(&b, "already stopped"));
}

// --- delete --------------------------------------------------------------------------------------

#[test]
fn delete_always_asks_on_cancel_then_forgets_and_moves_the_cursor_on() {
    let tmp = TempDir::new();
    let f = fake("", 0, "");
    let mut a = app();
    a.settings.deleted_path = Some(tmp.join("deleted.json"));
    on(&mut a, "a1"); // needs you, first in its project; a2 follows
    let buf = press(&mut a, "x");
    assert!(matches!(a.overlay, Some(Overlay::Confirm(_))), "delete always asks");
    assert!(screen(&buf).contains("$ claude rm a1b2c3d4"));
    press(&mut a, "enter");
    assert!(a.overlay.is_none(), "Enter starts on Cancel");
    assert!(a.run_tasks(&cli(&f)).is_empty());
    assert!(f.calls().is_empty());

    press(&mut a, "x,left,enter"); // ← moves to Delete
    let rest = a.run_tasks(&cli(&f));
    assert_eq!(f.calls(), ["claude rm a1b2c3d4"]);
    assert_eq!(rest, [Effect::Refresh]);
    assert!(toasted(&a, "Deleted “"));
    assert_eq!(a.cursor, Some(Cursor::Session("a2".into())), "the next session in its project");
    let deleted = std::fs::read_to_string(tmp.join("deleted.json")).unwrap();
    assert!(deleted.contains("\"a1\""), "{deleted}");

    // The last in its project: the one before.
    on(&mut a, "d2");
    press(&mut a, "x,y");
    a.run_tasks(&cli(&f));
    assert_eq!(a.cursor, Some(Cursor::Session("d1".into())));
}

#[test]
fn deleting_a_project_s_only_session_leaves_the_cursor_on_the_project() {
    let f = fake("", 0, "");
    let mut s = sample();
    s.sessions.retain(|s| s.session_id != "a2");
    let mut a = App::offline(s, "/home/u", "/tmp", Theme::default());
    on(&mut a, "a1");
    press(&mut a, "x,y");
    a.run_tasks(&cli(&f));
    assert_eq!(a.cursor, Some(Cursor::Project("/home/u/work/api".into())));
}

#[test]
fn a_failed_delete_keeps_the_cursor_and_forgets_nothing() {
    let tmp = TempDir::new();
    let f = fake("", 1, "No job matching 'a1b2c3d4'");
    let mut a = app();
    a.settings.deleted_path = Some(tmp.join("deleted.json"));
    on(&mut a, "a1");
    press(&mut a, "x,y");
    a.run_tasks(&cli(&f));
    assert_eq!(a.cursor, Some(Cursor::Session("a1".into())));
    assert!(!tmp.join("deleted.json").exists());
    assert!(toasted(&a, "Couldn't delete the session: No job matching"));
}

#[test]
fn a_forget_that_fails_doesnt_fail_the_delete() {
    let f = fake("", 0, "");
    let mut a = app();
    a.settings.deleted_path = Some("/proc/no-such-place/deleted.json".into());
    on(&mut a, "a1");
    press(&mut a, "x,y");
    a.run_tasks(&cli(&f));
    assert!(toasted(&a, "Deleted “"));
    assert_eq!(a.cursor, Some(Cursor::Session("a2".into())));
}

// --- new -----------------------------------------------------------------------------------------

#[test]
fn n_opens_the_form_prefilled_and_its_letters_are_text() {
    let home = TempDir::new();
    let mut a = App::offline(sample(), &home.str(), "/tmp", Theme::default());
    on(&mut a, "a1"); // its folder doesn't exist here, nor its project's: home
    let buf = press(&mut a, "n");
    let Some(Overlay::Form(form)) = &a.overlay else { panic!("no form") };
    assert_eq!(form.text(FOLDER), "~");
    assert_eq!(form.focus, NAME);
    assert!(screen(&buf).contains("New background session"));

    press(&mut a, "q,/,?,e,r,s,x,n,tab,shift+tab");
    assert!(!a.quit && !a.prefs.show_ended);
    assert!(a.take_effects().is_empty(), "no refresh, stop or delete");
    let Some(Overlay::Form(form)) = &a.overlay else { panic!("the form closed") };
    assert_eq!(form.text(NAME), "q/?ersxn");
}

#[test]
fn the_form_refuses_in_words_and_esc_cancels() {
    let home = TempDir::new();
    let mut a = App::offline(sample(), &home.str(), "/tmp", Theme::default());
    press(&mut a, "n,enter");
    let buf = press(&mut a, "");
    assert!(screen(&buf).contains("Give the session a name or a first prompt."));
    press(&mut a, "up");
    if let Some(Overlay::Form(f)) = &mut a.overlay {
        f.fields[FOLDER].set("~/no-such-folder");
    }
    press(&mut a, "down");
    type_text(&mut a, "probe");
    let buf = press(&mut a, "enter");
    // The message may wrap before the path (a Windows temp folder is long), so check both halves.
    let shown = screen(&buf);
    assert!(shown.contains("There's no folder"), "{shown}");
    assert!(shown.contains(&format!("{}/no-such-folder.", home.str())), "{shown}");
    press(&mut a, "esc");
    assert!(a.overlay.is_none());
    assert!(a.take_effects().is_empty());
}

#[test]
fn enter_in_the_form_starts_the_session_then_attaches_and_the_cursor_lands_on_it() {
    let home = TempDir::new();
    std::fs::create_dir(home.join("proj")).unwrap();
    let f = fake("backgrounded · 9f8e7d6c · sb probe\n", 0, "");
    let mut a = App::offline(sample(), &home.str(), "/tmp", Theme::default());
    press(&mut a, "n,up,ctrl+u");
    type_text(&mut a, "~/proj");
    press(&mut a, "tab");
    type_text(&mut a, "sb  probe");
    press(&mut a, "tab");
    type_text(&mut a, "-v first");
    press(&mut a, "enter");
    assert!(a.overlay.is_none());
    assert!(toasted(&a, "Starting “sb probe” in ~/proj…"));
    let rest = a.run_tasks(&cli(&f));
    assert_eq!(f.calls(), ["claude --bg --name sb probe -- -v first"]);
    assert_eq!(rest, [Effect::Enter(Enter::Attach { job: "9f8e7d6c".into() }), Effect::Refresh]);
    assert_eq!(a.pending.as_ref().map(|p| p.0.as_str()), Some("9f8e7d6c"));

    // Back from the session: the first snapshot without it changes nothing, the one with it
    // puts the cursor there.
    a.on_collect(Ok(sample()));
    assert!(a.pending.is_some());
    let mut s = sample();
    let mut new = board::facts::Session::new("n1", "sb probe", &home.join("proj").to_string_lossy(), "/x", State::Idle, s.now);
    new.job = Some("9f8e7d6c".into());
    s.sessions.push(new);
    a.on_collect(Ok(s));
    assert_eq!(a.cursor, Some(Cursor::Session("n1".into())));
    assert!(a.pending.is_none());
}

#[test]
fn a_pending_session_is_given_up_after_thirty_seconds() {
    let mut a = app();
    on(&mut a, "a1");
    let now = a.clock.unwrap();
    a.pending = Some(("a7b8c9d0".into(), now - 31.0)); // d1's job, but too late
    a.on_collect(Ok(sample()));
    assert_eq!(a.cursor, Some(Cursor::Session("a1".into())));
    assert!(a.pending.is_none());
    a.pending = Some(("a7b8c9d0".into(), now - 29.0));
    a.on_collect(Ok(sample()));
    assert_eq!(a.cursor, Some(Cursor::Session("d1".into())));
}

#[test]
fn a_failed_start_says_why_and_attaches_to_nothing() {
    let home = TempDir::new();
    let f = fake("", 1, "Error: not logged in");
    let mut a = App::offline(sample(), &home.str(), "/tmp", Theme::default());
    press(&mut a, "n");
    type_text(&mut a, "probe");
    press(&mut a, "enter");
    let rest = a.run_tasks(&cli(&f));
    assert_eq!(rest, [Effect::Refresh]);
    assert!(toasted(&a, "Couldn't start the session: Error: not logged in"));
    assert!(a.pending.is_none());
}

// --- Finder's menu -------------------------------------------------------------------------------

#[test]
fn ctrl_k_in_finder_opens_the_menu_with_the_configured_command_and_its_rows_run() {
    let f = fake("", 0, "");
    let mut a = app();
    a.settings.claude = vec!["ssh".into(), "box".into(), "claude".into()];
    a.view = ViewId::Finder;
    on(&mut a, "a2");
    let buf = press(&mut a, "ctrl+k");
    assert!(matches!(a.overlay, Some(Overlay::Menu(_))));
    let text = screen(&buf);
    assert!(text.contains("ssh box claude stop b2c3d4e5"), "{text}");
    assert!(text.contains("ssh box claude rm b2c3d4e5"));

    press(&mut a, "s"); // idle: stops without asking
    assert!(a.overlay.is_none());
    a.run_tasks(&cli(&f));
    assert_eq!(f.calls(), ["claude stop b2c3d4e5"]); // the fake Cli's own claude

    press(&mut a, "ctrl+k,x");
    assert!(matches!(a.overlay, Some(Overlay::Confirm(_))), "Delete… asks");
    press(&mut a, "esc,ctrl+k,n");
    assert!(matches!(a.overlay, Some(Overlay::Form(_))), "New session… opens the form");
    press(&mut a, "esc,ctrl+k,down,down,down,down,enter");
    assert!(a.prefs.show_ended, "the fifth row toggles ended sessions");
}

#[test]
fn ctrl_k_outside_finder_does_nothing_and_k_is_text_in_finder() {
    let mut a = app();
    press(&mut a, "ctrl+k");
    assert!(a.overlay.is_none());
    a.view = ViewId::Finder;
    press(&mut a, "k");
    assert!(a.overlay.is_none());
}

#[test]
fn esc_closes_every_overlay_and_runs_nothing() {
    let home = TempDir::new();
    let mut a = App::offline(sample(), &home.str(), "/tmp", Theme::default());
    on(&mut a, "a1");
    for (view, open) in [
        (ViewId::Columns, "f1"),
        (ViewId::Columns, "x"),
        (ViewId::Columns, "n"),
        (ViewId::Timeline, "?"),
        (ViewId::Finder, "ctrl+k"),
    ] {
        a.view = view;
        press(&mut a, open);
        assert!(a.overlay.is_some(), "{open} opens something");
        press(&mut a, "esc");
        assert!(a.overlay.is_none(), "Esc closes what {open} opened");
        assert_eq!(a.view, view, "and doesn't go back a view");
        assert!(a.take_effects().is_empty());
    }
}

#[test]
fn the_shot_path_reaches_the_form_and_the_menu() {
    let mut a = app();
    a.view = ViewId::Finder;
    on(&mut a, "i3");
    let keys: Vec<_> = ["ctrl+k"].iter().map(|k| shot::key(k).unwrap()).collect();
    let buf = shot::render(&mut a, 160, 45, &keys);
    assert!(screen(&buf).contains("Actions"));
    let mut a = app();
    let keys: Vec<_> = ["n", "p"].iter().map(|k| shot::key(k).unwrap()).collect();
    let buf = shot::render(&mut a, 160, 45, &keys);
    assert!(screen(&buf).contains("$ claude --bg --name p"));
    // Shots never run claude.
    assert!(!a.take_effects().iter().any(|e| matches!(e, Effect::Run(Task::New(_)))));
}
