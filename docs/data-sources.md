# Where Claude Code keeps session data

Verified with Claude Code 2.1.310 on 2026-10-26, and 2.1.316 on 2026-11-01. These formats are internal and
can change with any update. The documented interfaces are hooks and `claude agents --json --all`.

| Source | Path | Fields worth using | Notes |
|---|---|---|---|
| Transcript (one per session) | `~/.claude/projects/<cwd, with / and . turned into ->/<sessionId>.jsonl` | Per line: `type`, `timestamp`, `cwd`, `gitBranch`, `entrypoint` (`cli`, `sdk-py`, `sdk-cli`), `promptSource` (typed, queued, system, sdk), `message`. Title lines: `ai-title.aiTitle`, `custom-title.customTitle`, `agent-name`. Also `last-prompt`. Remote Control link: `system` lines with subtype `bridge_status`, or `attachment.remote_session_change.url` (`https://claude.ai/code/session_…`). | Deleted after 30 days (`cleanupPeriodDays`). Folder names are lossy, so read `cwd` from inside the file. Subagent transcripts are in `<sessionId>/subagents/`. |
| Live-process registry | `~/.claude/sessions/<pid>.json` | `pid`, `sessionId`, `cwd`, `procStart`, `kind` (interactive, bg), `name`, `status` (seen: `busy`), `statusUpdatedAt`, `jobId`, `parkedJobId`, `messagingSocketPath` | A crash leaves the file behind. A session is alive only if field 22 of `/proc/<pid>/stat` equals `procStart`. On Windows `procStart` is the process creation time as a FILETIME (100 ns ticks since 1601, e.g. `134380575810509906`), compared with `GetProcessTimes`. |
| Background jobs (agent view) | `~/.claude/jobs/<8-char id>/state.json` + `timeline.jsonl` | `name`, `state`, **`tempo`** (blocked = needs you, active = working, idle = parked), `detail` (one-line status), **`needs`** (what it waits for, in Claude Code's words, e.g. "Reply yes to commit…", "rate limited — wait and retry · …"), `suggestedReply`, `sessionId`, `cwd`, `updatedAt` | Reattach with `claude attach <id>`. |
| Background workers | `~/.claude/daemon/roster.json` | `supervisorPid`, `workers.<job id>.pid`, `.procStart`, `.sessionId`, `.attempt` | Which jobs have a process. Never print the whole file: it holds socket auth tokens (`rvAuth`, `ptyAuth`). |
| Prompt history | `~/.claude/history.jsonl` | `display`, `project`, `sessionId`, `timestamp` | Every prompt you typed. |
| Hooks | `settings.json` | `session_id`, `cwd`, `hook_event_name`, Notification reason, SessionEnd reason | The only documented live source of working vs waiting. |

## Copies of one conversation

Sending a session to the background copies its transcript under a new session id. Both copies keep the
`uuid` of the first user message, so that uuid identifies the conversation; a job's `resumeSessionId`
also names the transcript it continued from. The board shows one row per conversation and folds only
*ended* copies into it.

A session resumed with `claude --bg --resume <id>` and no prompt runs no turn. Its job reports
`tempo: blocked` with `detail: "(idle — send a prompt to start)"`: it is idle, not waiting for you.
Its short id is the first 8 characters of the session id.

## /clear in a background session

`/clear` starts a new transcript in the same job, but the job and `claude agents` keep reporting the
first session id. The job's `resumeSessionId` names the transcript it is on now (checked on 2.1.316 for
three jobs). A transcript that began with `/clear` starts with an `attachment` whose `hookName` is
`SessionStart:clear` and a `system` line whose `commandRun.command` is `clear`, and Claude Code writes the
session's name into it as `ai-title`/`agent-name`, so its title is inherited, not its own. The board
counts `resumeSessionId` as the current conversation only when that transcript began with `/clear`
(a `--bg --resume` copy uses the field for the original), shows the first transcript as
"<name> (before /clear)", and drops the job's row when the current conversation is open in a terminal
(`claude --continue`), so one conversation is one row.

