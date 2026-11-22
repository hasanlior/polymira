import fs from 'fs'
import path from 'path'
import type { PlayStats } from '@shared/types'
import { paths, store } from './store'

/** Play sessions log (one entry per finished game or app session) and the summaries built from it. */

interface Entry {
  gameId: string
  /** Session start (ms). */
  start: number
  sec: number
}

const MAX_ENTRIES = 5000
let entries: Entry[] | null = null

function file(): string {
  return path.join(paths.root, 'stats.json')
}

function load(): Entry[] {
  if (entries) return entries
  try {
    entries = JSON.parse(fs.readFileSync(file(), 'utf8')) as Entry[]
    if (!Array.isArray(entries)) entries = []
  } catch {
    entries = []
  }
  return entries
}

export function recordSession(gameId: string, start: number, sec: number): void {
  if (sec < 10) return
  const list = load()
  list.push({ gameId, start, sec: Math.round(sec) })
  if (list.length > MAX_ENTRIES) list.splice(0, list.length - MAX_ENTRIES)
  const tmp = `${file()}.tmp`
  fs.writeFileSync(tmp, JSON.stringify(list))
  fs.renameSync(tmp, file())
}

function dayStart(t: number): number {
  const d = new Date(t)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/** Last 7 days (per day), the most played games, and totals. */
export function playStats(): PlayStats {
  const list = load()
  const today = dayStart(Date.now())
  const DAY = 24 * 3600 * 1000
  const days = Array.from({ length: 7 }, (_, i) => ({ day: today - (6 - i) * DAY, sec: 0 }))
  const weekStart = days[0].day
  const perGameWeek = new Map<string, number>()
  for (const e of list) {
    const d = dayStart(e.start)
    const slot = days.find((x) => x.day === d)
    if (slot) slot.sec += e.sec
    if (e.start >= weekStart) perGameWeek.set(e.gameId, (perGameWeek.get(e.gameId) ?? 0) + e.sec)
  }
  const games = store.games.filter((g) => g.playTimeSec > 0)
  const top = [...games]
    .sort((a, b) => b.playTimeSec - a.playTimeSec)
    .slice(0, 8)
    .map((g) => ({ gameId: g.id, title: g.title, sec: g.playTimeSec, weekSec: perGameWeek.get(g.id) ?? 0, sessions: g.playCount }))
  return {
    days,
    top,
    weekSec: days.reduce((a, d) => a + d.sec, 0),
    totalSec: games.reduce((a, g) => a + g.playTimeSec, 0),
    sessions: list.length,
    gamesPlayed: games.length
  }
}
