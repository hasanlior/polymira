import { net } from 'electron'
import fs from 'fs'
import path from 'path'
import { getPlatform } from '@shared/platforms'
import type { Game, GameMeta } from '@shared/types'
import { normalizeTitle } from './covers'
import { paths, store } from './store'

/**
 * Game information page data from free sources that need no account:
 *  - PC games with a Steam app id: the Steam store's public app details
 *  - console games: libretro-database metadata (year, genre, developer) + a Wikipedia summary
 */

const UA = { 'user-agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36' }
const DB = 'https://raw.githubusercontent.com/libretro/libretro-database/master/metadat'

export interface InfoResult {
  meta: Partial<GameMeta>
  screenshots: string[]
  source: string
}

async function json<T>(url: string): Promise<T | null> {
  try {
    const res = await net.fetch(url, { headers: UA, signal: AbortSignal.timeout(15000) })
    return res.ok ? ((await res.json()) as T) : null
  } catch {
    return null
  }
}

function stripHtml(s: string): string {
  return s
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/<[^>]+>/g, '')
    .replace(/&quot;/g, '"')
    .replace(/&amp;/g, '&')
    .replace(/&#39;/g, "'")
    .replace(/\n{3,}/g, '\n\n')
    .trim()
}

// ── Steam ────────────────────────────────────────────────────────────

async function steamInfo(appId: string, lang: string): Promise<InfoResult | null> {
  type R = Record<string, { success: boolean; data?: { short_description?: string; release_date?: { date?: string }; genres?: { description: string }[]; developers?: string[]; publishers?: string[]; screenshots?: { path_full: string }[] } }>
  const r = await json<R>(`https://store.steampowered.com/api/appdetails?appids=${appId}&l=${lang === 'en' ? 'english' : 'turkish'}`)
  const d = r?.[appId]?.data
  if (!d) return null
  const year = Number(d.release_date?.date?.match(/\b(19|20)\d{2}\b/)?.[0])
  return {
    meta: {
      description: d.short_description ? stripHtml(d.short_description) : undefined,
      year: year || undefined,
      genre: d.genres?.map((g) => g.description).slice(0, 3).join(', ') || undefined,
      developer: d.developers?.join(', ') || undefined,
      publisher: d.publishers?.join(', ') || undefined
    },
    screenshots: (d.screenshots ?? []).slice(0, 8).map((s) => s.path_full),
    source: 'Steam'
  }
}

// ── libretro-database metadata ───────────────────────────────────────

const DAT_TTL = 14 * 24 * 3600 * 1000
const datCache = new Map<string, Promise<Map<string, string>>>()

/** Parses a clrmamepro .dat of one field (genre / releaseyear / developer) into title → value. */
async function loadDat(kind: string, system: string): Promise<Map<string, string>> {
  const file = path.join(paths.cache, 'metadat', `${kind}-${system.replace(/[^a-z0-9]+/gi, '_')}.dat`)
  let text: string | null = null
  try {
    if (Date.now() - fs.statSync(file).mtimeMs < DAT_TTL) text = fs.readFileSync(file, 'utf8')
  } catch {
    // not cached
  }
  if (!text) {
    try {
      const res = await net.fetch(`${DB}/${kind}/${encodeURIComponent(system)}.dat`, { headers: UA, signal: AbortSignal.timeout(30000) })
      text = res.ok ? await res.text() : ''
      fs.mkdirSync(path.dirname(file), { recursive: true })
      fs.writeFileSync(file, text)
    } catch {
      text = ''
    }
  }
  const out = new Map<string, string>()
  const field = kind === 'releaseyear' ? 'releaseyear' : kind
  for (const block of text.split(/\ngame \(/)) {
    const name = block.match(/(?:comment|name) "([^"]+)"/)?.[1]
    const value = block.match(new RegExp(`\\b${field} "?([^"\\n)]+)"?`))?.[1]?.trim()
    if (name && value) out.set(normalizeTitle(name), value)
  }
  return out
}

function dat(kind: string, system: string): Promise<Map<string, string>> {
  const k = `${kind}|${system}`
  let p = datCache.get(k)
  if (!p) {
    p = loadDat(kind, system)
    datCache.set(k, p)
  }
  return p
}

async function libretroInfo(game: Game): Promise<Partial<GameMeta>> {
  const system = getPlatform(game.platformId)?.thumbnails
  if (!system) return {}
  const keys = [normalizeTitle(path.basename(game.path).replace(/\.[^.]+$/, '')), normalizeTitle(game.title)]
  const find = (m: Map<string, string>) => keys.map((k) => m.get(k)).find(Boolean)
  const [genre, year, developer] = await Promise.all([dat('genre', system), dat('releaseyear', system), dat('developer', system)])
  const y = Number(find(year))
  return { genre: find(genre), year: y || undefined, developer: find(developer) }
}

// ── Wikipedia ────────────────────────────────────────────────────────

async function wikipedia(title: string, platform: string, lang: string): Promise<string | undefined> {
  const tries = lang === 'en' ? [['en', `${title} ${platform} video game`]] : [['tr', `${title} video oyunu`], ['en', `${title} ${platform} video game`]]
  for (const [wl, q] of tries) {
    type S = { query?: { search?: { title: string }[] } }
    const s = await json<S>(`https://${wl}.wikipedia.org/w/api.php?action=query&list=search&format=json&srlimit=3&srsearch=${encodeURIComponent(q)}`)
    const want = normalizeTitle(title)
    const hit = s?.query?.search?.find((r) => normalizeTitle(r.title.replace(/\s*\(.*\)$/, '')).startsWith(want.slice(0, Math.max(4, want.length - 2))))
    if (!hit) continue
    const sum = await json<{ extract?: string; type?: string }>(`https://${wl}.wikipedia.org/api/rest_v1/page/summary/${encodeURIComponent(hit.title.replace(/ /g, '_'))}`)
    const text = sum?.extract?.trim()
    if (text && sum?.type !== 'disambiguation' && /(oyun|game)/i.test(text)) return text
  }
  return undefined
}

/** Looks up information for one game. Fields the user or a scan already filled are kept. */
export async function fetchGameInfo(game: Game): Promise<InfoResult> {
  const lang = store.settings.language ?? 'tr'
  const appId = game.meta.steamAppId ?? (game.meta.source === 'steam' ? game.meta.storeId : undefined)
  if (appId) {
    const s = await steamInfo(appId, lang)
    if (s) return s
  }
  const meta: Partial<GameMeta> = game.platformId === 'pc' || game.platformId === 'apps' ? {} : await libretroInfo(game)
  const platformName = getPlatform(game.platformId)?.name ?? ''
  if (game.platformId !== 'apps') meta.description = await wikipedia(game.title, platformName, lang)
  return { meta, screenshots: [], source: meta.description ? 'Wikipedia' : 'libretro' }
}
