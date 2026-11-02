// Companion to the session board, a terminal app that replaces Agent View.
//
// It runs `board --json`: the command in SESSION_BOARD_BIN if set (put it in the `env` block of
// ~/.claude/settings.json), else ~/.local/bin/board.
//
// A band above the prompt: "◆ N other sessions need you: <titles>", and in a background session the
// way back to the board, "Ctrl+Z: board". Nothing when no other session needs you.
//
// /board opens a pane beside the chat: a mini board of the live sessions (needs you first, then each
// project's), with the selected one's details and its attach command to copy. It can't jump into a
// session (see below); Ctrl+Z to the full board does that.
//
// What it can't do (checked on Claude Code 2.1.316; details in the sb-board-mod bead):
// - Redirect ←. It opens Agent View from the prompt's own key handling, not through a keybinding
//   action a mod Button could take over (and a Button's `action` only answers chords or modified keys).
// - End `claude attach`. No call on `$` makes a client leave. Claude Code's /exit in a background
//   session, typed or run by a mod, detaches into Agent View. Ctrl+Z is the one key that goes
//   straight back to the board, so /board shows the board here rather than trying to leave.
import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, UiPressArgument } from 'claude-code'

import type { Live, LiveBoard, Others, Waiting } from '../types'

/** How often the band re-reads the board; `board --json` takes about 0.3 s. */
const POLL_MS = 30_000

const others = atom({ plugin: 'session-board', key: 'others' } as const, null, {
  shape: 'others-1',
})
/** Every live session, for the /board pane; read with the band's facts, in the same run. */
const live = atom({ plugin: 'session-board', key: 'live' } as const, null, { shape: 'live-1' })
/** The session selected in the pane, by id. */
const picked = atom({ plugin: 'session-board', key: 'picked' } as const, null, { shape: 'picked-1' })

const PANE = 'board'
/** The session's refresh, so /board can read the board at once. Set at session.start. */
let refreshNow: (() => Promise<void>) | undefined

/** One row of `board --json`, the fields the band and the pane use. */
type Row = {
  session_id: string
  job: string | null
  title: string
  state: string
  stopped?: boolean
  project?: string
  cwd?: string
  last?: number
  model?: string | null
  context?: number
}

/**
 * The other sessions that need you, or null when the board can't be read. The one place the band
 * gets its facts: `board --json` today; sb-tui-live will add a hooks-fed state folder to read instead.
 */
async function readOthers($: EngineInterface): Promise<{ others: Others; live: LiveBoard } | null> {
  // Windows sets OS=Windows_NT and USERPROFILE; HOME only under Git Bash or MSYS.
  const windows = (await $.env.get('OS')) === 'Windows_NT'
  const home = (await $.env.get('HOME')) ?? (windows ? await $.env.get('USERPROFILE') : undefined)
  const jobDir = await $.env.get('CLAUDE_JOB_DIR')
  const self = await $.session.id()
  const myJob = jobDir?.split(/[\\/]/).pop()
  const board =
    (await $.env.get('SESSION_BOARD_BIN')) || `${home ?? ''}/.local/bin/board${windows ? '.exe' : ''}`
  const run = await $.process.run([board, '--json'], { timeoutMs: 20_000 })
  if (run.exitCode !== 0) {
    return null
  }
  let rows: Row[]
  try {
    rows = JSON.parse(run.stdout) as Row[]
  } catch {
    return null
  }
  const isSelf = (s: Row) => s.session_id === self || Boolean(myJob && s.job === myJob)
  const needYou: Waiting[] = rows
    .filter(s => s.state === 'needs you' && !isSelf(s))
    .map(s => ({ title: s.title, job: s.job }))
  const at = await $.clock.now()
  // The pane shows live sessions only; ended ones are counted. The board's order is kept.
  const sessions: Live[] = rows
    .filter(s => s.state !== 'ended')
    .map(s => ({
      id: s.session_id,
      job: s.job,
      title: s.title,
      state: s.state,
      stopped: Boolean(s.stopped),
      project: s.project ?? s.cwd ?? '?',
      last: s.last ?? 0,
      model: s.model ?? null,
      context: s.context ?? 0,
      isSelf: isSelf(s),
    }))
  const ended = rows.length - sessions.length

  return { others: { needYou, at }, live: { sessions, ended, at, home: home ?? '' } }
}

