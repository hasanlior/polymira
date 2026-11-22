import { app, BrowserWindow, desktopCapturer, dialog, globalShortcut, ipcMain, net, protocol, screen, shell } from 'electron'
import fs from 'fs'
import path from 'path'
import { pathToFileURL } from 'url'
import { BRAND } from '@shared/brand'
import { MEDIA_SCHEME, type IpcContract, type IpcEvents, type IpcEvent } from '@shared/ipc'
import type { AppState, DirEntry, Screenshot, Toast, WindowMode } from '@shared/types'
import { initStores, paths, store } from './store'
import { addSingle, addWatchedFolder, moveGame, refreshMissing, removeGame, removeWatchedFolder, scanFolders, setCustomCover } from './library'
import { CoverQueue, coverFile } from './covers'
import { detectEmulators, emulatorStatus, LaunchError, planLaunch } from './emulators'
import { SessionManager } from './session'
import { GamepadWatcher } from './gamepadWatcher'

/** Keyboard fallback for opening the in-game menu (works while the emulator has focus). */
const OVERLAY_SHORTCUT = 'Control+Shift+Home'

protocol.registerSchemesAsPrivileged([
  { scheme: MEDIA_SCHEME, privileges: { standard: true, secure: true, supportFetchAPI: true, stream: true } }
])

if (!app.requestSingleInstanceLock()) {
  app.quit()
}

let mainWindow: BrowserWindow | null = null
let overlayWindow: BrowserWindow | null = null
let scanning = false
const session = new SessionManager()
let watcher: GamepadWatcher

// ── Helpers ───────────────────────────────────────────────────────────

function rendererUrl(page: 'index' | 'overlay'): { url?: string; file?: string } {
  const dev = process.env.ELECTRON_RENDERER_URL
  if (!app.isPackaged && dev) return { url: page === 'index' ? dev : `${dev}/overlay.html` }
  return { file: path.join(__dirname, `../renderer/${page}.html`) }
}

function load(win: BrowserWindow, page: 'index' | 'overlay'): void {
  const t = rendererUrl(page)
  if (t.url) void win.loadURL(t.url)
  else void win.loadFile(t.file!)
}

function send<E extends IpcEvent>(event: E, payload: IpcEvents[E]): void {
  for (const w of [mainWindow, overlayWindow]) {
    if (w && !w.isDestroyed()) w.webContents.send(event, payload)
  }
}

function toast(kind: Toast['kind'], text: string): void {
  send('toast', { kind, text })
}

function snapshot(): AppState {
  return {
    games: store.games,
    settings: store.settings,
    emulators: emulatorStatus(store.settings),
    session: session.info,
    version: app.getVersion(),
    scanning
  }
}

let broadcastTimer: NodeJS.Timeout | null = null
function broadcast(): void {
  if (broadcastTimer) return
  broadcastTimer = setTimeout(() => {
    broadcastTimer = null
    send('state', snapshot())
  }, 40)
}

const covers = new CoverQueue((id, ok) => {
  if (!ok) return
  const g = store.games.find((x) => x.id === id)
  store.updateGame(id, { hasCover: true, coverRev: (g?.coverRev ?? 0) + 1 })
  broadcast()
})

function queueCovers(): number {
  if (!store.settings.downloadCovers) return 0
  const todo = store.games.filter((g) => !g.hasCover && g.platformId !== 'pc')
  covers.push(todo)
  return todo.length
}

// ── Windows ───────────────────────────────────────────────────────────

function windowOptions(mode: WindowMode): Electron.BrowserWindowConstructorOptions {
  const d = screen.getPrimaryDisplay()
  const base: Electron.BrowserWindowConstructorOptions = {
    title: BRAND.name,
    backgroundColor: '#000000',
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, '../preload/index.js'),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false
    }
  }
  if (mode === 'fullscreen') return { ...base, fullscreen: true, frame: false }
  if (mode === 'borderless') return { ...base, frame: false, ...d.bounds }
  return { ...base, width: 1280, height: 720, minWidth: 960, minHeight: 540 }
}

