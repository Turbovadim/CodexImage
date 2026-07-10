#!/usr/bin/env node
import http from 'node:http'
import fs from 'node:fs'
import path from 'node:path'
import os from 'node:os'
import crypto from 'node:crypto'
import { spawn, type ChildProcess } from 'node:child_process'
import {
  MAX_ATTACHMENTS,
  MAX_ATTACHMENT_BYTES,
  MAX_ATTACHMENT_TOTAL_BYTES,
} from './src/types.ts'
import type { Board, BoardNode, BoardSummary, ServerEvent, StopReason } from './src/types.ts'

// Overridable because the Electron build runs a bundled copy from dist-electron/,
// and the packaged app keeps its data in ~/Library/Application Support.
const ROOT = process.env.CODEXIMAGE_ROOT || import.meta.dirname
const DIST_DIR = path.join(ROOT, 'dist')
const DATA_DIR = process.env.CODEXIMAGE_DATA || path.join(ROOT, 'data')
const BOARDS_FILE = path.join(DATA_DIR, 'boards.json')
const LEGACY_CHATS_FILE = path.join(DATA_DIR, 'chats.json')
const IMAGES_DIR = path.join(DATA_DIR, 'images')
const WORKSPACES_DIR = path.join(DATA_DIR, 'workspaces')
const LOGS_DIR = path.join(DATA_DIR, 'logs')
const CODEX_GEN_DIR = path.join(os.homedir(), '.codex', 'generated_images')

const PORT = Number(process.env.PORT || 4750)
const CODEX_BIN = process.env.CODEX_BIN || 'codex'
const POLL_MS = 1200
const MAX_BODY = 64 * 1024 * 1024
// Stop only jobs that have gone completely quiet. A fixed wall-clock deadline
// cuts off legitimate multi-image requests even while they are still making
// progress.
const JOB_IDLE_TIMEOUT_MINUTES = 20
const JOB_IDLE_TIMEOUT_MS = JOB_IDLE_TIMEOUT_MINUTES * 60 * 1000
const MAX_ACTIVE_PER_BOARD = 20

const PREAMBLE = [
  'You are an expert image-generation assistant.',
  '',
  'Hard rules:',
  '- ALWAYS create the requested image with your built-in image generation tool. Never draw images with code (SVG/HTML/canvas), never substitute placeholders, and never fetch images from the web.',
  '- The user automatically sees every image the tool produces, straight from where the tool saves it. Do NOT run shell commands to copy, move, inspect or verify image files unless the user explicitly asks for file operations.',
  '- Reply with one short sentence per image describing what you generated. No headings, no file paths, no questions unless the request is truly ambiguous.',
  '',
  'Prompting the image tool:',
  '- Rewrite the request into a clean spec ordered scene/backdrop -> subject -> key details -> constraints, and include the intended use (ad, UI mock, game asset, hero image) to set the polish level. For complex requests use short labeled lines (Subject, Style/medium, Composition/framing, Lighting/mood, Color palette, Text (verbatim), Constraints, Avoid) instead of one long paragraph.',
  '- Match augmentation to specificity: if the request is already detailed, normalize it into a spec without adding creative requirements; if it is generic, add tasteful detail only where it materially helps (composition and framing cues, polish level, reasonable scene concreteness). Never invent characters, props, brands, slogans, palettes, or story beats the user did not imply.',
  '- For photorealism, say "photorealistic", use photography language (lens, framing, lighting), and ask for real-world texture: pores, fabric wear, material grain, imperfect everyday detail.',
  '- If text must appear in the image, quote it verbatim, specify typography and placement, spell uncommon words letter-by-letter, and require exact rendering with no extra characters.',
  '- When image files are provided, treat each by its stated role: an image to continue from is the edit target; user attachments are style/composition references unless the request says to edit them. For compositing, say how the images interact with matched lighting, perspective, and scale.',
  '- For edits, state invariants explicitly ("change only X; keep Y unchanged"). Preserve identity aggressively when people are involved: lock face, body, pose, and expression unless asked to change them. Preserve everything the request does not ask to change.',
].join('\n')

interface Watcher {
  threadId: string
  seen: Set<string>
  sizes: Map<string, number>
}

interface Job {
  boardId: string
  nodeId: string
  proc: ChildProcess | null
  watchers: Map<string, Watcher>
  failures: string[]
  termination: StopReason | 'timeout' | 'replaced' | null
  poller: NodeJS.Timeout | null
  idleTimer: NodeJS.Timeout | null
  forceStopTimer: NodeJS.Timeout | null
  collection: Promise<boolean> | null
  log: fs.WriteStream
  completed: Promise<void>
  resolveCompleted: () => void
}

for (const dir of [DATA_DIR, IMAGES_DIR, WORKSPACES_DIR, LOGS_DIR]) fs.mkdirSync(dir, { recursive: true })

// ---------------------------------------------------------------------------
// Persistence + migration from the old linear-chat format
// ---------------------------------------------------------------------------

