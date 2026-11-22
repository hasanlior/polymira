import { app } from 'electron'
import fs from 'fs'
import path from 'path'
import type { Game, Settings } from '@shared/types'

export const DEFAULT_SETTINGS: Settings = {
  windowMode: 'fullscreen',
  animations: true,
  theme: 'auto',
  menuSounds: true,
  soundVolume: 0.5,
  downloadCovers: true,
  scanOnStartup: true,
  pauseOnOverlay: true,
  overlayHotkey: 'both',
  watchedFolders: [],
  emulatorPaths: {},
  platformEmulator: {},
  categoryOrder: [],
  hiddenCategories: [],
  setupDone: false,
  gmPriority: true,
  gmPowerPlan: true,
  gmStopExplorer: false,
  drmMode: 'auto',
  autoRestart: true,
  launchOnStartup: false,
  gameBackgrounds: true,
  gameMusic: true,
  musicVolume: 0.6,
  gameTrailers: true,
  installedThemes: [],
  enabledMods: [],
  storeCatalogUrl: ''
}

export const paths = {
  get root() {
    return app.getPath('userData')
  },
  get covers() {
    return path.join(app.getPath('userData'), 'covers')
  },
  get screenshots() {
    return path.join(app.getPath('userData'), 'screenshots')
  },
  get cache() {
    return path.join(app.getPath('userData'), 'cache')
  }
}

function readJson<T>(file: string, fallback: T): T {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8')) as T
  } catch {
    return fallback
  }
}

/** Write to a temp file then rename, so a crash never leaves a half-written file. */
function writeJsonAtomic(file: string, data: unknown): void {
  fs.mkdirSync(path.dirname(file), { recursive: true })
  const tmp = `${file}.tmp`
  fs.writeFileSync(tmp, JSON.stringify(data, null, 2), 'utf8')
  fs.renameSync(tmp, file)
}

class JsonStore<T> {
  private timer: NodeJS.Timeout | null = null
  data: T

  constructor(
    private file: string,
    fallback: T
  ) {
    this.data = readJson(file, fallback)
  }

  /** Debounced save; call `flush()` before quitting. */
  save(): void {
    if (this.timer) clearTimeout(this.timer)
    this.timer = setTimeout(() => this.flush(), 300)
  }

  flush(): void {
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
    writeJsonAtomic(this.file, this.data)
  }
}

let settingsStore: JsonStore<Settings>
let libraryStore: JsonStore<{ games: Game[] }>

export function initStores(): void {
  fs.mkdirSync(paths.covers, { recursive: true })
  fs.mkdirSync(paths.screenshots, { recursive: true })
  fs.mkdirSync(paths.cache, { recursive: true })
  settingsStore = new JsonStore(path.join(paths.root, 'settings.json'), { ...DEFAULT_SETTINGS })
  settingsStore.data = { ...DEFAULT_SETTINGS, ...settingsStore.data }
  libraryStore = new JsonStore(path.join(paths.root, 'library.json'), { games: [] })
}

export const store = {
  get settings(): Settings {
    return settingsStore.data
  },
  updateSettings(patch: Partial<Settings>): Settings {
    settingsStore.data = { ...settingsStore.data, ...patch }
    settingsStore.save()
    return settingsStore.data
  },
  get games(): Game[] {
    return libraryStore.data.games
  },
  setGames(games: Game[]): void {
    libraryStore.data.games = games
    libraryStore.save()
  },
  updateGame(id: string, patch: Partial<Game>): Game | undefined {
    const g = libraryStore.data.games.find((x) => x.id === id)
    if (!g) return undefined
    Object.assign(g, patch)
    libraryStore.save()
    return g
  },
  flush(): void {
    settingsStore?.flush()
    libraryStore?.flush()
  }
}
