import { shell } from 'electron'
import fs from 'fs'
import os from 'os'
import { paths } from './store'
import path from 'path'
import { EMULATORS, getEmulator, getPlatform, resolveRunner, runnerLabel } from '@shared/platforms'
import type { EmulatorStatus, Game, Settings } from '@shared/types'

function isFile(p: string): boolean {
  try {
    return fs.statSync(p).isFile()
  } catch {
    return false
  }
}

export function emulatorStatus(settings: Settings): EmulatorStatus[] {
  return EMULATORS.map((e) => {
    const p = settings.emulatorPaths[e.id] ?? null
    return { id: e.id, name: e.name, path: p, found: p ? isFile(p) : false }
  })
}

function commonRoots(): string[] {
  const env = process.env
  const roots = [
    env.ProgramFiles,
    env['ProgramFiles(x86)'],
    env.LOCALAPPDATA && path.join(env.LOCALAPPDATA, 'Programs'),
    env.APPDATA,
    env.USERPROFILE && path.join(env.USERPROFILE, 'scoop', 'apps'),
    env.USERPROFILE && path.join(env.USERPROFILE, 'Emulators'),
    env.USERPROFILE && path.join(env.USERPROFILE, 'Desktop'),
    env.USERPROFILE && path.join(env.USERPROFILE, 'Downloads'),
    'C:\\Emulators',
    'C:\\Games',
    'C:\\RetroArch-Win64',
    'D:\\Emulators',
    'D:\\Games'
  ]
  if (env.ProgramFiles) roots.push(path.join(env.ProgramFiles, 'Steam', 'steamapps', 'common'))
  if (env['ProgramFiles(x86)']) roots.push(path.join(env['ProgramFiles(x86)']!, 'Steam', 'steamapps', 'common'))
  return roots.filter((r): r is string => Boolean(r) && fs.existsSync(r as string))
}

/**
 * Looks for known emulator executables. Only fills in emulators that are not configured yet,
 * so manual choices are never overwritten.
 */
export function detectEmulators(settings: Settings, extraRoots: string[] = []): Record<string, string> {
  const wanted = new Map<string, string>() // exe name (lower) → emulator id
  for (const e of EMULATORS) {
    const current = settings.emulatorPaths[e.id]
    if (current && isFile(current)) continue
    for (const n of e.exeNames) wanted.set(n.toLowerCase(), e.id)
  }
  const found: Record<string, string> = {}
  if (wanted.size === 0) return found

  const maxDepth = 3
  const walk = (dir: string, depth: number) => {
    let entries: fs.Dirent[]
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true })
    } catch {
      return
    }
    for (const ent of entries) {
      if (ent.isFile()) {
        const id = wanted.get(ent.name.toLowerCase())
        if (id && !found[id]) found[id] = path.join(dir, ent.name)
      }
    }
    if (depth >= maxDepth) return
    for (const ent of entries) {
      if (ent.isDirectory() && !ent.name.startsWith('.') && !/^(windows|\$recycle\.bin)$/i.test(ent.name)) {
        walk(path.join(dir, ent.name), depth + 1)
      }
    }
  }
  for (const root of [...extraRoots, ...commonRoots()]) walk(root, 0)
  return found
}

export type LaunchPlan =
  | { type: 'spawn'; exe: string; args: string[]; cwd: string; label: string; verbatim?: boolean }
  /** URL shortcuts (e.g. store launchers) are handed to the shell and cannot be tracked. */
  | { type: 'url'; url: string; label: string }

export class LaunchError extends Error {}

