import fs from 'fs'
import path from 'path'
import { paths } from './store'

/**
 * Runtime state shared with the watchdog process (see watchdog.ts).
 * It tells the watchdog whether the last exit was intentional and what system changes
 * (desktop shell stopped, power plan switched) must be undone if the app dies.
 */
export interface RuntimeState {
  pid: number
  cleanExit: boolean
  autoRestart: boolean
  relaunch: { exe: string; args: string[]; cwd: string }
  explorerStopped: boolean
  /** Power scheme GUID to restore, when game mode switched it. */
  previousPowerScheme: string | null
  /** Timestamps of automatic restarts, used to stop crash loops. */
  restarts: number[]
}

export function runtimeFile(): string {
  return path.join(paths.root, 'runtime.json')
}

let state: RuntimeState | null = null

export function readRuntime(): RuntimeState | null {
  try {
    return JSON.parse(fs.readFileSync(runtimeFile(), 'utf8')) as RuntimeState
  } catch {
    return null
  }
}

export function initRuntime(init: Omit<RuntimeState, 'restarts'>): RuntimeState {
  const prev = readRuntime()
  state = { ...init, restarts: (prev?.restarts ?? []).filter((t) => Date.now() - t < 10 * 60 * 1000) }
  write()
  return state
}

export function updateRuntime(patch: Partial<RuntimeState>): void {
  if (!state) return
  state = { ...state, ...patch }
  write()
}

export function getRuntime(): RuntimeState | null {
  return state
}

function write(): void {
  try {
    fs.writeFileSync(runtimeFile(), JSON.stringify(state, null, 2))
  } catch (err) {
    console.warn('[runtime] write failed', err)
  }
}