function createMainWindow(): void {
  const old = mainWindow
  // `--windowed` forces a normal window (handy during development).
  const mode = process.argv.includes('--windowed') ? 'windowed' : store.settings.windowMode
  mainWindow = new BrowserWindow(windowOptions(mode))
  if (!app.isPackaged) {
    mainWindow.webContents.on('console-message', (_e, level, message) => {
      if (level >= 2) console.log(`[renderer] ${message}`)
    })
  }
  mainWindow.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  mainWindow.once('ready-to-show', () => {
    mainWindow?.show()
    mainWindow?.focus()
  })
  mainWindow.on('closed', () => {
    if (mainWindow?.isDestroyed()) mainWindow = null
  })
  load(mainWindow, 'index')
  if (old && !old.isDestroyed()) old.destroy()
}

function applyWindowMode(prev: WindowMode, next: WindowMode): void {
  if (!mainWindow || prev === next) return
  const framed = (m: WindowMode) => m === 'windowed'
  if (framed(prev) === framed(next) || (prev === 'fullscreen' && next === 'borderless') || (prev === 'borderless' && next === 'fullscreen')) {
    if (next === 'fullscreen') {
      mainWindow.setFullScreen(true)
    } else if (next === 'borderless') {
      mainWindow.setFullScreen(false)
      mainWindow.setBounds(screen.getPrimaryDisplay().bounds)
    } else {
      mainWindow.setFullScreen(false)
    }
    return
  }
  // Frame changes need a new window.
  createMainWindow()
}

function ensureOverlay(): BrowserWindow {
  if (overlayWindow && !overlayWindow.isDestroyed()) return overlayWindow
  const d = screen.getPrimaryDisplay()
  overlayWindow = new BrowserWindow({
    ...d.bounds,
    show: false,
    frame: false,
    transparent: true,
    resizable: false,
    movable: false,
    skipTaskbar: true,
    alwaysOnTop: true,
    fullscreenable: false,
    hasShadow: false,
    backgroundColor: '#00000000',
    webPreferences: {
      preload: path.join(__dirname, '../preload/index.js'),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      backgroundThrottling: false
    }
  })
  overlayWindow.setAlwaysOnTop(true, 'screen-saver')
  overlayWindow.on('closed', () => {
    overlayWindow = null
  })
  load(overlayWindow, 'overlay')
  return overlayWindow
}

// ── In-game menu ──────────────────────────────────────────────────────

function overlayVisible(): boolean {
  return Boolean(overlayWindow && !overlayWindow.isDestroyed() && overlayWindow.isVisible())
}

function openOverlay(): void {
  if (!session.info) return
  const w = ensureOverlay()
  if (store.settings.pauseOnOverlay) session.setPaused(true)
  w.setBounds(screen.getPrimaryDisplay().bounds)
  w.show()
  w.setAlwaysOnTop(true, 'screen-saver')
  w.focus()
  send('state', snapshot())
  send('overlay:opened', null)
}

function closeOverlay(): void {
  if (overlayWindow && !overlayWindow.isDestroyed()) overlayWindow.hide()
  session.setPaused(false)
  session.focusGame()
}

function toggleOverlay(): void {
  if (mainWindow?.isVisible() && mainWindow.isFocused()) return // game is behind the XMB; ignore
  if (overlayVisible()) closeOverlay()
  else openOverlay()
}

session.on('started', () => {
  mainWindow?.hide()
  ensureOverlay()
  watcher.hotkey = store.settings.overlayHotkey
  watcher.start()
  globalShortcut.register(OVERLAY_SHORTCUT, toggleOverlay)
})

session.on('changed', () => broadcast())

