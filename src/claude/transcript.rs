//! One transcript (`projects/<folder>/<session id>.jsonl`) read into the few facts the board uses.
//!
//! Reading is a fold: [`Parser::feed`] takes one entry at a time and [`Parser::finish`] turns the
//! state into a [`TranscriptInfo`], so parsing only the appended tail of a file can be added later
//! without a rewrite. Today a changed file is parsed whole (see `cache.rs`).

use super::{epoch, path_field, string, text, truthy};
use crate::mask::{flat, mask_secrets, truncate_chars};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// `asked` and `said` are cut to this many characters.
const TEXT_LIMIT: usize = 1200;
/// A pending tool call is described in at most this many characters.
const TOOL_LIMIT: usize = 300;
/// Entries further apart than this start a new activity span.
pub const SPAN_GAP: f64 = 600.0;
/// Spans kept per transcript, the most recent ones.
pub const SPAN_LIMIT: usize = 64;

/// Entry types the board reads. Everything else (`api-request` lines are most of the bytes) is
/// skipped without being parsed when its type comes first on the line.
const KINDS: [&str; 7] =
    ["user", "assistant", "ai-title", "custom-title", "agent-name", "attachment", "system"];

/// A pending tool call, structured: the tool's name and its main argument (secrets masked,
/// whitespace flattened, cut to the same 300 characters as the description). A question
/// (`AskUserQuestion`) is not a tool call to approve, so it has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTool {
    pub name: String,
    /// The main argument (`command`, `file_path`, …); may be empty.
    pub arg: String,
}

/// Changed files kept per transcript, the most recently touched.
pub const CHANGED_LIMIT: usize = 60;
/// Files only read kept per transcript, the most recently read.
pub const READ_LIMIT: usize = 40;

/// A file the conversation read or changed, from its tool results: `Read`, `Edit`, `Write` (and
/// any tool whose result carries a `filePath` and a patch), and files a shell command changed
/// (Claude Code's `bashEditDiff`). Paths are as recorded, usually absolute.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileTouch {
    pub path: String,
    /// Lines added and removed over all its changes.
    pub added: u32,
    pub removed: u32,
    /// Changes made with Edit or Write.
    pub edits: u32,
    /// Created by Write (it didn't exist before).
    pub created: bool,
    /// Changed by a shell command.
    pub shell: bool,
    /// Read with Read.
    pub read: bool,
    /// Epoch seconds of the latest touch.
    pub last: i64,
}

impl FileTouch {
    /// Changed at all, not only read.
    pub fn changed(&self) -> bool {
        self.edits > 0 || self.created || self.shell
    }
}

/// A stretch of activity: consecutive entries less than [`SPAN_GAP`] apart.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscriptInfo {
    /// The folder of the first entry that names one.
    pub cwd: Option<String>,
    /// The folder of the latest entry that names one: where the session is working now, after
    /// a `cd` or entering a worktree.
    pub cwd_last: Option<String>,
    /// How the session was started: `cli` for one you started, `sdk-py`, `sdk-cli` for automation.
    pub entry: Option<String>,
    /// Prompts you typed (not slash commands).
    pub typed: u32,
    /// The first prompt you typed, cut to 1200 characters.
    pub asked: Option<String>,
    /// The latest prompt you typed, cut to 1200 characters: what Claude's last reply answers.
    pub last_asked: Option<String>,
    pub ai: Option<String>,
    pub custom: Option<String>,
    pub agent: Option<String>,
    /// Epoch seconds of the latest user or assistant entry.
    pub last: f64,
    /// The last thing Claude said, cut to 1200 characters.
    pub said: Option<String>,
    /// The uuid of the first user entry: the same in every copy of one conversation.
    pub root: Option<String>,
    /// The conversation began with `/clear`, so its title was inherited, not its own.
    pub cleared: bool,
    /// The latest tool call with no result yet, described with secrets masked.
    pub pending: Option<String>,
    /// When that tool call was made.
    pub pending_since: Option<f64>,
    /// The same call as tool and argument, when it is one (not a question).
    pub pending_tool: Option<PendingTool>,
    /// The model of the last real reply (not Claude Code's own `<synthetic>` notices).
    pub model: Option<String>,
    /// Tokens in the context window at the last reply.
    pub context: u64,
    /// Output tokens over the whole conversation.
    pub output: u64,
    /// Activity spans, oldest first, at most [`SPAN_LIMIT`].
    pub spans: Vec<Span>,
    /// Files read or changed, most recent first: at most [`CHANGED_LIMIT`] changed and
    /// [`READ_LIMIT`] only read.
    pub files: Vec<FileTouch>,
}

