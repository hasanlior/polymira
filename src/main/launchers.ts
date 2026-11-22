import { execFile } from 'child_process'
import fs from 'fs'
import path from 'path'
import zlib from 'zlib'
import type { Candidate } from './scanner'

/**
 * Finds PC games installed through store launchers (Steam, Epic Games, GOG).
 * Everything is read from local files / the registry; nothing is sent online.
 */

function regQuery(key: string, recursive = false): Promise<string> {
  return new Promise((resolve) => {
    const args = ['query', key]
    if (recursive) args.push('/s')
    execFile('reg', args, { windowsHide: true, maxBuffer: 8 * 1024 * 1024 }, (err, stdout) => resolve(err ? '' : stdout))
  })
}

function regValue(output: string, name: string): string | undefined {
  const m = output.match(new RegExp(`^\\s+${name}\\s+REG_\\w+\\s+(.*)$`, 'mi'))
  return m?.[1].trim()
}

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, 'utf8')
  } catch {
    return null
  }
}

// ── Steam ────────────────────────────────────────────────────────────

/** Tools, runtimes and redistributables that show up as "apps" but are not games. */
const STEAM_IGNORE_IDS = new Set(['480', '228980', '1070560', '1391110', '1628350', '250820', '365670', '223850', '1493710', '2180100'])
const STEAM_IGNORE_NAME = /redistributable|steamworks|proton|steam linux runtime|steamvr|dedicated server|soundtrack|\bsdk\b/i

function vdfField(text: string, key: string): string | undefined {
  const m = text.match(new RegExp(`"${key}"\\s+"([^"]*)"`, 'i'))
  return m?.[1]
}

/** All Steam library `steamapps` folders (Steam lets users add libraries on any drive). */
export async function steamLibraries(): Promise<string[]> {
  const reg = await regQuery('HKCU\\Software\\Valve\\Steam')
  const steamPath = regValue(reg, 'SteamPath')?.replace(/\//g, '\\')
  if (!steamPath || !fs.existsSync(steamPath)) return []
  const libraries = new Set<string>([path.join(steamPath, 'steamapps')])
  const vdf = readText(path.join(steamPath, 'steamapps', 'libraryfolders.vdf'))
  if (vdf) {
    for (const m of vdf.matchAll(/"path"\s+"([^"]+)"/gi)) {
      libraries.add(path.join(m[1].replace(/\\\\/g, '\\'), 'steamapps'))
    }
  }
  return [...libraries]
}

export async function findSteamGames(): Promise<Candidate[]> {
  const libraries = await steamLibraries()
  const out: Candidate[] = []
  const seen = new Set<string>()
  for (const lib of libraries) {
    let files: string[]
    try {
      files = fs.readdirSync(lib).filter((f) => /^appmanifest_\d+\.acf$/i.test(f))
    } catch {
      continue
    }
    for (const f of files) {
      const acf = readText(path.join(lib, f))
      if (!acf) continue
      const appid = vdfField(acf, 'appid')
      const name = vdfField(acf, 'name')
      const installdir = vdfField(acf, 'installdir')
      const flags = Number(vdfField(acf, 'StateFlags') ?? '4')
      if (!appid || !name || seen.has(appid)) continue
      if (STEAM_IGNORE_IDS.has(appid) || STEAM_IGNORE_NAME.test(name)) continue
      if ((flags & 4) === 0) continue // not fully installed
      const dir = installdir ? path.join(lib, 'common', installdir) : undefined
      if (dir && !fs.existsSync(dir)) continue
      seen.add(appid)
      out.push({
        platformId: 'pc',
        title: name,
        path: `steam://rungameid/${appid}`,
        kind: 'uri',
        meta: { source: 'steam', storeId: appid, installDir: dir }
      })
    }
  }
  return out
}

