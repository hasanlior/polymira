import { net } from 'electron'
import fs from 'fs'
import path from 'path'
import { getPlatform } from '@shared/platforms'
import type { Game } from '@shared/types'
import { paths } from './store'

/**
 * Automatic box art from the libretro-thumbnails project (https://github.com/libretro-thumbnails).
 * No API key needed. Only the platform's thumbnail index and the matched image are requested.
 */
const BASE = 'https://thumbnails.libretro.com'
const INDEX_TTL_MS = 7 * 24 * 3600 * 1000

const indexCache = new Map<string, Promise<string[]>>()

export function coverFile(gameId: string): string {
  return path.join(paths.covers, `${gameId}.png`)
}

function normalize(s: string): string {
  return s
    .toLowerCase()
    .replace(/\[[^\]]*\]/g, '')
    .replace(/\([^)]*\)/g, '')
    .replace(/&/g, 'and')
    .replace(/^the\s+|,\s*the\b/g, '')
    .replace(/[^a-z0-9]+/g, '')
}

async function loadIndex(system: string): Promise<string[]> {
  const cacheFile = path.join(paths.cache, `thumbs-${system.replace(/[^a-z0-9]+/gi, '_')}.json`)
  try {
    const st = fs.statSync(cacheFile)
    if (Date.now() - st.mtimeMs < INDEX_TTL_MS) return JSON.parse(fs.readFileSync(cacheFile, 'utf8'))
  } catch {
    // no cache yet
  }
  const res = await net.fetch(`${BASE}/${encodeURIComponent(system)}/Named_Boxarts/`)
  if (!res.ok) throw new Error(`thumbnail index HTTP ${res.status}`)
  const html = await res.text()
  const names: string[] = []
  for (const m of html.matchAll(/href="([^"]+\.png)"/g)) {
    try {
      names.push(decodeURIComponent(m[1]).replace(/\.png$/, ''))
    } catch {
      // malformed entry
    }
  }
  fs.writeFileSync(cacheFile, JSON.stringify(names))
  return names
}

function getIndex(system: string): Promise<string[]> {
  let p = indexCache.get(system)
  if (!p) {
    p = loadIndex(system).catch((err) => {
      indexCache.delete(system)
      throw err
    })
    indexCache.set(system, p)
  }
  return p
}

function pickMatch(index: string[], game: Game): string | undefined {
  const raw = path.basename(game.path, path.extname(game.path)).replace(/[&*/:`<>?\\|]/g, '_')
  if (index.includes(raw)) return raw
  const wanted = normalize(game.kind === 'file' ? raw : game.title)
  const hits = index.filter((n) => normalize(n) === wanted)
  if (hits.length === 0) return undefined
  const region = game.meta.region
  return (region && hits.find((h) => h.includes(region))) || hits.find((h) => /USA|World/.test(h)) || hits[0]
}

/** Tries to download a cover for `game`. Resolves true when a cover was saved. */
export async function fetchCover(game: Game): Promise<boolean> {
  const system = getPlatform(game.platformId)?.thumbnails
  if (!system) return false
  const index = await getIndex(system)
  const name = pickMatch(index, game)
  if (!name) return false
  const url = `${BASE}/${encodeURIComponent(system)}/Named_Boxarts/${encodeURIComponent(name)}.png`
  const res = await net.fetch(url)
  if (!res.ok) return false
  const buf = Buffer.from(await res.arrayBuffer())
  if (buf.length < 100) return false
  fs.writeFileSync(coverFile(game.id), buf)
  return true
}

/** Small promise queue so a big scan does not open hundreds of connections at once. */
export class CoverQueue {
  private queue: Game[] = []
  private running = 0
  private queued = new Set<string>()

  constructor(
    private onDone: (gameId: string, ok: boolean) => void,
    private concurrency = 3
  ) {}

  push(games: Game[]): void {
    for (const g of games) {
      if (this.queued.has(g.id)) continue
      this.queued.add(g.id)
      this.queue.push(g)
    }
    this.pump()
  }

  private pump(): void {
    while (this.running < this.concurrency && this.queue.length) {
      const g = this.queue.shift()!
      this.running++
      fetchCover(g)
        .then((ok) => this.onDone(g.id, ok))
        .catch(() => this.onDone(g.id, false))
        .finally(() => {
          this.running--
          this.pump()
        })
    }
  }
}