## Deriving status

- **running:** the registry pid is alive and its start time matches.
- **needs you:** a background job with `tempo: blocked`. For an interactive session, use a Notification
  hook, or try the registry `status` field.
- **not running (stale):** a background job whose `tempo` says blocked or active while the roster has
  no live worker for it (pid alive, `procStart` matching). Checked on 2.1.316: the supervisor restarts a
  worker that dies (`attempt` goes up), but when the supervisor itself is gone (reboot, crash) no job
  runs, and every job keeps its last tempo. In one case the supervisor was down and a job had said
  "needs you" for a rate limit for over a month. With no roster file, nothing is called stale.
- **waiting on:** `state.json` `needs`, else the transcript's last `tool_use` with no `tool_result`
  (AskUserQuestion: its question). A typed prompt clears it. Secrets in commands are masked.
- **model and tokens:** each assistant transcript entry has `message.model` and `message.usage`. Context
  is the last reply's `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`; one reply is
  written as several entries with the same `message.id`, so output tokens are summed once per id.
  `<synthetic>` entries are Claude Code's own notices. Seen contexts reach 847k (Opus 5.5, Fable 5.1);
  auto-compact fired at 285k–580k, so there is no fixed threshold to warn at.
- **parked:** a background job with `tempo: idle`. After `claude stop` it reports `state: stopped`,
  `detail: stopped` (checked on 2.1.316); `claude attach` opens it again.
- **ended:** a transcript only. Resume with `cd <cwd> && claude --resume <id>`. `claude rm` deletes
  a background job but leaves its transcript, so a deleted session looks ended from here on; the board
  hides those (`~/.local/state/session-board/deleted.json`).

## Deriving origin

- `entrypoint: cli`: a session you started.
- `sdk-py` whose first prompt is "Review this change for security…": the security-guidance plugin.
- `sdk-cli`: a headless `claude -p` run.

No field records a parent session. Infer one from the same folder plus a time window that contains the
automated session's start; that matched 85 of 159.

## Files a session touched

A `user` entry that answers a tool call carries the tool's result in `toolUseResult` (checked on
2.1.316):

- `Read`: `{type: "text", file: {filePath, …}}`.
- `Edit` / `Write`: `{filePath, structuredPatch: [{lines: ["+…", "-…", " …"]}], …}`; `Write` adds
  `type: "create"` (the patch is empty, `content` is the new file) or `"update"`.
- `Bash`: `bashEditDiff: {files: [{filePath, hunks}], changedFiles, moreFiles}` when the command changed
  files, so `sed -i`, scripts and `git checkout` show up too.
- A failed call's result is a string.

The board counts `+`/`-` lines per file. It keeps 60 changed and 40 read-only files per transcript.

## Where a session works

Each entry's `cwd` follows the session as it moves: a `cd` in a shell command, or entering a worktree.
Claude Code (and `claude agents`) report the launch folder, and the transcript stays under the launch
folder's project directory. The board groups a session by its latest folder only when that folder is a
linked worktree, or when the session was launched from a workspace (`~`) and is now in a repository.
Otherwise a passing `cd` would move it. A linked worktree's `.git` is a file, `gitdir:
<repo>/.git/worktrees/<name>` (absolute, or relative with `worktree.useRelativePaths`). The board
follows it to the main repository, so a worktree joins its repository's project wherever its folder is.
Actions still run in the launch folder, where `claude --resume` finds the conversation.

## Git (per folder)

The Git card runs `git --no-optional-locks` (read-only: `status` doesn't refresh the index) with
`rev-parse --show-toplevel --git-common-dir`, `status --porcelain=v2 --branch`, `log -1` and
`worktree list --porcelain`. It never fetches. Ahead/behind are as of the last fetch, so the card shows
`FETCH_HEAD`'s time beside them. Folders are read in parallel, each answer reused for 20 seconds.
