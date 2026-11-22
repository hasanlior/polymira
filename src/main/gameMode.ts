import { execFile, spawn } from 'child_process'
import os from 'os'
import { getRuntime, updateRuntime } from './runtime'
import { store } from './store'

/**
 * Game mode: optional system tweaks applied while a game runs and undone afterwards.
 *  - higher CPU priority for the game process
 *  - High performance power plan
 *  - stopping the Windows desktop shell (explorer.exe) so nothing but the game runs
 *
 * Every change is recorded in runtime.json first, so the watchdog can undo it
 * even if the app crashes mid-game.
 */

const HIGH_PERFORMANCE = '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c'
/** Windows' Balanced and Power saver plans. */
const LIMITED_PLANS = new Set(['381b4222-f694-41f0-9685-ff5bb260df2e', 'a1841308-3541-4fab-bc81-f71556f20b4a'])

function run(cmd: string, args: string[]): Promise<{ ok: boolean; out: string }> {
  return new Promise((resolve) =>
    execFile(cmd, args, { windowsHide: true }, (err, out) => resolve({ ok: !err, out: String(out ?? '') }))
  )
}

async function activeScheme(): Promise<string | null> {
  const { out } = await run('powercfg', ['/getactivescheme'])
  return out.match(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i)?.[0] ?? null
}

async function explorerRunning(): Promise<boolean> {
  const { out } = await run('tasklist', ['/FI', 'IMAGENAME eq explorer.exe', '/NH'])
  return /explorer\.exe/i.test(out)
}

export function startExplorer(): void {
  spawn('explorer.exe', [], { detached: true, stdio: 'ignore' }).unref()
}

export async function enterGameMode(pid: number): Promise<string[]> {
  const s = store.settings
  const applied: string[] = []

  if (s.gmPriority) {
    try {
      os.setPriority(pid, os.constants.priority.PRIORITY_HIGH)
      applied.push('yüksek öncelik')
    } catch {
      // the process may not allow it (e.g. elevated games)
    }
  }

  if (s.gmPowerPlan) {
    const prev = await activeScheme()
    // Only upgrade from the power-limited plans. A custom or "Ultimate Performance" / vendor
    // gaming plan is already at least as fast, so it is left alone.
    if (prev && LIMITED_PLANS.has(prev.toLowerCase())) {
      updateRuntime({ previousPowerScheme: prev })
      const { ok } = await run('powercfg', ['/setactive', HIGH_PERFORMANCE])
      if (ok) applied.push('yüksek performans güç planı')
      else updateRuntime({ previousPowerScheme: null })
    }
  }

  if (s.gmStopExplorer && (await explorerRunning())) {
    updateRuntime({ explorerStopped: true })
    const { ok } = await run('taskkill', ['/F', '/IM', 'explorer.exe'])
    if (ok) applied.push('masaüstü kapatıldı')
    else updateRuntime({ explorerStopped: false })
  }
  return applied
}

/** Undo everything enterGameMode did. Safe to call more than once. */
export async function exitGameMode(): Promise<void> {
  const rt = getRuntime()
  if (!rt) return
  if (rt.explorerStopped) {
    if (!(await explorerRunning())) startExplorer()
    updateRuntime({ explorerStopped: false })
  }
  if (rt.previousPowerScheme) {
    await run('powercfg', ['/setactive', rt.previousPowerScheme])
    updateRuntime({ previousPowerScheme: null })
  }
}