impl TranscriptInfo {
    /// The title the conversation gave itself; `None` for one that began with `/clear`, whose
    /// title is the session's inherited name.
    pub fn own_title(&self) -> Option<&str> {
        if self.cleared {
            return None;
        }
        self.custom.as_deref().or(self.agent.as_deref()).or(self.ai.as_deref())
    }

    /// The first line of the first prompt, as a title of last resort.
    pub fn first_line(&self) -> String {
        first_line(self.asked.as_deref())
    }
}

/// The first line of `text`, 80 characters at most, or `(untitled)`.
pub fn first_line(text: Option<&str>) -> String {
    let text = text.unwrap_or("").trim();
    match text.split(is_line_break).next() {
        Some(line) if !text.is_empty() => truncate_chars(line, 80).to_string(),
        _ => "(untitled)".to_string(),
    }
}

/// Python's `str.splitlines` boundaries.
fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Read and fold one transcript file. Lines that aren't JSON are skipped; invalid UTF-8 is
/// replaced, never fatal.
pub fn parse_file(path: &Path) -> std::io::Result<TranscriptInfo> {
    let bytes = std::fs::read(path)?;
    let mut parser = Parser::default();
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() || skip_by_type(line) {
            continue;
        }
        let entry: Option<Value> = serde_json::from_slice(line).ok().or_else(|| {
            std::str::from_utf8(line)
                .is_err()
                .then(|| serde_json::from_str(&String::from_utf8_lossy(line)).ok())
                .flatten()
        });
        if let Some(e) = entry.filter(Value::is_object) {
            parser.feed(&e);
        }
    }
    Ok(parser.finish())
}

/// True when a line plainly is of a type the board ignores. Only a line whose first key is
/// `"type"` is judged; on others the first `"type"` may belong to a nested object (an
/// `attachment` line has its payload first), so they are parsed and judged by their real type.
fn skip_by_type(line: &[u8]) -> bool {
    const PREFIX: &[u8] = br#"{"type":""#;
    let Some(rest) = line.strip_prefix(PREFIX) else { return false };
    let Some(end) = rest.iter().take(64).position(|b| *b == b'"') else { return false };
    !KINDS.iter().any(|k| k.as_bytes() == &rest[..end])
}

#[derive(Debug)]
struct Pending {
    what: String,
    tool: Option<PendingTool>,
    since: f64,
}

/// The fold over a transcript's entries.
#[derive(Debug, Default)]
pub struct Parser {
    info: TranscriptInfo,
    /// Tool calls with no result yet, by tool_use id, in the order they were first made.
    pending: Vec<(Option<String>, Pending)>,
    /// One reply is written as several entries sharing its message id; the largest count wins.
    outputs: HashMap<Option<String>, u64>,
    /// The span being extended.
    open: Option<Span>,
    /// Files touched so far, by path.
    files: HashMap<String, FileTouch>,
}

impl Parser {
    pub fn feed(&mut self, e: &Value) {
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("");
        if self.info.typed == 0 && is_clear(e) {
            // It began with /clear: a new conversation in an old session. Claude Code copies the
            // session's name into it as its title, so the title isn't its own.
            self.info.cleared = true;
        }
        match kind {
            "ai-title" => self.info.ai = string(e, "aiTitle"),
            "custom-title" => self.info.custom = string(e, "customTitle"),
            "agent-name" => self.info.agent = string(e, "agentName"),
            "user" | "assistant" if !truthy(e.get("isSidechain")) => self.message(kind, e),
            // Sidechain entries are subagents' traffic: not what was said to you.
            _ => {}
        }
    }

