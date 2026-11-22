import fs from 'fs'
import path from 'path'
import { PLATFORMS, getPlatform, type PlatformDef } from '@shared/platforms'
import type { GameKind, GameMeta } from '@shared/types'
import { readSfo } from './sfo'

export interface Candidate {
  platformId: string
  title: string
  path: string
  kind: GameKind
  meta: GameMeta
}

const MAX_DEPTH = 6
const SKIP_DIRS = new Set(['$recycle.bin', 'system volume information', 'node_modules', '.git', 'windows'])
const VITA_TITLE_ID = /^[A-Z]{4}\d{5}$/
const REGIONS = ['USA', 'Europe', 'Japan', 'World', 'Korea', 'Asia', 'Brazil', 'Germany', 'France', 'Spain', 'Italy', 'Turkey']

/** "Crash Bandicoot (USA) [!]" → { title: "Crash Bandicoot", region: "USA" } */
export function cleanTitle(fileBase: string): { title: string; region?: string } {
  let region: string | undefined
  const groups = fileBase.match(/\(([^)]*)\)/g) ?? []
  for (const g of groups) {
    const inner = g.slice(1, -1)
    const r = REGIONS.find((x) => inner.split(/,\s*/).includes(x))
    if (r) {
      region = r
      break
    }
  }
  const title = fileBase
    .replace(/\[[^\]]*\]/g, '')
    .replace(/\([^)]*\)/g, '')
    .replace(/_/g, ' ')
    .replace(/\s{2,}/g, ' ')
    .trim()
  return { title: title || fileBase, region }
}

function sfoTitle(sfoPath: string, fallback: string): { title: string; meta: GameMeta; category?: string } {
  const sfo = readSfo(sfoPath)
  const title = typeof sfo?.TITLE === 'string' && sfo.TITLE ? sfo.TITLE.replace(/\n/g, ' ') : fallback
  const titleId = typeof sfo?.TITLE_ID === 'string' ? sfo.TITLE_ID : undefined
  const category = typeof sfo?.CATEGORY === 'string' ? sfo.CATEGORY : undefined
  return { title, meta: { titleId }, category }
}

function exists(p: string): boolean {
  try {
    fs.accessSync(p)
    return true
  } catch {
    return false
  }
}

/** Detects folder-style games (PS3, PS4, Vita). Returns a candidate if `dir` is one. */
function detectFolderGame(dir: string, allowed: (id: string) => boolean): Candidate | null {
  const name = path.basename(dir)

  if (allowed('ps3')) {
    const disc = path.join(dir, 'PS3_GAME', 'USRDIR', 'EBOOT.BIN')
    if (exists(disc)) {
      const { title, meta } = sfoTitle(path.join(dir, 'PS3_GAME', 'PARAM.SFO'), cleanTitle(name).title)
      return { platformId: 'ps3', title, path: disc, kind: 'folder', meta }
    }
    const installed = path.join(dir, 'USRDIR', 'EBOOT.BIN')
    const sfoPath = path.join(dir, 'PARAM.SFO')
    if (exists(installed) && exists(sfoPath)) {
      const { title, meta, category } = sfoTitle(sfoPath, name)
      // HG = installed game, DG = disc game data. Skip patches (GD) and other content.
      if (!category || category === 'HG' || category === 'DG') {
        return { platformId: 'ps3', title, path: installed, kind: 'folder', meta }
      }
    }
  }

  if (allowed('ps4')) {
    const eboot = path.join(dir, 'eboot.bin')
    const sfoPath = path.join(dir, 'sce_sys', 'param.sfo')
    if (exists(eboot) && exists(sfoPath)) {
      const { title, meta } = sfoTitle(sfoPath, name)
      return { platformId: 'ps4', title, path: eboot, kind: 'folder', meta }
    }
  }

  if (allowed('vita') && VITA_TITLE_ID.test(name)) {
    const sfoPath = path.join(dir, 'sce_sys', 'param.sfo')
    if (exists(sfoPath)) {
      const { title, meta } = sfoTitle(sfoPath, name)
      return { platformId: 'vita', title, path: dir, kind: 'titleId', meta: { ...meta, titleId: meta.titleId ?? name } }
    }
  }
  return null
}

const uniqueExtMap = new Map<string, string>()
for (const p of PLATFORMS) for (const e of p.uniqueExtensions ?? []) uniqueExtMap.set(e, p.id)

function platformFromHints(dirs: string[], ext: string): string | undefined {
  // Walk from the closest folder outwards, so "Games/PS2" beats "Games".
  for (let i = dirs.length - 1; i >= 0; i--) {
    const d = dirs[i].toLowerCase()
    const words = d.split(/[^a-z0-9]+/).filter(Boolean)
    for (const p of PLATFORMS) {
      if (!p.extensions.includes(ext)) continue
      for (const hint of p.folderHints ?? []) {
        const hit = hint.includes(' ') ? d.includes(hint) : words.includes(hint)
        if (hit) return p.id
      }
    }
  }
  return undefined
}

