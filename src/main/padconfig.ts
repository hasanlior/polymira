import { app } from 'electron'
import fs from 'fs'
import path from 'path'
import { store } from './store'

/**
 * Automatic controller setup for emulators, so a game starts ready to play with an
 * Xbox / PlayStation / generic pad and nobody has to open the emulator's own settings.
 *
 * - Emulators that bind pads on their own: RetroArch (autoconfig), PPSSPP, Vita3K, shadPS4,
 *   xemu, Xenia, Flycast — nothing to write.
 * - DuckStation, PCSX2, Dolphin (GameCube), RPCS3: their pad settings are written before
 *   the first launch. Done once per emulator, so later changes the user makes are kept
 *   (Settings → "Kol ayarlarını yeniden uygula" writes them again).
 * - Cemu, Ryujinx, melonDS, Azahar need a device-specific id in their files; they are
 *   reported as manual.
 */

export type PadSupport = 'auto' | 'written' | 'manual'

export const PAD_SUPPORT: Record<string, PadSupport> = {
  retroarch: 'auto',
  ppsspp: 'auto',
  vita3k: 'auto',
  shadps4: 'auto',
  xemu: 'auto',
  xenia: 'auto',
  flycast: 'auto',
  duckstation: 'written',
  pcsx2: 'written',
  dolphin: 'written',
  rpcs3: 'written',
  cemu: 'manual',
  ryujinx: 'manual',
  melonds: 'manual',
  azahar: 'manual'
}

