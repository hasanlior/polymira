import { net } from 'electron'
import fs from 'fs'
import path from 'path'
import { getPlatform } from '@shared/platforms'
import type { Game } from '@shared/types'
import { paths } from './store'

/**
 * Game artwork: a cover (box art / poster) and a background (shown full-screen when the game
 * is focused, like PIC1 on the PS3). Sources, in order:
 *  - the game's own files (PS3 ICON0/PIC1, PS4/Vita sce_sys images)
 *  - Steam CDN for Steam games, and Steam store search for other PC games with an exact title match
 *  - the Epic launcher's local art cache for Epic games
 *  - libretro-thumbnails (box art + in-game snapshot) for retro platforms
 * No API keys. Requests only contain the game title or store id.
 */

const LIBRETRO = 'https://thumbnails.libretro.com'
const STEAM_CDN = 'https://cdn.cloudflare.steamstatic.com/steam/apps'
const INDEX_TTL_MS = 7 * 24 * 3600 * 1000

export function coverFile(gameId: string): string {
  return path.join(paths.covers, `${gameId}.png`)
}

export function backgroundFile(gameId: string): string {
  return path.join(paths.covers, `${gameId}.bg.jpg`)
}

export function normalizeTitle(s: string): string {
  return s
    .toLowerCase()
    .replace(/\[[^\]]*\]/g, '')
    .replace(/\([^)]*\)/g, '')
    .replace(/[™®©]/g, '')
    .replace(/&/g, 'and')
    .replace(/^the\s+|,\s*the\b/g, '')
    .replace(/[^a-z0-9]+/g, '')
}

async function download(url: string, file: string): Promise<boolean> {
  try {
    const res = await net.fetch(url)
    if (!res.ok) return false
    const buf = Buffer.from(await res.arrayBuffer())
    if (buf.length < 1000) return false
    fs.writeFileSync(file, buf)
    return true
  } catch {
    return false
  }
}

function copyIfExists(src: string, dst: string): boolean {
  try {
    if (!fs.statSync(src).isFile()) return false
    fs.copyFileSync(src, dst)
    return true
  } catch {
    return false
  }
}

export interface ArtResult {
  cover: boolean
  background: boolean
  /** Trailer stream URL ('' = looked up, none found; undefined = not looked up). */
  trailer?: string
  steamAppId?: string
}

// ── Console games: artwork shipped with the game ─────────────────────

function localConsoleArt(game: Game, want: ArtResult): ArtResult {
  const got: ArtResult = { cover: false, background: false }
  const tryPair = (dir: string, icons: string[], bgs: string[]) => {
    if (want.cover && !got.cover) got.cover = icons.some((n) => copyIfExists(path.join(dir, n), coverFile(game.id)))
    if (want.background && !got.background) got.background = bgs.some((n) => copyIfExists(path.join(dir, n), backgroundFile(game.id)))
  }
  if (game.platformId === 'ps3') {
    // .../PS3_GAME/USRDIR/EBOOT.BIN or .../<TITLEID>/USRDIR/EBOOT.BIN
    tryPair(path.resolve(game.path, '..', '..'), ['ICON0.PNG'], ['PIC1.PNG', 'PIC0.PNG'])
  } else if (game.platformId === 'ps4') {
    tryPair(path.join(path.dirname(game.path), 'sce_sys'), ['icon0.png'], ['pic1.png', 'pic0.png'])
  } else if (game.platformId === 'vita') {
    tryPair(path.join(game.path, 'sce_sys'), ['icon0.png'], ['pic0.png', path.join('livearea', 'contents', 'bg.png')])
  }
  return got
}

// ── Steam ────────────────────────────────────────────────────────────

/** The game's featured trailer on the Steam store (an HLS stream on Steam's CDN). */
async function steamTrailer(appid: string): Promise<string> {
  try {
    const res = await net.fetch(`https://store.steampowered.com/api/appdetails?appids=${encodeURIComponent(appid)}&filters=movies`)
    if (!res.ok) return ''
    const data = (await res.json()) as Record<string, { data?: { movies?: { highlight?: boolean; hls_h264?: string }[] } }>
    const movies = data[appid]?.data?.movies ?? []
    const pick = movies.find((m) => m.highlight && m.hls_h264) ?? movies.find((m) => m.hls_h264)
    return pick?.hls_h264 ?? ''
  } catch {
    return ''
  }
}

async function steamArt(appid: string, game: Game, want: ArtResult): Promise<ArtResult> {
  const id = encodeURIComponent(appid)
  return {
    cover:
      want.cover &&
      ((await download(`${STEAM_CDN}/${id}/library_600x900.jpg`, coverFile(game.id))) ||
        (await download(`${STEAM_CDN}/${id}/header.jpg`, coverFile(game.id)))),
    background: want.background && (await download(`${STEAM_CDN}/${id}/library_hero.jpg`, backgroundFile(game.id)))
  }
}

const steamSearchCache = new Map<string, string | null>()

/** Finds a Steam app id for a PC game from another store, only on an exact title match. */
async function steamSearch(title: string): Promise<string | null> {
  const key = normalizeTitle(title)
  if (!key) return null
  if (steamSearchCache.has(key)) return steamSearchCache.get(key)!
  let id: string | null = null
  try {
    const res = await net.fetch(`https://store.steampowered.com/api/storesearch/?term=${encodeURIComponent(title)}&l=english&cc=US`)
    if (res.ok) {
      const data = (await res.json()) as { items?: { id: number; name: string; type?: string }[] }
      const hit = data.items?.find((i) => (i.type ?? 'app') === 'app' && normalizeTitle(i.name) === key)
      id = hit ? String(hit.id) : null
    }
  } catch {
    id = null
  }
  steamSearchCache.set(key, id)
  return id
}

