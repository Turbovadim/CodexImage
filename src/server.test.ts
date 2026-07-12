import assert from 'node:assert/strict'
import { spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'

async function freePort(): Promise<number> {
  const server = net.createServer()
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  assert(address && typeof address === 'object')
  const port = address.port
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  return port
}

function waitForServer(proc: ChildProcess, port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    let output = ''
    const timeout = setTimeout(() => reject(new Error(`server did not start: ${output}`)), 5_000)
    const onData = (chunk: Buffer) => {
      output += chunk
      if (!output.includes(`localhost:${port}`)) return
      clearTimeout(timeout)
      proc.off('exit', onExit)
      resolve()
    }
    const onExit = (code: number | null) => {
      clearTimeout(timeout)
      reject(new Error(`server exited with ${code}: ${output}`))
    }
    proc.stdout?.on('data', onData)
    proc.stderr?.on('data', (chunk: Buffer) => { output += chunk })
    proc.once('exit', onExit)
  })
}

async function requestJson<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init)
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${await response.text()}`)
  return response.json() as Promise<T>
}

test('recovers an ordered final selection after generation fails with completed candidates', { timeout: 15_000 }, async () => {
  const root = await fs.promises.mkdtemp(path.join(os.tmpdir(), 'codeximage-recovery-'))
  const dataDir = path.join(root, 'data')
  const generatedDir = path.join(root, 'generated')
  const fakeCodex = path.join(root, 'fake-codex.mjs')
  const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='
  await fs.promises.writeFile(fakeCodex, `#!/usr/bin/env node
import fs from 'node:fs'
import path from 'node:path'

const prompt = process.argv.at(-1) || ''
const generatedRoot = process.env.CODEXIMAGE_GENERATED_IMAGES
const threadId = 'generation-thread'
const threadDir = path.join(generatedRoot, threadId)
const paths = [1, 2, 3].map(number => path.join(threadDir, \`candidate-\${number}.png\`))
const emit = value => process.stdout.write(JSON.stringify(value) + '\\n')

if (prompt.includes('You are finalizing an interrupted image-generation run.')) {
  emit({ type: 'thread.started', thread_id: 'recovery-thread' })
  emit({ type: 'turn.started' })
  emit({ type: 'item.completed', item: { type: 'agent_message', text: JSON.stringify({
    summary: 'Recovered all three requested images.',
    complete: true,
    outputs: [
      { path: paths[1], label: 'Second' },
      { path: paths[0], label: 'First' },
      { path: paths[2], label: 'Third' },
    ],
  }) } })
  emit({ type: 'turn.completed', usage: { input_tokens: 10, output_tokens: 5 } })
} else {
  fs.mkdirSync(threadDir, { recursive: true })
  for (const candidate of paths) fs.writeFileSync(candidate, Buffer.from('${png}', 'base64'))
  emit({ type: 'thread.started', thread_id: threadId })
  emit({ type: 'turn.started' })
  emit({ type: 'item.completed', item: { type: 'agent_message', text: JSON.stringify({
    summary: 'Images are still rendering.',
    complete: false,
    outputs: [],
  }) } })
  emit({ type: 'error', message: 'Selected model is at capacity. Please try a different model.' })
  emit({ type: 'turn.failed', error: { message: 'Selected model is at capacity. Please try a different model.' } })
  process.exitCode = 1
}
`)
  await fs.promises.chmod(fakeCodex, 0o755)

  const port = await freePort()
  const proc = spawn(process.execPath, ['server.ts'], {
    cwd: path.resolve(import.meta.dirname, '..'),
    env: {
      ...process.env,
      PORT: String(port),
      CODEX_BIN: fakeCodex,
      CODEXIMAGE_DATA: dataDir,
      CODEXIMAGE_GENERATED_IMAGES: generatedDir,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  })

  try {
    await waitForServer(proc, port)
    const base = `http://127.0.0.1:${port}`
    const board = await requestJson<{ id: string }>(`${base}/api/boards`, { method: 'POST' })
    const created = await requestJson<{ nodes: Array<{ id: string }> }>(`${base}/api/boards/${board.id}/nodes`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ prompt: 'Create three ordered character images.', aspect: 'auto', count: 1 }),
    })
    const nodeId = created.nodes[0].id

    let node: any
    const deadline = Date.now() + 10_000
    do {
      const current = await requestJson<{ nodes: any[] }>(`${base}/api/boards/${board.id}`)
      node = current.nodes.find(candidate => candidate.id === nodeId)
      if (node?.status !== 'running') break
      await new Promise(resolve => setTimeout(resolve, 100))
    } while (Date.now() < deadline)

    assert.equal(node?.status, 'done')
    assert.equal(node?.attempts.length, 3)
    assert.deepEqual(node?.imageLabels, ['Second', 'First', 'Third'])
    assert.equal(node?.images.length, 3)
    assert.match(node.images[0], /candidate-2\.png$/)
    assert.match(node.images[1], /candidate-1\.png$/)
    assert.match(node.images[2], /candidate-3\.png$/)
    assert.equal(node?.text, 'Recovered all three requested images.')
  } finally {
    proc.kill('SIGTERM')
    await new Promise(resolve => proc.once('exit', resolve))
    await fs.promises.rm(root, { recursive: true, force: true })
  }
})
