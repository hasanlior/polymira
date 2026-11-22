import { app, BrowserWindow, desktopCapturer, dialog, globalShortcut, ipcMain, net, protocol, screen, shell } from 'electron'
import fs from 'fs'
import path from 'path'
import { pathToFileURL } from 'url'
import { BRAND } from '@shared/brand'
import { MEDIA_SCHEME, type IpcContract, type IpcEvents, type IpcEvent } from '@shared/ipc'
import type { AppState, DirEntry, Notice, Screenshot, Toast, WindowMode } from '@shared/types'
import { initStores, paths, store } from './store'
import { addSingle, addWatchedFolder, moveGame, refreshMissing, removeGame, removeWatchedFolder, scanFolders, setCustomBackground, setCustomCover, setMusic } from './library'
import { ArtQueue, backgroundFile, coverFile } from './covers'
import { detectEmulators, emulatorStatus, LaunchError, planLaunch } from './emulators'
import { SessionManager } from './session'
import { GamepadWatcher } from './gamepadWatcher'
import { spawn } from 'child_process'
import { applyDiscovery, runDiscovery } from './discover'
import { riotClientPath } from './launchers'
import { attachSoundtracks } from './music'
import { DownloadManager, EMULATOR_SOURCES } from './downloads'
import { appLaunchPlan, appPath, catalog, findItem, installedThemes, listMods, modsCss, openFolder } from './apps'
import { addCandidates } from './library'
import { appIconFile, appIconRevs, fetchAppIcon } from './appIcons'
import { drmStatus, initDrm } from './drm'
import { recordSession, playStats } from './stats'
import { createSocket } from 'dgram'
import { RETROARCH_CMD_PORT } from './emulators'
import { backupAll, emulatorOfGame, openSaveFolder, refreshSaves, restoreBackup, savesSnapshot } from './saves'
import { enterGameMode, exitGameMode, startExplorer } from './gameMode'
import { getRuntime, initRuntime, readRuntime, runtimeFile, updateRuntime } from './runtime'

/** Keyboard fallback for opening the in-game menu (works while the emulator has focus). */
const OVERLAY_SHORTCUT = 'Control+Shift+Home'
const appIcon = path.join(__dirname, '../../resources/icon.png')

protocol.registerSchemesAsPrivileged([
  { scheme: MEDIA_SCHEME, privileges: { standard: true, secure: true, supportFetchAPI: true, stream: true } }
])

/**
 * The product was renamed: on the first start under the new name, carry over the library,
 * settings, artwork and app logins from the old data folder (Chromium caches are skipped).
 */
function migrateLegacyData(): void {
  const target = app.getPath('userData')
  if (fs.existsSync(path.join(target, 'settings.json'))) return
  for (const old of BRAND.legacyNames) {
    const src = path.join(app.getPath('appData'), old)
    if (src === target || !fs.existsSync(path.join(src, 'settings.json'))) continue
    const skip = new Set(['Cache', 'Code Cache', 'GPUCache', 'DawnGraphiteCache', 'DawnWebGPUCache', 'Crashpad', 'runtime.json', 'SingletonLock'])
    try {
      fs.cpSync(src, target, { recursive: true, filter: (p) => !skip.has(path.basename(p)) })
      console.log('[migrate] copied data from', src)
    } catch (err) {
      console.warn('[migrate] failed:', (err as Error).message)
    }
    return
  }
}
migrateLegacyData()

if (!app.requestSingleInstanceLock()) {
  app.quit()
}

let mainWindow: BrowserWindow | null = null
let quitting = false
let replacingWindow = false
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

// ── Notifications ─────────────────────────────────────────────────────

const notices: Notice[] = []
let noticeId = 1
let noticeTimer: NodeJS.Timeout | null = null

/**
 * Shows a notification card. While a game runs, the in-game menu window is shown for a few
 * seconds without taking focus or input, so the card appears over the game.
 */
