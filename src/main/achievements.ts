import { net } from 'electron'
import fs from 'fs'
import path from 'path'
import type { Game, Trophy, TrophyGame } from '@shared/types'
import { steamLibraries } from './launchers'
import { store } from './store'

/**
 * Trophies (achievements) from two sources that work without scraping:
 *  - Steam: the achievement schema and the user's unlocks that the Steam client keeps
 *    on disk (appcache/stats), so it works offline and needs no API key
 *  - RetroAchievements (emulated games): the user's own RetroAchievements account
 *    (username + web API key from retroachievements.org → Settings)
 */

// ── Steam binary KeyValues ───────────────────────────────────────────

type KV = { [k: string]: KV | string | number }

function parseKv(b: Buffer, start = 0): [KV, number] {
  const out: KV = {}
  let i = start
  while (i < b.length) {
    const t = b[i++]
    if (t === 8) return [out, i]
    const end = b.indexOf(0, i)
    const name = b.toString('utf8', i, end)
    i = end + 1
    let v: KV | string | number
    if (t === 0) [v, i] = parseKv(b, i)
    else if (t === 1) {
      const e = b.indexOf(0, i)
      v = b.toString('utf8', i, e)
      i = e + 1
    } else if (t === 2) {
      v = b.readInt32LE(i)
      i += 4
    } else if (t === 3) {
      v = b.readFloatLE(i)
      i += 4
    } else if (t === 7 || t === 10) {
      v = Number(b.readBigInt64LE(i))
      i += 8
    } else break
    out[name] = v
  }
  return [out, i]
}

function readKv(file: string): KV | null {
  try {
    return parseKv(fs.readFileSync(file))[0]
  } catch {
    return null
  }
}

let steamRoot: string | null | undefined
async function steamStatsDir(): Promise<string | null> {
  if (steamRoot === undefined) {
    const libs = await steamLibraries()
    steamRoot = libs[0] ? path.dirname(libs[0]) : null
  }
  const dir = steamRoot ? path.join(steamRoot, 'appcache', 'stats') : null
  return dir && fs.existsSync(dir) ? dir : null
}

function steamAppIdOf(g: Game): string | undefined {
  return g.meta.steamAppId ?? (g.meta.source === 'steam' ? g.meta.storeId : undefined)
}

function userStatsFile(dir: string, appId: string): string | null {
  const files = fs
    .readdirSync(dir)
    .filter((f) => f.startsWith('UserGameStats_') && f.endsWith(`_${appId}.bin`))
    .map((f) => path.join(dir, f))
    .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)
  return files[0] ?? null
}

function text(v: KV | string | number | undefined, lang: string): string {
  if (!v) return ''
  if (typeof v !== 'object') return String(v)
  return String(v[lang === 'en' ? 'english' : 'turkish'] ?? v.english ?? Object.values(v)[0] ?? '')
}

export async function steamTrophies(appId: string): Promise<Trophy[]> {
  const dir = await steamStatsDir()
  if (!dir) return []
  const schema = readKv(path.join(dir, `UserGameStatsSchema_${appId}.bin`))
  const root = schema?.[appId] as KV | undefined
  const stats = root?.stats as KV | undefined
  if (!stats) return []
  const uf = userStatsFile(dir, appId)
  const user = uf ? ((readKv(uf)?.cache as KV | undefined) ?? {}) : {}
  const lang = store.settings.language
  const out: Trophy[] = []
  for (const [statId, stat] of Object.entries(stats)) {
    const bits = (stat as KV).bits as KV | undefined
    if (!bits) continue
    const u = user[statId] as KV | undefined
    const data = typeof u?.data === 'number' ? u.data : 0
    const times = (u?.AchievementTimes as KV | undefined) ?? {}
    for (const [bit, a] of Object.entries(bits)) {
      const ach = a as KV
      const display = (ach.display as KV) ?? {}
      const unlocked = ((data >>> Number(bit)) & 1) === 1
      const icon = String(unlocked ? display.icon ?? '' : display.icon_gray ?? display.icon ?? '')
      out.push({
        id: String(ach.name ?? `${statId}.${bit}`),
        title: text(display.name, lang),
        description: text(display.desc, lang),
        icon: icon ? `https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps/${appId}/${icon}` : undefined,
        unlocked,
        unlockedAt: typeof times[bit] === 'number' ? (times[bit] as number) * 1000 : undefined,
        hidden: Number(display.hidden ?? 0) === 1
      })
    }
  }
  return out
}

// ── RetroAchievements ────────────────────────────────────────────────

const RA = 'https://retroachievements.org/API'

function raAuth(): { u: string; y: string } | null {
  const { raUser, raKey } = store.settings
  return raUser && raKey ? { u: raUser, y: raKey } : null
}

async function raGet<T>(endpoint: string, params: Record<string, string>): Promise<T | null> {
  const auth = raAuth()
  if (!auth) return null
  const q = new URLSearchParams({ ...params, ...auth })
  try {
    const res = await net.fetch(`${RA}/${endpoint}?${q}`, { signal: AbortSignal.timeout(15000) })
    return res.ok ? ((await res.json()) as T) : null
  } catch {
    return null
  }
}