session.on('ended', (end: import('./session').SessionEnd) => {
  watcher.stop()
  globalShortcut.unregister(OVERLAY_SHORTCUT)
  if (overlayWindow && !overlayWindow.isDestroyed()) overlayWindow.destroy()
  overlayWindow = null
  const g = store.games.find((x) => x.id === end.gameId)
  if (g) {
    store.updateGame(g.id, {
      playTimeSec: g.playTimeSec + end.playedSec,
      lastLaunch: { at: Date.now(), result: end.result, message: end.message }
    })
  }
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.show()
    if (store.settings.windowMode === 'fullscreen') mainWindow.setFullScreen(true)
    mainWindow.focus()
  }
  if (end.result === 'crashed' && end.message) toast('error', end.message)
  broadcast()
})

async function takeScreenshot(): Promise<Screenshot | null> {
  const info = session.info
  if (!info) return null
  const wasVisible = overlayVisible()
  overlayWindow?.hide()
  await new Promise((r) => setTimeout(r, 250))
  try {
    const d = screen.getPrimaryDisplay()
    const sources = await desktopCapturer.getSources({
      types: ['screen'],
      thumbnailSize: { width: Math.round(d.size.width * d.scaleFactor), height: Math.round(d.size.height * d.scaleFactor) }
    })
    const src = sources.find((s) => s.display_id === String(d.id)) ?? sources[0]
    if (!src) return null
    const dir = path.join(paths.screenshots, info.gameId)
    fs.mkdirSync(dir, { recursive: true })
    const takenAt = Date.now()
    const file = path.join(dir, `${takenAt}.png`)
    fs.writeFileSync(file, src.thumbnail.toPNG())
    return { gameId: info.gameId, file, url: `${MEDIA_SCHEME}://shots/${info.gameId}/${takenAt}.png`, takenAt }
  } finally {
    if (wasVisible) {
      overlayWindow?.show()
      overlayWindow?.focus()
    }
  }
}

function listScreenshots(gameId?: string): Screenshot[] {
  const out: Screenshot[] = []
  const ids = gameId ? [gameId] : fs.existsSync(paths.screenshots) ? fs.readdirSync(paths.screenshots) : []
  for (const id of ids) {
    const dir = path.join(paths.screenshots, id)
    if (!fs.existsSync(dir)) continue
    for (const f of fs.readdirSync(dir)) {
      if (!f.endsWith('.png')) continue
      const takenAt = Number(path.basename(f, '.png')) || fs.statSync(path.join(dir, f)).mtimeMs
      out.push({ gameId: id, file: path.join(dir, f), url: `${MEDIA_SCHEME}://shots/${id}/${f}`, takenAt })
    }
  }
  return out.sort((a, b) => b.takenAt - a.takenAt)
}

// ── File browsing (controller-friendly picker) ────────────────────────

function listDir(dir: string | null): { path: string | null; parent: string | null; entries: DirEntry[] } {
  if (!dir) {
    const entries: DirEntry[] = []
    for (const l of 'CDEFGHIJKLMNOPQRSTUVWXYZ') {
      const root = `${l}:\\`
      if (fs.existsSync(root)) entries.push({ name: root, path: root, isDir: true })
    }
    const home = app.getPath('home')
    for (const [name, p] of [
      ['Masaüstü', app.getPath('desktop')],
      ['İndirilenler', app.getPath('downloads')],
      ['Belgeler', app.getPath('documents')],
      ['Kullanıcı klasörü', home]
    ]) {
      if (fs.existsSync(p)) entries.push({ name, path: p, isDir: true })
    }
    return { path: null, parent: null, entries }
  }
  const resolved = path.resolve(dir)
  const parentDir = path.dirname(resolved)
  const entries: DirEntry[] = []
  for (const e of fs.readdirSync(resolved, { withFileTypes: true })) {
    if (e.name.startsWith('$') || e.name.startsWith('.')) continue
    entries.push({ name: e.name, path: path.join(resolved, e.name), isDir: e.isDirectory() })
  }
  entries.sort((a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name, undefined, { numeric: true }))
  return { path: resolved, parent: parentDir === resolved ? null : parentDir, entries }
}