/** Validates everything a launch needs and produces the exact command line. */
export function planLaunch(game: Game, settings: Settings): LaunchPlan {
  const platform = getPlatform(game.platformId)
  if (!platform) throw new LaunchError('Bilinmeyen platform.')

  if (game.kind === 'uri') return { type: 'url', url: game.path, label: game.meta.source ?? 'PC' }

  const target = game.kind === 'titleId' ? game.meta.titleId ?? path.basename(game.path) : game.path
  if (game.kind !== 'titleId' && !fs.existsSync(game.path)) {
    throw new LaunchError('Oyun dosyası bulunamadı. Taşınmış ya da silinmiş olabilir.')
  }

  if (platform.id === 'pc') {
    const ext = path.extname(game.path).toLowerCase()
    if (ext === '.lnk') {
      const link = shell.readShortcutLink(game.path)
      if (!link.target || !isFile(link.target)) throw new LaunchError('Kısayolun hedefi bulunamadı.')
      return {
        type: 'spawn',
        exe: link.target,
        args: link.args ? [link.args] : [],
        verbatim: true,
        cwd: link.cwd || path.dirname(link.target),
        label: 'PC'
      }
    }
    if (ext === '.url') {
      const m = fs.readFileSync(game.path, 'utf8').match(/^URL=(.+)$/m)
      if (!m) throw new LaunchError('Geçersiz internet kısayolu.')
      return { type: 'url', url: m[1].trim(), label: 'PC' }
    }
    return { type: 'spawn', exe: game.path, args: [], cwd: path.dirname(game.path), label: 'PC' }
  }

  if (platform.support === 'unsupported' || platform.runners.length === 0) {
    throw new LaunchError(platform.note ?? 'Bu platform için çalışan bir emülatör yok.')
  }

  const runner = resolveRunner(platform, game.emulator ?? settings.platformEmulator[platform.id])!
  const emu = getEmulator(runner.emulatorId)!
  const exe = settings.emulatorPaths[emu.id]
  if (!exe) throw new LaunchError(`${emu.name} ayarlanmamış. Ayarlar → Emülasyon bölümünden yolunu seçin.`)
  if (!isFile(exe)) throw new LaunchError(`${emu.name} bulunamadı: ${exe}`)

  let core = ''
  if (runner.core) {
    core = path.join(path.dirname(exe), 'cores', `${runner.core}_libretro.dll`)
    if (!isFile(core)) {
      throw new LaunchError(
        `RetroArch çekirdeği eksik: ${runner.core}. RetroArch → Online Updater → Core Downloader ile yükleyin.`
      )
    }
  }

  const args = emu.args.map((a) => a.replace('{rom}', target).replace('{core}', core))
  const extra: string[] = []
  if (emu.id === 'retroarch') extra.push(...retroArchExtras(game, exe))
  if (game.emuOptions?.extraArgs) extra.push(...splitArgs(game.emuOptions.extraArgs))
  // Extra options go before the game path (emulators read the last argument as the game).
  const romAt = args.lastIndexOf(target)
  if (romAt >= 0) args.splice(romAt, 0, ...extra)
  else args.push(...extra)
  return { type: 'spawn', exe, args, cwd: path.dirname(exe), label: runnerLabel(runner) }
}

/** Splits a command line string, keeping "quoted parts" together. */
export function splitArgs(s: string): string[] {
  return (s.match(/"[^"]*"|\S+/g) ?? []).map((a) => a.replace(/^"|"$/g, ''))
}

export const RETROARCH_CMD_PORT = 55355

const SHADERS: Record<string, string[]> = {
  crt: ['shaders_slang/crt/crt-easymode.slangp', 'shaders_glsl/crt/crt-easymode.glslp', 'shaders_slang/crt/crt-geom.slangp', 'shaders_glsl/crt/crt-geom.glslp'],
  lcd: ['shaders_slang/handheld/lcd-grid-v2.slangp', 'shaders_glsl/handheld/lcd-grid-v2.glslp', 'shaders_slang/handheld/lcd3x.slangp'],
  smooth: ['shaders_slang/interpolation/sharp-bilinear-simple.slangp', 'shaders_glsl/interpolation/sharp-bilinear-simple.glslp']
}

/**
 * RetroArch: enable its local command port (used for quick save / load from the in-game menu)
 * through an append config, plus the game's shader choice.
 */
function retroArchExtras(game: Game, exe: string): string[] {
  const dir = path.dirname(exe)
  const cfgDir = path.join(paths.cache, 'retroarch')
  fs.mkdirSync(cfgDir, { recursive: true })
  const cfg = path.join(cfgDir, 'append.cfg')
  fs.writeFileSync(cfg, `network_cmd_enable = "true"\nnetwork_cmd_port = "${RETROARCH_CMD_PORT}"\nsavestate_auto_index = "false"\n`)
  const out = [`--appendconfig=${cfg}`]
  const shader = game.emuOptions?.shader
  if (shader && shader !== 'none') {
    const found = SHADERS[shader]?.map((rel) => path.join(dir, 'shaders', rel)).find((p) => fs.existsSync(p))
    if (found) out.push('--set-shader', found)
    else console.warn('[retroarch] shader not installed:', shader)
  }
  return out
}

export function homeDir(): string {
  return os.homedir()
}
