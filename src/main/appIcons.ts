import { nativeImage, net } from 'electron'
import fs from 'fs'
import path from 'path'
import type { StoreItem } from '@shared/catalog'
import { paths } from './store'

/**
 * App icons are fetched from each app's own website at runtime (the same icon a browser
 * shows for a bookmark), so no third-party logos ship with the source code.
 * Order: the catalog's `icon` URL → the site's web app manifest → apple-touch-icon →
 * <link rel="icon"> → /favicon.ico. The largest raster image wins.
 */

const REFRESH_MS = 30 * 24 * 3600 * 1000
const MIN_SIZE = 48
const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36'

export function appIconFile(id: string): string {
  return path.join(paths.cache, 'appicons', `${id.replace(/[^a-z0-9_-]/gi, '_')}.png`)
}

async function get(url: string, timeoutMs = 10000): Promise<Response | null> {
  try {
    const res = await net.fetch(url, { headers: { 'user-agent': UA, 'accept-language': 'tr,en;q=0.8' }, signal: AbortSignal.timeout(timeoutMs) })
    return res.ok ? res : null
  } catch {
    return null
  }
}

interface Candidate {
  url: string
  size: number
}

function attr(tag: string, name: string): string | null {
  const m = tag.match(new RegExp(`\\b${name}\\s*=\\s*("([^"]*)"|'([^']*)'|([^\\s>]+))`, 'i'))
  return m ? (m[2] ?? m[3] ?? m[4] ?? '') : null
}

function sizeOf(sizes: string | null | undefined, fallback: number): number {
  if (!sizes) return fallback
  if (/any/i.test(sizes)) return 512
  return Math.max(0, ...sizes.split(/\s+/).map((s) => Number(s.split(/x/i)[0]) || 0)) || fallback
}

/** Banner image of an app (its site's og:image), shown on the store page. */
export function appBannerFile(id: string): string {
  return path.join(paths.cache, 'appicons', `${id.replace(/[^a-z0-9_-]/gi, '_')}__banner.png`)
}

let pendingBanner: string | null = null

async function candidates(item: StoreItem): Promise<Candidate[]> {
  const out: Candidate[] = []
  pendingBanner = item.banner ?? null
  if (item.icon) out.push({ url: item.icon, size: 10000 })
  if (!item.app) return out
  // The site's home page usually lists the big icons; app sub-pages (e.g. a TV UI) often do not.
  let home = item.app.url
  try {
    home = new URL(item.app.url).origin + '/'
  } catch {
    // keep the app URL
  }
  const page = (await get(home)) ?? (await get(item.app.url))
  const base = page?.url || home
  const html = page ? (await page.text()).slice(0, 400_000) : ''
  if (!pendingBanner) {
    for (const tag of html.match(/<meta\b[^>]*>/gi) ?? []) {
      const key = (attr(tag, 'property') ?? attr(tag, 'name') ?? '').toLowerCase()
      const content = attr(tag, 'content')
      if (!content || !/^(og:image(:secure_url)?|twitter:image)$/.test(key)) continue
      try {
        pendingBanner = new URL(content.replace(/&amp;/g, '&'), base).toString()
        break
      } catch {
        // bad URL
      }
    }
  }
  for (const tag of html.match(/<link\b[^>]*>/gi) ?? []) {
    const rel = (attr(tag, 'rel') ?? '').toLowerCase()
    const href = attr(tag, 'href')
    if (!href) continue
    let abs: string
    try {
      abs = new URL(href, base).toString()
    } catch {
      continue
    }
    if (rel === 'manifest') {
      const m = await get(abs)
      try {
        const json = (await m?.json()) as { icons?: { src: string; sizes?: string; type?: string }[] } | undefined
        for (const ic of json?.icons ?? []) {
          if (/svg/i.test(ic.type ?? '') || /\.svg(\?|$)/i.test(ic.src)) continue
          out.push({ url: new URL(ic.src, abs).toString(), size: sizeOf(ic.sizes, 192) + 1 })
        }
      } catch {
        // invalid manifest
      }
    } else if (rel.includes('apple-touch-icon')) {
      out.push({ url: abs, size: sizeOf(attr(tag, 'sizes'), 180) })
    } else if (/(^|\s)icon(\s|$)/.test(rel) && !/\.svg(\?|$)/i.test(abs)) {
      out.push({ url: abs, size: sizeOf(attr(tag, 'sizes'), 32) })
    }
  }
  try {
    out.push({ url: new URL('/favicon.ico', base).toString(), size: 16 })
  } catch {
    // ignore
  }
  return out.sort((a, b) => b.size - a.size)
}