// ── IPC ───────────────────────────────────────────────────────────────

type Handlers = { [C in keyof IpcContract]: (...args: Parameters<IpcContract[C]>) => ReturnType<IpcContract[C]> | Promise<ReturnType<IpcContract[C]>> }

function runScan(fn: () => ReturnType<typeof scanFolders>) {
  scanning = true
  broadcast()
  try {
    const { report } = fn()
    queueCovers()
    return report
  } finally {
    scanning = false
    broadcast()
  }
}

const handlers: Handlers = {
  'state:get': () => snapshot(),

  'settings:set': (patch) => {
    const prev = store.settings
    const next = store.updateSettings(patch)
    if (patch.windowMode) applyWindowMode(prev.windowMode, next.windowMode)
    if (patch.overlayHotkey && watcher) watcher.hotkey = next.overlayHotkey
    if (patch.downloadCovers) queueCovers()
    broadcast()
    return next
  },

  'library:addFolder': (folder, platformId) => {
    addWatchedFolder(folder, platformId)
    return runScan(() => scanFolders([{ path: folder, platformId }]))
  },
  'library:removeFolder': (folder) => {
    const n = removeWatchedFolder(folder)
    broadcast()
    return n
  },
  'library:rescan': () => runScan(() => scanFolders()),
  'library:addFile': (file, platformId) => {
    const g = addSingle(file, platformId)
    queueCovers()
    broadcast()
    return g
  },
  'library:remove': (id) => {
    removeGame(id)
    broadcast()
  },
  'library:favorite': (id, favorite) => {
    store.updateGame(id, { favorite })
    broadcast()
  },
  'library:move': (id, platformId) => {
    moveGame(id, platformId)
    queueCovers()
    broadcast()
  },
  'library:setCover': (id, image) => {
    setCustomCover(id, image)
    broadcast()
  },
  'library:fetchCovers': () => queueCovers(),

  'fs:list': (dir) => listDir(dir),
  'dialog:folder': async () => {
    const r = await dialog.showOpenDialog(mainWindow!, { properties: ['openDirectory'] })
    return r.canceled ? null : r.filePaths[0] ?? null
  },
  'dialog:file': async (kind) => {
    const filters =
      kind === 'exe'
        ? [{ name: 'Programlar', extensions: ['exe', 'lnk', 'url', 'bat'] }]
        : kind === 'image'
          ? [{ name: 'Görseller', extensions: ['png', 'jpg', 'jpeg', 'webp'] }]
          : [{ name: 'Tüm dosyalar', extensions: ['*'] }]
    const r = await dialog.showOpenDialog(mainWindow!, { properties: ['openFile'], filters })
    return r.canceled ? null : r.filePaths[0] ?? null
  },

  'emu:detect': (extraRoot) => {
    const found = detectEmulators(store.settings, extraRoot ? [extraRoot] : [])
    const n = Object.keys(found).length
    if (n) store.updateSettings({ emulatorPaths: { ...store.settings.emulatorPaths, ...found } })
    broadcast()
    return n
  },
  'emu:setPath': (id, exe) => {
    const next = { ...store.settings.emulatorPaths }
    if (exe) next[id] = exe
    else delete next[id]
    store.updateSettings({ emulatorPaths: next })
    broadcast()
  },

  'game:launch': async (id) => {
    const game = store.games.find((g) => g.id === id)
    if (!game) throw new Error('Oyun bulunamadı.')
    if (session.info) throw new Error('Önce çalışan oyunu kapatın.')
    let plan
    try {
      plan = planLaunch(game, store.settings)
    } catch (err) {
      const message = (err as Error).message
      if (err instanceof LaunchError) store.updateGame(id, { lastLaunch: { at: Date.now(), result: 'error', message } })
      broadcast()
      throw err
    }
    store.updateGame(id, { lastPlayedAt: Date.now(), playCount: game.playCount + 1 })
    if (plan.type === 'url') {
      await shell.openExternal(plan.url)
      toast('info', `${game.title} başlatıcı üzerinden açılıyor. Bu oyunun süresi takip edilemez.`)
      broadcast()
      return
    }
    await session.launch(game, plan)
    broadcast()
  },

  'session:pause': (paused) => session.setPaused(paused),
  'session:stop': async () => {
    if (overlayWindow && !overlayWindow.isDestroyed()) overlayWindow.hide()
    await session.stop()
  },
  'session:screenshot': async () => {
    const shot = await takeScreenshot()
    if (shot) toast('success', 'Ekran görüntüsü kaydedildi.')
    return shot
  },
  'session:home': () => {
    if (!session.info) return
    session.setPaused(true)
    if (overlayWindow && !overlayWindow.isDestroyed()) overlayWindow.hide()
    if (mainWindow) {
      mainWindow.show()
      if (store.settings.windowMode === 'fullscreen') mainWindow.setFullScreen(true)
      mainWindow.focus()
    }
    broadcast()
  },
  'session:returnToGame': () => {
    if (!session.info) return
    mainWindow?.hide()
    session.setPaused(false)
    session.focusGame()
  },
  'session:perf': () => session.perf(),
  'overlay:close': () => closeOverlay(),

  'media:screenshots': (gameId) => listScreenshots(gameId),
  'media:deleteScreenshot': (file) => {
    const resolved = path.resolve(file)
    if (!resolved.startsWith(path.resolve(paths.screenshots) + path.sep)) throw new Error('Geçersiz dosya.')
    fs.rmSync(resolved, { force: true })
  },
  'app:quit': () => app.quit()
}

