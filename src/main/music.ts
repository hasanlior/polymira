import fs from 'fs'
import path from 'path'
import { normalizeTitle } from './covers'
import { steamLibraries } from './launchers'
import { store } from './store'

/**
 * Theme music: games have no standard "menu music" file, so music comes from
 *  - soundtracks the user owns on Steam (installed to steamapps/music/<Soundtrack name>)
 *  - a file the user picks per game (game options → "Tema müziği seç")
 */

const AUDIO = /\.(mp3|ogg|flac|wav|m4a|opus)$/i

function firstAudio(dir: string, depth = 2): string | null {
  let entries: fs.Dirent[]
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true })
  } catch {
    return null
  }
  const files = entries
    .filter((e) => e.isFile() && AUDIO.test(e.name))
    .map((e) => e.name)
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
  if (files.length) return path.join(dir, files[0])
  if (depth <= 0) return null
  for (const e of entries) {
    if (!e.isDirectory()) continue
    const hit = firstAudio(path.join(dir, e.name), depth - 1)
    if (hit) return hit
  }
  return null
}

/** Assigns installed Steam soundtracks to matching games that have no music yet. Returns how many were set. */
export async function attachSoundtracks(): Promise<number> {
  const soundtracks: { key: string; dir: string }[] = []
  for (const lib of await steamLibraries()) {
    const musicDir = path.join(lib, 'music')
    let names: string[]
    try {
      names = fs.readdirSync(musicDir)
    } catch {
      continue
    }
    for (const n of names) soundtracks.push({ key: normalizeTitle(n), dir: path.join(musicDir, n) })
  }
  if (!soundtracks.length) return 0

  let set = 0
  for (const g of store.games) {
    if (g.musicPath) continue
    const key = normalizeTitle(g.title)
    if (key.length < 3) continue
    const st = soundtracks.find((s) => s.key.startsWith(key))
    const file = st && firstAudio(st.dir)
    if (file) {
      store.updateGame(g.id, { musicPath: file })
      set++
    }
  }
  return set
}
