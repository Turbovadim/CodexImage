import { app, BrowserWindow, nativeTheme, shell } from 'electron'
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
const PORT = Number(process.env.PORT || 4750)
const APP_URL = `http://127.0.0.1:${PORT}`

// When launched from Finder/Dock the process gets launchd's minimal PATH, and
// the server needs to find `codex`. Extend PATH with the usual install spots.
const home = os.homedir()
process.env.PATH = [
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
