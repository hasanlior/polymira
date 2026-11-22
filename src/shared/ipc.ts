import type { StoreItem } from './catalog'
import type { AppState, DirEntry, DiscoverResult, Game, PerfSnapshot, ScanReport, Screenshot, Settings, Toast } from './types'

/** Renderer → main request/response channels. The single source of truth for the IPC surface. */
export interface IpcContract {
  'state:get': () => AppState
  'settings:set': (patch: Partial<Settings>) => Settings
  'library:addFolder': (folder: string, platformId: string) => ScanReport
  'library:removeFolder': (folder: string) => number
  'library:rescan': () => ScanReport
  'library:addFile': (file: string, platformId: string) => Game
  'library:remove': (id: string) => void
  'library:favorite': (id: string, favorite: boolean) => void
  'library:move': (id: string, platformId: string) => void
  'library:setCover': (id: string, image: string) => void
  'library:setBackground': (id: string, image: string) => void
  'library:setMusic': (id: string, file: string | null) => void
  'library:fetchCovers': () => number
  'fs:list': (dir: string | null) => { path: string | null; parent: string | null; entries: DirEntry[] }
  'dialog:folder': () => string | null
  'dialog:file': (kind: 'exe' | 'image' | 'any') => string | null
  'emu:detect': (extraRoot?: string) => number
  'emu:setPath': (id: string, exe: string | null) => void
  'emu:download': (ids: string[]) => number
  'emu:cancelDownload': (id: string) => void
  'game:launch': (id: string) => void
  'session:pause': (paused: boolean) => boolean
  'session:stop': () => void
  'session:screenshot': () => Screenshot | null
  'session:home': () => void
  'session:returnToGame': () => void
  'session:perf': () => PerfSnapshot
  'overlay:close': () => void
  'media:screenshots': (gameId?: string) => Screenshot[]
  'media:deleteScreenshot': (file: string) => void
  'discover:run': () => DiscoverResult
  'discover:apply': (keys: string[]) => { games: number; emulators: number; folders: number }
  'store:catalog': (refresh?: boolean) => StoreItem[]
  'store:install': (id: string) => void
  'store:uninstall': (id: string) => void
  'mods:css': () => string
  'mods:toggle': (id: string, enabled: boolean) => void
  'folder:open': (which: 'mods' | 'themes') => void
  'app:quit': () => void
}

export type IpcChannel = keyof IpcContract

export const IPC_CHANNELS: IpcChannel[] = [
  'state:get',
  'settings:set',
  'library:addFolder',
  'library:removeFolder',
  'library:rescan',
  'library:addFile',
  'library:remove',
  'library:favorite',
  'library:move',
  'library:setCover',
  'library:setBackground',
  'library:setMusic',
  'library:fetchCovers',
  'fs:list',
  'dialog:folder',
  'dialog:file',
  'emu:detect',
  'emu:setPath',
  'emu:download',
  'emu:cancelDownload',
  'game:launch',
  'session:pause',
  'session:stop',
  'session:screenshot',
  'session:home',
  'session:returnToGame',
  'session:perf',
  'overlay:close',
  'media:screenshots',
  'media:deleteScreenshot',
  'discover:run',
  'discover:apply',
  'store:catalog',
  'store:install',
  'store:uninstall',
  'mods:css',
  'mods:toggle',
  'folder:open',
  'app:quit'
]

/** Main → renderer push events. */
export interface IpcEvents {
  state: AppState
  toast: Toast
  'overlay:opened': null
  /** Progress text for long tasks (PC scan). */
  progress: string
  /** Shutdown sequence: what is happening right now; `done` when the app is about to close. */
  shutdown: { text: string; done?: boolean }
}

export type IpcEvent = keyof IpcEvents
export const IPC_EVENTS: IpcEvent[] = ['state', 'toast', 'overlay:opened', 'progress', 'shutdown']

/** Custom protocol used to serve covers and screenshots to the renderer. */
export const MEDIA_SCHEME = 'app-media'

export interface Bridge {
  invoke<C extends IpcChannel>(channel: C, ...args: Parameters<IpcContract[C]>): Promise<ReturnType<IpcContract[C]>>
  on<E extends IpcEvent>(event: E, cb: (payload: IpcEvents[E]) => void): () => void
}