async function download(url: string): Promise<Electron.NativeImage | null> {
  const res = await get(url)
  if (!res) return null
  const buf = Buffer.from(await res.arrayBuffer())
  let img = nativeImage.createFromBuffer(buf)
  if (img.isEmpty()) {
    // .ico files: nativeImage only decodes them from disk on Windows.
    const tmp = path.join(paths.cache, `icon-${process.pid}-${Date.now()}.ico`)
    try {
      fs.writeFileSync(tmp, buf)
      img = nativeImage.createFromPath(tmp)
    } finally {
      fs.rmSync(tmp, { force: true })
    }
  }
  return img.isEmpty() ? null : img
}

/** Saves the banner; when the site has none, a marker stops refetching until the next refresh. */
async function saveBanner(item: StoreItem, url: string | null): Promise<boolean> {
  const file = appBannerFile(item.id)
  const marker = file.replace(/\.png$/, '.none')
  fs.mkdirSync(path.dirname(file), { recursive: true })
  const img = url ? await download(url) : null
  const size = img?.getSize()
  if (!img || !size || size.width < 320 || size.height < 160) {
    if (!fs.existsSync(file)) fs.writeFileSync(marker, '')
    return false
  }
  fs.writeFileSync(file, (size.width > 1280 ? img.resize({ width: 1280, quality: 'best' }) : img).toPNG())
  fs.rmSync(marker, { force: true })
  return true
}

/** Fetches and caches one app's icon (and banner). Returns true when something new was stored. */
export async function fetchAppIcon(item: StoreItem, force = false): Promise<boolean> {
  const file = appIconFile(item.id)
  const fresh = (f: string) => fs.existsSync(f) && Date.now() - fs.statSync(f).mtimeMs < REFRESH_MS
  const bannerFresh = () => fresh(appBannerFile(item.id)) || fresh(appBannerFile(item.id).replace(/\.png$/, '.none'))
  if (!force && fresh(file) && (!item.app || bannerFresh())) return false
  const list = await candidates(item)
  const banner = !bannerFresh() || force ? await saveBanner(item, pendingBanner).catch(() => false) : false
  if (!force && fresh(file)) return banner
  for (const c of list.slice(0, 6)) {
    const img = await download(c.url)
    if (!img) continue
    const { width } = img.getSize()
    if (width < MIN_SIZE && c.size > 16) continue
    fs.mkdirSync(path.dirname(file), { recursive: true })
    const out = width > 256 ? img.resize({ width: 256, quality: 'best' }) : img
    fs.writeFileSync(file, out.toPNG())
    return true
  }
  // Keep an old icon rather than losing it when offline.
  if (fs.existsSync(file)) fs.utimesSync(file, new Date(), new Date())
  return banner
}

/** id → icon revision (file mtime) for every cached app icon. */
export function appIconRevs(): Record<string, number> {
  const dir = path.join(paths.cache, 'appicons')
  const out: Record<string, number> = {}
  try {
    for (const f of fs.readdirSync(dir)) {
      if (f.endsWith('.png')) out[path.basename(f, '.png')] = Math.floor(fs.statSync(path.join(dir, f)).mtimeMs)
    }
  } catch {
    // none yet
  }
  return out
}