function notify(n: Omit<Notice, 'id' | 'at'>): void {
  const notice: Notice = { ...n, id: noticeId++, at: Date.now() }
  notices.unshift(notice)
  notices.length = Math.min(notices.length, 40)
  send('notice', notice)
  broadcast()
  if (session.info && !overlayVisible() && store.settings.notifyInGame) {
    const w = ensureOverlay()
    w.setIgnoreMouseEvents(true)
    w.setFocusable(false)
    w.showInactive()
    w.setAlwaysOnTop(true, 'screen-saver')
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      if (overlayWindow && !overlayWindow.isDestroyed() && !overlayMenuOpen) overlayWindow.hide()
    }, 6000)
  }
}

const downloads = new DownloadManager((id, exe) => {
  store.updateSettings({ emulatorPaths: { ...store.settings.emulatorPaths, [id]: exe } })
  const name = downloads.jobs.find((j) => j.id === id)?.name ?? id
  notify({ kind: 'download', title: `${name} hazır`, text: 'İndirildi ve kuruldu.' })
  broadcast()
})
downloads.on('changed', () => broadcast())

function snapshot(): AppState {
  return {
    themes: installedThemes(),
    appIcons: appIconRevs(),
    notices,
    saves: savesSnapshot(),
    drm: drmStatus(),
    mods: listMods(),
    downloads: downloads.jobs,
    downloadable: Object.keys(EMULATOR_SOURCES),
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
  }, 150)
}

const ART_RECHECK_MS = 7 * 24 * 3600 * 1000
/** Bump when artwork sources improve, so games without art are looked up again. */
const ART_VERSION = 2

const art = new ArtQueue(
  (id, r) => {
    const g = store.games.find((x) => x.id === id)
    if (!g) return
    const patch: Partial<typeof g> = { artCheckedAt: Date.now(), artVersion: ART_VERSION }
    if (r.cover) Object.assign(patch, { hasCover: true, coverIsIcon: Boolean(r.coverIsIcon), coverRev: (g.coverRev ?? 0) + 1 })
    if (r.background) Object.assign(patch, { hasBackground: true, bgRev: (g.bgRev ?? 0) + 1 })
    if (r.trailer !== undefined) patch.trailerUrl = r.trailer
    if (r.steamAppId && r.steamAppId !== g.meta.steamAppId) patch.meta = { ...g.meta, steamAppId: r.steamAppId }
    store.updateGame(id, patch)
    if (r.cover || r.background || r.trailer) broadcast()
  },
  () => store.settings.downloadCovers
)

/** Queues artwork lookup for games missing a cover or background. Returns how many were queued. */
function queueCovers(force = false): number {
  const todo = store.games.filter(
    (g) =>
      (!g.hasCover || !g.hasBackground || (g.platformId === 'pc' && g.trailerUrl === undefined)) &&
      (force || g.artVersion !== ART_VERSION || !g.artCheckedAt || Date.now() - g.artCheckedAt > ART_RECHECK_MS)
  )
  art.push(todo)
  return todo.length
}

// ── Windows ───────────────────────────────────────────────────────────