function migrateLegacyChats(): Board[] {
  const chats: any[] = JSON.parse(fs.readFileSync(LEGACY_CHATS_FILE, 'utf8'))
  return chats.map((chat: any): Board => {
    const nodes: BoardNode[] = []
    let parent: BoardNode | null = null
    const msgs: any[] = chat.messages || []
    for (let i = 0; i < msgs.length; i++) {
      const m = msgs[i]
      if (m.role !== 'user') continue
      const a = msgs[i + 1]?.role === 'assistant' ? msgs[i + 1] : null
      const node: BoardNode = {
        id: m.id,
        parentId: parent ? parent.id : null,
        prompt: m.text || '',
        aspect: m.options?.aspect || 'auto',
        sourceImages: parent ? parent.images.slice() : [],
        attachments: m.images || [],
        images: a?.images || [],
        text: a?.text || '',
        status: !a || a.status === 'running' ? 'error' : (a.status || 'done'),
        error: a?.error,
        createdAt: m.createdAt,
        finishedAt: a?.finishedAt,
        usage: a?.usage,
      }
      nodes.push(node)
      parent = node
    }
    return { id: chat.id, title: chat.title, createdAt: chat.createdAt, nodes }
  })
}

let boards: Board[] = []
try {
  boards = JSON.parse(fs.readFileSync(BOARDS_FILE, 'utf8'))
} catch {
  // Unparseable (e.g. crash mid-write) is different from missing: preserve the
  // bytes so the boards aren't silently clobbered by the next save.
  if (fs.existsSync(BOARDS_FILE)) {
    const backup = `${BOARDS_FILE}.corrupt-${Date.now()}`
    fs.copyFileSync(BOARDS_FILE, backup)
    console.error(`boards.json is unreadable; saved a copy to ${backup}`)
  }
  try {
    boards = migrateLegacyChats()
    writeBoardsFile()
  } catch { /* first run */ }
}
// A crash mid-generation leaves nodes stuck in 'running'. Record the reason
// instead of silently turning these into anonymous failures on next launch.
let recoveredInterruptedNodes = false
for (const board of boards) {
  for (const node of board.nodes) {
    if (node.status !== 'running') continue
    node.status = 'error'
    node.error = 'Generation was interrupted because CodexImage closed unexpectedly.'
    node.finishedAt = Date.now()
    recoveredInterruptedNodes = true
  }
}
if (recoveredInterruptedNodes) writeBoardsFile()

const jobs = new Map<string, Job>() // keyed by nodeId
const sseClients = new Map<string, Set<http.ServerResponse>>() // keyed by boardId
const thumbnailProcesses = new Set<ChildProcess>()

// Deleted subtrees are held here for a grace period so the client can offer
// Undo. Image files are untouched by node deletion (only board deletion
// removes them), so restoring is purely reinserting the node records.
const TRASH_TTL_MS = 5 * 60 * 1000
interface TrashEntry { boardId: string; nodes: BoardNode[]; timer: NodeJS.Timeout }
const trash = new Map<string, TrashEntry>() // keyed by undoId

// Write-to-temp + rename so a crash mid-write can never leave boards.json
// half-written (fs.writeFileSync alone is not atomic).
function writeBoardsFile(): void {
  const tmp = `${BOARDS_FILE}.tmp`
  fs.writeFileSync(tmp, JSON.stringify(boards, null, 2))
  fs.renameSync(tmp, BOARDS_FILE)
}

// Debounced: on large boards, image-collect polls and SSE bursts would
// otherwise rewrite the whole JSON file many times per second.
let saveTimer: NodeJS.Timeout | null = null
function saveBoards(): void {
  if (saveTimer) return
  saveTimer = setTimeout(() => {
    saveTimer = null
    writeBoardsFile()
  }, 400)
}
function flushBoards(): void {
  if (saveTimer) { clearTimeout(saveTimer); saveTimer = null }
  writeBoardsFile()
}
for (const sig of ['SIGINT', 'SIGTERM'] as const) {
  process.on(sig, () => {
    stopAllJobsImmediately()
    process.exit(0)
  })
}
// Generation processes run in detached groups, so they would otherwise
// survive an abrupt server shutdown. The normal app-close path drains them
// through the API; this is the final synchronous safety net.
process.on('exit', () => {
  for (const job of jobs.values()) killTree(job.proc, 'SIGKILL')
  for (const proc of thumbnailProcesses) proc.kill('SIGKILL')
})

// Card-size thumbnails next to originals (t_<name>). The queue keeps bursts
// from launching an unbounded number of sips processes while leaving the Node
// event loop free to serve Electron, SSE, and stop requests.
const MAX_CONCURRENT_THUMBNAILS = 2
const THUMBNAIL_TIMEOUT_MS = 10_000
interface ThumbnailTask { absPath: string; resolve: () => void }
const thumbnailQueue: ThumbnailTask[] = []
let activeThumbnails = 0

function thumbnailPath(absPath: string): string {
  return path.join(path.dirname(absPath), `t_${path.basename(absPath)}`)
}

async function removeImagesAndThumbnails(absPaths: string[]): Promise<void> {
  await Promise.allSettled(absPaths.flatMap(absPath => [
    fs.promises.rm(absPath, { force: true }),
    fs.promises.rm(thumbnailPath(absPath), { force: true }),
  ]))
}

function runThumbnail(absPath: string): Promise<void> {
  return new Promise(resolve => {
    const out = thumbnailPath(absPath)
    let proc: ChildProcess
    try {
      proc = spawn('sips', ['-Z', '720', absPath, '--out', out], { stdio: 'ignore' })
    } catch {
      resolve()
      return
    }
    thumbnailProcesses.add(proc)
    let settled = false
    const finish = (succeeded: boolean) => {
      if (settled) return
      settled = true
      thumbnailProcesses.delete(proc)
      clearTimeout(timeout)
      if (succeeded) resolve()
      else void fs.promises.rm(out, { force: true }).then(resolve, resolve)
    }
    const timeout = setTimeout(() => {
      proc.kill('SIGKILL')
      finish(false)
    }, THUMBNAIL_TIMEOUT_MS)
    proc.once('error', () => finish(false))
    proc.once('close', code => finish(code === 0))
  })
}

