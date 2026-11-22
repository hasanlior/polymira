import { net, shell } from 'electron'
import fs from 'fs'
import path from 'path'
import { BUILTIN_CATALOG, type StoreItem, type ThemeDef } from '@shared/catalog'
import { BRAND } from '@shared/brand'
import type { Game, ModInfo } from '@shared/types'
import type { LaunchPlan } from './emulators'
import { paths, store } from './store'

// ── Store catalog ────────────────────────────────────────────────────

let remote: StoreItem[] = []

function validItem(x: unknown): x is StoreItem {
  const i = x as StoreItem
  return Boolean(i && typeof i.id === 'string' && (i.kind === 'app' || i.kind === 'theme' || (i.kind === 'mod' && typeof i.mod?.css === 'string' && (/^https:\/\//.test(i.mod.css) || !i.mod.css.includes(':')))) && typeof i.name === 'string' && /^[a-z0-9_-]+$/i.test(i.id))
}

/** Built-in items plus the optional remote catalog (remote entries never replace built-in ids). */
/** The user's catalog URL, or the community catalog in the project's GitHub repository. */
export function catalogUrl(): string {
  if (store.settings.storeCatalogUrl) return store.settings.storeCatalogUrl
  return BRAND.repo ? `https://raw.githubusercontent.com/${BRAND.repo}/main/community/catalog.json` : ''
}

export async function catalog(refresh = false): Promise<StoreItem[]> {
  const url = catalogUrl()
  if (url && (refresh || remote.length === 0)) {
    try {
      const res = await net.fetch(url)
      if (res.ok) {
        const data = (await res.json()) as unknown[]
        remote = Array.isArray(data) ? data.filter(validItem) : []
      }
    } catch {
      // offline: keep what we have
    }
  }
  const builtinIds = new Set(BUILTIN_CATALOG.map((i) => i.id))
  return [...BUILTIN_CATALOG, ...remote.filter((i) => !builtinIds.has(i.id))]
}

export function findItem(id: string): StoreItem | undefined {
  return BUILTIN_CATALOG.find((i) => i.id === id) ?? remote.find((i) => i.id === id)
}

// ── Apps ─────────────────────────────────────────────────────────────

export function appPath(id: string): string {
  return `app://${id}`
}

function edgePath(): string | null {
  const candidates = [
    path.join(process.env['ProgramFiles(x86)'] ?? '', 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
    path.join(process.env.ProgramFiles ?? '', 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
    path.join(process.env.LOCALAPPDATA ?? '', 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
    path.join(process.env.ProgramFiles ?? '', 'Google', 'Chrome', 'Application', 'chrome.exe'),
    path.join(process.env.LOCALAPPDATA ?? '', 'Google', 'Chrome', 'Application', 'chrome.exe')
  ]
  return candidates.find((p) => p && fs.existsSync(p)) ?? null
}

/**
 * Apps run in Microsoft Edge with a profile that belongs to this app: it has DRM for streaming
 * services, keeps logins between launches, and runs as its own process so the in-game menu
 * can pause and close it like a game.
 */
export function appLaunchPlan(game: Game): Extract<LaunchPlan, { type: 'spawn' }> {
  const id = game.path.replace(/^app:\/\//, '')
  const item = findItem(id)
  if (!item?.app) throw new Error('Uygulama mağazada bulunamadı.')
  const exe = edgePath()
  if (!exe) throw new Error('Microsoft Edge veya Google Chrome bulunamadı.')
  const profile = path.join(paths.root, 'web-profile', item.app.userAgent ? `${id}` : 'default')
  const args = ['--no-first-run', '--no-default-browser-check', `--user-data-dir=${profile}`, '--autoplay-policy=no-user-gesture-required']
  if (item.app.userAgent) args.push(`--user-agent=${item.app.userAgent}`)
  if (item.app.kiosk) args.push('--kiosk', item.app.url, '--edge-kiosk-type=fullscreen')
  else args.push('--start-fullscreen', item.app.url)
  return { type: 'spawn', exe, args, cwd: path.dirname(exe), label: item.name }
}

// ── Themes and mods (user folders) ───────────────────────────────────

export function modsDir(): string {
  return path.join(paths.root, 'mods')
}

export function themesDir(): string {
  return path.join(paths.root, 'themes')
}

/** Themes: installed store themes + `themes/*.json` files the user dropped in. */
export function installedThemes(): ThemeDef[] {
  const out: ThemeDef[] = []
  for (const id of store.settings.installedThemes) {
    const t = findItem(id)?.theme
    if (t) out.push(t)
  }
  try {
    fs.mkdirSync(themesDir(), { recursive: true })
    for (const f of fs.readdirSync(themesDir())) {
      if (!f.toLowerCase().endsWith('.json')) continue
      try {
        const t = JSON.parse(fs.readFileSync(path.join(themesDir(), f), 'utf8')) as Partial<ThemeDef>
        if (typeof t.name === 'string') {
          out.push({ id: `file:${f}`, name: t.name, hue: typeof t.hue === 'number' ? t.hue : null, night: Boolean(t.night), css: typeof t.css === 'string' ? t.css : undefined })
        }
      } catch {
        // invalid theme file
      }
    }
  } catch {
    // no folder
  }
  return out
}

/**
 * Mods: `mods/<name>/mod.json` ({ "name", "description" }) + `style.css`.
 * Mods are CSS only: they can restyle anything but cannot run code.
 */
export function listMods(): ModInfo[] {
  const dir = modsDir()
  fs.mkdirSync(dir, { recursive: true })
  const out: ModInfo[] = []
  for (const d of fs.readdirSync(dir, { withFileTypes: true })) {
    if (!d.isDirectory()) continue
    const meta = (() => {
      try {
        return JSON.parse(fs.readFileSync(path.join(dir, d.name, 'mod.json'), 'utf8')) as { name?: string; description?: string }
      } catch {
        return {}
      }
    })()
    if (!fs.existsSync(path.join(dir, d.name, 'style.css'))) continue
    out.push({ id: d.name, name: meta.name ?? d.name, description: meta.description, enabled: store.settings.enabledMods.includes(d.name) })
  }
  return out
}

export function modsCss(): string {
  return listMods()
    .filter((m) => m.enabled)
    .map((m) => {
      try {
        return `/* mod: ${m.id} */\n${fs.readFileSync(path.join(modsDir(), m.id, 'style.css'), 'utf8')}`
      } catch {
        return ''
      }
    })
    .join('\n')
}

/** Downloads a community mod into mods/<id>/ (style.css + mod.json). */
export async function installMod(item: StoreItem): Promise<void> {
  if (!item.mod) throw new Error('Geçersiz mod.')
  // Relative paths are resolved against the catalog file (mods stored next to catalog.json).
  const res = await net.fetch(new URL(item.mod.css, catalogUrl() || 'https://invalid/').toString())
  if (!res.ok) throw new Error(`Mod indirilemedi (HTTP ${res.status}).`)
  const css = await res.text()
  if (css.length > 512 * 1024) throw new Error('Mod dosyası çok büyük.')
  const dir = path.join(modsDir(), item.id)
  fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'style.css'), css)
  fs.writeFileSync(path.join(dir, 'mod.json'), JSON.stringify({ name: item.name, description: item.description, author: item.author, version: item.mod.version, source: 'store' }, null, 2))
}

export function removeMod(id: string): void {
  if (!/^[a-z0-9_-]+$/i.test(id)) return
  fs.rmSync(path.join(modsDir(), id), { recursive: true, force: true })
}

export function openFolder(which: 'mods' | 'themes'): void {
  const dir = which === 'mods' ? modsDir() : themesDir()
  fs.mkdirSync(dir, { recursive: true })
  void shell.openPath(dir)
}
