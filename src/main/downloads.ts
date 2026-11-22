import { net } from 'electron'
import { execFile } from 'child_process'
import { EventEmitter } from 'events'
import fs from 'fs'
import path from 'path'
import { EMULATORS, PLATFORMS } from '@shared/platforms'
import type { DownloadJob } from '@shared/types'
import { BRAND } from '@shared/brand'
import { paths } from './store'

/**
 * Downloads official emulator builds in the background, with progress, speed and time left.
 * Sources are the projects' own GitHub releases (or their official build server).
 * Emulators only: no BIOS, firmware, keys or games are ever downloaded.
 */

type Source = { github: string; asset: RegExp } | { url: string }

export const EMULATOR_SOURCES: Record<string, Source> = {
  retroarch: { url: 'https://buildbot.libretro.com/nightly/windows/x86_64/RetroArch.7z' },
  duckstation: { github: 'stenzek/duckstation', asset: /windows-x64-release\.zip$/i },
  pcsx2: { github: 'PCSX2/pcsx2', asset: /windows-x64-Qt\.7z$/i },
  ppsspp: { github: 'hrydgard/ppsspp', asset: /Windows-x64\.zip$/i },
  rpcs3: { github: 'RPCS3/rpcs3-binaries-win', asset: /win64(_msvc)?\.7z$/i },
  vita3k: { github: 'Vita3K/Vita3K', asset: /^windows-latest\.zip$/i },
  shadps4: { github: 'shadps4-emu/shadPS4', asset: /win64.*\.zip$/i },
  xemu: { github: 'xemu-project/xemu', asset: /^xemu-[\d.]+-windows-x86_64\.zip$/i },
  xenia: { github: 'xenia-canary/xenia-canary-releases', asset: /xenia_canary_windows\.zip$/i },
  cemu: { github: 'cemu-project/Cemu', asset: /windows-x64\.zip$/i },
  melonds: { github: 'melonDS-emu/melonDS', asset: /windows-x86_64\.zip$/i },
  azahar: { github: 'azahar-emu/azahar', asset: /windows-msvc-[^-]+\.zip$/i },
  flycast: { github: 'flyinghead/flycast', asset: /win64.*\.zip$/i }
}

const CORE_URL = 'https://buildbot.libretro.com/nightly/windows/x86_64/latest'

/** RetroArch cores used by the platform definitions. */
export function neededCores(): string[] {
  return [...new Set(PLATFORMS.flatMap((p) => p.runners.filter((r) => r.emulatorId === 'retroarch' && r.core).map((r) => r.core!)))]
}

function emulatorDir(id: string): string {
  return path.join(paths.root, 'emulators', id)
}

async function resolveGithub(repo: string, asset: RegExp): Promise<{ url: string; size: number; version: string }> {
  const headers = { 'User-Agent': `${BRAND.slug}-app`, Accept: 'application/vnd.github+json' }
  const pick = (rel: { tag_name: string; assets: { name: string; browser_download_url: string; size: number }[] }) => {
    const a = rel.assets.find((x) => asset.test(x.name))
    return a ? { url: a.browser_download_url, size: a.size, version: rel.tag_name } : null
  }
  // Prefer the latest stable release; fall back to the newest release that has a Windows build.
  const latest = await net.fetch(`https://api.github.com/repos/${repo}/releases/latest`, { headers })
  if (latest.ok) {
    const hit = pick(await latest.json())
    if (hit) return hit
  }
  const list = await net.fetch(`https://api.github.com/repos/${repo}/releases?per_page=10`, { headers })
  if (!list.ok) throw new Error(`GitHub yanıt vermedi (${list.status}).`)
  for (const rel of (await list.json()) as Parameters<typeof pick>[0][]) {
    const hit = pick(rel)
    if (hit) return hit
  }
  throw new Error('Windows sürümü bulunamadı.')
}

function extract(archive: string, dest: string): Promise<void> {
  fs.mkdirSync(dest, { recursive: true })
  // Windows 10/11 ship bsdtar, which reads .zip (and .7z on recent Windows 11 builds).
  // Always Windows' own bsdtar: another tar on PATH (e.g. Git's GNU tar) cannot handle C:\ paths.
  const tar = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe')
  return new Promise((resolve, reject) =>
    execFile(fs.existsSync(tar) ? tar : 'tar', ['-xf', archive, '-C', dest], { windowsHide: true, maxBuffer: 16 * 1024 * 1024 }, (err, _o, stderr) =>
      err
        ? reject(new Error(archive.endsWith('.7z') ? 'Bu Windows sürümü .7z arşivini açamıyor. 7-Zip ile elle açabilirsiniz.' : String(stderr || err.message).trim()))
        : resolve()
    )
  )
}

function findFile(dir: string, names: string[], depth = 4): string | null {
  const wanted = new Set(names.map((n) => n.toLowerCase()))
  const walk = (d: string, level: number): string | null => {
    let entries: fs.Dirent[]
    try {
      entries = fs.readdirSync(d, { withFileTypes: true })
    } catch {
      return null
    }
    for (const e of entries) if (e.isFile() && wanted.has(e.name.toLowerCase())) return path.join(d, e.name)
    if (level >= depth) return null
    for (const e of entries) {
      if (e.isDirectory()) {
        const hit = walk(path.join(d, e.name), level + 1)
        if (hit) return hit
      }
    }
    return null
  }
  return walk(dir, 0)
}

export class DownloadManager extends EventEmitter {
  jobs: DownloadJob[] = []
  private aborts = new Map<string, AbortController>()
  private running = false

  /** Called with (emulatorId, exePath) when an emulator is ready to use. */
  constructor(private onInstalled: (id: string, exe: string) => void) {
    super()
  }