    fn message(&mut self, kind: &str, e: &Value) {
        if kind == "user"
            && let Some(r) = e.get("toolUseResult").filter(|r| r.is_object())
        {
            self.tool_result(r, epoch(e.get("timestamp")));
        }
        let t = &mut self.info;
        if let Some(c) = path_field(e, "cwd") {
            t.cwd.get_or_insert_with(|| c.clone());
            t.cwd_last = Some(c);
        }
        if t.entry.is_none() {
            t.entry = string(e, "entrypoint");
        }
        if kind == "user" && t.root.is_none() {
            // Copies of one conversation (a session moved to the background) keep the uuid of
            // its first message, so it identifies the conversation across copies.
            t.root = string(e, "uuid");
        }
        let ts = epoch(e.get("timestamp"));
        t.last = t.last.max(ts);
        let msg = e.get("message").filter(|m| m.is_object()).unwrap_or(&Value::Null);
        let content = msg.get("content");
        let said = message_text(content);
        let said = said.trim();

        let mut bridged = false;
        if kind == "user" {
            for part in parts(content).filter(|p| p.get("type").and_then(Value::as_str) == Some("tool_result")) {
                let id = string(part, "tool_use_id");
                if let Some(i) = self.pending.iter().position(|(k, _)| *k == id) {
                    // A tool run, however long, is one stretch of work.
                    bridged = true;
                    self.pending.remove(i);
                }
            }
            if matches!(text(e, "promptSource"), Some("typed" | "queued")) {
                self.pending.clear(); // a new prompt: whatever was asked before isn't waiting any more
                if !said.is_empty() && !said.starts_with('/') {
                    t.typed += 1;
                    let prompt = truncate_chars(said, TEXT_LIMIT).to_string();
                    if t.asked.is_none() {
                        t.asked = Some(prompt.clone());
                    }
                    t.last_asked = Some(prompt);
                }
            }
        } else {
            if !said.is_empty() {
                t.said = Some(truncate_chars(said, TEXT_LIMIT).to_string());
            }
            for part in parts(content).filter(|p| p.get("type").and_then(Value::as_str) == Some("tool_use")) {
                let id = string(part, "id");
                let (what, tool) = describe_call(part.get("name").and_then(Value::as_str), part.get("input"));
                let call = Pending { what, tool, since: ts };
                // Like a dict: a repeated id keeps its place and takes the new value.
                match self.pending.iter_mut().find(|(k, _)| *k == id) {
                    Some(slot) => slot.1 = call,
                    None => self.pending.push((id, call)),
                }
            }
            self.usage(e, msg);
        }
        self.mark(ts, bridged);
    }

    /// The files a tool result read or changed.
    fn tool_result(&mut self, r: &Value, ts: f64) {
        let ts = ts as i64;
        if let Some(path) = text(r, "filePath") {
            if r.get("structuredPatch").is_some() || r.get("content").is_some() {
                let f = self.touch(path, ts);
                f.edits += 1;
                if text(r, "type") == Some("create") {
                    f.created = true;
                    // A new file's patch is empty: all of its content is added.
                    let content = text(r, "content").unwrap_or("");
                    f.added += content.lines().count() as u32;
                } else {
                    let (a, d) = patch_counts(r.get("structuredPatch"));
                    f.added += a;
                    f.removed += d;
                }
            }
        } else if let Some(path) = r.get("file").and_then(|f| text(f, "filePath")) {
            self.touch(path, ts).read = true;
        }
        if let Some(diff) = r.get("bashEditDiff").filter(|d| d.is_object()) {
            let mut seen: Vec<&str> = Vec::new();
            for f in diff.get("files").and_then(Value::as_array).into_iter().flatten() {
                let Some(path) = text(f, "filePath") else { continue };
                let (a, d) = patch_counts(f.get("hunks"));
                let t = self.touch(path, ts);
                t.shell = true;
                t.added += a;
                t.removed += d;
                seen.push(path);
            }
            // Files past the ones with hunks are only named.
            for path in diff.get("changedFiles").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                if !seen.contains(&path) {
                    self.touch(path, ts).shell = true;
                }
            }
        }
    }

    fn touch(&mut self, path: &str, ts: i64) -> &mut FileTouch {
        let path = crate::config::canon(path);
        let f = self.files.entry(path.clone()).or_insert_with(|| FileTouch { path, ..FileTouch::default() });
        f.last = f.last.max(ts);
        f
    }

    fn usage(&mut self, e: &Value, msg: &Value) {
        let Some(model) = text(msg, "model").filter(|m| *m != "<synthetic>") else { return };
        let Some(usage) = msg.get("usage").filter(|u| truthy(Some(u))) else { return };
        let count = |k: &str| count(usage.get(k));
        self.info.model = Some(model.to_string());
        self.info.context =
            count("input_tokens") + count("cache_creation_input_tokens") + count("cache_read_input_tokens");
        let key = string(msg, "id").or_else(|| string(e, "uuid"));
        let out = self.outputs.entry(key).or_default();
        *out = (*out).max(count("output_tokens"));
    }

    /// Extend the open span with an entry at `ts`, or close it and open a new one.
    fn mark(&mut self, ts: f64, bridged: bool) {
        if ts <= 0.0 {
            return;
        }
        match &mut self.open {
            Some(span) if ts - span.end < SPAN_GAP || bridged => {
                span.start = span.start.min(ts);
                span.end = span.end.max(ts);
            }
            open => {
                if let Some(done) = open.replace(Span { start: ts, end: ts }) {
                    self.info.spans.push(done);
                    if self.info.spans.len() > SPAN_LIMIT {
                        self.info.spans.remove(0);
                    }
                }
            }
        }
    }

    pub fn finish(mut self) -> TranscriptInfo {
        self.info.output = self.outputs.values().sum();
        let mut files: Vec<FileTouch> = self.files.into_values().collect();
        files.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.path.cmp(&b.path)));
        let (mut changed, mut read) = (0, 0);
        files.retain(|f| {
            let n = if f.changed() { &mut changed } else { &mut read };
            *n += 1;
            *n <= if f.changed() { CHANGED_LIMIT } else { READ_LIMIT }
        });
        self.info.files = files;
        if let Some((_, p)) = self.pending.pop() {
            self.info.pending = Some(p.what);
            self.info.pending_tool = p.tool;
            self.info.pending_since = (p.since > 0.0).then_some(p.since);
        }
        if let Some(span) = self.open.take() {
            self.info.spans.push(span);
            if self.info.spans.len() > SPAN_LIMIT {
                self.info.spans.remove(0);
            }
        }
        self.info
    }
}