// ── Epic Games ───────────────────────────────────────────────────────
//
// Epic has changed where its launcher keeps data several times, and games can be installed
// anywhere. So games are found from every known source and merged:
//  1. launcher manifests (old launcher: ProgramData, new launcher: LocalAppData\com.epicgames.eglv2)
//  2. LauncherInstalled.dat
//  3. any folder containing `.egstore` inside an "Epic Games" folder on any drive
// The `.egstore` folder exists in every Epic install, whatever launcher version installed it.

interface EpicManifest {
  DisplayName?: string
  InstallLocation?: string
  AppName?: string
  CatalogNamespace?: string
  CatalogItemId?: string
  bIsApplication?: boolean
  bIsIncompleteInstall?: boolean
  AppCategories?: string[]
}

interface EpicInfo {
  title?: string
  appName?: string
  namespace?: string
  itemId?: string
}

function readJson<T>(file: string): T | null {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8').replace(/^\uFEFF/, '')) as T
  } catch {
    return null
  }
}

function listFiles(dir: string, filter: (name: string) => boolean, depth = 0): string[] {
  let entries: fs.Dirent[]
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true })
  } catch {
    return []
  }
  const out: string[] = []
  for (const e of entries) {
    const p = path.join(dir, e.name)
    if (e.isFile() && filter(e.name)) out.push(p)
    else if (e.isDirectory() && depth > 0) out.push(...listFiles(p, filter, depth - 1))
  }
  return out
}

const norm = (p: string) => path.resolve(p).toLowerCase()
const original = new Map<string, string>() // normalized → original path

/** Reads AppName / launch exe from an Epic binary manifest in `.egstore`. */
function readEgstoreManifest(file: string): { appName?: string; launchExe?: string } {
  try {
    const st = fs.statSync(file)
    if (st.size > 64 * 1024 * 1024) return {}
    const b = fs.readFileSync(file)
    if (b.readUInt32LE(0) !== 0x44bec00c) return {}
    const headerSize = b.readUInt32LE(4)
    const compressed = b.readUInt32LE(12)
    let d = b.subarray(headerSize, headerSize + compressed)
    if (b[36] & 1) d = zlib.inflateSync(d)
    let o = 4 + 1 + 4 + 1 + 4 // meta size, data version, feature level, is-file-data, app id
    const fstr = (): string => {
      const n = d.readInt32LE(o)
      o += 4
      if (n === 0) return ''
      if (n > 0) {
        const s = d.toString('latin1', o, o + n - 1)
        o += n
        return s
      }
      const s = d.toString('utf16le', o, o + (-n - 1) * 2)
      o += -n * 2
      return s
    }
    const appName = fstr()
    fstr() // build version
    const launchExe = fstr()
    return { appName: appName || undefined, launchExe: launchExe || undefined }
  } catch {
    return {}
  }
}

/** "AmongUs" → "Among Us", "Rocket_League" → "Rocket League" */
function prettyFolder(name: string): string {
  return name
    .replace(/[_]+/g, ' ')
    .replace(/([a-z])([A-Z])/g, '$1 $2')
    .trim()
}