function m3uEntries(dir: string, files: string[]): Set<string> {
  const listed = new Set<string>()
  for (const f of files) {
    if (path.extname(f).toLowerCase() !== '.m3u') continue
    try {
      for (const line of fs.readFileSync(path.join(dir, f), 'utf8').split(/\r?\n/)) {
        const t = line.trim()
        if (t && !t.startsWith('#')) listed.add(path.basename(t).toLowerCase())
      }
    } catch {
      // unreadable playlist: ignore
    }
  }
  return listed
}

/**
 * Scans `root` for games.
 * @param platformId a platform id to force, or 'auto' to guess per file.
 */
export function scanFolder(root: string, platformId: string): { candidates: Candidate[]; errors: string[] } {
  const forced: PlatformDef | undefined = platformId === 'auto' ? undefined : getPlatform(platformId)
  const allowed = (id: string) => !forced || forced.id === id
  const candidates: Candidate[] = []
  const errors: string[] = []
  const rootChain = [path.basename(root)]

  const walk = (dir: string, depth: number, chain: string[]) => {
    if (depth > MAX_DEPTH) return
    let entries: fs.Dirent[]
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true })
    } catch (err) {
      errors.push(`${dir}: ${(err as Error).message}`)
      return
    }

    const folderGame = detectFolderGame(dir, allowed)
    if (folderGame) {
      candidates.push(folderGame)
      return
    }

    const files = entries.filter((e) => e.isFile()).map((e) => e.name)
    const lowerFiles = new Set(files.map((f) => f.toLowerCase()))
    const inPlaylist = m3uEntries(dir, files)
    const hasCueSheet = files.some((f) => ['.cue', '.gdi'].includes(path.extname(f).toLowerCase()))

    for (const file of files) {
      const ext = path.extname(file).toLowerCase()
      const lower = file.toLowerCase()
      if (inPlaylist.has(lower)) continue
      // Track files belong to a .cue/.gdi sheet: list the sheet, not every track.
      if (hasCueSheet && (ext === '.bin' || ext === '.img' || ext === '.raw')) continue
      // .cue next to a .chd with the same name: prefer the chd
      if (ext === '.cue' && lowerFiles.has(lower.replace(/\.cue$/, '.chd'))) continue

      let pid: string | undefined
      if (forced) {
        if (!forced.extensions.includes(ext)) continue
        // PC folders contain lots of helper executables: only take shortcuts, and exe files at the top level.
        if (forced.id === 'pc' && ext === '.exe' && depth > 0) continue
        if (forced.id === 'pc' && /unins|setup|crash|launcher_helper|redist/i.test(file)) continue
        pid = forced.id
      } else {
        pid = uniqueExtMap.get(ext) ?? platformFromHints(chain, ext)
      }
      if (!pid) continue

      const base = path.basename(file, path.extname(file))
      const { title, region } = cleanTitle(base)
      candidates.push({
        platformId: pid,
        title,
        path: path.join(dir, file),
        kind: pid === 'pc' ? 'exe' : 'file',
        meta: region ? { region } : {}
      })
    }

    for (const e of entries) {
      if (!e.isDirectory()) continue
      if (e.name.startsWith('.') || SKIP_DIRS.has(e.name.toLowerCase())) continue
      walk(path.join(dir, e.name), depth + 1, [...chain, e.name])
    }
  }

  walk(root, 0, rootChain)
  return { candidates, errors }
}

/** Builds a candidate for a single file/folder the user picked by hand. */
export function candidateFromPath(p: string, platformId: string): Candidate | null {
  const platform = getPlatform(platformId)
  if (!platform) return null
  let stat: fs.Stats
  try {
    stat = fs.statSync(p)
  } catch {
    return null
  }
  if (stat.isDirectory()) {
    return detectFolderGame(p, (id) => id === platformId)
  }
  const ext = path.extname(p).toLowerCase()
  if (path.basename(p).toUpperCase() === 'EBOOT.BIN' && (platformId === 'ps3' || platformId === 'ps4')) {
    return detectFolderGame(path.resolve(p, platformId === 'ps3' ? '../../..' : '..'), (id) => id === platformId)
  }
  if (!platform.extensions.includes(ext)) return null
  const { title, region } = cleanTitle(path.basename(p, path.extname(p)))
  return { platformId, title, path: p, kind: platform.kind === 'exe' ? 'exe' : 'file', meta: region ? { region } : {} }
}
