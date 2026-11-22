import electron, { app } from 'electron'
import fs from 'fs'
import path from 'path'

/**
 * Protected streaming (Netflix, Disney+, Prime Video, Spotify) inside the console.
 *
 * Stock Electron has no Widevine. The castlabs "Electron for Content Security" build adds it
 * (`components` API, the CDM itself is installed by Chromium's component updater on first run).
 * Streaming services also require the app binary to carry a VMP signature (castlabs EVS);
 * without it they refuse playback, so DRM apps fall back to Microsoft Edge in that case.
 */

export interface DrmStatus {
  /** Widevine CDM is installed and ready. */
  widevine: boolean
  /** The running binary has a VMP signature. */
  vmp: boolean
  /** DRM apps can run inside the console. */
  inConsole: boolean
  detail: string
}

let status: DrmStatus = { widevine: false, vmp: false, inConsole: false, detail: 'Kontrol ediliyor…' }

type Components = { whenReady: () => Promise<unknown>; status: () => Record<string, { status: string; version: string | null }> }

function vmpSigned(): boolean {
  const exe = process.execPath
  return fs.existsSync(`${exe}.sig`) || fs.existsSync(path.join(path.dirname(exe), `${path.basename(exe)}.sig`))
}

export function drmStatus(): DrmStatus {
  return status
}

/** Waits for the Widevine component (castlabs builds only). Calls `onChange` when the status is known. */
export async function initDrm(onChange: () => void): Promise<void> {
  const components = (electron as unknown as { components?: Components }).components
  if (!components) {
    status = { widevine: false, vmp: false, inConsole: false, detail: 'Bu Electron sürümünde Widevine yok.' }
    onChange()
    return
  }
  try {
    await app.whenReady()
    await components.whenReady()
    // castlabs release binaries carry a development signature; streaming services only accept
    // a production signature, which the packaged app gets in the afterPack hook.
    const signed = vmpSigned()
    const vmp = signed && app.isPackaged
    status = {
      widevine: true,
      vmp,
      inConsole: vmp,
      detail: vmp
        ? 'Widevine hazır, uygulama VMP imzalı.'
        : signed
          ? 'Widevine hazır. Geliştirme imzası: Netflix gibi servisler üretim imzası ister (kurulu sürüm imzalanır).'
          : 'Widevine hazır, ancak uygulama VMP imzalı değil.'
    }
  } catch (err) {
    status = { widevine: false, vmp: false, inConsole: false, detail: `Widevine yüklenemedi: ${(err as Error).message}` }
  }
  console.log('[drm]', status.detail)
  onChange()
}