export async function findEpicGames(): Promise<Candidate[]> {
  const programData = process.env.ProgramData ?? 'C:\\ProgramData'
  const localAppData = process.env.LOCALAPPDATA ?? ''
  const byDir = new Map<string, EpicInfo>()
  const skip = new Set<string>() // DLC / non-game installs

  // 1. Launcher manifests (old and new launcher locations).
  const manifestFiles = [
    ...listFiles(path.join(programData, 'Epic', 'EpicGamesLauncher', 'Data', 'Manifests'), (n) => n.toLowerCase().endsWith('.item')),
    ...listFiles(path.join(localAppData, 'com.epicgames.eglv2', 'import-scan'), (n) => /^[^.]+\.item$/i.test(n), 3)
  ]
  for (const f of manifestFiles) {
    const m = readJson<EpicManifest>(f)
    if (!m?.InstallLocation) continue
    const dir = norm(m.InstallLocation)
    const notGame = m.bIsApplication === false || (m.AppCategories && (!m.AppCategories.includes('games') || m.AppCategories.includes('addons')))
    if (notGame) {
      skip.add(dir)
      continue
    }
    if (m.bIsIncompleteInstall) continue
    original.set(dir, path.resolve(m.InstallLocation))
    byDir.set(dir, { title: m.DisplayName, appName: m.AppName, namespace: m.CatalogNamespace, itemId: m.CatalogItemId })
  }

  // 2. LauncherInstalled.dat
  const installed = readJson<{ InstallationList?: { InstallLocation: string; AppName: string; NamespaceId?: string; ItemId?: string }[] }>(
    path.join(programData, 'Epic', 'UnrealEngineLauncher', 'LauncherInstalled.dat')
  )
  for (const i of installed?.InstallationList ?? []) {
    if (!i.InstallLocation || !i.AppName) continue
    const dir = norm(i.InstallLocation)
    const prev = byDir.get(dir) ?? {}
    byDir.set(dir, { ...prev, appName: prev.appName ?? i.AppName, namespace: prev.namespace ?? i.NamespaceId, itemId: prev.itemId ?? i.ItemId })
  }

  // App names the new launcher has recently installed/updated ("namespace:item:appName").
  const activityNames = new Map<string, { namespace: string; itemId: string; appName: string }>()
  const activity = readJson<Record<string, { key?: string }[]>>(path.join(localAppData, 'com.epicgames.eglv2', 'download', 'activity.json'))
  for (const list of Object.values(activity ?? {})) {
    for (const a of Array.isArray(list) ? list : []) {
      const parts = a.key?.split(':')
      if (parts?.length === 3) activityNames.set(parts[2].toLowerCase(), { namespace: parts[0], itemId: parts[1], appName: parts[2] })
    }
  }

  // 3. Any "Epic Games" library folder on any drive, plus the parents of known installs.
  const libraries = new Set<string>()
  const home = process.env.USERPROFILE
  if (home) libraries.add(path.join(home, 'Epic Games'))
  for (const letter of 'CDEFGHIJKLMNOPQRSTUVWXYZ') {
    const drive = `${letter}:\\`
    if (!fs.existsSync(drive)) continue
    for (const rel of ['Epic Games', 'Program Files\\Epic Games', 'Program Files (x86)\\Epic Games', 'Games\\Epic Games', 'Games']) {
      libraries.add(path.join(drive, rel))
    }
  }
  for (const dir of byDir.keys()) libraries.add(path.dirname(dir))
  for (const lib of libraries) {
    let children: fs.Dirent[]
    try {
      children = fs.readdirSync(lib, { withFileTypes: true })
    } catch {
      continue
    }
    for (const c of children) {
      if (!c.isDirectory()) continue
      const dir = path.join(lib, c.name)
      if (fs.existsSync(path.join(dir, '.egstore')) && !byDir.has(norm(dir))) {
        original.set(norm(dir), dir)
        byDir.set(norm(dir), {})
      }
    }
  }

  const out: Candidate[] = []
  for (const [key, info] of byDir) {
    const dir = original.get(key) ?? key
    if (skip.has(key) || !fs.existsSync(dir)) continue
    const folder = path.basename(dir)
    let { appName, namespace, itemId } = info
    const egstore = path.join(dir, '.egstore')
    if (!appName) {
      // .mancpn files (some launcher versions) carry the full catalog ids.
      for (const f of listFiles(egstore, (n) => n.toLowerCase().endsWith('.mancpn'))) {
        const m = readJson<{ AppName?: string; CatalogNamespace?: string; CatalogItemId?: string }>(f)
        if (m?.AppName) {
          appName = m.AppName
          namespace = m.CatalogNamespace
          itemId = m.CatalogItemId
          break
        }
      }
    }
    if (!appName) {
      const act = activityNames.get(folder.toLowerCase())
      if (act) ({ appName, namespace, itemId } = act)
    }
    if (!appName) {
      const mf = listFiles(egstore, (n) => n.toLowerCase().endsWith('.manifest'))[0]
      const meta = mf ? readEgstoreManifest(mf) : {}
      appName = meta.appName && activityNames.get(meta.appName.toLowerCase())?.appName
      appName ??= meta.appName
    }
    if (!appName) continue
    const id = namespace && itemId ? `${namespace}%3A${itemId}%3A${appName}` : appName
    out.push({
      platformId: 'pc',
      title: info.title ?? prettyFolder(folder),
      path: `com.epicgames.launcher://apps/${id}?action=launch&silent=true`,
      kind: 'uri',
      meta: { source: 'epic', storeId: appName, installDir: dir, namespace }
    })
  }
  return out
}