function drainThumbnailQueue(): void {
  while (activeThumbnails < MAX_CONCURRENT_THUMBNAILS && thumbnailQueue.length > 0) {
    const task = thumbnailQueue.shift()!
    activeThumbnails++
    void runThumbnail(task.absPath).finally(() => {
      activeThumbnails--
      task.resolve()
      drainThumbnailQueue()
    })
  }
}

/** Best-effort thumbnail creation with bounded concurrency. */
function makeThumb(absPath: string): Promise<void> {
  return new Promise(resolve => {
    thumbnailQueue.push({ absPath, resolve })
    drainThumbnailQueue()
  })
}

function getBoard(id: string): Board | undefined {
  return boards.find(b => b.id === id)
}

function generatingIds(boardId: string): string[] {
  return [...jobs.values()].filter(j => j.boardId === boardId).map(j => j.nodeId)
}

function sseSend(boardId: string, event: ServerEvent): void {
  const set = sseClients.get(boardId)
  if (!set) return
  const payload = `data: ${JSON.stringify(event)}\n\n`
  for (const res of set) res.write(payload)
}

function imageUrlToAbs(boardId: string, url: string): string | null {
  const prefix = `/images/${boardId}/`
  if (!url.startsWith(prefix)) return null
  const abs = path.normalize(path.join(IMAGES_DIR, boardId, url.slice(prefix.length)))
  return abs.startsWith(path.join(IMAGES_DIR, boardId) + path.sep) ? abs : null
}

// ---------------------------------------------------------------------------
// Generation — every node runs a fresh codex session. Context comes from the
// ancestor prompt chain (text) and the parent's output image (file on disk),
// so any node can be regenerated or branched at any time with no session
// state to fork or race on.
// ---------------------------------------------------------------------------

function ancestorPrompts(board: Board, node: BoardNode): string[] {
  const byId = new Map(board.nodes.map(n => [n.id, n]))
  const chain: string[] = []
  let cur = node.parentId ? byId.get(node.parentId) : undefined
  while (cur && chain.length < 12) {
    chain.unshift(cur.prompt.length > 400 ? cur.prompt.slice(0, 397) + '…' : cur.prompt)
    cur = cur.parentId ? byId.get(cur.parentId) : undefined
  }
  return chain
}

function buildNodePrompt(board: Board, node: BoardNode, index: number, count: number): string {
  const parts: string[] = [PREAMBLE]

  const chain = ancestorPrompts(board, node)
  if (chain.length) {
    parts.push('This request continues earlier work on an image. The prompts so far, oldest first:\n'
      + chain.map((p, i) => `${i + 1}. ${p}`).join('\n'))
  }

  const sourceAbs = node.sourceImages
    .map(u => imageUrlToAbs(board.id, u))
    .filter((p): p is string => p !== null && fs.existsSync(p))
  if (sourceAbs.length) {
    parts.push('The current image to continue from is saved at:\n'
      + sourceAbs.map(p => `- ${p}`).join('\n')
      + '\nView it first. The request below applies to this image: keep everything it does not ask to change.')
  }

  const attachAbs = node.attachments
    .map(u => imageUrlToAbs(board.id, u))
    .filter((p): p is string => p !== null && fs.existsSync(p))
  if (attachAbs.length) {
    parts.push('The user attached reference image file(s). View them before generating:\n'
      + attachAbs.map(p => `- ${p}`).join('\n'))
  }

  parts.push(`Request: ${node.prompt}`)

  const extras: string[] = []
  if (node.aspect && node.aspect !== 'auto') extras.push(`Aspect ratio: ${node.aspect}.`)
  if (count > 1) {
    extras.push('Produce exactly ONE image (a single image generation tool call). '
      + `${count} variations of this request are generated in parallel; this is variation ${index + 1} — give it its own distinct interpretation.`)
  }
  if (extras.length) parts.push(extras.join(' '))

  return parts.join('\n\n')
}

function addUsage(a: Record<string, number> | undefined, b: Record<string, number>): Record<string, number> {
  const out = { ...(a || {}) }
  for (const k of Object.keys(b)) {
    if (typeof b[k] === 'number') out[k] = (out[k] || 0) + b[k]
  }
  return out
}