// ── Epic: the launcher's own art cache ───────────────────────────────

interface EpicArt {
  portraitUrl?: string
  landscapeUrl?: string
}

function epicCachedArt(game: Game): EpicArt | null {
  const base = path.join(process.env.LOCALAPPDATA ?? '', 'com.epicgames.eglv2', 'cache', 'prod')
  try {
    const mapping = JSON.parse(fs.readFileSync(path.join(base, 'product-mapping-cache.json'), 'utf8')).mapping as Record<string, string>
    const entries = JSON.parse(fs.readFileSync(path.join(base, 'product-art-cache.json'), 'utf8')).entries as Record<string, { art?: EpicArt }>
    const slug = (game.meta.namespace && mapping[game.meta.namespace]) || (game.meta.storeId && mapping[game.meta.storeId])
    if (!slug) return null
    const key = Object.keys(entries).find((k) => k.endsWith(`:${slug}`))
    return key ? entries[key].art ?? null : null
  } catch {
    return null
  }
}

// ── Retro: libretro-thumbnails ───────────────────────────────────────

const indexCache = new Map<string, Promise<string[]>>()

async function loadIndex(system: string, kind: string): Promise<string[]> {
  const cacheFile = path.join(paths.cache, `thumbs-${kind}-${system.replace(/[^a-z0-9]+/gi, '_')}.json`)
  try {
    const st = fs.statSync(cacheFile)
    if (Date.now() - st.mtimeMs < INDEX_TTL_MS) return JSON.parse(fs.readFileSync(cacheFile, 'utf8'))
  } catch {
    // no cache yet
  }
  const res = await net.fetch(`${LIBRETRO}/${encodeURIComponent(system)}/${kind}/`)
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

function getIndex(system: string, kind: string): Promise<string[]> {
  const k = `${kind}|${system}`
  let p = indexCache.get(k)
  if (!p) {
    p = loadIndex(system, kind).catch((err) => {
      indexCache.delete(k)
      throw err
    })
    indexCache.set(k, p)
  }
  return p
}

function pickMatch(index: string[], game: Game): string | undefined {
  const raw = path.basename(game.path, path.extname(game.path)).replace(/[&*/:`<>?\\|]/g, '_')
  if (index.includes(raw)) return raw
  const wanted = normalizeTitle(game.kind === 'file' ? raw : game.title)
  const hits = index.filter((n) => normalizeTitle(n) === wanted)
  if (hits.length === 0) return undefined
  const region = game.meta.region
  return (region && hits.find((h) => h.includes(region))) || hits.find((h) => /USA|World/.test(h)) || hits[0]
}

async function libretroArt(system: string, game: Game, want: ArtResult): Promise<ArtResult> {
  const got: ArtResult = { cover: false, background: false }
  const fetchKind = async (kind: string, file: string) => {
    const index = await getIndex(system, kind).catch(() => [] as string[])
    const name = pickMatch(index, game)
    return name ? download(`${LIBRETRO}/${encodeURIComponent(system)}/${kind}/${encodeURIComponent(name)}.png`, file) : false
  }
  if (want.cover) got.cover = await fetchKind('Named_Boxarts', coverFile(game.id))
  if (want.background) got.background = (await fetchKind('Named_Snaps', backgroundFile(game.id))) || (await fetchKind('Named_Titles', backgroundFile(game.id)))
  return got
}

/** Fetches whatever artwork `game` is missing. */
export async function fetchArt(game: Game, online: boolean): Promise<ArtResult> {
  const want: ArtResult = { cover: !game.hasCover, background: !game.hasBackground }
  const wantTrailer = game.platformId === 'pc' && game.trailerUrl === undefined
  const got = localConsoleArt(game, want)
  const need = (): ArtResult => ({ cover: want.cover && !got.cover, background: want.background && !got.background })
  const merge = (r: ArtResult) => {
    got.cover ||= r.cover
    got.background ||= r.background
  }
  if (!online || (!need().cover && !need().background && !wantTrailer)) return got

  if (game.platformId === 'pc') {
    if (game.meta.source === 'epic') {
      const art = epicCachedArt(game)
      if (art) {
        const n = need()
        merge({
          cover: n.cover && Boolean(art.portraitUrl) && (await download(art.portraitUrl!, coverFile(game.id))),
          background: n.background && Boolean(art.landscapeUrl) && (await download(art.landscapeUrl!, backgroundFile(game.id)))
        })
      }
    }
    const n = need()
    if (n.cover || n.background || wantTrailer) {
      const appid = game.meta.source === 'steam' ? game.meta.storeId : game.meta.steamAppId ?? (await steamSearch(game.title))
      if (appid) {
        if (n.cover || n.background) merge(await steamArt(appid, game, n))
        got.steamAppId = appid
        if (wantTrailer) got.trailer = await steamTrailer(appid)
      } else if (wantTrailer) {
        got.trailer = ''
      }
    }
    return got
  }

  const system = getPlatform(game.platformId)?.thumbnails
  if (system) merge(await libretroArt(system, game, need()))
  return got
}

/** Small promise queue so a big scan does not open hundreds of connections at once. */
export class ArtQueue {
  private queue: Game[] = []
  private running = 0
  private queued = new Set<string>()

  constructor(
    private onDone: (gameId: string, result: ArtResult) => void,
    private online: () => boolean,
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
      fetchArt(g, this.online())
        .then((r) => this.onDone(g.id, r))
        .catch(() => this.onDone(g.id, { cover: false, background: false }))
        .finally(() => {
          this.running--
          this.queued.delete(g.id)
          this.pump()
        })
    }
  }
}
