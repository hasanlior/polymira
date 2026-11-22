/**
 * Watchdog: a tiny Node process (Electron run as Node) that outlives the app.
 *
 * When the app process disappears without a clean exit it:
 *  1. restores the Windows desktop shell if game mode had stopped it,
 *  2. restores the previous power plan,
 *  3. relaunches the app (unless it is crash-looping).
 *
 * Usage: <electron> watchdog.js <runtime.json>
 * Kept dependency-free on purpose: it must keep working when everything else has failed.
 */
import { execFile, spawn } from 'child_process'
import fs from 'fs'

const file = process.argv[2]
const MAX_RESTARTS = 3
const WINDOW_MS = 2 * 60 * 1000

interface RuntimeState {
  pid: number
  cleanExit: boolean
  autoRestart: boolean
  relaunch: { exe: string; args: string[]; cwd: string }
  explorerStopped: boolean
  previousPowerScheme: string | null
  restarts: number[]
}

function read(): RuntimeState | null {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'))
  } catch {
    return null
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === 'EPERM'
  }
}

function run(cmd: string, args: string[]): Promise<string> {
  return new Promise((resolve) => execFile(cmd, args, { windowsHide: true }, (_e, out) => resolve(String(out ?? ''))))
}

async function explorerRunning(): Promise<boolean> {
  const out = await run('tasklist', ['/FI', 'IMAGENAME eq explorer.exe', '/NH'])
  return /explorer\.exe/i.test(out)
}

async function restoreSystem(s: RuntimeState): Promise<void> {
  if (s.explorerStopped && !(await explorerRunning())) {
    spawn('explorer.exe', [], { detached: true, stdio: 'ignore' }).unref()
  }
  if (s.previousPowerScheme) {
    await run('powercfg', ['/setactive', s.previousPowerScheme])
  }
}

async function onParentGone(): Promise<void> {
  // Give the app a moment to write its final state on a normal quit.
  await new Promise((r) => setTimeout(r, 800))
  const s = read()
  if (!s) return process.exit(0)
  await restoreSystem(s)
  if (s.cleanExit || !s.autoRestart) return process.exit(0)

  const recent = s.restarts.filter((t) => Date.now() - t < WINDOW_MS)
  if (recent.length >= MAX_RESTARTS) return process.exit(0) // crash loop: give up

  s.restarts = [...recent, Date.now()]
  s.explorerStopped = false
  s.previousPowerScheme = null
  try {
    fs.writeFileSync(file, JSON.stringify(s, null, 2))
  } catch {
    // ignore
  }
  const env: NodeJS.ProcessEnv = { ...process.env, APP_RESTARTED: '1' }
  delete env.ELECTRON_RUN_AS_NODE
  spawn(s.relaunch.exe, s.relaunch.args, { cwd: s.relaunch.cwd, detached: true, stdio: 'ignore', env }).unref()
  process.exit(0)
}

const initial = read()
if (!file || !initial) process.exit(0)
const parentPid = initial!.pid

const timer = setInterval(() => {
  if (!alive(parentPid)) {
    clearInterval(timer)
    void onParentGone()
  }
}, 1000)