async function collectImages(board: Board, node: BoardNode, job: Job): Promise<boolean> {
  let changed = false
  for (const w of job.watchers.values()) {
    const dir = path.join(CODEX_GEN_DIR, w.threadId)
    let files: string[]
    try { files = await fs.promises.readdir(dir) } catch { continue }
    for (const f of files) {
      if (w.seen.has(f)) continue
      const src = path.join(dir, f)
      let st: fs.Stats
      try { st = await fs.promises.stat(src) } catch { continue }
      if (!st.isFile() || st.size === 0) continue
      // Wait until the size is stable across two polls so we never copy a half-written file
      if (w.sizes.get(f) !== st.size) { w.sizes.set(f, st.size); continue }
      w.seen.add(f)
      const destDir = path.join(IMAGES_DIR, board.id)
      await fs.promises.mkdir(destDir, { recursive: true })
      const name = `${Date.now().toString(36)}-${crypto.randomUUID().slice(0, 8)}-${f.replace(/[^\w.-]/g, '_')}`
      const dest = path.join(destDir, name)
      try {
        await fs.promises.copyFile(src, dest)
        await makeThumb(dest)
      } catch {
        // A transient source-file race should be retried on the next poll.
        w.seen.delete(f)
        continue
      }
      const stillOwned = jobs.get(node.id) === job
        && job.termination !== 'deleted'
        && job.termination !== 'replaced'
        && board.nodes.includes(node)
      if (!stillOwned) {
        await removeImagesAndThumbnails([dest])
        continue
      }
      node.images.push(`/images/${board.id}/${name}`)
      changed = true
    }
  }
  if (changed) {
    saveBoards()
    sseSend(board.id, { type: 'node', node })
  }
  return changed
}

/** Serialize collection sweeps for a job so interval and final sweeps cannot race. */
function collectJobImages(board: Board, node: BoardNode, job: Job): Promise<boolean> {
  if (job.collection) return job.collection
  const collection = collectImages(board, node, job).finally(() => {
    if (job.collection === collection) job.collection = null
  })
  job.collection = collection
  return collection
}

function handleCodexEvent(board: Board, node: BoardNode, job: Job, ev: any): void {
  if (ev.type === 'thread.started' && ev.thread_id) {
    if (!job.watchers.has(ev.thread_id)) {
      job.watchers.set(ev.thread_id, { threadId: ev.thread_id, seen: new Set(), sizes: new Map() })
    }
  } else if (ev.type === 'item.completed' && ev.item) {
    const item = ev.item
    if (item.type === 'agent_message' && item.text) {
      node.text = node.text ? `${node.text}\n\n${item.text}` : item.text
      sseSend(board.id, { type: 'node', node })
    } else if (item.type === 'reasoning' && item.text) {
      sseSend(board.id, { type: 'activity', nodeId: node.id, text: String(item.text).split('\n')[0].slice(0, 140) })
    } else if (item.type === 'command_execution' && item.command) {
      sseSend(board.id, { type: 'activity', nodeId: node.id, text: `Running: ${String(item.command).slice(0, 140)}` })
    }
  } else if (ev.type === 'turn.completed') {
    if (ev.usage) node.usage = addUsage(node.usage, ev.usage)
  } else if (ev.type === 'turn.failed' || ev.type === 'error') {
    job.failures.push(ev.error?.message || ev.message || 'Generation failed')
  }
}

function startNodeJob(board: Board, node: BoardNode, index: number, count: number): void {
  const ws = path.join(WORKSPACES_DIR, board.id)
  fs.mkdirSync(ws, { recursive: true })

  let resolveCompleted!: () => void
  const completed = new Promise<void>(resolve => { resolveCompleted = resolve })
  const job: Job = {
    boardId: board.id,
    nodeId: node.id,
    proc: null,
    watchers: new Map(),
    failures: [],
    termination: null,
    poller: null,
    idleTimer: null,
    forceStopTimer: null,
    collection: null,
    log: fs.createWriteStream(path.join(LOGS_DIR, `${board.id}.jsonl`), { flags: 'a' }),
    completed,
    resolveCompleted,
  }
  jobs.set(node.id, job)

  job.poller = setInterval(() => {
    void collectJobImages(board, node, job)
      .then(changed => { if (changed) resetIdleTimer(job) })
      .catch(error => job.failures.push(`Image collection failed: ${(error as Error).message}`))
  }, POLL_MS)
  resetIdleTimer(job)

  const promptText = buildNodePrompt(board, node, index, count)
  const args = ['exec', '-s', 'workspace-write', '-C', ws, '--json', '--skip-git-repo-check', promptText]

  // stdin must be closed: codex exec waits for EOF on a piped stdin before starting.
  // detached puts codex in its own process group: the `codex` command is a JS
  // shim that spawns the real binary as a child, so stopping a generation must
  // kill the whole group — signaling just the shim leaves the worker running.
  const proc = spawn(CODEX_BIN, args, { cwd: ws, env: process.env, stdio: ['ignore', 'pipe', 'pipe'], detached: true })
  job.proc = proc

  let stdoutBuf = ''
  proc.stdout!.on('data', (chunk: Buffer) => {
    resetIdleTimer(job)
    stdoutBuf += chunk
    let nl: number
    while ((nl = stdoutBuf.indexOf('\n')) >= 0) {
      const line = stdoutBuf.slice(0, nl).trim()
      stdoutBuf = stdoutBuf.slice(nl + 1)
      if (!line) continue
      job.log.write(line + '\n')
      try { handleCodexEvent(board, node, job, JSON.parse(line)) } catch { /* non-JSON line */ }
    }
  })

  let stderrTail = ''
  proc.stderr!.on('data', (chunk: Buffer) => {
    resetIdleTimer(job)
    stderrTail = (stderrTail + chunk).slice(-4000)
  })

  let settled = false
  const settle = (code: number | null) => {
    if (settled) return
    settled = true
    if (code !== 0 && !job.termination) {
      job.failures.push((stderrTail || `codex exited with code ${code}`).trim().slice(-1000))
    }
    void finalizeJob(board, node, job)
  }
  proc.on('close', settle)
  proc.on('error', err => {
    job.failures.push(`Failed to launch codex: ${err.message}`)
    settle(0)
  })
}

