import { app, net } from 'electron'
import { createHash } from 'crypto'
import { spawn } from 'child_process'
import fs from 'fs'
import os from 'os'
import path from 'path'
import { BRAND } from '@shared/brand'
import type { UpdateState } from '@shared/types'

/**
 * Updates from GitHub Releases: checks the latest release of BRAND.repo, downloads the
 * Windows installer, verifies its SHA-256 digest (published by GitHub for every asset),
 * then runs it silently and restarts.
 */

let state: UpdateState = { status: 'idle' }
let installer: string | null = null

const HEADERS = { 'user-agent': `${BRAND.slug}-app`, accept: 'application/vnd.github+json' }

export function updateState(): UpdateState {
  return state
}

function newer(a: string, b: string): boolean {
  const pa = a.replace(/^v/, '').split(/[.-]/).map((x) => parseInt(x, 10) || 0)
  const pb = b.replace(/^v/, '').split(/[.-]/).map((x) => parseInt(x, 10) || 0)
  for (let i = 0; i < 3; i++) if ((pa[i] ?? 0) !== (pb[i] ?? 0)) return (pa[i] ?? 0) > (pb[i] ?? 0)
  return false
}

interface Asset {
  name: string
  browser_download_url: string
  size: number
  digest?: string
}

export async function checkForUpdate(onChange: () => void): Promise<UpdateState> {
  if (!BRAND.repo) {
    state = { status: 'disabled', message: 'Güncelleme deposu ayarlanmamış (brand.ts → repo).' }
    onChange()
    return state
  }
  if (!app.isPackaged) {
    state = { status: 'disabled', message: 'Geliştirme sürümü: güncellemeler kurulu sürümde çalışır.' }
    onChange()
    return state
  }
  if (state.status === 'downloading' || state.status === 'ready') return state
  state = { status: 'checking' }
  onChange()
  try {
    const res = await net.fetch(`https://api.github.com/repos/${BRAND.repo}/releases/latest`, { headers: HEADERS })
    if (!res.ok) throw new Error(`GitHub HTTP ${res.status}`)
    const rel = (await res.json()) as { tag_name: string; body?: string; assets: Asset[] }
    if (!newer(rel.tag_name, app.getVersion())) {
      state = { status: 'none', version: app.getVersion() }
      onChange()
      return state
    }
    const asset = rel.assets.find((a) => /setup.*\.exe$/i.test(a.name)) ?? rel.assets.find((a) => /\.exe$/i.test(a.name) && !/portable/i.test(a.name))
    if (!asset) throw new Error('Sürümde Windows kurulum dosyası yok.')
    state = { status: 'downloading', version: rel.tag_name.replace(/^v/, ''), notes: rel.body?.slice(0, 600), progress: 0 }
    onChange()
    installer = await download(asset, (p) => {
      state = { ...state, progress: p }
      onChange()
    })
    state = { ...state, status: 'ready', progress: 1 }
  } catch (err) {
    state = { status: 'error', message: (err as Error).message }
  }
  onChange()
  return state
}

async function download(asset: Asset, progress: (p: number) => void): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `${BRAND.slug}-update-`))
  const file = path.join(dir, asset.name.replace(/[^\w.-]/g, '_'))
  const res = await net.fetch(asset.browser_download_url, { headers: { 'user-agent': HEADERS['user-agent'] } })
  if (!res.ok || !res.body) throw new Error(`İndirme hatası (HTTP ${res.status})`)
  const hash = createHash('sha256')
  const out = fs.createWriteStream(file)
  const reader = res.body.getReader()
  let got = 0
  let last = 0
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    hash.update(value)
    got += value.length
    if (!out.write(value)) await new Promise((r) => out.once('drain', r))
    if (Date.now() - last > 500) {
      last = Date.now()
      progress(asset.size ? got / asset.size : 0)
    }
  }
  await new Promise<void>((r) => out.end(r))
  const want = asset.digest?.match(/^sha256:([0-9a-f]{64})$/i)?.[1]
  if (want && hash.digest('hex').toLowerCase() !== want.toLowerCase()) {
    fs.rmSync(file, { force: true })
    throw new Error('İndirilen güncelleme doğrulanamadı (SHA-256 uyuşmuyor).')
  }
  return file
}

/** Release notes of the installed version (GitHub release body). */
export async function releaseNotes(): Promise<{ version: string; body: string }> {
  const version = app.getVersion()
  if (!BRAND.repo) throw new Error('Sürüm notları için depo ayarlanmamış.')
  const res = await net.fetch(`https://api.github.com/repos/${BRAND.repo}/releases/tags/v${version}`, { headers: HEADERS })
  if (!res.ok) throw new Error('Sürüm notları alınamadı. İnternet bağlantını kontrol et.')
  const rel = (await res.json()) as { body?: string }
  return { version, body: rel.body?.trim() || 'Bu sürüm için not yok.' }
}

/** Runs the downloaded installer silently; it replaces the app and starts it again. */
export function installUpdate(): boolean {
  if (state.status !== 'ready' || !installer || !fs.existsSync(installer)) return false
  spawn(installer, ['/S', '--force-run'], { detached: true, stdio: 'ignore' }).unref()
  return true
}
