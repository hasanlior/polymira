import { net } from 'electron'
import type { Game, GameCompat } from '@shared/types'
import { BRAND } from '@shared/brand'

/**
 * Per-game compatibility from the emulator projects' own public lists:
 *  - PS3: RPCS3 compatibility database (rpcs3.net API, by Title ID)
 *  - PS4: shadPS4 game compatibility repository (GitHub issues titled "CUSAxxxxx - Name",
 *    status in labels)
 * Results are cached on the game for a week.
 */

const WEEK = 7 * 24 * 3600 * 1000

function normalise(s: string): GameCompat['level'] {
  const t = s.toLowerCase()
  if (t.includes('playable')) return 'playable'
  if (t.includes('ingame') || t.includes('in-game')) return 'ingame'
  if (t.includes('intro') || t.includes('menus') || t.includes('menu')) return 'menus'
  if (t.includes('boot') || t.includes('loadable')) return 'boots'
  if (t.includes('nothing') || t.includes('unplayable')) return 'nothing'
  return 'unknown'
}

async function rpcs3(titleId: string): Promise<GameCompat | null> {
  const res = await net.fetch(`https://rpcs3.net/compatibility?api=v1&g=${encodeURIComponent(titleId)}`, { signal: AbortSignal.timeout(15000) })
  if (!res.ok) return null
  const data = (await res.json()) as { return_code?: number; results?: Record<string, { status?: string; date?: string; thread?: number }> }
  // The API can answer with the same game's other regions; prefer the exact Title ID.
  const hit = data.results?.[titleId] ?? Object.values(data.results ?? {})[0]
  if (!hit?.status) return null
  return {
    level: normalise(hit.status),
    label: hit.status,
    source: 'RPCS3',
    url: hit.thread ? `https://forums.rpcs3.net/thread-${hit.thread}.html` : `https://rpcs3.net/compatibility?g=${titleId}`,
    updated: hit.date,
    checkedAt: Date.now()
  }
}

async function shadps4(titleId: string): Promise<GameCompat | null> {
  const q = encodeURIComponent(`repo:shadps4-compatibility/shadps4-game-compatibility ${titleId} in:title`)
  const res = await net.fetch(`https://api.github.com/search/issues?q=${q}&per_page=10`, {
    headers: { 'user-agent': `${BRAND.slug}-app`, accept: 'application/vnd.github+json' },
    signal: AbortSignal.timeout(15000)
  })
  if (!res.ok) return null
  const data = (await res.json()) as { items?: { title: string; html_url: string; updated_at: string; labels: { name: string }[] }[] }
  // Prefer the Windows report when there are several.
  const matches = (data.items ?? []).filter((i) => i.title.toUpperCase().includes(titleId.toUpperCase()))
  const hit = matches.find((i) => i.labels.some((l) => l.name === 'os-windows')) ?? matches[0]
  const status = hit?.labels.map((l) => l.name).find((n) => /^status-/i.test(n))
  if (!hit || !status) return null
  const label = status.replace(/^status-/i, '')
  return { level: normalise(label), label: label.charAt(0).toUpperCase() + label.slice(1), source: 'shadPS4', url: hit.html_url, updated: hit.updated_at.slice(0, 10), checkedAt: Date.now() }
}

/** Looks up compatibility for PS3 and PS4 games with a Title ID. Returns null when there is no entry. */
export async function fetchCompat(game: Game): Promise<GameCompat | null> {
  if (game.compat && Date.now() - game.compat.checkedAt < WEEK) return game.compat
  const id = game.meta.titleId?.toUpperCase()
  if (!id) return null
  try {
    if (game.platformId === 'ps3') return await rpcs3(id)
    if (game.platformId === 'ps4') return await shadps4(id)
  } catch {
    // offline or rate limited: try again next time
  }
  return null
}