async function finalizeJob(board: Board, node: BoardNode, job: Job): Promise<void> {
  if (job.poller) clearInterval(job.poller)
  if (job.idleTimer) clearTimeout(job.idleTimer)
  if (job.forceStopTimer) clearTimeout(job.forceStopTimer)
  // A newer job may have replaced this one (regenerate while running); if so,
  // this job must not touch the node — the new run owns it now.
  if (jobs.get(node.id) !== job) {
    job.log.end()
    job.resolveCompleted()
    return
  }
  try { await collectJobImages(board, node, job) }
  catch (error) { job.failures.push(`Image collection failed: ${(error as Error).message}`) }
  // one delayed sweep to catch a file that was still being written on exit
  setTimeout(() => void (async () => {
    if (jobs.get(node.id) !== job) {
      job.log.end()
      job.resolveCompleted()
      return
    }
    try { await collectJobImages(board, node, job) }
    catch (error) { job.failures.push(`Image collection failed: ${(error as Error).message}`) }
    jobs.delete(node.id)
    if (node.status === 'running') {
      if (job.termination === 'user' || job.termination === 'app-quit' || job.termination === 'deleted') {
        node.status = 'stopped'
        node.stopReason = job.termination
      } else if (job.termination === 'timeout') {
        const saved = node.images.length
        node.status = 'error'
        node.error = `No generation activity for ${JOB_IDLE_TIMEOUT_MINUTES} minutes. ${saved
          ? `${saved} ${saved === 1 ? 'image was' : 'images were'} saved before the timeout.`
          : 'No images were saved.'}`
      } else if (job.failures.length && node.images.length === 0) {
        node.status = 'error'
        node.error = job.failures[0]
      } else {
        node.status = 'done'
        node.error = job.failures.length ? job.failures[0] : undefined
      }
      node.finishedAt = Date.now()
    }
    saveBoards()
    // The node may have been deleted while running; broadcasting it would
    // make clients re-add it. (The object itself may live on in the trash.)
    if (board.nodes.some(n => n.id === node.id)) {
      sseSend(board.id, { type: 'node', node })
      sseSend(board.id, { type: 'done', nodeId: node.id })
    }
    job.log.end()
    job.resolveCompleted()
  })(), 800)
}

function resetIdleTimer(job: Job): void {
  if (job.termination || jobs.get(job.nodeId) !== job) return
  if (job.idleTimer) clearTimeout(job.idleTimer)
  job.idleTimer = setTimeout(() => {
    if (job.termination || jobs.get(job.nodeId) !== job) return
    job.termination = 'timeout'
    killTree(job.proc, 'SIGKILL')
  }, JOB_IDLE_TIMEOUT_MS)
}

// Signal the whole process group (see the detached spawn above); fall back to
// the direct child if the group is already gone.
function killTree(proc: ChildProcess | null, signal: NodeJS.Signals): void {
  if (!proc?.pid) return
  try {
    process.kill(-proc.pid, signal)
  } catch {
    try { proc.kill(signal) } catch { /* already dead */ }
  }
}

function stopJob(
  nodeId: string,
  reason: StopReason | 'replaced' = 'user',
  signal: NodeJS.Signals = 'SIGTERM',
): void {
  const job = jobs.get(nodeId)
  if (!job) return
  job.termination = reason
  if (job.idleTimer) clearTimeout(job.idleTimer)
  if (job.forceStopTimer) clearTimeout(job.forceStopTimer)
  killTree(job.proc, signal)
  if (signal === 'SIGTERM') {
    job.forceStopTimer = setTimeout(() => killTree(job.proc, 'SIGKILL'), 3000)
  }
}

let stoppingAllJobs = false

async function stopAllJobs(): Promise<number> {
  const active = [...jobs.values()]
  if (!active.length) return 0

  stoppingAllJobs = true
  try {
    for (const job of active) stopJob(job.nodeId, 'app-quit', 'SIGKILL')
    await new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('Timed out stopping generations')), 6000)
      Promise.all(active.map(job => job.completed)).then(
        () => { clearTimeout(timeout); resolve() },
        error => { clearTimeout(timeout); reject(error) },
      )
    })
    flushBoards()
    return active.length
  } finally {
    stoppingAllJobs = false
  }
}

function stopAllJobsImmediately(): void {
  const stoppedAt = Date.now()
  for (const job of jobs.values()) {
    job.termination = 'app-quit'
    if (job.idleTimer) clearTimeout(job.idleTimer)
    if (job.forceStopTimer) clearTimeout(job.forceStopTimer)
    killTree(job.proc, 'SIGKILL')
    const node = getBoard(job.boardId)?.nodes.find(candidate => candidate.id === job.nodeId)
    if (node?.status === 'running') {
      node.status = 'stopped'
      node.stopReason = 'app-quit'
      node.finishedAt = stoppedAt
    }
  }
  flushBoards()
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

function json(res: http.ServerResponse, code: number, body: unknown): void {
  res.writeHead(code, { 'Content-Type': 'application/json; charset=utf-8' })
  res.end(JSON.stringify(body))
}

function readBody(req: http.IncomingMessage): Promise<any> {
  return new Promise((resolve, reject) => {
    let size = 0
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => {
      size += c.length
      if (size > MAX_BODY) { reject(new Error('Body too large')); req.destroy(); return }
      chunks.push(c)
    })
    req.on('end', () => {
      try { resolve(chunks.length ? JSON.parse(Buffer.concat(chunks).toString()) : {}) }
      catch { reject(new Error('Invalid JSON')) }
    })
    req.on('error', reject)
  })
}

