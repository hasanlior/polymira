import { app } from 'electron'
import { execFile, spawn } from 'child_process'
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
  if (json.error?.startsWith('pkg:')) {
    const [, contentId, state] = json.error.split(':')
    throw new Error(
      state === 'encrypted'
        ? `PS4 paketi (${contentId}): oyun dosyaları paketin şifreli dosya sisteminde. Paketi oluşturduğun oyun klasörünü (eboot.bin içeren) ekle ya da paketi shadPS4 ile kurup çıkan klasörü ekle.`
        : `PS4 paketi (${contentId}): şifresiz paketler henüz okunmuyor; oyun klasörünü ekle.`
    )
  }
  if (json.error) throw new Error(json.error)
  return json
}

/** Launches the game in a live POLYMIRA Orbis window (Vulkan / RTX) without a forced timeout. */
export async function tryRunGame(game: Game): Promise<{ code: number; log: string }> {
  const exe = orbisExe()
  if (!exe) throw new Error('Orbis bu sürümde bulunamadı.')
  const file = executableOf(game)
  if (!file) throw new Error('Oyunun çalıştırılabilir dosyası (eboot.bin) bulunamadı.')
  // Start it the way a terminal does: its own visible window, the log in a file (a pipe slows
  // the game down, since Orbis logs every frame), the game folder as working directory.
  const dataDir = path.join(app.getPath('userData'), 'orbis')
  fs.mkdirSync(dataDir, { recursive: true })
  const logFile = path.join(dataDir, 'orbis.log')
  const logFd = fs.openSync(logFile, 'w')
  const child = spawn(exe, ['run', file], {
    env: { ...process.env, ORBIS_TITLE: game.title, ORBIS_DATA: dataDir },
    cwd: path.dirname(file),
    detached: true,
    stdio: ['ignore', logFd, logFd],
  })
  fs.closeSync(logFd)
  const readLog = (): string => {
    try {
      return fs.readFileSync(logFile, 'utf8').split(/\r?\n/).filter(Boolean).slice(0, 120).join('\n')
    } catch {
      return ''
    }
  }
  return new Promise((resolve) => {
    let settled = false
    const finish = (code: number) => {
      if (settled) return
      settled = true
      child.unref()
      resolve({ code, log: readLog() || 'POLYMIRA Orbis penceresi açıldı.' })
    }
    child.on('exit', (c) => finish(typeof c === 'number' ? c : 0))
    child.on('error', () => finish(1))
    setTimeout(() => finish(0), 4000)
  })
}

export async function orbisSelftest(): Promise<{ code: number; log: string }> {
  const r = await run(['selftest'], 20000)
  return { code: typeof r.code === 'number' ? r.code : 1, log: (r.out + r.err).trim() }
}