function windowOptions(mode: WindowMode): Electron.BrowserWindowConstructorOptions {
  const d = screen.getPrimaryDisplay()
  const base: Electron.BrowserWindowConstructorOptions = {
    title: BRAND.name,
    icon: appIcon,
    backgroundColor: '#000000',
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, '../preload/index.js'),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      // Menu sounds, theme music and trailers start without a click, like on a console.
      autoplayPolicy: 'no-user-gesture-required',
      // The built-in browser and web apps run in <webview> guests (locked down in will-attach-webview).
      webviewTag: true
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
    mainWindow.webContents.on('console-message', (e) => {
      const d = e as unknown as { level: string | number; message: string }
      if (d.level === 'warning' || d.level === 'error' || (typeof d.level === 'number' && d.level >= 2)) console.log(`[renderer] ${d.message}`)
    })
  }
  mainWindow.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  mainWindow.on('focus', () => send('window:focus', true))
  mainWindow.on('blur', () => send('window:focus', false))
  mainWindow.once('ready-to-show', () => {
    mainWindow?.show()
    mainWindow?.focus()
  })
  mainWindow.on('closed', () => {
    if (mainWindow?.isDestroyed()) mainWindow = null
  })
  // Logging off / shutting down Windows is not a crash, and must not be delayed.
  mainWindow.on('session-end', () => {
    quitting = true
    updateRuntime({ cleanExit: true })
  })
  // Alt+F4 / window close: run the same fade-out shutdown as "Quit".
  mainWindow.on('close', (e) => {
    if (quitting || replacingWindow) return
    e.preventDefault()
    void gracefulQuit()
  })
  mainWindow.webContents.on('render-process-gone', (_e, details) => {
    console.warn('[main] renderer gone:', details.reason, 'exit', details.exitCode)
    if (details.reason !== 'clean-exit') setTimeout(() => mainWindow?.webContents.reload(), 500)
  })
  load(mainWindow, 'index')
  if (old && !old.isDestroyed()) {
    replacingWindow = true
    old.destroy()
    replacingWindow = false
  }
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

let overlayMenuOpen = false

function hideOverlay(): void {
  overlayMenuOpen = false
  if (overlayWindow && !overlayWindow.isDestroyed()) overlayWindow.hide()
}

/** The in-game menu is open (not just a notification over the game). */
function overlayVisible(): boolean {
  return overlayMenuOpen && Boolean(overlayWindow && !overlayWindow.isDestroyed() && overlayWindow.isVisible())
}

function openOverlay(): void {
  if (!session.info) return
  const w = ensureOverlay()
  if (store.settings.pauseOnOverlay) session.setPaused(true)
  overlayMenuOpen = true
  w.setIgnoreMouseEvents(false)
  w.setFocusable(true)
  w.setBounds(screen.getPrimaryDisplay().bounds)
  w.show()
  w.setAlwaysOnTop(true, 'screen-saver')
  w.focus()
  send('state', snapshot())
  send('overlay:opened', null)
}

function closeOverlay(): void {
  overlayMenuOpen = false
  if (overlayWindow && !overlayWindow.isDestroyed()) overlayWindow.hide()
  session.setPaused(false)
  session.focusGame()
}

function toggleOverlay(): void {
  if (mainWindow?.isVisible() && mainWindow.isFocused()) return // game is behind the XMB; ignore
  if (overlayVisible()) closeOverlay()
  else openOverlay()
}

session.on('started', (info: import('@shared/types').SessionInfo) => {
  void enterGameMode(info.pid).then((applied) => {
    if (applied.length) console.log('[game mode]', applied.join(', '))
  })
  // The console stays open behind the game (no desktop flashing between game and menu);
  // it pauses its own rendering while the game has focus.
  for (const ms of [1500, 4000]) setTimeout(() => {
      // Bring the game forward if the console still has focus (launchers can leave it behind).
      if (session.info && !session.info.paused && mainWindow?.isFocused()) session.focusGame()
    }, ms)
  ensureOverlay()
  watcher.hotkey = store.settings.overlayHotkey
  watcher.start()
  globalShortcut.register(OVERLAY_SHORTCUT, toggleOverlay)
})

session.on('changed', () => broadcast())

