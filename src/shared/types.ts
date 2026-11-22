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
  /** When game information was last looked up online. */
  infoCheckedAt?: number
  /** Screenshot URLs from the store page (PC games). */
  onlineShots?: string[]
  /** Per-game emulator choice (runner key), overriding the platform default. */
  emulator?: string
  /** Per-game emulator options. */
  emuOptions?: GameEmuOptions
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
  /** Button symbols: follow the controller in use, or always one style. */
  padStyle: 'auto' | 'xbox' | 'ps' | 'switch' | 'keyboard'
  /** Remapped buttons: action → standard-mapping button index. */
  padMap: Record<string, number>
  /** Short rumble on confirm. */
  padRumble: boolean
  /** Show notification cards over a running game. */
  notifyInGame: boolean
  /** Back up an emulator's saves after each play session. */
  saveAutoBackup: boolean
  /** Folder that receives a copy of every save backup (e.g. inside OneDrive). Empty = off. */
  saveSyncFolder: string
  /** Interface language. */
  language: 'tr' | 'en'
  /** RetroAchievements account (username + web API key) for emulator trophies. */
  raUser: string
  raKey: string
  /** Write controller settings into emulators before launching, so they work with no setup. */
  autoPadConfig: boolean
  /** Emulators whose controller settings were written (id → exe path). */
  padConfigured: Record<string, string>
  /** Check GitHub Releases for a new version on start. */
  autoUpdate: boolean
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
  /** Where DRM apps (Netflix…) open: auto = in the console when the build is VMP-signed, else Edge. */
  drmMode: 'auto' | 'console' | 'edge'
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
  /** Recent notifications, newest first. */
  notices: Notice[]
  saves: SaveInfo[]
  update: UpdateState
  drm: { widevine: boolean; vmp: boolean; inConsole: boolean; detail: string }
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

/** Per-game emulator options (RetroArch: shader and resolution; others: extra arguments). */
export interface GameEmuOptions {
  shader?: 'none' | 'crt' | 'lcd' | 'smooth'
  /** Extra command line arguments appended for this game. */
  extraArgs?: string
}

export interface UpdateState {
  status: 'idle' | 'disabled' | 'checking' | 'none' | 'downloading' | 'ready' | 'error'
  version?: string
  notes?: string
  progress?: number
  message?: string
}

export interface Trophy {
  id: string
  title: string
  description: string
  icon?: string
  unlocked: boolean
  unlockedAt?: number
  hidden?: boolean
}

/** A game that has trophies, with the user's progress. */
export interface TrophyGame {
  /** "steam:<appid>" or "ra:<game id>" */
  key: string
  source: 'steam' | 'ra'
  title: string
  subtitle?: string
  /** Matching library game, if any. */
  gameId?: string
  unlocked: number
  total: number
  icon?: string
}

export interface PlayStats {
  /** Last 7 days, oldest first. */
  days: { day: number; sec: number }[]
  top: { gameId: string; title: string; sec: number; weekSec: number; sessions: number }[]
  weekSec: number
  totalSec: number
  sessions: number
  gamesPlayed: number
}

export interface SaveBackup {
  file: string
  name: string
  at: number
  size: number
  /** Also present in the sync folder. */
  synced: boolean
}

/** Save data of one emulator: where it lives and its backups. */
export interface SaveInfo {
  emuId: string
  name: string
  locations: string[]
  backups: SaveBackup[]
}

/** PS3-style notification card (top right), also shown over a running game. */
export interface Notice {
  id: number
  kind: 'download' | 'trophy' | 'update' | 'save' | 'info'
  title: string
  text?: string
  /** Optional image URL (app-media://… or https://…). */
  icon?: string
  at: number
}

export interface Toast {
  kind: 'info' | 'success' | 'warning' | 'error'
  text: string
}