// ── GOG ──────────────────────────────────────────────────────────────

export async function findGogGames(): Promise<Candidate[]> {
  const out: Candidate[] = []
  for (const root of ['HKLM\\SOFTWARE\\WOW6432Node\\GOG.com\\Games', 'HKLM\\SOFTWARE\\GOG.com\\Games']) {
    const text = await regQuery(root, true)
    if (!text) continue
    for (const block of text.split(/\r?\n(?=HKEY_)/)) {
      const name = regValue(block, 'gameName')
      const exe = regValue(block, 'exe')
      const dependsOn = regValue(block, 'dependsOn')
      const gameId = regValue(block, 'gameID')
      if (!name || !exe || dependsOn) continue // dependsOn = DLC
      if (!fs.existsSync(exe)) continue
      if (out.some((c) => c.path.toLowerCase() === exe.toLowerCase())) continue
      out.push({ platformId: 'pc', title: name, path: exe, kind: 'exe', meta: { source: 'gog', storeId: gameId } })
    }
    if (out.length) break
  }
  return out
}

// ── Xbox app / Microsoft Store (PC Game Pass) ────────────────────────

/** "Fable Base Game" → "Fable", "X (PC)" → "X" */
function stripEdition(title: string): string {
  return title.replace(/\s*[-:(]?\s*(base game|standard edition|pc|windows( 10)?|for windows)\)?$/i, '').trim() || title
}

/** Reads `<drive>:\.GamingRoot`, which lists the Xbox app's game folders on that drive. */
function gamingRoots(): string[] {
  const roots: string[] = []
  for (const letter of 'CDEFGHIJKLMNOPQRSTUVWXYZ') {
    const drive = `${letter}:\\`
    let buf: Buffer
    try {
      buf = fs.readFileSync(path.join(drive, '.GamingRoot'))
    } catch {
      continue
    }
    if (buf.length < 8 || buf.toString('ascii', 0, 4) !== 'RGBX') continue
    const text = buf.toString('utf16le', 8)
    for (const rel of text.split('\0').filter(Boolean)) roots.push(path.join(drive, rel))
  }
  return roots
}

function attr(xml: string, tag: string, name: string): string | undefined {
  const m = xml.match(new RegExp(`<${tag}\\b[^>]*\\b${name}="([^"]*)"`, 'i'))
  return m?.[1]
}

function appxFamilies(): Promise<Map<string, string>> {
  return new Promise((resolve) => {
    execFile(
      'powershell',
      ['-NoProfile', '-NonInteractive', '-Command', 'Get-AppxPackage | Select-Object Name,PackageFamilyName | ConvertTo-Json -Compress'],
      { windowsHide: true, maxBuffer: 32 * 1024 * 1024, timeout: 30000 },
      (err, stdout) => {
        const map = new Map<string, string>()
        if (!err) {
          try {
            const list = JSON.parse(stdout) as { Name: string; PackageFamilyName: string }[] | { Name: string; PackageFamilyName: string }
            for (const p of Array.isArray(list) ? list : [list]) map.set(p.Name.toLowerCase(), p.PackageFamilyName)
          } catch {
            // ignore
          }
        }
        resolve(map)
      }
    )
  })
}

