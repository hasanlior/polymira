import { app } from 'electron'
import fs from 'fs'
import path from 'path'
import { PLATFORMS, getPlatform } from '@shared/platforms'
import type { DiscoverGroup, DiscoverResult } from '@shared/types'
import { findEpicGames, findGogGames, findRiotGames, findSteamGames, findUbisoftGames, findXboxGames } from './launchers'
import { scanFolder, type Candidate } from './scanner'
import { detectEmulators } from './emulators'
import { addCandidates, gameIdFor } from './library'
import { store } from './store'

/**
 * "Scan this PC": finds store games, emulators and ROM folders in the usual places.
 * Results are kept in memory until the user picks which groups to import.
 */

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

/** Platform names in folder names ("PS2 ISO", "Nintendo 64", ...). Short hints like "gb" are left out. */
const PLATFORM_HINT = new RegExp(
  `\\b(${[...new Set(PLATFORMS.flatMap((p) => p.folderHints ?? []))]
    .filter((h) => h.length > 2)
    .map(escapeRe)
    .join('|')})\\b`,
  'i'
)

/** Folder names that usually hold games or ROMs, on any drive or in the user folder. */
const GAME_DIR = /\b(games?|oyun(lar)?|roms?|emulators?|emulation|em[uü]lat[oö]r(ler)?|retro|consoles?|konsol|isos?)\b/i
const SYSTEM_DIRS = /^(windows|program files( \(x86\))?|programdata|users|xboxgames|recovery|perflogs|intel|amd|nvidia|msocache|\$.*)$/i

function isDir(p: string): boolean {
  try {
    return fs.statSync(p).isDirectory()
  } catch {
    return false
  }
}

/**
 * Where to look for ROMs/ISOs without scanning whole disks: the user's own folders plus
 * every top-level folder (on any drive, and in the home folder) whose name suggests games.
 */
function romRoots(): string[] {
  const roots = new Set<string>()
  const home = app.getPath('home')
  for (const p of [app.getPath('desktop'), app.getPath('downloads'), app.getPath('documents')]) roots.add(p)
  const scanTop = (dir: string) => {
    let names: string[]
    try {
      names = fs.readdirSync(dir)
    } catch {
      return
    }
    for (const n of names) {
      if (SYSTEM_DIRS.test(n) || n.startsWith('.')) continue
      if (GAME_DIR.test(n) || PLATFORM_HINT.test(n)) roots.add(path.join(dir, n))
    }
  }
  scanTop(home)
  for (const letter of 'CDEFGHIJKLMNOPQRSTUVWXYZ') {
    const drive = `${letter}:\\`
    if (fs.existsSync(drive)) scanTop(drive)
  }
  // Drop roots nested inside other roots (they are scanned anyway).
  const list = [...roots].filter(isDir)
  return list.filter((r) => !list.some((o) => o !== r && r.toLowerCase().startsWith(o.toLowerCase() + path.sep)))
}

interface Pending {
  byGroup: Map<string, Candidate[]>
  emulators: Record<string, string>
  /** Root folder each ROM came from, so it can be watched for new games later. */
  romRootOf: Map<string, string>
}

let pending: Pending | null = null

const sample = (c: Candidate[]) => c.slice(0, 4).map((x) => x.title)

export async function runDiscovery(progress: (text: string) => void): Promise<DiscoverResult> {
  const started = Date.now()
  const known = new Set(store.games.map((g) => g.id))
  const fresh = (list: Candidate[]) => list.filter((c) => !known.has(gameIdFor(c.path)))
  const byGroup = new Map<string, Candidate[]>()
  const groups: DiscoverGroup[] = []

  progress('Steam kütüphanesi okunuyor…')
  const steam = fresh(await findSteamGames())
  progress('Epic Games kütüphanesi okunuyor…')
  const epic = fresh(await findEpicGames())
  progress('GOG kütüphanesi okunuyor…')
  const gog = fresh(await findGogGames())
  progress('Xbox uygulaması oyunları okunuyor…')
  const xbox = fresh(await findXboxGames())
  progress('Riot Games ve Ubisoft Connect okunuyor…')
  const riot = fresh(await findRiotGames())
  const ubisoft = fresh(await findUbisoftGames())
  for (const [key, label, list] of [
    ['steam', 'Steam oyunları', steam],
    ['epic', 'Epic Games oyunları', epic],
    ['gog', 'GOG oyunları', gog],
    ['xbox', 'Xbox uygulaması / Game Pass oyunları', xbox],
    ['riot', 'Riot Games oyunları', riot],
    ['ubisoft', 'Ubisoft Connect oyunları', ubisoft]
  ] as const) {
    if (list.length) {
      byGroup.set(key, list)
      groups.push({ key, label, count: list.length, sample: sample(list) })
    }
  }

  progress('Emülatörler aranıyor…')
  await new Promise((r) => setImmediate(r))
  const emulators = detectEmulators(store.settings)
  const emuCount = Object.keys(emulators).length
  if (emuCount) {
    groups.push({ key: 'emulators', label: 'Emülatörler', count: emuCount, sample: Object.keys(emulators).slice(0, 4) })
  }

  const roots = romRoots()
  const romRootOf = new Map<string, string>()
  const roms: Candidate[] = []
  let dirs = 0
  for (const root of roots) {
    const { candidates } = await scanFolder(root, 'auto', (d) => {
      if (++dirs % 40 === 0) progress(`Oyun dosyaları aranıyor… ${d}`)
    })
    for (const c of fresh(candidates)) {
      if (romRootOf.has(c.path)) continue
      romRootOf.set(c.path, root)
      roms.push(c)
    }
  }
  const byPlatform = new Map<string, Candidate[]>()
  for (const c of roms) byPlatform.set(c.platformId, [...(byPlatform.get(c.platformId) ?? []), c])
  for (const [pid, list] of byPlatform) {
    const key = `roms:${pid}`
    byGroup.set(key, list)
    groups.push({ key, label: getPlatform(pid)?.name ?? pid, count: list.length, sample: sample(list) })
  }

  pending = { byGroup, emulators, romRootOf }
  progress('Tamamlandı')
  return { groups, romRoots: roots, durationMs: Date.now() - started }
}

export function applyDiscovery(keys: string[]): { games: number; emulators: number; folders: number } {
  if (!pending) return { games: 0, emulators: 0, folders: 0 }
  const chosen = new Set(keys)
  const candidates: Candidate[] = []
  const folders = new Set<string>()
  for (const [key, list] of pending.byGroup) {
    if (!chosen.has(key)) continue
    candidates.push(...list)
    if (key.startsWith('roms:')) for (const c of list) folders.add(pending.romRootOf.get(c.path)!)
  }
  const added = addCandidates(candidates)

  let emulators = 0
  if (chosen.has('emulators')) {
    emulators = Object.keys(pending.emulators).length
    store.updateSettings({ emulatorPaths: { ...store.settings.emulatorPaths, ...pending.emulators } })
  }

  // Watch ROM roots so new games there are picked up on the next startup scan.
  const watched = new Set(store.settings.watchedFolders.map((f) => f.path.toLowerCase()))
  const newFolders = [...folders].filter((f) => !watched.has(f.toLowerCase()))
  if (newFolders.length) {
    store.updateSettings({ watchedFolders: [...store.settings.watchedFolders, ...newFolders.map((p) => ({ path: p, platformId: 'auto' }))] })
  }
  pending = null
  return { games: added.length, emulators, folders: newFolders.length }
}
