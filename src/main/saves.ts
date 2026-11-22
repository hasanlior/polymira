import { app, shell } from 'electron'
import { execFile } from 'child_process'
import fs from 'fs'
import os from 'os'
import path from 'path'
import { EMULATORS, PLATFORMS, resolveRunner } from '@shared/platforms'
import type { SaveBackup, SaveInfo } from '@shared/types'
import { paths, store } from './store'

/**
 * One save-data manager for every emulator: finds where each emulator keeps its saves
 * (portable folder or the user's Documents/AppData), backs them up as zip files, restores
 * them, and can mirror the backups to a sync folder (OneDrive, Google Drive, a USB disk…).
 */

const KEEP = 10
const docs = () => app.getPath('documents')
const appData = () => app.getPath('appData')

/** Candidate save locations for an emulator. The first existing ones are used. */
function candidates(emuId: string, dir: string): string[][] {
  const j = path.join
  switch (emuId) {
    case 'retroarch':
      return [[j(dir, 'saves'), j(dir, 'states')]]
    case 'duckstation':
      return [
        [j(dir, 'memcards'), j(dir, 'savestates')],
        [j(docs(), 'DuckStation', 'memcards'), j(docs(), 'DuckStation', 'savestates')]
      ]
    case 'pcsx2':
      return [
        [j(dir, 'memcards'), j(dir, 'sstates')],
        [j(docs(), 'PCSX2', 'memcards'), j(docs(), 'PCSX2', 'sstates')]
      ]
    case 'ppsspp':
      return [
        [j(dir, 'memstick', 'PSP', 'SAVEDATA'), j(dir, 'memstick', 'PSP', 'PPSSPP_STATE')],
        [j(docs(), 'PPSSPP', 'PSP', 'SAVEDATA'), j(docs(), 'PPSSPP', 'PSP', 'PPSSPP_STATE')]
      ]
    case 'rpcs3':
      return [[j(dir, 'dev_hdd0', 'home', '00000001', 'savedata')]]
    case 'vita3k':
      return [[j(dir, 'ux0', 'user', '00', 'savedata')], [j(appData(), 'Vita3K', 'Vita3K', 'ux0', 'user', '00', 'savedata')]]
    case 'shadps4':
      return [[j(dir, 'user', 'savedata')], [j(appData(), 'shadPS4', 'savedata')]]
    case 'xenia':
      return [[j(dir, 'content')], [j(docs(), 'Xenia', 'content')]]
    case 'dolphin':
      return [
        [j(dir, 'User', 'GC'), j(dir, 'User', 'Wii', 'title'), j(dir, 'User', 'StateSaves')],
        [j(docs(), 'Dolphin Emulator', 'GC'), j(docs(), 'Dolphin Emulator', 'Wii', 'title'), j(docs(), 'Dolphin Emulator', 'StateSaves')]
      ]
    case 'cemu':
      return [[j(dir, 'mlc01', 'usr', 'save')], [j(appData(), 'Cemu', 'mlc01', 'usr', 'save')]]
    case 'ryujinx':
      return [[j(dir, 'portable', 'bis', 'user', 'save')], [j(appData(), 'Ryujinx', 'bis', 'user', 'save')]]
    case 'azahar':
      return [
        [j(dir, 'user', 'sdmc', 'Nintendo 3DS'), j(dir, 'user', 'states')],
        [j(appData(), 'Azahar', 'sdmc', 'Nintendo 3DS'), j(appData(), 'Azahar', 'states')]
      ]
    case 'flycast':
      return [[j(dir, 'data')], [j(appData(), 'flycast', 'data')]]
    default:
      return []
  }
}

/** melonDS (and others in their default setup) write saves next to the game file. */
function romSideSaves(emuId: string): string[] {
  if (emuId !== 'melonds') return []
  const platformIds = PLATFORMS.filter((p) => p.runners.some((r) => r.emulatorId === emuId)).map((p) => p.id)
  const out: string[] = []
  for (const g of store.games) {
    if (!platformIds.includes(g.platformId)) continue
    const sav = g.path.replace(/\.[^.\\/]+$/, '.sav')
    if (sav !== g.path && fs.existsSync(sav)) out.push(sav)
  }
  return out
}

function locations(emuId: string): string[] {
  const exe = store.settings.emulatorPaths[emuId]
  if (!exe || !fs.existsSync(exe)) return []
  const dir = path.dirname(exe)
  const found = candidates(emuId, dir)
    .map((set) => set.filter((p) => fs.existsSync(p)))
    .find((set) => set.length > 0)
  return [...(found ?? []), ...romSideSaves(emuId)]
}

function backupDir(emuId: string): string {
  return path.join(paths.root, 'saves', emuId)
}

function syncDir(emuId: string): string | null {
  const root = store.settings.saveSyncFolder
  return root && fs.existsSync(root) ? path.join(root, `${app.getName()}-saves`, emuId) : null
}

function listBackups(emuId: string): SaveBackup[] {
  const out = new Map<string, SaveBackup>()
  for (const [dir, synced] of [[backupDir(emuId), false], [syncDir(emuId), true]] as const) {
    if (!dir || !fs.existsSync(dir)) continue
    for (const f of fs.readdirSync(dir)) {
      if (!f.endsWith('.zip')) continue
      const st = fs.statSync(path.join(dir, f))
      const prev = out.get(f)
      if (prev) prev.synced = prev.synced || synced
      else out.set(f, { file: path.join(dir, f), name: f, at: st.mtimeMs, size: st.size, synced })
    }
  }
  return [...out.values()].sort((a, b) => b.at - a.at)
}