export async function findXboxGames(): Promise<Candidate[]> {
  const games: { dir: string; identity?: string; title: string; appId: string; exe?: string }[] = []
  for (const root of gamingRoots()) {
    let dirs: string[]
    try {
      dirs = fs.readdirSync(root)
    } catch {
      continue
    }
    for (const d of dirs) {
      const content = path.join(root, d, 'Content')
      const xml = readText(path.join(content, 'MicrosoftGame.config'))
      if (!xml) continue
      let title = attr(xml, 'ShellVisuals', 'DefaultDisplayName') ?? d
      if (title.startsWith('ms-resource:')) title = d
      title = stripEdition(title)
      const exeName = attr(xml, 'Executable', 'Name')
      games.push({
        dir: content,
        identity: attr(xml, 'Identity', 'Name'),
        title,
        appId: attr(xml, 'Executable', 'Id') ?? 'App',
        exe: exeName ? path.join(content, exeName) : undefined
      })
    }
  }
  if (!games.length) return []

  const families = await appxFamilies()
  const out: Candidate[] = []
  for (const g of games) {
    const pfn = g.identity ? families.get(g.identity.toLowerCase()) : undefined
    if (pfn) {
      out.push({
        platformId: 'pc',
        title: g.title,
        path: `shell:AppsFolder\\${pfn}!${g.appId}`,
        kind: 'uri',
        meta: { source: 'xbox', storeId: g.identity, installDir: g.dir }
      })
    } else if (g.exe && fs.existsSync(g.exe)) {
      out.push({ platformId: 'pc', title: g.title, path: g.exe, kind: 'exe', meta: { source: 'xbox', storeId: g.identity } })
    }
  }
  return out
}

// ── Riot Games ───────────────────────────────────────────────────────

const RIOT_TITLES: Record<string, string> = {
  league_of_legends: 'League of Legends',
  teamfighttactics: 'Teamfight Tactics',
  valorant: 'VALORANT',
  bacon: 'Legends of Runeterra',
  lion: '2XKO'
}

/** Riot keeps per-product metadata in ProgramData; only installed products have an install path. */
export async function findRiotGames(): Promise<Candidate[]> {
  const programData = process.env.ProgramData ?? 'C:\\ProgramData'
  const metaDir = path.join(programData, 'Riot Games', 'Metadata')
  let dirs: string[]
  try {
    dirs = fs.readdirSync(metaDir).filter((d) => /^[a-z_]+\.live$/i.test(d))
  } catch {
    return []
  }
  const out: Candidate[] = []
  for (const d of dirs) {
    const product = d.replace(/\.live$/i, '')
    const yaml = readText(path.join(metaDir, d, `${d}.product_settings.yaml`))
    const installDir = yaml?.match(/^product_install_full_path:\s*"([^"]+)"/m)?.[1]
    if (!installDir || !fs.existsSync(installDir)) continue
    out.push({
      platformId: 'pc',
      title: RIOT_TITLES[product] ?? prettyFolder(product.replace(/_/g, ' ')),
      path: `riot://${product}/live`,
      kind: 'uri',
      meta: { source: 'riot', storeId: product, installDir: path.resolve(installDir) }
    })
  }
  return out
}