function registerIpc(): void {
  for (const [channel, fn] of Object.entries(handlers)) {
    ipcMain.handle(channel, (_e, ...args) => (fn as (...a: unknown[]) => unknown)(...args))
  }
}

function registerMediaProtocol(): void {
  protocol.handle(MEDIA_SCHEME, (req) => {
    const url = new URL(req.url)
    const rel = decodeURIComponent(url.pathname).replace(/^\/+/, '')
    let file: string | null = null
    if (url.host === 'covers') file = coverFile(path.basename(rel, '.png'))
    else if (url.host === 'shots') {
      const candidate = path.resolve(paths.screenshots, rel)
      if (candidate.startsWith(path.resolve(paths.screenshots) + path.sep)) file = candidate
    }
    if (!file || !fs.existsSync(file)) return new Response(null, { status: 404 })
    return net.fetch(pathToFileURL(file).toString())
  })
}

// ── Lifecycle ─────────────────────────────────────────────────────────

app.on('second-instance', () => {
  if (mainWindow) {
    if (mainWindow.isMinimized()) mainWindow.restore()
    mainWindow.show()
    mainWindow.focus()
  }
})

app.whenReady().then(() => {
  initStores()
  watcher = new GamepadWatcher(store.settings.overlayHotkey)
  watcher.on('menu', toggleOverlay)
  registerMediaProtocol()
  registerIpc()
  createMainWindow()

  // Heavy work after the UI is up.
  setTimeout(() => {
    if (Object.keys(store.settings.emulatorPaths).length === 0) {
      const found = detectEmulators(store.settings)
      if (Object.keys(found).length) store.updateSettings({ emulatorPaths: found })
    }
    if (store.settings.scanOnStartup && store.settings.watchedFolders.length) runScan(() => scanFolders())
    else refreshMissing()
    queueCovers()
    broadcast()
  }, 1200)
})

app.on('before-quit', () => {
  void session.stop()
  store.flush()
})

app.on('will-quit', () => globalShortcut.unregisterAll())

app.on('window-all-closed', () => app.quit())