session.on('ended', (end: import('./session').SessionEnd) => {
  void exitGameMode()
  watcher.stop()
  globalShortcut.unregister(OVERLAY_SHORTCUT)
  if (overlayWindow && !overlayWindow.isDestroyed()) overlayWindow.destroy()
  overlayMenuOpen = false
  overlayWindow = null
  const g = store.games.find((x) => x.id === end.gameId)
  if (g) {
    recordSession(g.id, Date.now() - end.playedSec * 1000, end.playedSec)
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
  // Back up this emulator's saves after playing.
  if (g && store.settings.saveAutoBackup) {
    const emu = emulatorOfGame(g.platformId, g.emulator)
    if (emu) {
      void backupAll([emu]).then((n) => {
        broadcast()
        if (n) notify({ kind: 'save', title: 'Kayıtlar yedeklendi', text: `${g.title} oturumundan sonra otomatik yedek alındı.` })
      })
    }
  }
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

async function runScan(fn: () => ReturnType<typeof scanFolders>) {
  scanning = true
  broadcast()
  try {
    const { report } = await fn()
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
    if (patch.autoRestart !== undefined) updateRuntime({ autoRestart: next.autoRestart })
    if (patch.launchOnStartup !== undefined) {
      if (app.isPackaged) app.setLoginItemSettings({ openAtLogin: next.launchOnStartup })
      else toast('warning', 'Windows ile başlatma yalnızca kurulu sürümde çalışır.')
    }
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
  'library:fetchCovers': () => queueCovers(true),
  'library:setBackground': (id, image) => {
    setCustomBackground(id, image)
    broadcast()
  },
  'library:setMusic': (id, file) => {
    setMusic(id, file)
    broadcast()
  },

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
  'emu:download': (ids) => downloads.enqueue(ids),
  'emu:cancelDownload': (id) => downloads.cancel(id),
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
      plan = game.platformId === 'apps' ? appLaunchPlan(game) : planLaunch(game, store.settings)
    } catch (err) {
      const message = (err as Error).message
      if (err instanceof LaunchError) store.updateGame(id, { lastLaunch: { at: Date.now(), result: 'error', message } })
      broadcast()
      throw err
    }
    store.updateGame(id, { lastPlayedAt: Date.now(), playCount: game.playCount + 1 })
    if (plan.type === 'url') {
      const riot = plan.url.match(/^riot:\/\/([a-z_]+)\/([a-z]+)/i)
      if (riot) {
        const client = riotClientPath()
        if (!client) throw new Error('Riot Client bulunamadı.')
        spawn(client, [`--launch-product=${riot[1]}`, `--launch-patchline=${riot[2]}`], { detached: true, stdio: 'ignore' }).unref()
      } else if (/^shell:/i.test(plan.url)) {
        // Xbox app / Microsoft Store games are started through their app id.
        spawn('explorer.exe', [plan.url], { detached: true, stdio: 'ignore' }).unref()
      } else {
        await shell.openExternal(plan.url)
      }
      const dir = game.meta.installDir
      if (dir && fs.existsSync(dir)) {
        toast('info', `${game.title} başlatıcı üzerinden açılıyor…`)
        void session.attachByFolder(game, dir).then((info) => {
          if (!info) toast('warning', `${game.title} işlemi bulunamadı; süre ve oyun içi menü bu oturumda kullanılamaz.`)
        })
      } else {
        toast('info', `${game.title} başlatıcı üzerinden açılıyor. Bu oyunun süresi takip edilemez.`)
      }
      broadcast()
      return
    }
    await session.launch(game, plan)
    broadcast()
  },

  'session:pause': (paused) => session.setPaused(paused),
  'session:stop': async () => {
    hideOverlay()
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
    hideOverlay()
    if (mainWindow) {
      mainWindow.show()
      if (store.settings.windowMode === 'fullscreen') mainWindow.setFullScreen(true)
      mainWindow.focus()
    }
    broadcast()
  },
  'session:returnToGame': () => {
    if (!session.info) return
    session.setPaused(false)
    if (!session.focusGame()) toast('warning', 'Oyun penceresi bulunamadı.')
  },
  'session:perf': () => session.perf(),
  'overlay:close': () => closeOverlay(),

  'media:screenshots': (gameId) => listScreenshots(gameId),
  'media:deleteScreenshot': (file) => {
    const resolved = path.resolve(file)
    if (!resolved.startsWith(path.resolve(paths.screenshots) + path.sep)) throw new Error('Geçersiz dosya.')
    fs.rmSync(resolved, { force: true })
  },
  'discover:run': async () => {
    scanning = true
    broadcast()
    try {
      return await runDiscovery((text) => send('progress', text))
    } finally {
      scanning = false
      broadcast()
    }
  },
  'discover:apply': (keys) => {
    const r = applyDiscovery(keys)
    void attachSoundtracks().then((n) => n && broadcast())
    queueCovers()
    broadcast()
    return r
  },
  'store:catalog': async (refresh) => {
    const items = await catalog(refresh)
    void refreshAppIcons(items)
    return items
  },
  'store:install': async (id) => {
    const item = findItem(id) ?? (await catalog(true)).find((i) => i.id === id)
    if (!item) throw new Error('Mağazada bulunamadı.')
    if (item.kind === 'app') {
      addCandidates([{ platformId: 'apps', title: item.name, path: appPath(item.id), kind: 'uri', meta: { source: 'manual', storeId: item.id } }])
    } else if (!store.settings.installedThemes.includes(id)) {
      store.updateSettings({ installedThemes: [...store.settings.installedThemes, id] })
    }
    broadcast()
  },
  'store:uninstall': (id) => {
    const item = findItem(id)
    if (item?.kind === 'theme') {
      store.updateSettings({
        installedThemes: store.settings.installedThemes.filter((t) => t !== id),
        theme: store.settings.theme === id ? 'auto' : store.settings.theme
      })
    } else {
      const g = store.games.find((x) => x.path === appPath(id))
      if (g) removeGame(g.id)
    }
    broadcast()
  },
  'mods:css': () => modsCss(),
  'mods:toggle': (id, enabled) => {
    const set = new Set(store.settings.enabledMods)
    if (enabled) set.add(id)
    else set.delete(id)
    store.updateSettings({ enabledMods: [...set] })
    broadcast()
  },
  'folder:open': (which) => openFolder(which),
  'app:opened': (id) => {
    const g = store.games.find((x) => x.id === id)
    if (g) store.updateGame(id, { lastPlayedAt: Date.now(), playCount: g.playCount + 1 })
    broadcast()
  },
  'app:closed': (id, playedSec) => {
    const g = store.games.find((x) => x.id === id)
    if (g && playedSec > 0) recordSession(id, Date.now() - playedSec * 1000, playedSec)
    if (g && playedSec > 0) store.updateGame(id, { playTimeSec: g.playTimeSec + Math.round(playedSec), lastLaunch: { at: Date.now(), result: 'ok' } })
    broadcast()
  },
  'saves:refresh': () => {
    const r = refreshSaves()
    broadcast()
    return r
  },
  'saves:backup': async (emuIds) => {
    const n = await backupAll(emuIds)
    broadcast()
    if (n) notify({ kind: 'save', title: 'Kayıtlar yedeklendi', text: `${n} emülatörün kayıtları yedeklendi.` })
    return n
  },
  'saves:restore': async (emuId, file) => {
    await restoreBackup(emuId, file)
    broadcast()
    notify({ kind: 'save', title: 'Kayıtlar geri yüklendi', text: 'Önceki kayıtların da ayrıca yedeklendi.' })
  },
  'saves:openFolder': (emuId) => openSaveFolder(emuId),
  'stats:get': () => playStats(),
  'library:setEmulator': (id, runner, options) => {
    const g = store.games.find((x) => x.id === id)
    if (!g) return
    store.updateGame(id, { emulator: runner ?? undefined, emuOptions: options ?? g.emuOptions })
    broadcast()
  },
  'session:quickSave': () => retroArchCommand('SAVE_STATE'),
  'session:quickLoad': () => retroArchCommand('LOAD_STATE'),
  'notices:clear': () => {
    notices.length = 0
    broadcast()
  },
  'app:quit': () => void gracefulQuit()
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
    else if (url.host === 'appicon') file = appIconFile(path.basename(rel, '.png'))
    else if (url.host === 'bg') file = backgroundFile(path.basename(rel, '.jpg'))
    else if (url.host === 'music') {
      // Only files the user (or discovery) attached to a game can be served.
      file = store.games.find((g) => g.id === path.basename(rel))?.musicPath ?? null
    }
    else if (url.host === 'shots') {
      const candidate = path.resolve(paths.screenshots, rel)
      if (candidate.startsWith(path.resolve(paths.screenshots) + path.sep)) file = candidate
    }
    if (!file || !fs.existsSync(file)) return new Response(null, { status: 404 })
    return net.fetch(pathToFileURL(file).toString())
  })
}