/** Path of the Riot Client, used to start Riot games. */
/** Steam client executable (to start Steam minimised instead of showing its window). */
export async function steamExePath(): Promise<string | null> {
  const reg = await regQuery('HKCU\\Software\\Valve\\Steam')
  const exe = regValue(reg, 'SteamExe')?.replace(/\//g, '\\')
  return exe && fs.existsSync(exe) ? exe : null
}

export function riotClientPath(): string | null {
  const programData = process.env.ProgramData ?? 'C:\\ProgramData'
  const cfg = readJson<{ rc_default?: string; rc_live?: string }>(path.join(programData, 'Riot Games', 'RiotClientInstalls.json'))
  const exe = cfg?.rc_default ?? cfg?.rc_live
  return exe && fs.existsSync(exe) ? path.resolve(exe) : null
}

// ── Ubisoft Connect ──────────────────────────────────────────────────

export async function findUbisoftGames(): Promise<Candidate[]> {
  const text = await regQuery('HKLM\\SOFTWARE\\WOW6432Node\\Ubisoft\\Launcher\\Installs', true)
  if (!text) return []
  const out: Candidate[] = []
  for (const block of text.split(/\r?\n(?=HKEY_)/)) {
    const id = block.match(/Installs\\(\d+)/)?.[1]
    const dir = regValue(block, 'InstallDir')?.replace(/\//g, '\\')
    if (!id || !dir || !fs.existsSync(dir)) continue
    out.push({
      platformId: 'pc',
      title: prettyFolder(path.basename(dir.replace(/\\+$/, ''))),
      path: `uplay://launch/${id}/0`,
      kind: 'uri',
      meta: { source: 'ubisoft', storeId: id, installDir: path.resolve(dir) }
    })
  }
  return out
}

// ── Everything else: Windows "installed apps" + game engine fingerprints ──
//
// EA app, Battle.net, Rockstar, Bethesda.net, itch.io, standalone installers… all register in
// the Windows Uninstall list. An entry is treated as a game when its publisher is a known game
// publisher, or its install folder contains a game engine / game SDK fingerprint.

const GAME_PUBLISHERS =
  /electronic arts|\bea\b|blizzard|activision|rockstar|ubisoft|bethesda|cd projekt|2k\b|bandai|square enix|\bsega\b|capcom|konami|warner bros|paradox|focus (home|entertainment)|devolver|mojang|take-two|thq|deep silver|505 games|team17|annapurna|hoyoverse|mihoyo|kuro games|nexon|wargaming|gaijin|smilegate|ncsoft|krafton|pearl abyss|riot games/i

const NOT_GAMES =
  /launcher|\bclient\b|\bapp\b$|desktop|connect$|battle\.net|ea app|origin$|redistributable|runtime|\bsdk\b|driver|directx|visual c\+\+|anti-?cheat|easyanticheat|battleye|vanguard|overlay|updater|uninstall|support|tools?$|editor|steamvr|nvidia|amd |intel|microsoft|windows/i

/** Files/folders that only game installs tend to have. */
const ENGINE_MARKERS = [
  'UnityPlayer.dll',
  'GameAssembly.dll',
  'steam_api64.dll',
  'steam_api.dll',
  'EOSSDK-Win64-Shipping.dll',
  'bink2w64.dll',
  'binkw64.dll',
  'fmod.dll',
  'fmodstudio.dll',
  'PhysX3_x64.dll',
  'GFSDK_Aftermath_Lib.x64.dll',
  'nvngx_dlss.dll',
  'sl.interposer.dll'
]

function looksLikeGameFolder(dir: string): boolean {
  const has = (d: string) => {
    let names: string[]
    try {
      names = fs.readdirSync(d)
    } catch {
      return false
    }
    const lower = new Set(names.map((n) => n.toLowerCase()))
    if (ENGINE_MARKERS.some((m) => lower.has(m.toLowerCase()))) return true
    // Unreal: <Game>/Binaries/Win64 + Engine folder
    return lower.has('engine') && names.some((n) => fs.existsSync(path.join(d, n, 'Binaries', 'Win64')))
  }
  if (has(dir)) return true
  try {
    for (const sub of fs.readdirSync(dir, { withFileTypes: true })) {
      if (sub.isDirectory() && /^(bin|bin64|binaries|x64|win64|game|retail|_retail_)$/i.test(sub.name)) {
        if (has(path.join(dir, sub.name))) return true
      }
    }
  } catch {
    // unreadable
  }
  return false
}

const HELPER = /unins|setup|crash|report|redist|launcher_helper|easyanticheat|battleye|vc_redist|dxsetup|prereq|cefprocess|helper|updater/i

function bestExe(dir: string, hint?: string): string | null {
  if (hint) {
    const exe = hint.replace(/^"|"$/g, '').replace(/,\s*-?\d+$/, '')
    if (/\.exe$/i.test(exe) && !HELPER.test(path.basename(exe)) && exe.toLowerCase().startsWith(dir.toLowerCase()) && fs.existsSync(exe)) return exe
  }
  let best: { p: string; size: number } | null = null
  const walk = (d: string, level: number) => {
    let entries: fs.Dirent[]
    try {
      entries = fs.readdirSync(d, { withFileTypes: true })
    } catch {
      return
    }
    for (const e of entries) {
      const p = path.join(d, e.name)
      if (e.isFile() && /\.exe$/i.test(e.name) && !HELPER.test(e.name)) {
        const size = fs.statSync(p).size
        if (!best || size > best.size) best = { p, size }
      } else if (e.isDirectory() && level < 2 && !/^(redist|_commonredist|support|directx|__installer|installer)$/i.test(e.name)) {
        walk(p, level + 1)
      }
    }
  }
  walk(dir, 0)
  return (best as { p: string } | null)?.p ?? null
}

function editDistance(a: string, b: string): number {
  if (Math.abs(a.length - b.length) > 3) return 99
  const dp = Array.from({ length: b.length + 1 }, (_, j) => j)
  for (let i = 1; i <= a.length; i++) {
    let prev = dp[0]
    dp[0] = i
    for (let j = 1; j <= b.length; j++) {
      const tmp = dp[j]
      dp[j] = Math.min(dp[j] + 1, dp[j - 1] + 1, prev + (a[i - 1] === b[j - 1] ? 0 : 1))
      prev = tmp
    }
  }
  return dp[b.length]
}

/** Installed games from the Windows Uninstall list. `knownDirs` are already-found installs to skip. */
export async function findRegistryGames(knownDirs: Set<string>): Promise<Candidate[]> {
  const roots = [
    'HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall',
    'HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall',
    'HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall'
  ]
  const out: Candidate[] = []
  const seen = new Set<string>()
  for (const root of roots) {
    const text = await regQuery(root, true)
    for (const block of text.split(/\r?\n(?=HKEY_)/)) {
      const name = regValue(block, 'DisplayName')
      const dirRaw = regValue(block, 'InstallLocation')
      if (!name || !dirRaw || regValue(block, 'SystemComponent') === '0x1' || regValue(block, 'ParentKeyName')) continue
      const dir = path.resolve(dirRaw.replace(/^"|"$/g, ''))
      const key = dir.toLowerCase()
      if (seen.has(key) || knownDirs.has(key) || dir.length <= 3) continue
      if (NOT_GAMES.test(name) || !fs.existsSync(dir)) continue
      const publisher = regValue(block, 'Publisher') ?? ''
      if (!GAME_PUBLISHERS.test(publisher) && !looksLikeGameFolder(dir)) continue
      const exe = bestExe(dir, regValue(block, 'DisplayIcon'))
      if (!exe) continue
      seen.add(key)
      const source = /electronic arts|\bea\b/i.test(publisher) ? 'ea' : /blizzard|activision/i.test(publisher) ? 'battlenet' : 'other'
      // Registry names sometimes carry typos ("Battlefieldt 1"); the folder name is usually clean.
      const folder = path.basename(dir)
      const n1 = name.toLowerCase().replace(/[^a-z0-9]/g, '')
      const n2 = folder.toLowerCase().replace(/[^a-z0-9]/g, '')
      const title = n1 !== n2 && editDistance(n1, n2) <= 2 ? folder : name
      out.push({
        platformId: 'pc',
        title: stripEdition(title.replace(/[™®]/g, '').trim()),
        path: exe,
        kind: 'exe',
        meta: { source, installDir: dir, publisher: publisher || undefined }
      })
    }
  }
  return out
}