const norm = (s: string) => s.toLowerCase().replace(/\(.*?\)|\[.*?\]/g, '').replace(/[^a-z0-9]+/g, '')

async function raGames(): Promise<TrophyGame[]> {
  type R = { Results?: { GameID: number; Title: string; ConsoleName: string; MaxPossible: number; NumAwarded: number; ImageIcon: string }[] }
  const r = await raGet<R>('API_GetUserCompletionProgress.php', { c: '500' })
  const games = store.games
  return (r?.Results ?? []).map((x) => ({
    key: `ra:${x.GameID}`,
    source: 'ra' as const,
    title: x.Title,
    subtitle: x.ConsoleName,
    gameId: games.find((g) => g.platformId !== 'pc' && norm(g.title) === norm(x.Title))?.id,
    unlocked: x.NumAwarded,
    total: x.MaxPossible,
    icon: `https://media.retroachievements.org${x.ImageIcon}`
  }))
}

async function raTrophies(gameId: string): Promise<Trophy[]> {
  type A = { ID: number; Title: string; Description: string; BadgeName: string; DateEarned?: string; DateEarnedHardcore?: string }
  const r = await raGet<{ Achievements?: Record<string, A> }>('API_GetGameInfoAndUserProgress.php', { g: gameId })
  return Object.values(r?.Achievements ?? {}).map((a) => {
    const earned = a.DateEarnedHardcore || a.DateEarned
    return {
      id: String(a.ID),
      title: a.Title,
      description: a.Description,
      icon: `https://media.retroachievements.org/Badge/${a.BadgeName}${earned ? '' : '_lock'}.png`,
      unlocked: Boolean(earned),
      unlockedAt: earned ? Date.parse(earned.replace(' ', 'T') + 'Z') : undefined
    }
  })
}

// ── Public API ───────────────────────────────────────────────────────

/** Every game with trophies: Steam games in the library + games played on RetroAchievements. */
export async function trophyGames(): Promise<TrophyGame[]> {
  const out: TrophyGame[] = []
  const dir = await steamStatsDir()
  if (dir) {
    for (const g of store.games) {
      const appId = steamAppIdOf(g)
      if (!appId || !fs.existsSync(path.join(dir, `UserGameStatsSchema_${appId}.bin`))) continue
      const list = await steamTrophies(appId)
      if (!list.length) continue
      out.push({ key: `steam:${appId}`, source: 'steam', title: g.title, subtitle: 'Steam', gameId: g.id, unlocked: list.filter((t) => t.unlocked).length, total: list.length })
    }
  }
  out.push(...(await raGames()))
  return out.sort((a, b) => b.unlocked / Math.max(1, b.total) - a.unlocked / Math.max(1, a.total))
}

export async function trophyList(key: string): Promise<Trophy[]> {
  const [src, id] = key.split(':')
  const list = src === 'steam' ? await steamTrophies(id) : src === 'ra' ? await raTrophies(id) : []
  return list.sort((a, b) => Number(b.unlocked) - Number(a.unlocked) || (b.unlockedAt ?? 0) - (a.unlockedAt ?? 0))
}

/**
 * Watches for trophies unlocked during a play session and reports them (for notifications).
 * Steam: the client rewrites the stats file on unlock. RetroAchievements: recent unlocks API.
 */
export function watchTrophies(game: Game, onUnlock: (t: Trophy) => void): () => void {
  let stopped = false
  const appId = steamAppIdOf(game)
  const seen = new Set<string>()
  let timer: NodeJS.Timeout | null = null

  const init = async () => {
    if (appId) for (const t of await steamTrophies(appId)) if (t.unlocked) seen.add(t.id)
  }
  const tick = async () => {
    if (stopped) return
    try {
      if (appId) {
        for (const t of await steamTrophies(appId)) {
          if (t.unlocked && !seen.has(t.id)) {
            seen.add(t.id)
            onUnlock(t)
          }
        }
      } else if (raAuth() && game.platformId !== 'pc') {
        type R = { AchievementID: number; Title: string; Description: string; BadgeName: string; Date: string; GameTitle: string }[]
        const recent = await raGet<R>('API_GetUserRecentAchievements.php', { m: '3' })
        for (const a of recent ?? []) {
          const id = String(a.AchievementID)
          if (seen.has(id)) continue
          seen.add(id)
          onUnlock({ id, title: a.Title, description: a.Description, icon: `https://media.retroachievements.org/Badge/${a.BadgeName}.png`, unlocked: true })
        }
      }
    } catch {
      // try again next tick
    }
    if (!stopped) timer = setTimeout(() => void tick(), appId ? 10000 : 45000)
  }
  void init().then(() => {
    if (!stopped) timer = setTimeout(() => void tick(), 10000)
  })
  return () => {
    stopped = true
    if (timer) clearTimeout(timer)
  }
}
