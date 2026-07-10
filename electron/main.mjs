import { app, BrowserWindow, dialog, nativeTheme, shell, utilityProcess } from 'electron'
import { execSync } from 'node:child_process'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

// Packaged: app files live in Contents/Resources/app, data goes to userData.
// Dev (`electron .`): everything stays in the project directory.
const ROOT = app.isPackaged
  ? app.getAppPath()
  : path.join(path.dirname(fileURLToPath(import.meta.url)), '..')
if (app.isPackaged) {
  process.env.CODEXIMAGE_DATA = path.join(app.getPath('userData'), 'data')
}
// Packaged app gets its own port so it never attaches to (or serves) a dev
// server's state — dev mode (4750) and the installed app (4751) stay isolated.
const PORT = Number(process.env.PORT || (app.isPackaged ? 4751 : 4750))
process.env.PORT = String(PORT)
const APP_URL = `http://127.0.0.1:${PORT}`

// When launched from Finder/Dock the process gets launchd's minimal PATH.
// The server needs `codex` AND whatever its shebang points at (`env node`,
// which usually lives in an nvm dir), so import the real PATH from the user's
// login shell; fall back to common install spots if that fails.
function loginShellPath() {
  try {
    const sh = process.env.SHELL || '/bin/zsh'
    const out = execSync(`${sh} -ilc env`, { encoding: 'utf8', timeout: 4000, stdio: ['ignore', 'pipe', 'ignore'] })
    const line = out.split('\n').find(l => l.startsWith('PATH='))
    return line ? line.slice(5) : null
  } catch {
    return null
  }
}
const home = os.homedir()
process.env.PATH = [
  loginShellPath(),
  process.env.PATH,
  `${home}/.bun/bin`,
  `${home}/.local/bin`,
  '/opt/homebrew/bin',
  '/usr/local/bin',
].filter(Boolean).join(':')

function ping() {
  return new Promise(resolve => {
    const req = http.get(`${APP_URL}/api/boards`, res => { res.resume(); resolve(true) })
    req.on('error', () => resolve(false))
    req.setTimeout(700, () => { req.destroy(); resolve(false) })
  })
}

function apiRequest(pathname, method = 'GET', timeout = 2000) {
  return new Promise((resolve, reject) => {
    const req = http.request(`${APP_URL}${pathname}`, { method }, res => {
      const chunks = []
      res.on('data', chunk => chunks.push(chunk))
      res.on('end', () => {
        let body
        try {
          body = JSON.parse(Buffer.concat(chunks).toString() || '{}')
        } catch {
          reject(new Error(`API returned invalid JSON (${res.statusCode})`))
          return
        }
        if (!res.statusCode || res.statusCode < 200 || res.statusCode >= 300) {
          reject(new Error(body.error || `API request failed (${res.statusCode})`))
          return
        }
        resolve(body)
      })
    })
    req.on('error', reject)
    req.setTimeout(timeout, () => req.destroy(new Error('API request timed out')))
    req.end()
  })
}

let quitAuthorized = false
let quitCheckInProgress = false
let serverReady = false
let serverProcess = null
let serverShutdownInProgress = false

async function ensureServer() {
  // Reuse an already-running server (e.g. `npm run dev` / `npm start`)
  if (await ping()) {
    serverReady = true
    return
  }
  const child = utilityProcess.fork(path.join(ROOT, 'dist-electron', 'server.mjs'), [], {
    cwd: ROOT,
    env: { ...process.env, CODEXIMAGE_ROOT: ROOT },
    serviceName: 'CodexImage Server',
    stdio: 'inherit',
  })
  serverProcess = child
  child.on('exit', code => {
    if (serverProcess === child) serverProcess = null
    const unexpected = serverReady && !quitAuthorized && !serverShutdownInProgress
    serverReady = false
    if (unexpected) console.error(`CodexImage server exited unexpectedly (${code})`)
  })
  for (let i = 0; i < 50; i++) {
    if (await ping()) {
      serverReady = true
      return
    }
    await new Promise(r => setTimeout(r, 100))
  }
  child.kill()
  throw new Error('API server failed to start')
}

function shutdownOwnedServer() {
  const child = serverProcess
  if (!child) return Promise.resolve()
  serverShutdownInProgress = true
  return new Promise(resolve => {
    let settled = false
    const finish = () => {
      if (settled) return
      settled = true
      clearTimeout(timeout)
      if (serverProcess === child) serverProcess = null
      serverShutdownInProgress = false
      resolve()
    }
    const timeout = setTimeout(finish, 3000)
    child.once('exit', finish)
    if (!child.kill()) finish()
  })
}

function showMessageBox(win, options) {
  return win && !win.isDestroyed()
    ? dialog.showMessageBox(win, options)
    : dialog.showMessageBox(options)
}

async function requestQuit(win) {
  if (quitAuthorized || quitCheckInProgress) return
  if (!serverReady) {
    quitAuthorized = true
    app.quit()
    return
  }
  quitCheckInProgress = true
  try {
    const { active } = await apiRequest('/api/generations')
    if (active > 0) {
      const noun = active === 1 ? 'generation is' : 'generations are'
      const { response } = await showMessageBox(win, {
        type: 'warning',
        title: 'Generations are still running',
        message: `${active} ${noun} still running`,
        detail: 'Quitting now will terminate the running work. Any images already received will be kept.',
        buttons: ['Keep Running', 'Terminate and Quit'],
        defaultId: 0,
        cancelId: 0,
        noLink: true,
      })
      if (response !== 1) return
      await apiRequest('/api/generations/stop-all', 'POST', 8000)
    }
    await shutdownOwnedServer()
    quitAuthorized = true
    app.quit()
  } catch (err) {
    console.error('Unable to prepare the app for shutdown:', err)
    const reason = err instanceof Error ? err.message : String(err)
    await showMessageBox(win, {
      type: 'error',
      title: 'Couldn\u2019t quit safely',
      message: 'CodexImage couldn\u2019t stop its running generations.',
      detail: `${reason}\n\nThe app will remain open so generation processes are not left running in the background.`,
      buttons: ['OK'],
      defaultId: 0,
      noLink: true,
    })
  } finally {
    quitCheckInProgress = false
  }
}

async function createWindow() {
  const win = new BrowserWindow({
    width: 1500,
    height: 950,
    minWidth: 900,
    minHeight: 600,
    backgroundColor: '#0d0e12',
    title: 'CodexImage',
    show: false,
    webPreferences: { contextIsolation: true, sandbox: true },
  })
  win.once('ready-to-show', () => win.show())
  win.on('close', event => {
    if (quitAuthorized) return
    event.preventDefault()
    void requestQuit(win)
  })
  // External links (e.g. "Open original") go to the default browser
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (!url.startsWith(APP_URL)) {
      shell.openExternal(url)
      return { action: 'deny' }
    }
    return { action: 'allow' }
  })
  await win.loadURL(APP_URL)
}

nativeTheme.themeSource = 'dark'

app.whenReady().then(async () => {
  try {
    await ensureServer()
  } catch (err) {
    console.error(err)
    quitAuthorized = true
    app.quit()
    return
  }
  await createWindow()
})

app.on('activate', () => {
  if (BrowserWindow.getAllWindows().length === 0) void createWindow()
})

app.on('before-quit', event => {
  if (quitAuthorized) return
  event.preventDefault()
  const win = BrowserWindow.getFocusedWindow() || BrowserWindow.getAllWindows()[0]
  void requestQuit(win)
})

app.on('window-all-closed', () => {
  app.quit()
})
