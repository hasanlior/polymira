import { spawn, execFile, type ChildProcess } from 'child_process'
import { EventEmitter } from 'events'
import os from 'os'
import type { Game, LaunchResult, PerfSnapshot, SessionInfo } from '@shared/types'
import type { LaunchPlan } from './emulators'
import path from 'path'
import {
  focusProcessWindow,
  listProcesses,
  processAlive,
  processCpuTimeMs,
  processWorkingSetMb,
  resumeProcess,
  suspendProcess
} from './native/win32'

interface Running {
  game: Game
  /** Null for games started by a store launcher (tracked by polling instead). */
  child: ChildProcess | null
  info: SessionInfo
  pausedAt: number | null
  stoppedByUser: boolean
  lastCpu: { at: number; ms: number } | null
}

export interface SessionEnd {
  gameId: string
  playedSec: number
  result: LaunchResult
  message?: string
}

/**
 * Owns the one running game. Emits:
 *  'started' (SessionInfo), 'changed' (SessionInfo | null), 'ended' (SessionEnd)
 */
export class SessionManager extends EventEmitter {
  private cur: Running | null = null

  get info(): SessionInfo | null {
    return this.cur ? { ...this.cur.info } : null
  }

  get game(): Game | null {
    return this.cur?.game ?? null
  }

  launch(game: Game, plan: Extract<LaunchPlan, { type: 'spawn' }>): Promise<SessionInfo> {
    if (this.cur) return Promise.reject(new Error('Zaten çalışan bir oyun var.'))
    return new Promise((resolve, reject) => {
      const child = spawn(plan.exe, plan.args, {
        cwd: plan.cwd,
        detached: false,
        windowsHide: false,
        windowsVerbatimArguments: plan.verbatim,
        stdio: 'ignore'
      })
      let settled = false
      child.once('error', (err) => {
        if (!settled) {
          settled = true
          reject(new Error(`Başlatılamadı: ${err.message}`))
        }
      })
      child.once('spawn', () => {
        settled = true
        const info: SessionInfo = { gameId: game.id, pid: child.pid!, startedAt: Date.now(), paused: false, pausedMs: 0 }
        this.cur = { game, child, info, pausedAt: null, stoppedByUser: false, lastCpu: null }
        child.once('exit', (code) => this.finish(code))
        this.emit('started', { ...info })
        this.emit('changed', this.info)
        resolve({ ...info })
      })
    })
  }

  /**
   * Store games (Steam, Epic…) are started by their launcher, not by us. Wait for a process
   * running from the game's install folder and track that one, so the in-game menu,
   * pausing, game mode and play time work for them too.
   */
  private attachCancelled = false
  /** Stops waiting for a launcher-started game (the user went back). */
  cancelAttach(): void {
    this.attachCancelled = true
  }