/** Sends a command to RetroArch's local command port (enabled by our append config). */
async function retroArchCommand(cmd: 'SAVE_STATE' | 'LOAD_STATE'): Promise<void> {
  if (!session.info) throw new Error('Çalışan oyun yok.')
  const wasPaused = session.info.paused
  // A suspended process cannot answer: resume for a moment.
  if (wasPaused) session.setPaused(false)
  await new Promise<void>((resolve, reject) => {
    const sock = createSocket('udp4')
    sock.send(cmd, RETROARCH_CMD_PORT, '127.0.0.1', (err) => {
      sock.close()
      if (err) reject(err)
      else resolve()
    })
  })
  await new Promise((r) => setTimeout(r, 700))
  if (wasPaused) session.setPaused(true)
  notify({ kind: 'save', title: cmd === 'SAVE_STATE' ? 'Durum kaydedildi' : 'Durum yüklendi', text: 'RetroArch hızlı kayıt' })
}

// ── Built-in browser (webview guests) ────────────────────────────────

let iconsBusy = false
async function refreshAppIcons(items?: import('@shared/catalog').StoreItem[]): Promise<void> {
  if (iconsBusy) return
  iconsBusy = true
  try {
    let changed = false
    for (const item of (items ?? (await catalog())).filter((i) => i.kind === 'app')) {
      if (await fetchAppIcon(item)) changed = true
    }
    if (changed) broadcast()
  } finally {
    iconsBusy = false
  }
}