const MIME: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.webp': 'image/webp',
  '.gif': 'image/gif',
  '.svg': 'image/svg+xml',
  '.ico': 'image/x-icon',
  '.woff2': 'font/woff2',
}

function sendFile(res: http.ServerResponse, baseDir: string, relPath: string, fallback?: string): void {
  const abs = path.normalize(path.join(baseDir, relPath))
  if (!abs.startsWith(baseDir + path.sep) && abs !== baseDir) { json(res, 403, { error: 'Forbidden' }); return }
  fs.stat(abs, (err, st) => {
    if (err || !st.isFile()) {
      if (fallback) return sendFile(res, baseDir, fallback)
      return json(res, 404, { error: 'Not found' })
    }
    res.writeHead(200, {
      'Content-Type': MIME[path.extname(abs).toLowerCase()] || 'application/octet-stream',
      'Content-Length': st.size,
      'Cache-Control': baseDir === IMAGES_DIR || relPath.startsWith('assets/')
        ? 'public, max-age=31536000, immutable'
        : 'no-cache',
    })
    fs.createReadStream(abs).pipe(res)
  })
}

function nodeTokens(usage: Record<string, number> | undefined): number {
  return usage ? (usage.input_tokens || 0) + (usage.output_tokens || 0) : 0
}

function boardSummary(board: Board): BoardSummary {
  let lastImage: string | null = null
  let updatedAt = board.createdAt
  for (const n of board.nodes) {
    if (n.createdAt > updatedAt) updatedAt = n.createdAt
    if (n.images.length) lastImage = n.images[n.images.length - 1]
  }
  return {
    id: board.id,
    title: board.title,
    createdAt: board.createdAt,
    updatedAt,
    imageCount: board.nodes.reduce((n, node) => n + node.images.length, 0),
    lastImage,
    generating: generatingIds(board.id).length > 0,
    totalTokens: board.nodes.reduce((s, node) => s + nodeTokens(node.usage), 0),
  }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

async function handleApi(req: http.IncomingMessage, res: http.ServerResponse, url: URL): Promise<void> {
  const parts = url.pathname.split('/').filter(Boolean) // ['api', 'boards', boardId?, 'nodes'?, nodeId?, action?]

  if (req.method === 'GET' && url.pathname === '/api/generations') {
    return json(res, 200, { active: jobs.size })
  }

  if (req.method === 'POST' && url.pathname === '/api/generations/stop-all') {
    const stopped = await stopAllJobs()
    return json(res, 200, { stopped })
  }

  if (req.method === 'GET' && url.pathname === '/api/boards') {
    return json(res, 200, boards.map(boardSummary).sort((a, b) => b.updatedAt - a.updatedAt))
  }

  if (req.method === 'POST' && url.pathname === '/api/boards') {
    const board: Board = { id: crypto.randomUUID(), title: 'New board', createdAt: Date.now(), nodes: [] }
    boards.push(board)
    saveBoards()
    return json(res, 200, board)
  }

  const board = parts[1] === 'boards' && parts[2] ? getBoard(parts[2]) : undefined
  if (!board) return json(res, 404, { error: 'Board not found' })
  const sub = parts[3]

  if (req.method === 'GET' && !sub) {
    return json(res, 200, { ...board, generating: generatingIds(board.id) })
  }

  // PATCH /api/boards/:id — rename
  if (req.method === 'PATCH' && !sub) {
    const body = await readBody(req)
    if (typeof body.title === 'string' && body.title.trim()) {
      board.title = body.title.trim().slice(0, 120)
      saveBoards()
    }
    return json(res, 200, boardSummary(board))
  }

  if (req.method === 'DELETE' && !sub) {
    for (const id of generatingIds(board.id)) stopJob(id, 'deleted', 'SIGKILL')
    boards = boards.filter(b => b.id !== board.id)
    for (const [undoId, entry] of trash) {
      if (entry.boardId === board.id) { clearTimeout(entry.timer); trash.delete(undoId) }
    }
    saveBoards()
    for (const dir of [path.join(IMAGES_DIR, board.id), path.join(WORKSPACES_DIR, board.id)]) {
      fs.rm(dir, { recursive: true, force: true }, () => {})
    }
    fs.rm(path.join(LOGS_DIR, `${board.id}.jsonl`), { force: true }, () => {})
    return json(res, 200, { ok: true })
  }

  // POST /api/boards/:id/undo/:undoId — restore a trashed subtree
  if (req.method === 'POST' && sub === 'undo' && parts[4]) {
    const entry = trash.get(parts[4])
    if (!entry || entry.boardId !== board.id) return json(res, 404, { error: 'Nothing to undo' })
    clearTimeout(entry.timer)
    trash.delete(parts[4])
    const existing = new Set(board.nodes.map(n => n.id))
    const restored = entry.nodes.filter(n => !existing.has(n.id))
    board.nodes.push(...restored)
    saveBoards()
    for (const n of restored) sseSend(board.id, { type: 'node', node: n })
    return json(res, 200, { restored: restored.map(n => n.id) })
  }

  // POST /api/boards/:id/nodes — create N sibling nodes and start generating
  if (req.method === 'POST' && sub === 'nodes' && !parts[4]) {
    if (stoppingAllJobs) return json(res, 409, { error: 'All generations are stopping' })
    const body = await readBody(req)
    const prompt = String(body.prompt || '').trim()
    if (!prompt) return json(res, 400, { error: 'Empty prompt' })
    const count = Math.min(4, Math.max(1, Number(body.count) || 1))
    const aspect = typeof body.aspect === 'string' ? body.aspect : 'auto'

    if (generatingIds(board.id).length + count > MAX_ACTIVE_PER_BOARD) {
      return json(res, 429, { error: `Too many generations running on this board (max ${MAX_ACTIVE_PER_BOARD})` })
    }

    const parent = body.parentId ? board.nodes.find(n => n.id === body.parentId) : undefined
    if (body.parentId && !parent) return json(res, 404, { error: 'Parent node not found' })

    // Source images: explicit selection (branch from one image) or all parent outputs
    let sourceImages: string[] = []
    if (parent) {
      const requested: string[] | null = Array.isArray(body.sourceImages) ? body.sourceImages.map(String) : null
      sourceImages = (requested ?? parent.images).filter(u => imageUrlToAbs(board.id, u) !== null)
    }

    // Validate the complete batch before writing anything: the renderer uses
    // the same limits, while this server-side check protects other clients.
    const incomingAttachments = Array.isArray(body.attachments) ? body.attachments : []
    const requestedAttachmentUrls = Array.isArray(body.attachmentUrls)
      ? body.attachmentUrls.map(String)
      : []
    if (incomingAttachments.length + requestedAttachmentUrls.length > MAX_ATTACHMENTS) {
      return json(res, 400, { error: `Too many attachments (max ${MAX_ATTACHMENTS})` })
    }
    const preparedAttachments: Array<{ name: string; data: Buffer }> = []
    let attachmentBytes = 0
    for (const att of incomingAttachments) {
      const data = Buffer.from(String(att.data || ''), 'base64')
      if (data.byteLength > MAX_ATTACHMENT_BYTES) {
        return json(res, 400, { error: `Attachment exceeds ${MAX_ATTACHMENT_BYTES / 1024 / 1024} MB` })
      }
      attachmentBytes += data.byteLength
      if (attachmentBytes > MAX_ATTACHMENT_TOTAL_BYTES) {
        return json(res, 400, { error: `Attachments exceed ${MAX_ATTACHMENT_TOTAL_BYTES / 1024 / 1024} MB total` })
      }
      preparedAttachments.push({
        name: String(att.name || 'image.png').replace(/[^\w.-]/g, '_'),
        data,
      })
    }

    const destDir = path.join(IMAGES_DIR, board.id)
    if (preparedAttachments.length) await fs.promises.mkdir(destDir, { recursive: true })
    const createdAttachmentPaths: string[] = []
    let attachmentUrls: string[]
    try {
      attachmentUrls = await Promise.all(preparedAttachments.map(async attachment => {
        const fname = `${Date.now().toString(36)}-${crypto.randomUUID().slice(0, 6)}-${attachment.name}`
        const abs = path.join(destDir, fname)
        createdAttachmentPaths.push(abs)
        await fs.promises.writeFile(abs, attachment.data)
        await makeThumb(abs)
        return `/images/${board.id}/${fname}`
      }))
    } catch (error) {
      await removeImagesAndThumbnails(createdAttachmentPaths)
      throw error
    }

    // Attachment writes yield to the event loop; the board may have been
    // deleted or other requests may have filled its generation slots meanwhile.
    if (getBoard(board.id) !== board) {
      await removeImagesAndThumbnails(createdAttachmentPaths)
      return json(res, 409, { error: 'Board was deleted while attachments were uploading' })
    }
    if (generatingIds(board.id).length + count > MAX_ACTIVE_PER_BOARD) {
      await removeImagesAndThumbnails(createdAttachmentPaths)
      return json(res, 429, { error: `Too many generations running on this board (max ${MAX_ACTIVE_PER_BOARD})` })
    }
    // Reuse attachments already stored on this board (node duplication)
    for (const u of requestedAttachmentUrls) {
      if (imageUrlToAbs(board.id, u) !== null) attachmentUrls.push(u)
    }

    const nodes: BoardNode[] = []
    const runStartedAt = Date.now()
    for (let i = 0; i < count; i++) {
      const node: BoardNode = {
        id: crypto.randomUUID(),
        parentId: parent ? parent.id : null,
        prompt,
        aspect,
        sourceImages,
        attachments: attachmentUrls,
        images: [],
        text: '',
        status: 'running',
        createdAt: runStartedAt + i, // stable sibling ordering
        runStartedAt,
      }
      board.nodes.push(node)
      nodes.push(node)
    }
    if (board.title === 'New board') board.title = prompt.length > 60 ? prompt.slice(0, 57) + '…' : prompt
    saveBoards()
    for (let i = 0; i < nodes.length; i++) startNodeJob(board, nodes[i], i, count)
    return json(res, 200, { nodes })
  }

  const node = sub === 'nodes' && parts[4] ? board.nodes.find(n => n.id === parts[4]) : undefined
  if (!node) return json(res, 404, { error: 'Node not found' })
  const action = parts[5]

  // POST /api/boards/:id/nodes/:nodeId/regenerate — fresh session, same sources.
  // Optional body { prompt, aspect } edits the node before rerunning (inline node edit).
  // Children are unaffected: they snapshotted their source image paths and the files stay on disk.
  if (req.method === 'POST' && action === 'regenerate') {
    if (stoppingAllJobs) return json(res, 409, { error: 'All generations are stopping' })
    const body = await readBody(req)
    if (jobs.has(node.id)) stopJob(node.id, 'replaced', 'SIGKILL')
    // The killed job stays in the map until its async finalize runs, so it
    // must not count against the limit — regenerating frees its own slot.
    if (generatingIds(board.id).filter(id => id !== node.id).length >= MAX_ACTIVE_PER_BOARD) {
      return json(res, 429, { error: `Too many generations running on this board (max ${MAX_ACTIVE_PER_BOARD})` })
    }
    if (typeof body.prompt === 'string' && body.prompt.trim()) node.prompt = body.prompt.trim()
    if (typeof body.aspect === 'string' && body.aspect) node.aspect = body.aspect
    node.images = []
    node.text = ''
    node.error = undefined
    node.stopReason = undefined
    node.status = 'running'
    node.runStartedAt = Date.now()
    node.finishedAt = undefined
    saveBoards()
    sseSend(board.id, { type: 'node', node })
    startNodeJob(board, node, 0, 1)
    return json(res, 200, { node })
  }

  if (req.method === 'POST' && action === 'stop') {
    stopJob(node.id)
    return json(res, 200, { ok: true })
  }

  // PATCH /api/boards/:id/nodes/:nodeId — persist manual canvas position
  if (req.method === 'PATCH' && !action) {
    const body = await readBody(req)
    if (typeof body.x === 'number' && typeof body.y === 'number') {
      node.x = body.x
      node.y = body.y
      saveBoards()
    }
    return json(res, 200, { node })
  }

  // DELETE /api/boards/:id/nodes/:nodeId — move the node and its whole subtree
  // to the trash; POST /undo/:undoId restores it within TRASH_TTL_MS
  if (req.method === 'DELETE' && !action) {
    const doomed = new Set<string>([node.id])
    let grew = true
    while (grew) {
      grew = false
      for (const n of board.nodes) {
        if (n.parentId && doomed.has(n.parentId) && !doomed.has(n.id)) { doomed.add(n.id); grew = true }
      }
    }
    for (const id of doomed) if (jobs.has(id)) stopJob(id, 'deleted', 'SIGKILL')
    const removed = board.nodes.filter(n => doomed.has(n.id))
    board.nodes = board.nodes.filter(n => !doomed.has(n.id))
    const undoId = crypto.randomUUID()
    trash.set(undoId, {
      boardId: board.id,
      nodes: removed,
      timer: setTimeout(() => trash.delete(undoId), TRASH_TTL_MS),
    })
    saveBoards()
    sseSend(board.id, { type: 'nodesDeleted', ids: [...doomed] })
    return json(res, 200, { deleted: [...doomed], undoId })
  }

  json(res, 404, { error: 'Not found' })
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url || '/', `http://${req.headers.host || 'localhost'}`)

  // SSE: GET /api/boards/:id/events
  const sseMatch = url.pathname.match(/^\/api\/boards\/([^/]+)\/events$/)
  if (sseMatch && req.method === 'GET') {
    const board = getBoard(sseMatch[1])
    if (!board) return json(res, 404, { error: 'Board not found' })
    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      'Connection': 'keep-alive',
    })
    res.write(`data: ${JSON.stringify({ type: 'hello', generating: generatingIds(board.id) } satisfies ServerEvent)}\n\n`)
    let set = sseClients.get(board.id)
    if (!set) sseClients.set(board.id, set = new Set())
    set.add(res)
    const heartbeat = setInterval(() => res.write(': ping\n\n'), 25000)
    req.on('close', () => {
      clearInterval(heartbeat)
      set.delete(res)
      if (!set.size) sseClients.delete(board.id)
    })
    return
  }

  if (url.pathname.startsWith('/api/')) {
    handleApi(req, res, url).catch(err => json(res, 500, { error: (err as Error).message }))
    return
  }
  if (url.pathname.startsWith('/images/')) {
    return sendFile(res, IMAGES_DIR, decodeURIComponent(url.pathname.slice('/images/'.length)))
  }
  const rel = url.pathname === '/' ? 'index.html' : decodeURIComponent(url.pathname.slice(1))
  sendFile(res, DIST_DIR, rel, 'index.html')
})

// One-time backfill for images created before thumbnails existed. It feeds
// the same bounded async queue one file at a time, so live generations are
// never trapped behind an entire historical library.
async function backfillThumbs(): Promise<void> {
  const queue: string[] = []
  try {
    for (const boardDir of await fs.promises.readdir(IMAGES_DIR)) {
      const dir = path.join(IMAGES_DIR, boardDir)
      let files: string[]
      try { files = await fs.promises.readdir(dir) } catch { continue }
      const have = new Set(files)
      for (const f of files) {
        if (!f.startsWith('t_') && !have.has(`t_${f}`)) queue.push(path.join(dir, f))
      }
    }
  } catch { return }
  for (const absPath of queue) await makeThumb(absPath)
}

server.listen(PORT, '127.0.0.1', () => {
  console.log(`CodexImage API + app on http://localhost:${PORT}`)
  void backfillThumbs()
})
