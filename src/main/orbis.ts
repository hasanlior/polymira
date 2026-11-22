import { execFile } from 'child_process'
import fs from 'fs'
import path from 'path'
import type { Game, OrbisReport } from '@shared/types'

/**
 * POLYMIRA Orbis: our own experimental PS4 executable loader (Rust, emu/orbis).
 * The app ships orbis.exe in resources/bin (unpacked from the asar archive in installed builds).
 */

export function orbisExe(): string | null {
  const p = path.join(__dirname, '../../resources/bin/orbis.exe').replace(`app.asar${path.sep}`, `app.asar.unpacked${path.sep}`)
  return fs.existsSync(p) ? p : null
}

/** The executable of a PS4/PS5 game: eboot.bin in a game folder, or the file itself. */
function executableOf(game: Game): string | null {
  const candidates = fs.existsSync(game.path) && fs.statSync(game.path).isDirectory()
    ? [path.join(game.path, 'eboot.bin'), path.join(game.path, 'EBOOT.BIN')]
    : [game.path]
  return candidates.find((p) => fs.existsSync(p)) ?? null
}

function run(args: string[], timeoutMs: number): Promise<{ code: number; out: string; err: string }> {
  const exe = orbisExe()
  if (!exe) return Promise.reject(new Error('Orbis bu sürümde bulunamadı.'))
  return new Promise((resolve) =>
    execFile(exe, args, { windowsHide: true, timeout: timeoutMs, maxBuffer: 1 << 22 }, (err, out, stderr) =>
      resolve({ code: err ? (typeof err.code === 'number' ? err.code : 1) : 0, out: String(out), err: String(stderr) })
    )
  )
}

export async function analyzeGame(game: Game): Promise<OrbisReport> {
  const file = executableOf(game)
  if (!file) throw new Error('Oyunun çalıştırılabilir dosyası (eboot.bin) bulunamadı.')
  const r = await run(['analyze', file, '--json'], 30000)
  const json = JSON.parse(r.out.trim().split('\n').pop() || '{}') as OrbisReport & { error?: string }
  if (json.error) throw new Error(json.error)
  return json
}

/** Starts the game in Orbis and returns what it printed (first lines). */
export async function tryRunGame(game: Game): Promise<{ code: number; log: string }> {
  const file = executableOf(game)
  if (!file) throw new Error('Oyunun çalıştırılabilir dosyası (eboot.bin) bulunamadı.')
  const r = await run(['run', file], 20000)
  const log = (r.out + r.err).split(/\r?\n/).filter(Boolean).slice(0, 120).join('\n')
  return { code: typeof r.code === 'number' ? r.code : 1, log }
}

export async function orbisSelftest(): Promise<{ code: number; log: string }> {
  const r = await run(['selftest'], 20000)
  return { code: typeof r.code === 'number' ? r.code : 1, log: (r.out + r.err).trim() }
}