/** The state's glyph, as the board draws it. */
const GLYPH: Record<string, string> = {
  'needs you': '◆',
  running: '●',
  elsewhere: '●',
  idle: '■',
  stale: '◇',
}

/** How the board names a state. */
function stateWord(s: Live): string {
  if (s.stopped) {
    return 'stopped'
  }
  return s.state === 'elsewhere' ? 'in another terminal' : s.state === 'stale' ? 'not running' : s.state
}

/** "now", "5m", "3h", "2d", as the board says it. */
function ago(last: number, nowMs: number): string {
  const s = nowMs / 1000 - last
  if (s < 90) return 'now'
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`
  return `${Math.floor(s / 86400)}d`
}

function tokens(n: number): string {
  if (n < 1000) return String(n)
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`
  return `${(n / 1_000_000).toFixed(1)}M`
}

/** claude-opus-5-5 → Opus 5.5. */
function modelName(m: string | null): string {
  const hit = m?.match(/^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?(?!\d)/)
  if (!hit) return m ?? ''
  const name = hit[1]!.charAt(0).toUpperCase() + hit[1]!.slice(1)
  return `${name} ${hit[2]}${hit[3] ? '.' + hit[3] : ''}`
}

/** A project's name: its folder's last part, `~` for home. */
function projectName(path: string, home: string): string {
  if (home && path === home) return '~ (home)'
  return path.split('/').filter(Boolean).pop() ?? path
}

