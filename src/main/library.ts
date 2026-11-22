import crypto from 'crypto'
import fs from 'fs'
import path from 'path'
import type { Game, ScanReport } from '@shared/types'
import { getPlatform } from '@shared/platforms'
import { candidateFromPath, scanFolder, type Candidate } from './scanner'
import { coverFile } from './covers'
import { store } from './store'

export function gameIdFor(p: string): string {
  return crypto.createHash('sha1').update(path.resolve(p).toLowerCase()).digest('hex').slice(0, 16)
}

function toGame(c: Candidate): Game {
  const id = gameIdFor(c.path)
  return {
    id,
    platformId: c.platformId,
    title: c.title,
    path: c.path,
    kind: c.kind,
    addedAt: Date.now(),
    playTimeSec: 0,
    playCount: 0,
    favorite: false,
    hasCover: fs.existsSync(coverFile(id)),
    meta: c.meta
  }
}

/** Adds candidates that are not in the library yet. Returns the newly added games. */
export function addCandidates(candidates: Candidate[]): Game[] {
  const games = store.games
  const known = new Set(games.map((g) => g.id))
  const added: Game[] = []
  for (const c of candidates) {
    const g = toGame(c)
    if (known.has(g.id)) continue
    known.add(g.id)
    added.push(g)
  }
  if (added.length) store.setGames([...games, ...added])
  return added
}

export function scanFolders(folders = store.settings.watchedFolders): { report: ScanReport; added: Game[] } {
  const report: ScanReport = { added: 0, skipped: 0, folders: folders.length, errors: [] }
  const added: Game[] = []
  for (const f of folders) {
    if (!fs.existsSync(f.path)) {
      report.errors.push(`Klasör bulunamadı: ${f.path}`)
      continue
    }
    const { candidates, errors } = scanFolder(f.path, f.platformId)
    report.errors.push(...errors.slice(0, 5))
    const fresh = addCandidates(candidates)
    added.push(...fresh)
    report.skipped += candidates.length - fresh.length
  }
  report.added = added.length
  refreshMissing()
  return { report, added }
}

export function addWatchedFolder(folder: string, platformId: string): void {
  const resolved = path.resolve(folder)
  const list = store.settings.watchedFolders.filter((f) => path.resolve(f.path).toLowerCase() !== resolved.toLowerCase())
  store.updateSettings({ watchedFolders: [...list, { path: resolved, platformId }] })
}

/** Removes a folder and the games that were found inside it. */
export function removeWatchedFolder(folder: string): number {
  const resolved = path.resolve(folder).toLowerCase() + path.sep
  store.updateSettings({
    watchedFolders: store.settings.watchedFolders.filter((f) => path.resolve(f.path).toLowerCase() + path.sep !== resolved)
  })
  const before = store.games.length
  store.setGames(store.games.filter((g) => !path.resolve(g.path).toLowerCase().startsWith(resolved)))
  return before - store.games.length
}

export function addSingle(p: string, platformId: string): Game {
  const c = candidateFromPath(p, platformId)
  if (!c) {
    const plat = getPlatform(platformId)
    throw new Error(`Bu dosya ${plat?.name ?? platformId} için tanınmadı.`)
  }
  const id = gameIdFor(c.path)
  const existing = store.games.find((g) => g.id === id)
  if (existing) throw new Error(`"${existing.title}" zaten kütüphanede.`)
  return addCandidates([c])[0]
}

export function removeGame(id: string): void {
  store.setGames(store.games.filter((g) => g.id !== id))
  try {
    fs.rmSync(coverFile(id), { force: true })
  } catch {
    // ignore
  }
}

export function moveGame(id: string, platformId: string): void {
  if (!getPlatform(platformId)) throw new Error('Bilinmeyen platform.')
  store.updateGame(id, { platformId, lastLaunch: undefined })
}

export function setCustomCover(id: string, imagePath: string): void {
  fs.copyFileSync(imagePath, coverFile(id))
  const g = store.games.find((x) => x.id === id)
  store.updateGame(id, { hasCover: true, coverRev: (g?.coverRev ?? 0) + 1 })
}

/** Flags games whose file disappeared (e.g. an unplugged drive) instead of deleting them. */
export function refreshMissing(): void {
  let changed = false
  for (const g of store.games) {
    if (g.kind === 'titleId') continue
    const missing = !fs.existsSync(g.path)
    if (Boolean(g.missing) !== missing) {
      g.missing = missing
      changed = true
    }
  }
  if (changed) store.setGames([...store.games])
}
