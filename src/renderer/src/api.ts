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

export function formatDate(ts?: number): string {
  if (!ts) return '—'
  return new Date(ts).toLocaleString('tr-TR', { day: 'numeric', month: 'long', year: 'numeric', hour: '2-digit', minute: '2-digit' })
}