  private update(id: string, patch: Partial<DownloadJob>): void {
    const j = this.jobs.find((x) => x.id === id)
    if (!j) return
    Object.assign(j, patch)
    this.emit('changed')
  }

  enqueue(ids: string[]): number {
    let n = 0
    for (const id of ids) {
      if (!EMULATOR_SOURCES[id]) continue
      const existing = this.jobs.find((j) => j.id === id)
      if (existing && (existing.status === 'queued' || existing.status === 'downloading' || existing.status === 'extracting')) continue
      this.jobs = this.jobs.filter((j) => j.id !== id)
      const name = EMULATORS.find((e) => e.id === id)?.name ?? id
      this.jobs.push({ id, name, status: 'queued', received: 0, total: 0, speed: 0, etaSec: null })
      n++
    }
    this.emit('changed')
    void this.pump()
    return n
  }

  cancel(id: string): void {
    this.aborts.get(id)?.abort()
    const j = this.jobs.find((x) => x.id === id)
    if (j?.status === 'queued') this.update(id, { status: 'error', error: 'İptal edildi' })
  }

  get active(): boolean {
    return this.jobs.some((j) => j.status === 'queued' || j.status === 'downloading' || j.status === 'extracting')
  }

  private async pump(): Promise<void> {
    if (this.running) return
    this.running = true
    try {
      for (;;) {
        const job = this.jobs.find((j) => j.status === 'queued')
        if (!job) break
        try {
          await this.install(job.id)
        } catch (err) {
          const aborted = (err as Error).name === 'AbortError'
          this.update(job.id, { status: 'error', error: aborted ? 'İptal edildi' : (err as Error).message, etaSec: null, speed: 0 })
        }
      }
    } finally {
      this.running = false
    }
  }

  /** Streams `url` to `file`, reporting progress. */
  private async fetchTo(id: string, url: string, file: string, sizeHint: number, signal: AbortSignal, countToJob = true): Promise<void> {
    const res = await net.fetch(url, { signal } as RequestInit)
    if (!res.ok || !res.body) throw new Error(`İndirme başarısız (HTTP ${res.status}).`)
    const total = Number(res.headers.get('content-length')) || sizeHint
    if (countToJob) this.update(id, { total })
    const out = fs.createWriteStream(file)
    const reader = res.body.getReader()
    let received = 0
    let windowStart = Date.now()
    let windowBytes = 0
    let speed = 0
    let lastEmit = 0
    try {
      for (;;) {
        const { done, value } = await reader.read()
        if (done) break
        received += value.length
        windowBytes += value.length
        if (!out.write(value)) await new Promise((r) => out.once('drain', r))
        const now = Date.now()
        if (now - windowStart >= 1000) {
          const inst = (windowBytes * 1000) / (now - windowStart)
          speed = speed ? speed * 0.7 + inst * 0.3 : inst
          windowStart = now
          windowBytes = 0
        }
        if (countToJob && now - lastEmit > 250) {
          lastEmit = now
          this.update(id, { received, speed, etaSec: speed > 0 && total ? Math.round((total - received) / speed) : null })
        }
      }
    } finally {
      await new Promise<void>((r) => out.end(() => r()))
    }
    if (countToJob) this.update(id, { received, total: total || received })
  }

  private async install(id: string): Promise<void> {
    const src = EMULATOR_SOURCES[id]
    const emu = EMULATORS.find((e) => e.id === id)!
    const abort = new AbortController()
    this.aborts.set(id, abort)
    this.update(id, { status: 'downloading', error: undefined })
    try {
      let url: string
      let size = 0
      if ('github' in src) {
        const r = await resolveGithub(src.github, src.asset)
        url = r.url
        size = r.size
        this.update(id, { version: r.version })
      } else {
        url = src.url
      }
      const tmpDir = path.join(paths.root, 'downloads')
      fs.mkdirSync(tmpDir, { recursive: true })
      const ext = url.toLowerCase().endsWith('.7z') ? '.7z' : '.zip'
      const archive = path.join(tmpDir, `${id}${ext}`)
      await this.fetchTo(id, url, archive, size, abort.signal)

      this.update(id, { status: 'extracting', etaSec: null, speed: 0 })
      const dest = emulatorDir(id)
      fs.rmSync(dest, { recursive: true, force: true })
      await extract(archive, dest)
      fs.rmSync(archive, { force: true })
      const exe = findFile(dest, emu.exeNames)
      if (!exe) throw new Error('Arşivde çalıştırılabilir dosya bulunamadı.')

      if (id === 'retroarch') {
        // RetroArch is only a shell: fetch the cores the platforms use.
        const coresDir = path.join(path.dirname(exe), 'cores')
        fs.mkdirSync(coresDir, { recursive: true })
        const cores = neededCores()
        for (let i = 0; i < cores.length; i++) {
          this.update(id, { detail: `Çekirdekler ${i + 1}/${cores.length}: ${cores[i]}` })
          const zip = path.join(tmpDir, `${cores[i]}.zip`)
          try {
            await this.fetchTo(id, `${CORE_URL}/${cores[i]}_libretro.dll.zip`, zip, 0, abort.signal, false)
            await extract(zip, coresDir)
          } catch (err) {
            if ((err as Error).name === 'AbortError') throw err
            // A missing core only affects its platform; keep going.
          } finally {
            fs.rmSync(zip, { force: true })
          }
        }
      }
      this.update(id, { status: 'done', detail: undefined, etaSec: null, speed: 0 })
      this.onInstalled(id, exe)
    } finally {
      this.aborts.delete(id)
    }
  }
}