let cache: SaveInfo[] = []

/** Recomputes save locations and backups for every configured emulator. */
export function refreshSaves(): SaveInfo[] {
  cache = EMULATORS.filter((e) => store.settings.emulatorPaths[e.id]).map((e) => ({
    emuId: e.id,
    name: e.name,
    locations: locations(e.id),
    backups: listBackups(e.id)
  }))
  return cache
}

export function savesSnapshot(): SaveInfo[] {
  return cache
}

const TAR = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe')

function tar(args: string[]): Promise<void> {
  return new Promise((resolve, reject) =>
    execFile(TAR, args, { windowsHide: true, maxBuffer: 1 << 24 }, (err, _o, stderr) => (err ? reject(new Error(String(stderr || err.message).trim())) : resolve()))
  )
}

function stamp(): string {
  const d = new Date()
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
}

/** Zips an emulator's saves. Each location goes in as its own numbered folder plus a manifest. */
export async function backupEmulator(emuId: string): Promise<SaveBackup | null> {
  const locs = locations(emuId)
  if (!locs.length) return null
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'saves-'))
  try {
    const manifest: { entries: { slot: string; path: string; file: boolean }[]; emuId: string; at: number } = { entries: [], emuId, at: Date.now() }
    locs.forEach((loc, i) => {
      const slot = String(i)
      const isFile = fs.statSync(loc).isFile()
      const dest = path.join(work, slot)
      if (isFile) {
        fs.mkdirSync(dest)
        fs.copyFileSync(loc, path.join(dest, path.basename(loc)))
      } else {
        fs.cpSync(loc, dest, { recursive: true })
      }
      manifest.entries.push({ slot, path: loc, file: isFile })
    })
    fs.writeFileSync(path.join(work, 'manifest.json'), JSON.stringify(manifest, null, 2))
    const dir = backupDir(emuId)
    fs.mkdirSync(dir, { recursive: true })
    const name = `${emuId}-${stamp()}.zip`
    const out = path.join(dir, name)
    await tar(['-a', '-c', '-f', out, '-C', work, '.'])
    // Rotation: keep the newest KEEP backups.
    const all = fs.readdirSync(dir).filter((f) => f.endsWith('.zip')).sort().reverse()
    for (const old of all.slice(KEEP)) fs.rmSync(path.join(dir, old), { force: true })
    const sync = syncDir(emuId)
    if (sync) {
      fs.mkdirSync(sync, { recursive: true })
      fs.copyFileSync(out, path.join(sync, name))
    }
    const st = fs.statSync(out)
    return { file: out, name, at: st.mtimeMs, size: st.size, synced: Boolean(sync) }
  } finally {
    fs.rmSync(work, { recursive: true, force: true })
  }
}

/** Backs up all (or the given) emulators. Returns how many were backed up. */
export async function backupAll(emuIds?: string[]): Promise<number> {
  let n = 0
  for (const e of EMULATORS) {
    if (emuIds && !emuIds.includes(e.id)) continue
    if (!store.settings.emulatorPaths[e.id]) continue
    try {
      if (await backupEmulator(e.id)) n++
    } catch (err) {
      console.warn('[saves] backup failed', e.id, (err as Error).message)
    }
  }
  refreshSaves()
  return n
}

/**
 * Restores a backup over the current saves. The current saves are backed up first,
 * so a restore can always be undone.
 */
export async function restoreBackup(emuId: string, file: string): Promise<void> {
  const allowed = [backupDir(emuId), syncDir(emuId)].filter(Boolean).map((d) => path.resolve(d!) + path.sep)
  if (!allowed.some((d) => path.resolve(file).startsWith(d))) throw new Error('Geçersiz yedek dosyası.')
  await backupEmulator(emuId).catch(() => null)
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'restore-'))
  try {
    await tar(['-x', '-f', file, '-C', work])
    const manifest = JSON.parse(fs.readFileSync(path.join(work, 'manifest.json'), 'utf8')) as { entries: { slot: string; path: string; file: boolean }[]; emuId: string }
    if (manifest.emuId !== emuId) throw new Error('Bu yedek başka bir emülatöre ait.')
    for (const e of manifest.entries) {
      const src = path.join(work, e.slot)
      if (!fs.existsSync(src)) continue
      if (e.file) {
        const f = fs.readdirSync(src)[0]
        if (f) {
          fs.mkdirSync(path.dirname(e.path), { recursive: true })
          fs.copyFileSync(path.join(src, f), e.path)
        }
      } else {
        fs.mkdirSync(e.path, { recursive: true })
        fs.cpSync(src, e.path, { recursive: true, force: true })
      }
    }
  } finally {
    fs.rmSync(work, { recursive: true, force: true })
  }
  refreshSaves()
}

export function openSaveFolder(emuId: string): void {
  const loc = locations(emuId)[0]
  const dir = loc ? (fs.statSync(loc).isFile() ? path.dirname(loc) : loc) : backupDir(emuId)
  fs.mkdirSync(dir, { recursive: true })
  void shell.openPath(dir)
}

/** Emulator used by a game (for automatic backup after playing). */
export function emulatorOfGame(platformId: string, chosen?: string): string | null {
  const p = PLATFORMS.find((x) => x.id === platformId)
  if (!p || !p.runners.length) return null
  return resolveRunner(p, chosen ?? store.settings.platformEmulator[p.id])?.emulatorId ?? null
}
