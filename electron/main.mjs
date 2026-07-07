import { app, BrowserWindow, nativeTheme, shell } from 'electron'
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

async function ensureServer() {
  // Reuse an already-running server (e.g. `npm run dev` / `npm start`)
  if (await ping()) return
  process.env.CODEXIMAGE_ROOT = ROOT
  await import(path.join(ROOT, 'dist-electron', 'server.mjs'))
  for (let i = 0; i < 50; i++) {
    if (await ping()) return
    await new Promise(r => setTimeout(r, 100))
  }
  throw new Error('API server failed to start')
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
    app.quit()
    return
  }
  await createWindow()
})

app.on('activate', () => {
  if (BrowserWindow.getAllWindows().length === 0) void createWindow()
})

app.on('window-all-closed', () => {
  app.quit()
})