function cut(text: string, width: number): string {
  return text.length <= width ? text : text.slice(0, Math.max(1, width - 1)) + '…'
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    let isReading = false
    const refresh = async () => {
      // Nothing draws in a headless run (`claude -p`, the SDK): don't read the board for it.
      if (isReading || (await $.session.surfaces()).length === 0) {
        return
      }
      isReading = true
      try {
        const now = await readOthers($)
        await update($, others, () => now?.others ?? null)
        await update($, live, () => now?.live ?? null)
      } catch {
        await update($, others, () => null)
        await update($, live, () => null)
      } finally {
        isReading = false
      }
    }
    void refresh()
    $.clock.every(POLL_MS, () => {
      void refresh()
    })
    refreshNow = refresh
    await $.command.register({
      name: 'board',
      description: 'Show every live session in a pane: what needs you, what is running',
      immediate: true,
    })

    return next(e)
  })

  // The details follow the focus ring: moving onto a session's row selects it.
  on('ui.focus', { component: 'Pane', requestId: PANE }, async ($, e, next) => {
    const id = e.element?.match(/^(?:needs|project):(.+)$/)?.[1]
    if (id) {
      await update($, picked, () => id)
    }
    return next(e)
  })

  // /board opens the pane (or brings it back) and reads the board at once.
  on('command.run', { command: 'board' }, async $ => {
    await $.ui.open({ id: PANE, title: 'Sessions', focus: true, closeOnEscape: true })
    void refreshNow?.()

    return {}
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Button } = $.ui.resolve(e)
    const board = await read($, live)
    const sel = await read($, picked)
    const width = Math.max(20, e.props.bodyColumns)
    if (board === null) {
      return (
        <Box flexDirection="column">
          <Text dimColor>Reading the board…</Text>
          <Text dimColor>(It runs ~/.local/bin/board --json, or $SESSION_BOARD_BIN.)</Text>
        </Box>
      )
    }
    const { sessions, ended, at, home } = board
    const needs = sessions.filter(s => s.state === 'needs you')
    const running = sessions.filter(s => s.state === 'running').length

    // Projects in the board's order: the first appearance of each, its sessions under it.
    const projects: { path: string; rows: Live[] }[] = []
    for (const s of sessions) {
      const p = projects.find(x => x.path === s.project)
      if (p) p.rows.push(s)
      else projects.push({ path: s.project, rows: [s] })
    }

    /** Copy a session's attach command, the one way into it from here; say so when it has none. */
    const copyAttach = (s: Live, surface?: UiPressArgument['surface']) => {
      if (!s.job) {
        $.ui.toast(s.state === 'elsewhere' ? `“${s.title}” is open in another terminal.` : 'Enter it from the board (Ctrl+Z).')
        return
      }
      const text = `claude attach ${s.job}`
      void $.ui.copy({ text, surface }).then(r => {
        $.ui.toast(r.isCopied ? `Copied: ${text}` : 'Could not copy')
      })
    }

    const row = (s: Live, under: 'project' | 'needs') => {
      const right = under === 'needs' ? `${projectName(s.project, home)} · ${ago(s.last, at)}` : ago(s.last, at)
      const room = width - right.length - 5
      const title = cut(s.title + (s.isSelf ? ' (this)' : ''), room)
      const hot = s.state === 'needs you'
      return (
        <Button key={`${under}:${s.id}`} plain onPress={press => copyAttach(s, press.surface)}>
          <Text color={hot ? 'warning' : s.state === 'running' ? 'success' : undefined} dimColor={!hot && s.state !== 'running'}>
            {GLYPH[s.state] ?? '○'}{' '}
          </Text>
          <Text bold={sel === s.id} color={hot ? 'warning' : undefined}>
            {title.padEnd(room)}
          </Text>
          <Text dimColor> {right}</Text>
        </Button>
      )
    }

    // The details follow the focus ring (see the ui.focus hook); before it moves, the first session
    // that needs you, else the first one listed.
    const chosen = sessions.find(s => s.id === sel) ?? needs[0] ?? sessions[0]

    return (
      <Box flexDirection="column">
        <Text>
          <Text color="warning" bold>
            ◆ {needs.length} need you
          </Text>
          <Text dimColor>
            {'  '}● {running} running · {sessions.length} live{ended ? ` · ${ended} ended` : ''}
          </Text>
        </Text>
        {chosen && (
          <Box flexDirection="column">
            <Text bold color={chosen.state === 'needs you' ? 'warning' : undefined}>
              {cut(chosen.title, width)}
            </Text>
            <Text dimColor>
              {cut(`${stateWord(chosen)} · ${ago(chosen.last, at)} · ${projectName(chosen.project, home)}`, width)}
            </Text>
            {chosen.model && (
              <Text dimColor>{cut(`${modelName(chosen.model)} · ${tokens(chosen.context)} in context`, width)}</Text>
            )}
            {chosen.job ? (
              <Button key="copy" hotkey="c" plain dimColor onPress={press => copyAttach(chosen, press.surface)}>
                {cut(`copy claude attach ${chosen.job}`, width - 3)}
              </Button>
            ) : (
              <Text dimColor>{chosen.state === 'elsewhere' ? 'Open in another terminal.' : 'Enter it from the board (Ctrl+Z).'}</Text>
            )}
          </Box>
        )}
        <Text dimColor>{'─'.repeat(Math.min(width, 60))}</Text>
        {needs.length > 0 && <Text bold>Needs you</Text>}
        {needs.map(s => row(s, 'needs'))}
        {needs.length > 0 && <Text> </Text>}
        {projects.map(p => (
          <Box key={p.path} flexDirection="column">
            <Text dimColor bold>
              {projectName(p.path, home)}
            </Text>
            {p.rows.map(s => row(s, 'project'))}
          </Box>
        ))}
        <Text dimColor>{cut('↑↓ move · Enter or c: copy attach command · Esc close · Ctrl+Z: full board', width)}</Text>
      </Box>
    )
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const now = await read($, others)
    if (e.props.hasSurvey || now === null || now.needYou.length === 0) {
      return next(e)
    }

    const { Box, Text } = $.ui.resolve(e)
    const n = now.needYou.length
    const head = n === 1 ? '1 other session needs you' : `${n} other sessions need you`
    const titles = now.needYou.map(w => w.title).join(' · ')
    // Ctrl+Z ends `claude attach` only in a background session; elsewhere it suspends Claude Code.
    const isBackground = (await $.env.get('CLAUDE_CODE_SESSION_KIND')) === 'bg'

    return (
      <Box>
        <Text wrap="truncate-end">
          <Text color="warning" bold>
            ◆ {head}
          </Text>
          <Text dimColor>
            : {titles}
            {isBackground ? '  ·  Ctrl+Z: board' : ''}
          </Text>
        </Text>
      </Box>
    )
  })
}
