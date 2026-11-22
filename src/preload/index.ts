import { contextBridge, ipcRenderer, type IpcRendererEvent } from 'electron'
import { IPC_CHANNELS, IPC_EVENTS, type Bridge } from '@shared/ipc'

const channels = new Set<string>(IPC_CHANNELS)
const events = new Set<string>(IPC_EVENTS)

const bridge: Bridge = {
  invoke(channel, ...args) {
    if (!channels.has(channel)) return Promise.reject(new Error(`Unknown channel ${channel}`))
    return ipcRenderer.invoke(channel, ...args)
  },
  on(event, cb) {
    if (!events.has(event)) throw new Error(`Unknown event ${event}`)
    const listener = (_e: IpcRendererEvent, payload: unknown) => cb(payload as never)
    ipcRenderer.on(event, listener)
    return () => ipcRenderer.removeListener(event, listener)
  }
}

contextBridge.exposeInMainWorld('bridge', bridge)
