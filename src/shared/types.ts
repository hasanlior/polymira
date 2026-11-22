export type PlatformFamily = 'playstation' | 'xbox' | 'nintendo' | 'sega' | 'other' | 'pc'

/** How mature the emulation path for a platform is. Shown to the user as-is. */
export type SupportLevel = 'stable' | 'experimental' | 'unsupported'

/** What a library entry points at. */
export type GameKind = 'file' | 'folder' | 'titleId' | 'exe'

export interface GameMeta {
  year?: number
  genre?: string
  developer?: string
  publisher?: string
  description?: string
  titleId?: string
  region?: string
}

export type LaunchResult = 'ok' | 'crashed' | 'error'

export interface Game {
  id: string
  platformId: string
  title: string
  path: string
  kind: GameKind
  addedAt: number
  lastPlayedAt?: number
  playTimeSec: number
  playCount: number
  favorite: boolean
  hasCover: boolean
  /** Cache-busting token for the cover image. */
  coverRev?: number
  missing?: boolean
  meta: GameMeta
  lastLaunch?: { at: number; result: LaunchResult; message?: string }
}

export interface WatchedFolder {
  path: string
  /** A platform id, or 'auto' to guess from extensions and folder names. */
  platformId: string
}

export type WindowMode = 'fullscreen' | 'borderless' | 'windowed'
export type OverlayHotkey = 'guide' | 'back-start' | 'both'

export interface Settings {
  windowMode: WindowMode
  animations: boolean
  theme: string
  menuSounds: boolean
  soundVolume: number
  downloadCovers: boolean
  scanOnStartup: boolean
  pauseOnOverlay: boolean
  overlayHotkey: OverlayHotkey
  watchedFolders: WatchedFolder[]
  emulatorPaths: Record<string, string>
  /** Per-platform emulator choice; falls back to the platform's first option. */
  platformEmulator: Record<string, string>
  categoryOrder: string[]
  hiddenCategories: string[]
}

export interface SessionInfo {
  gameId: string
  pid: number
  startedAt: number
  paused: boolean
  /** Time spent paused so far, in ms. */
  pausedMs: number
}

export interface PerfSnapshot {
  gameCpu: number | null
  gameMemMb: number | null
  sysCpu: number
  sysMemUsedMb: number
  sysMemTotalMb: number
  gpu: { util: number; memUsedMb: number; name: string } | null
  sessionSec: number
}

export interface EmulatorStatus {
  id: string
  name: string
  path: string | null
  found: boolean
}

export interface ScanReport {
  added: number
  skipped: number
  folders: number
  errors: string[]
}

export interface DirEntry {
  name: string
  path: string
  isDir: boolean
}

export interface Screenshot {
  gameId: string
  file: string
  url: string
  takenAt: number
}

export interface AppState {
  games: Game[]
  settings: Settings
  emulators: EmulatorStatus[]
  session: SessionInfo | null
  version: string
  scanning: boolean
}

export interface Toast {
  kind: 'info' | 'success' | 'warning' | 'error'
  text: string
}