/** Sets keys inside one [section] of an INI file, keeping everything else as it is. */
export function setIni(file: string, section: string, entries: Record<string, string>): void {
  let text = ''
  try {
    text = fs.readFileSync(file, 'utf8')
  } catch {
    // new file
  }
  const lines = text.split(/\r?\n/)
  let start = lines.findIndex((l) => l.trim().toLowerCase() === `[${section.toLowerCase()}]`)
  if (start < 0) {
    if (lines.length && lines[lines.length - 1].trim() !== '') lines.push('')
    lines.push(`[${section}]`)
    start = lines.length - 1
  }
  let end = lines.findIndex((l, i) => i > start && /^\s*\[.*\]\s*$/.test(l))
  if (end < 0) end = lines.length
  const pending = new Map(Object.entries(entries))
  for (let i = start + 1; i < end; i++) {
    const m = lines[i].match(/^\s*([^=;#]+?)\s*=/)
    if (m && pending.has(m[1])) {
      lines[i] = `${m[1]} = ${pending.get(m[1])}`
      pending.delete(m[1])
    }
  }
  const add = [...pending].map(([k, v]) => `${k} = ${v}`)
  lines.splice(end, 0, ...add)
  fs.mkdirSync(path.dirname(file), { recursive: true })
  fs.writeFileSync(file, lines.join('\n'))
}

const docs = () => app.getPath('documents')

/** DualShock-style layout on an SDL (XInput, DualShock, DualSense…) pad, as DuckStation/PCSX2 name it. */
function sdlPsPad(n = 0): Record<string, string> {
  const p = `SDL-${n}/`
  return {
    Up: `${p}DPadUp`,
    Down: `${p}DPadDown`,
    Left: `${p}DPadLeft`,
    Right: `${p}DPadRight`,
    Cross: `${p}A`,
    Circle: `${p}B`,
    Square: `${p}X`,
    Triangle: `${p}Y`,
    L1: `${p}LeftShoulder`,
    R1: `${p}RightShoulder`,
    L2: `${p}+LeftTrigger`,
    R2: `${p}+RightTrigger`,
    L3: `${p}LeftStick`,
    R3: `${p}RightStick`,
    Select: `${p}Back`,
    Start: `${p}Start`,
    LUp: `${p}-LeftY`,
    LDown: `${p}+LeftY`,
    LLeft: `${p}-LeftX`,
    LRight: `${p}+LeftX`,
    RUp: `${p}-RightY`,
    RDown: `${p}+RightY`,
    RLeft: `${p}-RightX`,
    RRight: `${p}+RightX`,
    SmallMotor: `${p}SmallMotor`,
    LargeMotor: `${p}LargeMotor`
  }
}

function duckstation(dir: string): void {
  const base = fs.existsSync(path.join(dir, 'portable.txt')) || fs.existsSync(path.join(dir, 'settings.ini')) ? dir : path.join(docs(), 'DuckStation')
  const ini = path.join(base, 'settings.ini')
  setIni(ini, 'InputSources', { SDL: 'true', XInput: 'false' })
  setIni(ini, 'Pad1', { Type: 'AnalogController', ...sdlPsPad(0) })
  setIni(ini, 'Main', { SetupWizardIncomplete: 'false', ConfirmPowerOff: 'false' })
}

function pcsx2(dir: string): void {
  const portable = fs.existsSync(path.join(dir, 'portable.ini')) || fs.existsSync(path.join(dir, 'portable.txt'))
  const ini = path.join(portable ? dir : path.join(docs(), 'PCSX2'), 'inis', 'PCSX2.ini')
  const pad = sdlPsPad(0)
  setIni(ini, 'InputSources', { SDL: 'true', XInput: 'false' })
  setIni(ini, 'Pad1', { Type: 'DualShock2', ...pad })
  setIni(ini, 'UI', { SetupWizardIncomplete: 'false', ConfirmShutdown: 'false' })
}

function dolphin(dir: string): void {
  const user = fs.existsSync(path.join(dir, 'portable.txt')) ? path.join(dir, 'User') : path.join(docs(), 'Dolphin Emulator')
  const cfg = path.join(user, 'Config')
  setIni(path.join(cfg, 'GCPadNew.ini'), 'GCPad1', {
    Device: 'XInput/0/Gamepad',
    'Buttons/A': '`Button A`',
    'Buttons/B': '`Button B`',
    'Buttons/X': '`Button X`',
    'Buttons/Y': '`Button Y`',
    'Buttons/Z': '`Shoulder R`',
    'Buttons/Start': 'Start',
    'Main Stick/Up': '`Left Y+`',
    'Main Stick/Down': '`Left Y-`',
    'Main Stick/Left': '`Left X-`',
    'Main Stick/Right': '`Left X+`',
    'C-Stick/Up': '`Right Y+`',
    'C-Stick/Down': '`Right Y-`',
    'C-Stick/Left': '`Right X-`',
    'C-Stick/Right': '`Right X+`',
    'Triggers/L': '`Trigger L`',
    'Triggers/R': '`Trigger R`',
    'Triggers/L-Analog': '`Trigger L`',
    'Triggers/R-Analog': '`Trigger R`',
    'D-Pad/Up': '`Pad N`',
    'D-Pad/Down': '`Pad S`',
    'D-Pad/Left': '`Pad W`',
    'D-Pad/Right': '`Pad E`',
    'Rumble/Motor': '`Motor L` | `Motor R`'
  })
  setIni(path.join(cfg, 'Dolphin.ini'), 'Core', { SIDevice0: '6' })
  setIni(path.join(cfg, 'Dolphin.ini'), 'Interface', { ConfirmStop: 'False' })
}

function rpcs3(dir: string): void {
  const base = fs.existsSync(path.join(dir, 'config')) ? path.join(dir, 'config') : dir
  const file = path.join(base, 'input_configs', 'global', 'Default.yml')
  let text = ''
  try {
    text = fs.readFileSync(file, 'utf8')
  } catch {
    // not created yet
  }
  if (!text) {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, 'Player 1 Input:\n  Handler: XInput\n  Device: XInput Pad #0\n  Buddy Device: ""\n')
    return
  }
  // Only switch player 1 from the keyboard default; a pad the user picked is left alone.
  const next = text.replace(/(Player 1 Input:\s*\n\s*Handler:\s*)Keyboard(\s*\n\s*Device:\s*)Keyboard/, '$1XInput$2XInput Pad #0')
  if (next !== text) fs.writeFileSync(file, next)
}

const WRITERS: Record<string, (dir: string) => void> = { duckstation, pcsx2, dolphin, rpcs3 }

// ── Borderless full screen ────────────────────────────────────────────
// The in-game menu can only draw over games in borderless (not exclusive) full screen.
// Only the display keys below are written; the rest of the emulator's settings stay as they are.

const DISPLAY_WRITERS: Record<string, (dir: string) => void> = {
  duckstation(dir) {
    const base = fs.existsSync(path.join(dir, 'portable.txt')) || fs.existsSync(path.join(dir, 'settings.ini')) ? dir : path.join(docs(), 'DuckStation')
    setIni(path.join(base, 'settings.ini'), 'GPU', { FullscreenMode: '' })
  },
  pcsx2(dir) {
    const portable = fs.existsSync(path.join(dir, 'portable.ini')) || fs.existsSync(path.join(dir, 'portable.txt'))
    setIni(path.join(portable ? dir : path.join(docs(), 'PCSX2'), 'inis', 'PCSX2.ini'), 'EmuCore/GS', { FullscreenMode: '' })
  },
  dolphin(dir) {
    const user = fs.existsSync(path.join(dir, 'portable.txt')) ? path.join(dir, 'User') : path.join(docs(), 'Dolphin Emulator')
    setIni(path.join(user, 'Config', 'GFX.ini'), 'Settings', { BorderlessFullscreen: 'True' })
  }
}

/** Switches an emulator to borderless full screen, once per emulator install. */
export function ensureBorderless(emuId: string, exe: string): void {
  if (!store.settings.emuBorderless) return
  const write = DISPLAY_WRITERS[emuId]
  if (!write) return
  const done = store.settings.displayConfigured ?? {}
  if (done[emuId] === exe) return
  try {
    write(path.dirname(exe))
    store.updateSettings({ displayConfigured: { ...done, [emuId]: exe } })
    console.log('[display] borderless full screen set for', emuId)
  } catch (err) {
    console.warn('[display] could not write display settings for', emuId, (err as Error).message)
  }
}

/**
 * Called right before an emulator starts. Writes its controller settings the first time
 * (or again after the user asked to re-apply them).
 */
export function ensurePadConfig(emuId: string, exe: string): void {
  if (!store.settings.autoPadConfig) return
  const write = WRITERS[emuId]
  if (!write) return
  const done = store.settings.padConfigured ?? {}
  if (done[emuId] === exe) return
  try {
    write(path.dirname(exe))
    store.updateSettings({ padConfigured: { ...done, [emuId]: exe } })
    console.log('[pad] controller settings written for', emuId)
  } catch (err) {
    console.warn('[pad] could not write controller settings for', emuId, (err as Error).message)
  }
}
