export type PlatformFamily = 'playstation' | 'xbox' | 'nintendo' | 'sega' | 'other' | 'pc' | 'apps'

/** How mature the emulation path for a platform is. Shown to the user as-is. */
export type SupportLevel = 'stable' | 'experimental' | 'unsupported'

/** What a library entry points at. `uri` = a store launcher link such as steam://rungameid/… */
export type GameKind = 'file' | 'folder' | 'titleId' | 'exe' | 'uri'

/** Where a PC game was found. */
export type GameSource = 'manual' | 'steam' | 'epic' | 'gog' | 'xbox' | 'riot' | 'ubisoft' | 'ea' | 'battlenet' | 'other'

export interface GameMeta {
  year?: number
  genre?: string
  developer?: string
  publisher?: string
  description?: string
  titleId?: string
  region?: string
  source?: GameSource
  /** Store-specific id (Steam app id, Epic app name, GOG game id). */
  storeId?: string
  /** Install folder of a store game; used to find its process after the launcher starts it. */
  installDir?: string
  /** Epic catalog namespace (used to find launcher artwork). */
  namespace?: string
  /** Steam app id found for this game (for PC games from other stores too). */
  steamAppId?: string
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
  /** The cover is only an app icon (no box art found): shown small, on a tile. */
  coverIsIcon?: boolean
  /** Full-screen background art shown when the game is focused. */
  hasBackground?: boolean
  bgRev?: number
  /** When artwork lookup last ran; avoids re-querying games that have no art online. */
  artCheckedAt?: number
  /** Artwork lookup version that last ran; a new app version with better sources re-checks. */
  artVersion?: number
  /** Theme music played while the game is focused (a local audio file). */
  musicPath?: string
  /** Official trailer stream (HLS) shown with sound when the game is focused. Empty string = none found. */
  trailerUrl?: string
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
  /** False until the first-run setup wizard is finished. */
  setupDone: boolean
  /** Game mode: raise the game's CPU priority. */
  gmPriority: boolean
  /** Game mode: switch to the High performance power plan while playing. */
  gmPowerPlan: boolean
  /** Game mode: close the Windows desktop shell (explorer.exe) while playing. */
  gmStopExplorer: boolean
  /** Relaunch the app automatically if it crashes. */
  autoRestart: boolean
  /** Start with Windows (packaged builds only). */
  launchOnStartup: boolean
  /** Show the focused game's artwork as the background. */
  gameBackgrounds: boolean
  /** Play the focused game's theme music. */
  gameMusic: boolean
  musicVolume: number
  /** Play the focused game's official trailer in the background. */
  gameTrailers: boolean
  /** Store themes the user added. */
  installedThemes: string[]
  /** Enabled CSS mods (folder names under mods/). */
  enabledMods: string[]
  /** Optional URL of a remote store catalog (JSON array of StoreItem). */
  storeCatalogUrl: string
}

export interface DiscoverGroup {
  /** 'steam' | 'epic' | 'gog' | 'roms:<platformId>' | 'emulators' */
  key: string
  label: string
  count: number
  /** A few example titles to show the user. */
  sample: string[]
}

export interface DiscoverResult {
  groups: DiscoverGroup[]
  romRoots: string[]
  durationMs: number
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

export interface DownloadJob {
  id: string
  name: string
  status: 'queued' | 'downloading' | 'extracting' | 'done' | 'error'
  received: number
  total: number
  /** Bytes per second (smoothed). */
  speed: number
  etaSec: number | null
  version?: string
  detail?: string
  error?: string
}

export interface ModInfo {
  id: string
  name: string
  description?: string
  enabled: boolean
}

export interface AppState {
  themes: import('./catalog').ThemeDef[]
  /** Store app id → cached icon revision. */
  appIcons: Record<string, number>
  mods: ModInfo[]
  downloads: DownloadJob[]
  /** Emulators that can be downloaded automatically. */
  downloadable: string[]
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
