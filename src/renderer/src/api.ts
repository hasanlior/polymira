import { useEffect, useState } from 'react'
import type { IpcChannel, IpcContract } from '@shared/ipc'
import type { AppState } from '@shared/types'
import { MEDIA_SCHEME } from '@shared/ipc'

/** Typed IPC call. Strips Electron's "Error invoking remote method" prefix from errors. */
export async function call<C extends IpcChannel>(
  channel: C,
  ...args: Parameters<IpcContract[C]>
): Promise<Awaited<ReturnType<IpcContract[C]>>> {
  try {
    return (await window.bridge.invoke(channel, ...args)) as Awaited<ReturnType<IpcContract[C]>>
  } catch (err) {
    const msg = String((err as Error)?.message ?? err).replace(/^Error invoking remote method '[^']+': (Error: )?/, '')
    throw new Error(msg)
  }
}

export function useAppState(): AppState | null {
  const [state, setState] = useState<AppState | null>(null)
  useEffect(() => {
    let alive = true
    void call('state:get').then((s) => alive && setState(s))
    const off = window.bridge.on('state', (s) => setState(s))
    return () => {
      alive = false
      off()
    }
  }, [])
  return state
}

export function coverUrl(id: string, rev?: number): string {
  return `${MEDIA_SCHEME}://covers/${id}.png?v=${rev ?? 0}`
}

export function formatPlayTime(sec: number): string {
  if (sec < 60) return sec > 0 ? '1 dakikadan az' : 'Hiç oynanmadı'
  const h = Math.floor(sec / 3600)
  const m = Math.floor((sec % 3600) / 60)
  return h > 0 ? `${h} sa ${m} dk` : `${m} dk`
}

export function formatBytes(n: number): string {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(1)} GB`
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`
  return `${Math.max(0, Math.round(n / 1024))} KB`
}

export function formatEta(sec: number | null): string {
  if (sec === null) return ''
  if (sec < 60) return `${sec} sn kaldı`
  const m = Math.round(sec / 60)
  return m < 60 ? `${m} dk kaldı` : `${Math.floor(m / 60)} sa ${m % 60} dk kaldı`
}

/** One line describing a download: "%45 · 120 / 260 MB · 8.2 MB/s · 2 dk kaldı" */
export function downloadLine(j: import('@shared/types').DownloadJob): string {
  if (j.status === 'queued') return 'Sırada'
  if (j.status === 'extracting') return j.detail ?? 'Kuruluyor…'
  if (j.status === 'done') return j.version ? `Kuruldu (${j.version})` : 'Kuruldu'
  if (j.status === 'error') return `Hata: ${j.error ?? ''}`
  const pct = j.total ? Math.floor((j.received / j.total) * 100) : 0
  const parts = [`%${pct}`, `${formatBytes(j.received)} / ${j.total ? formatBytes(j.total) : '?'}`]
  if (j.speed) parts.push(`${formatBytes(j.speed)}/s`)
  const eta = formatEta(j.etaSec)
  if (eta) parts.push(eta)
  return parts.join(' · ')
}

export function formatDate(ts?: number): string {
  if (!ts) return '—'
  return new Date(ts).toLocaleString('tr-TR', { day: 'numeric', month: 'long', year: 'numeric', hour: '2-digit', minute: '2-digit' })
}

export function appIconUrl(id: string, rev?: number): string {
  return `${MEDIA_SCHEME}://appicon/${id.replace(/[^a-z0-9_-]/gi, '_')}.png?v=${rev ?? 0}`
}

/** Whether DRM apps open inside the console (Widevine present and the user's choice). */
export function drmInConsole(state: AppState): boolean {
  const mode = state.settings.drmMode
  if (mode === 'edge' || !state.drm.widevine) return false
  return mode === 'console' || state.drm.inConsole
}

