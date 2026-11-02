# session-board

A terminal board for every Claude Code session on a machine, as a replacement for Claude Code's Agent
View. It shows background sessions (`claude --bg`), sessions open in other terminals, and the last 30
days of ended conversations, grouped by project, with what each one is doing and what it needs from you.
Enter jumps into a session (`claude attach`); **Ctrl+Z** inside it comes back to the board.

![Columns view](docs/shots/columns-tool-call-160x45.svg)

It reads Claude Code's own files and its CLI, and every action goes through the `claude` CLI. Nothing
leaves the machine. Written in Rust with [Ratatui](https://ratatui.rs).

## Three views

Tab (and Shift+Tab) cycles them.

- **Columns** (the default): projects → sessions → the conversation. A pinned "Needs you" entry gathers
  every session waiting on you; the conversation column shows your ask, Claude's reply, and what it is
  waiting on, with a pending tool call split into the tool and its command. Below that are two cards:
  - **Files:** the files the session changed (✎ edited, + created, $ changed by a shell command, with
    lines added and removed), then the ones it only read, folded by folder.
  - **Git:** its folder's branch, ahead/behind its upstream as of the last fetch, uncommitted files,
    the last commit and the number of worktrees. The board never fetches.

  ![Files and Git cards](docs/shots/columns-work-160x50.svg)
- **Timeline**: each session's activity on a 24-hour or 7-day axis (`t` switches), with a marker where a
  session started waiting on you and how long it has waited.
  ![Timeline view](docs/shots/timeline-24h.svg)
- **Finder**: type to fuzzy-search titles, projects and folders (ended sessions too), with a preview.
  `/` opens it from the other views; Ctrl+K opens an action menu that shows the exact `claude` command.
  ![Finder view](docs/shots/finder-2-query-infra.svg)

## Requirements

- Claude Code with background sessions (`claude --bg`, `claude attach`, `claude agents`). Checked on
  2.1.316. The board reads files Claude Code doesn't document, so a Claude Code update can break it;
  `docs/data-sources.md` lists what it reads.
- Linux, macOS or Windows. On Linux and Windows, liveness checks also compare process start times,
  which macOS can't. See [Windows](#windows) for what differs there.
- To build: Rust (stable) via [rustup](https://rustup.rs).

## Install

```bash
git clone https://github.com/ryanckelly/session-board.git ~/session-board
cd ~/session-board
cargo install --path . --root ~/.local     # installs ~/.local/bin/board
board                                      # or: board --list, board --json, board --help
```

Make sure `~/.local/bin` is on your `PATH`. To update, `git pull` and run the `cargo install` line again.

### Windows

The same steps work in PowerShell; the build needs the MSVC toolchain (rustup's default, with the
Visual Studio C++ Build Tools):

```powershell
git clone https://github.com/ryanckelly/session-board.git $HOME\session-board
cd $HOME\session-board
cargo install --path . --root $HOME\.local    # installs ~\.local\bin\board.exe, next to claude.exe
```

What differs from Linux:

- Home is `$HOME` if set (Git Bash sets it), else `USERPROFILE`. Paths are shown with `/`
  (`C:/opt/app`), which Windows, git and `claude` all accept.
- `SESSION_BOARD_WORKSPACES` is separated by `;`, as `PATH` is. `scratch` defaults to `%TEMP%`.
- The board's files get your profile folder's permissions (its ACL) rather than `0600`/`0700`.
- Liveness compares a process's creation time from `GetProcessTimes` with `procStart`, which Claude
  Code writes on Windows in that form.

## Keys

| Key | Does |
|---|---|
| Enter | Jump into the session. An ended one is resumed in the background first. |
| Ctrl+Z (inside a session) | Back to the board. ← then Esc also works, through Agent View. |
| Tab / Shift+Tab | Next / previous view. |
| `n` | New background session: folder, optional name, optional first prompt; then jump in. |
| `s` / `x` | Stop / delete a background session. Delete asks first. |
| `e` | Show or hide ended sessions. Hidden at first; remembered. |
| `/` | Search, in Finder. |
| `r` / `q` | Refresh / quit (Ctrl+C quits too). |
| Esc | Close a dialog, clear the search, or go back. Never quits. |
| F1 (or `?` outside Finder) | Help. |
| ← → and `c` | Columns: move between columns; show or hide the metadata strip. |
| `t` | Timeline: 24 hours / 7 days. |
| Ctrl+K, Ctrl+S, Ctrl+X, Ctrl+E | Finder, where letters are search text: menu, stop, delete, ended. |

## States

| | State | Meaning |
|---|---|---|
| ◆ | needs you | A background session is waiting for you. The row says what for, from Claude Code's own words or the pending tool call. |
| ● | running | Working on a turn. |
| ● | in another terminal | An interactive session open elsewhere. It can't be entered from here. |
| ■ | idle | Waiting for a prompt, or stopped. |
| ◇ | not running | A background job that claims to need you or to be working, but Claude Code has no process for it: its state is out of date (after a reboot, say). Enter starts it again. |
| ○ | ended | A saved conversation. Enter resumes it in the background. |

A session that moved into a git worktree is listed under the worktree's repository, wherever the
worktree's folder is. So is one launched from a workspace (`~`) that moved into a repository.

## Configuration

Defaults work without any config. To change them, create `~/.config/session-board/config.toml`:

```toml
# A folder directly under a workspace is a project of its own (~/work/api, ~/work/infra), even when
# the workspace itself is a git repo. Sessions elsewhere are grouped by their git repo.
workspaces = ["~", "~/work"]
scratch = "/tmp"        # throwaway sessions under here are one "scratch" project
days = 30               # how far back ended sessions go; match Claude Code's cleanupPeriodDays
poll = 10               # seconds between refreshes
claude = "claude"       # the claude CLI, if it isn't on PATH as "claude" (may include arguments)
```

Each setting can also come from an environment variable, which wins over the file:
`SESSION_BOARD_WORKSPACES` (folders separated by `:`, or `;` on Windows), `SESSION_BOARD_SCRATCH`, `SESSION_BOARD_DAYS`,
`SESSION_BOARD_POLL` and `SESSION_BOARD_CLAUDE`. `SESSION_BOARD_CONFIG` points at another config file.
Claude Code's folder follows `CLAUDE_CONFIG_DIR`, as Claude Code does. The cache and the board's own
state follow `XDG_CACHE_HOME` and `XDG_STATE_HOME`.

Colours are true colour, falling back to 256 colours when `COLORTERM` isn't `truecolor` or `24bit`.
Under tmux, which often hides `COLORTERM`, set `SESSION_BOARD_COLOR=truecolor` (or `256`).

`board --config` prints the settings in effect and where each came from.

## The mod: "N other sessions need you" inside Claude Code

`mod/` is a Claude Code mod. It draws a band above the prompt in every session when other sessions need
you, e.g. "◆ 2 other sessions need you: Fix the flaky auth test · Rotate the staging certs ·
Ctrl+Z: board". It reads `board --json` every 30 seconds.

It also adds **`/board`**: a pane beside the chat listing every live session, needs-you first and then
by project. The details at the top (state, model, context) follow the arrow keys; Enter or `c` copies
the session's `claude attach` command; Esc closes the pane. It can't jump into another session (no mod can end
`claude attach`); Ctrl+Z to the full board does that.

To load it in every session, add this to the `env` block of `~/.claude/settings.json`, using the path
of your clone:

```json
"CLAUDE_CODE_PLUGIN_DIRS": "/path/to/session-board/mod"
```

It runs `~/.local/bin/board` (`board.exe` on Windows). If the board is installed elsewhere, also set `"SESSION_BOARD_BIN":
"/path/to/board"` in the same block. To try it in a single session: `claude --plugin-dir
/path/to/session-board/mod`. Check it with `claude plugin validate mod/` and `claude plugin test mod/`.

What a mod can't do (Claude Code 2.1.316): redirect ← to this board instead of Agent View, or end
`claude attach`. Ctrl+Z is the direct way back.

## Files the board writes

- `~/.cache/session-board/transcripts-rs.json`: parsed transcripts, cached by size and mtime, readable
  by you only. It holds excerpts (first prompt, last reply), with anything that looks like a credential
  in a pending command masked. Safe to delete.
- `~/.local/state/session-board/prefs.json`: the `e` and `c` choices.
- `~/.local/state/session-board/deleted.json`: conversations deleted with `x`. `claude rm` deletes the
  background session but keeps its transcript, which would otherwise come back as an ended row. Entries
  expire after the `days` setting.

## Development

```bash
cargo test                          # the board: facts, actions, config, views
cargo clippy --all-targets
claude plugin test mod/             # the mod
board --shot out.svg --size 160x45 --view columns --data tests/fixtures/sample.json --home /home/u
```

- `src/facts.rs` and `src/claude/` turn Claude Code's files into sessions and states.
- `src/actions.rs` holds the rules for each action and wraps the `claude` CLI.
- `src/config.rs` reads the settings.
- `src/tui/` is the app: `app.rs` routes keys and runs actions, `views/` holds the three views (see
  `src/tui/views/README.md`), `widgets.rs` and `theme.rs` are shared by all of them.
- `board --shot` draws one frame as an SVG through the real app; the screenshots in `docs/shots/` come
  from it, using the made-up sessions in `tests/fixtures/`.

The tests never read the machine's own config or Claude Code folder.

## License

MIT; see `LICENSE`.