  async attachByFolder(game: Game, installDir: string, timeoutMs = 120000): Promise<SessionInfo | null> {
    if (this.cur) throw new Error('Zaten çalışan bir oyun var.')
    this.attachCancelled = false
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
      if (this.cur || this.attachCancelled) return null
      const pid = findGameProcess(installDir)
      if (pid) {
        const info: SessionInfo = { gameId: game.id, pid, startedAt: Date.now(), paused: false, pausedMs: 0 }
        this.cur = { game, child: null, info, pausedAt: null, stoppedByUser: false, lastCpu: null }
        this.watchDetached(installDir)
        this.emit('started', { ...info })
        this.emit('changed', this.info)
        return { ...info }
      }
      await sleep(1500)
    }
    return null
  }

  private watchDetached(installDir: string): void {
    const tick = async () => {
      const cur = this.cur
      if (!cur || cur.child) return
      if (!processAlive(cur.info.pid)) {
        // Launchers often hand over to the real game executable: give it a moment.
        let next: number | null = null
        if (!cur.stoppedByUser) {
          for (let i = 0; i < 5 && !next; i++) {
            await sleep(1500)
            next = findGameProcess(installDir)
          }
        }
        if (next && this.cur === cur) {
          cur.info.pid = next
          this.emit('changed', this.info)
        } else {
          if (this.cur === cur) this.finish(0)
          return
        }
      }
      setTimeout(tick, 2000)
    }
    setTimeout(tick, 2000)
  }

  private finish(code: number | null): void {
    const cur = this.cur
    if (!cur) return
    this.cur = null
    const now = Date.now()
    const pausedMs = cur.info.pausedMs + (cur.pausedAt ? now - cur.pausedAt : 0)
    const elapsed = now - cur.info.startedAt
    const playedSec = Math.max(0, Math.round((elapsed - pausedMs) / 1000))

    let result: LaunchResult = 'ok'
    let message: string | undefined
    if (!cur.stoppedByUser && code !== 0 && code !== null && elapsed < 15000) {
      result = 'crashed'
      message = `Emülatör hemen kapandı (çıkış kodu ${code}). Sistem dosyalarını ve emülatör ayarlarını kontrol edin.`
    }
    this.emit('ended', { gameId: cur.game.id, playedSec, result, message } satisfies SessionEnd)
    this.emit('changed', null)
  }

  setPaused(paused: boolean): boolean {
    const cur = this.cur
    if (!cur || cur.info.paused === paused) return Boolean(cur)
    const ok = paused ? suspendProcess(cur.info.pid) : resumeProcess(cur.info.pid)
    if (!ok) return false
    if (paused) {
      cur.pausedAt = Date.now()
    } else if (cur.pausedAt) {
      cur.info.pausedMs += Date.now() - cur.pausedAt
      cur.pausedAt = null
    }
    cur.info.paused = paused
    this.emit('changed', this.info)
    return true
  }

  focusGame(): boolean {
    return this.cur ? focusProcessWindow([this.cur.info.pid]) : false
  }

  /** Closes the game and its child processes. */
  stop(): Promise<void> {
    const cur = this.cur
    if (!cur) return Promise.resolve()
    cur.stoppedByUser = true
    // A suspended process cannot shut down: resume first.
    if (cur.info.paused) resumeProcess(cur.info.pid)
    return new Promise((resolve) => {
      execFile('taskkill', ['/PID', String(cur.info.pid), '/T', '/F'], { windowsHide: true }, () => resolve())
    })
  }

  private sysCpuPrev = os.cpus()

  private sysCpu(): number {
    const now = os.cpus()
    let idle = 0
    let total = 0
    now.forEach((c, i) => {
      const p = this.sysCpuPrev[i]
      if (!p) return
      const sum = (t: os.CpuInfo['times']) => t.user + t.nice + t.sys + t.idle + t.irq
      total += sum(c.times) - sum(p.times)
      idle += c.times.idle - p.times.idle
    })
    this.sysCpuPrev = now
    return total > 0 ? Math.round((1 - idle / total) * 100) : 0
  }

  async perf(): Promise<PerfSnapshot> {
    const cur = this.cur
    let gameCpu: number | null = null
    let gameMemMb: number | null = null
    if (cur) {
      const ms = processCpuTimeMs(cur.info.pid)
      const at = Date.now()
      if (ms !== null && cur.lastCpu) {
        const wall = at - cur.lastCpu.at
        if (wall > 0) gameCpu = Math.round(((ms - cur.lastCpu.ms) / wall / os.cpus().length) * 100)
      }
      if (ms !== null) cur.lastCpu = { at, ms }
      gameMemMb = processWorkingSetMb(cur.info.pid)
    }
    const total = os.totalmem() / 1048576
    const free = os.freemem() / 1048576
    const sessionSec = cur
      ? Math.round((Date.now() - cur.info.startedAt - cur.info.pausedMs - (cur.pausedAt ? Date.now() - cur.pausedAt : 0)) / 1000)
      : 0
    return {
      gameCpu,
      gameMemMb: gameMemMb === null ? null : Math.round(gameMemMb),
      sysCpu: this.sysCpu(),
      sysMemUsedMb: Math.round(total - free),
      sysMemTotalMb: Math.round(total),
      gpu: await readGpu(),
      sessionSec
    }
  }
}

let nvidiaSmi: boolean | null = null

/** GPU stats are only available through nvidia-smi for now; other vendors report null. */
function readGpu(): Promise<PerfSnapshot['gpu']> {
  if (nvidiaSmi === false) return Promise.resolve(null)
  return new Promise((resolve) => {
    execFile(
      'nvidia-smi',
      ['--query-gpu=utilization.gpu,memory.used,name', '--format=csv,noheader,nounits'],
      { timeout: 1500, windowsHide: true },
      (err, stdout) => {
        if (err) {
          nvidiaSmi = false
          return resolve(null)
        }
        nvidiaSmi = true
        const [util, mem, ...name] = stdout.trim().split('\n')[0].split(',').map((s) => s.trim())
        resolve({ util: Number(util), memUsedMb: Number(mem), name: name.join(',') })
      }
    )
  })
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const HELPERS = /crash|report|unins|setup|redist|easyanticheat|battleye|launcherhelper|cefprocess|webhelper/i

function findGameProcess(installDir: string): number | null {
  const prefix = path.resolve(installDir).toLowerCase() + path.sep
  const hits = listProcesses().filter((p) => p.exe.toLowerCase().startsWith(prefix) && !HELPERS.test(path.basename(p.exe)))
  if (!hits.length) return null
  // Prefer the process using the most memory: that is the game, not its bootstrapper.
  let best = hits[0]
  let bestMem = -1
  for (const h of hits) {
    const mem = processWorkingSetMb(h.pid) ?? 0
    if (mem > bestMem) {
      best = h
      bestMem = mem
    }
  }
  return best.pid
}