const WEB_PARTITION = /^persist:(web|app-[a-z0-9_-]+)$/i
// mediaKeySystem: protected video (Widevine) on castlabs builds.
const allowedPermissions = new Set(['fullscreen', 'clipboard-sanitized-write', 'pointerLock', 'mediaKeySystem'])
const configuredSessions = new WeakSet<Electron.Session>()

function configureWebSession(ses: Electron.Session): void {
  if (configuredSessions.has(ses)) return
  configuredSessions.add(ses)
  ses.setPermissionRequestHandler((_wc, permission, cb) => cb(allowedPermissions.has(permission)))
  ses.setPermissionCheckHandler((_wc, permission) => allowedPermissions.has(permission))
  ses.on('will-download', (_e, item) => {
    item.cancel()
    toast('warning', 'Konsol tarayıcısı dosya indirmez.')
  })
}

app.on('web-contents-created', (_e, contents) => {
  // Only the console's own pages may embed guests, and every guest is locked down the same way.
  contents.on('will-attach-webview', (e, prefs, params) => {
    if (!/^https?:\/\//i.test(params.src) || !WEB_PARTITION.test(params.partition ?? '')) {
      e.preventDefault()
      return
    }
    delete (prefs as { preloadURL?: string }).preloadURL
    prefs.preload = path.join(__dirname, '../preload/webview.js')
    prefs.nodeIntegration = false
    prefs.nodeIntegrationInSubFrames = false
    prefs.contextIsolation = true
    prefs.sandbox = true
    prefs.webSecurity = true
    prefs.allowRunningInsecureContent = false
  })
  contents.on('did-attach-webview', (_e, guest) => {
    configureWebSession(guest.session)
    // Pop-ups open in the same view, like a console browser without tabs.
    guest.setWindowOpenHandler(({ url }) => {
      if (/^https?:\/\//i.test(url)) void guest.loadURL(url)
      return { action: 'deny' }
    })
    guest.on('will-navigate', (ev, url) => {
      if (!/^(https?|about|blob|data):/i.test(url)) ev.preventDefault()
    })
  })
})

// ── Lifecycle ─────────────────────────────────────────────────────────

app.on('second-instance', () => {
  if (mainWindow) {
    if (mainWindow.isMinimized()) mainWindow.restore()
    mainWindow.show()
    mainWindow.focus()
  }
})

/** Undo leftovers from a previous run that ended badly (desktop shell stopped, power plan). */
function recoverPreviousRun(): void {
  const prev = readRuntime()
  if (!prev) return
  if (prev.explorerStopped) startExplorer()
  if (prev.previousPowerScheme) spawn('powercfg', ['/setactive', prev.previousPowerScheme], { windowsHide: true })
}

function startWatchdog(): void {
  if (process.argv.includes('--no-watchdog')) return
  const script = path.join(__dirname, 'watchdog.js')
  if (!fs.existsSync(script)) return
  const child = spawn(process.execPath, [script, runtimeFile()], {
    detached: true,
    stdio: 'ignore',
    windowsHide: true,
    env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' }
  })
  child.unref()
}

app.whenReady().then(() => {
  initStores()
  recoverPreviousRun()
  initRuntime({
    pid: process.pid,
    cleanExit: false,
    autoRestart: store.settings.autoRestart,
    relaunch: { exe: process.execPath, args: process.argv.slice(1), cwd: process.cwd() },
    explorerStopped: false,
    previousPowerScheme: null
  })
  startWatchdog()
  void initDrm(broadcast)
  watcher = new GamepadWatcher(store.settings.overlayHotkey)
  watcher.on('menu', toggleOverlay)
  registerMediaProtocol()
  registerIpc()
  createMainWindow()

  if (!store.games.some((g) => g.platformId === 'apps') && !store.settings.setupDone) {
    for (const id of ['youtube', 'netflix', 'browser']) {
      const item = findItem(id)!
      addCandidates([{ platformId: 'apps', title: item.name, path: appPath(id), kind: 'uri', meta: { source: 'manual', storeId: id } }])
    }
  }

  // Heavy work after the UI is up.
  setTimeout(() => {
    if (Object.keys(store.settings.emulatorPaths).length === 0) {
      const found = detectEmulators(store.settings)
      if (Object.keys(found).length) store.updateSettings({ emulatorPaths: found })
    }
    if (store.settings.scanOnStartup && store.settings.watchedFolders.length) runScan(() => scanFolders())
    else refreshMissing()
    queueCovers()
    void attachSoundtracks().then((n) => n && broadcast())
    void refreshAppIcons()
    refreshSaves()
    broadcast()
    if (process.env.APP_RESTARTED) {
      delete process.env.APP_RESTARTED
      toast('warning', `${BRAND.name} beklenmedik şekilde kapandı ve yeniden başlatıldı.`)
    }
  }, 1200)
})

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

/**
 * Console-style shutdown: the screen fades out, each cleanup step is shown in the middle
 * of the screen (closing the game, bringing the Windows desktop back…), then the app exits.
 */
async function gracefulQuit(): Promise<void> {
  if (quitting) return
  quitting = true
  const step = (text: string) => send('shutdown', { text })
  hideOverlay()
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.show()
    mainWindow.focus()
  }
  step('Kapatılıyor…')
  await sleep(700) // fade-out
  try {
    if (session.info) {
      step('Oyun kapatılıyor…')
      await session.stop()
      await sleep(400)
    }
    const rt = getRuntime()
    if (rt?.explorerStopped) step('Windows masaüstü açılıyor…')
    else if (rt?.previousPowerScheme) step('Güç planı geri yükleniyor…')
    await exitGameMode()
    step('Ayarlar kaydediliyor…')
    store.flush()
    updateRuntime({ cleanExit: true })
    await sleep(350)
  } catch (err) {
    console.warn('[quit]', err)
  }
  send('shutdown', { text: 'Görüşmek üzere', done: true })
  await sleep(900)
  app.exit(0)
}

app.on('before-quit', (e) => {
  if (quitting) return
  // Any other quit request (tray, OS, second instance…) goes through the same sequence.
  e.preventDefault()
  void gracefulQuit()
})

app.on('will-quit', () => globalShortcut.unregisterAll())

app.on('window-all-closed', () => app.quit())