fn is_clear(e: &Value) -> bool {
    let nested = |outer: &str, key: &str| e.get(outer).and_then(|o| o.get(key)).and_then(Value::as_str);
    match e.get("type").and_then(Value::as_str) {
        Some("attachment") => nested("attachment", "hookName") == Some("SessionStart:clear"),
        Some("system") => nested("commandRun", "command") == Some("clear"),
        _ => false,
    }
}

/// The objects in a message's content list (a string content has none).
fn parts(content: Option<&Value>) -> impl Iterator<Item = &Value> {
    content.and_then(Value::as_array).into_iter().flatten().filter(|p| p.is_object())
}

/// A message's text: a string content as is, or its text parts joined by newlines.
fn message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        _ => parts(content)
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .map(|p| p.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Lines added and removed in a list of patch hunks (`{lines: ["+…", "-…", " …"]}`).
fn patch_counts(hunks: Option<&Value>) -> (u32, u32) {
    let (mut added, mut removed) = (0, 0);
    for h in hunks.and_then(Value::as_array).into_iter().flatten() {
        for l in h.get("lines").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if l.starts_with('+') {
                added += 1;
            } else if l.starts_with('-') {
                removed += 1;
            }
        }
    }
    (added, removed)
}

/// A token count: a number (truncated) or numeric text; anything else is 0.
fn count(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// The arguments that say what a tool acts on, in order of preference.
const MAIN_ARGS: [&str; 7] = ["command", "file_path", "path", "url", "pattern", "description", "prompt"];

/// A pending tool call in a few words: the question asked, or the tool and what it acts on.
pub fn describe_tool(name: Option<&str>, args: Option<&Value>) -> String {
    describe_call(name, args).0
}

/// [`describe_tool`]'s text, and the call as tool and argument when it is a tool call (not a
/// question, and the tool has a name).
pub fn describe_call(name: Option<&str>, args: Option<&Value>) -> (String, Option<PendingTool>) {
    let empty = serde_json::Map::new();
    let args = args.and_then(Value::as_object).unwrap_or(&empty);
    if name == Some("AskUserQuestion") {
        let question = args
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|qs| qs.first())
            .and_then(|q| q.get("question"))
            .filter(|q| truthy(Some(q)));
        if let Some(q) = question {
            let q = q.as_str().map(String::from).unwrap_or_else(|| q.to_string());
            return (truncate_chars(&mask_secrets(&flat(&q)), TOOL_LIMIT).to_string(), None);
        }
    }
    // The first preferred argument that is set; if it isn't text, the first text argument.
    let main = MAIN_ARGS.iter().find_map(|k| args.get(*k).filter(|v| truthy(Some(v))));
    let main = match main.and_then(Value::as_str) {
        Some(s) => s,
        None => args.values().find_map(Value::as_str).unwrap_or(""),
    };
    let tool = name.filter(|n| !n.is_empty()).map(|n| PendingTool {
        name: n.to_string(),
        arg: truncate_chars(&mask_secrets(&flat(main)), TOOL_LIMIT).to_string(),
    });
    let name = name.filter(|n| !n.is_empty()).unwrap_or("a tool");
    let described = if main.is_empty() { name.to_string() } else { format!("{name}: {}", flat(main)) };
    (truncate_chars(&mask_secrets(&described), TOOL_LIMIT).to_string(), tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_skips_only_lines_led_by_an_ignored_type() {
        assert!(skip_by_type(br#"{"type":"api-request","x":1}"#));
        assert!(!skip_by_type(br#"{"type":"user","x":1}"#));
        // An attachment's own "type" comes after its payload's.
        assert!(!skip_by_type(br#"{"parentUuid":null,"attachment":{"type":"hook"},"type":"attachment"}"#));
    }

    #[test]
    fn first_line_is_trimmed_and_cut() {
        assert_eq!(first_line(Some("  \n hello\nworld")), "hello");
        assert_eq!(first_line(Some("   ")), "(untitled)");
        assert_eq!(first_line(None), "(untitled)");
        assert_eq!(first_line(Some(&"x".repeat(100))).len(), 80);
    }

    #[test]
    fn describe_tool_picks_the_main_argument() {
        let args = serde_json::json!({"timeout": 5, "description": "list", "command": "ls  -la"});
        assert_eq!(describe_tool(Some("Bash"), Some(&args)), "Bash: ls -la");
        let args = serde_json::json!({"n": 1, "other": "x"});
        assert_eq!(describe_tool(Some("T"), Some(&args)), "T: x");
        assert_eq!(describe_tool(None, None), "a tool");
        let args = serde_json::json!({"command": "export API_KEY=sk-123456\nmake"});
        let (_, tool) = describe_call(Some("Bash"), Some(&args));
        assert_eq!(tool, Some(PendingTool { name: "Bash".into(), arg: "export API_KEY=… make".into() }));
        let args = serde_json::json!({"questions": [{"question": "Which: this or that?"}]});
        assert_eq!(describe_call(Some("AskUserQuestion"), Some(&args)).1, None, "a question isn't a tool call");
        assert_eq!(describe_call(None, None).1, None);
    }

    #[test]
    fn spans_split_on_gaps_and_bridge_tool_runs() {
        let mut p = Parser::default();
        p.mark(1000.0, false);
        p.mark(1100.0, false);
        p.mark(5000.0, false); // a gap
        p.mark(9000.0, true); // a long tool run's result
        let t = p.finish();
        assert_eq!(t.spans, vec![Span { start: 1000.0, end: 1100.0 }, Span { start: 5000.0, end: 9000.0 }]);
    }

    #[test]
    fn files_come_from_tool_results() {
        use serde_json::json;
        let user = |ts: &str, r: Value| json!({"type": "user", "timestamp": ts, "message": {"content": []}, "toolUseResult": r});
        let mut p = Parser::default();
        p.feed(&user("2026-01-01T00:00:01Z", json!({"type": "text", "file": {"filePath": "/r/a.rs"}})));
        p.feed(&user("2026-01-01T00:00:02Z", json!({"filePath": "/r/a.rs", "oldString": "x",
            "structuredPatch": [{"lines": [" k", "-old", "+new", "+more"]}]})));
        p.feed(&user("2026-01-01T00:00:03Z", json!({"type": "create", "filePath": "/r/new.md", "content": "a\nb\nc\n", "structuredPatch": []})));
        p.feed(&user("2026-01-01T00:00:04Z", json!({"stdout": "", "bashEditDiff": {
            "files": [{"filePath": "/r/a.rs", "hunks": [{"lines": ["-gone"]}]}], "changedFiles": ["/r/a.rs", "/r/Cargo.lock"]}})));
        p.feed(&user("2026-01-01T00:00:05Z", json!({"type": "text", "file": {"filePath": "/r/b.rs"}})));
        // A failed tool's result is text: nothing.
        p.feed(&user("2026-01-01T00:00:06Z", json!("Error: no such file")));
        let t = p.finish();
        let got: Vec<_> = t.files.iter().map(|f| (f.path.as_str(), f.added, f.removed, f.edits, f.created, f.shell, f.read)).collect();
        assert_eq!(got, vec![
            ("/r/b.rs", 0, 0, 0, false, false, true),
            ("/r/Cargo.lock", 0, 0, 0, false, true, false),
            ("/r/a.rs", 2, 2, 1, false, true, true),
            ("/r/new.md", 3, 0, 1, true, false, false),
        ]);
        assert!(t.files[2].changed() && !t.files[0].changed());
    }

    #[test]
    fn spans_are_capped_to_the_latest() {
        let mut p = Parser::default();
        for i in 0..100 {
            p.mark(1000.0 + i as f64 * 1000.0, false);
        }
        let t = p.finish();
        assert_eq!(t.spans.len(), SPAN_LIMIT);
        assert_eq!(t.spans.last().unwrap().start, 100_000.0);
    }
}
