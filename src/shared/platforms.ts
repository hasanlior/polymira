import type { GameKind, PlatformFamily, SupportLevel } from './types'

/**
 * Emulator definitions.
 *
 * `args` is a template. Tokens:
 *   {rom}   absolute path to the game file/folder (or the title id for `titleId` games)
 *   {core}  absolute path to a libretro core (RetroArch only)
 *
 * Adding an emulator = adding an entry here. Nothing else in the app needs to change.
 */
export interface EmulatorDef {
  id: string
  name: string
  /** Executable file names to look for during auto-detection (case-insensitive). */
  exeNames: string[]
  args: string[]
  homepage: string
  notes?: string
}

export const EMULATORS: EmulatorDef[] = [
  {
    id: 'retroarch',
    name: 'RetroArch',
    exeNames: ['retroarch.exe'],
    args: ['-f', '-L', '{core}', '{rom}'],
    homepage: 'https://www.retroarch.com',
    notes: 'Çekirdekler RetroArch → Online Updater → Core Downloader üzerinden yüklenir.'
  },
  {
    id: 'duckstation',
    name: 'DuckStation',
    exeNames: ['duckstation-qt-x64-ReleaseLTCG.exe', 'duckstation-qt.exe'],
    args: ['-batch', '-fullscreen', '--', '{rom}'],
    homepage: 'https://www.duckstation.org',
    notes: 'Kendi konsolunuzdan alınmış bir PS1 BIOS gerektirir.'
  },
  {
    id: 'pcsx2',
    name: 'PCSX2',
    exeNames: ['pcsx2-qt.exe', 'pcsx2-qtx64-avx2.exe', 'pcsx2-qtx64.exe'],
    args: ['-batch', '-nogui', '-fullscreen', '--', '{rom}'],
    homepage: 'https://pcsx2.net',
    notes: 'Kendi konsolunuzdan alınmış bir PS2 BIOS gerektirir.'
  },
  {
    id: 'ppsspp',
    name: 'PPSSPP',
    exeNames: ['PPSSPPWindows64.exe', 'PPSSPPWindows.exe'],
    args: ['--fullscreen', '{rom}'],
    homepage: 'https://www.ppsspp.org'
  },
  {
    id: 'rpcs3',
    name: 'RPCS3',
    exeNames: ['rpcs3.exe'],
    args: ['--no-gui', '--fullscreen', '{rom}'],
    homepage: 'https://rpcs3.net',
    notes: 'Önce resmi PS3 sistem yazılımını RPCS3 içine kurun. Uyumluluk oyuna göre değişir.'
  },
  {
    id: 'vita3k',
    name: 'Vita3K',
    exeNames: ['Vita3K.exe'],
    args: ['-F', '-r', '{rom}'],
    homepage: 'https://vita3k.org',
    notes: 'Oyunlar Vita3K içine kurulmalıdır. Title ID ile başlatılır.'
  },
  {
    id: 'shadps4',
    name: 'shadPS4',
    exeNames: ['shadPS4.exe'],
    args: ['-g', '{rom}', '-f', 'true'],
    homepage: 'https://shadps4.net',
    notes: 'Erken aşamada bir emülatör. Yalnızca az sayıda oyun oynanabilir.'
  },
  {
    id: 'xemu',
    name: 'xemu',
    exeNames: ['xemu.exe'],
    args: ['-full-screen', '-dvd_path', '{rom}'],
    homepage: 'https://xemu.app',
    notes: 'Kendi konsolunuzdan alınmış MCPX boot ROM, BIOS ve HDD imajı gerektirir.'
  },
  {
    id: 'xenia',
    name: 'Xenia Canary',
    exeNames: ['xenia_canary.exe', 'xenia.exe'],
    args: ['--fullscreen', '{rom}'],
    homepage: 'https://xenia.jp',
    notes: 'Uyumluluk oyuna göre değişir.'
  },
  {
    id: 'dolphin',
    name: 'Dolphin',
    exeNames: ['Dolphin.exe'],
    args: ['-b', '-C', 'Dolphin.Display.Fullscreen=True', '-e', '{rom}'],
    homepage: 'https://dolphin-emu.org'
  },
  {
    id: 'cemu',
    name: 'Cemu',
    exeNames: ['Cemu.exe'],
    args: ['-f', '-g', '{rom}'],
    homepage: 'https://cemu.info',
    notes: 'Şifreli oyunlar için kendi konsolunuzdan alınmış anahtarlar gerekir.'
  },
  {
    id: 'ryujinx',
    name: 'Ryujinx',
    exeNames: ['Ryujinx.exe'],
    args: ['--fullscreen', '{rom}'],
    homepage: 'https://ryujinx.app',
    notes: 'Kendi konsolunuzdan alınmış anahtar ve sistem yazılımı gerektirir.'
  },
  {
    id: 'melonds',
    name: 'melonDS',
    exeNames: ['melonDS.exe'],
    args: ['-f', '{rom}'],
    homepage: 'https://melonds.kuribo64.net'
  },
  {
    id: 'azahar',
    name: 'Azahar',
    exeNames: ['azahar.exe', 'azahar-qt.exe', 'lime3ds.exe', 'citra-qt.exe'],
    args: ['-f', '{rom}'],
    homepage: 'https://azahar-emu.org'
  },
  {
    id: 'flycast',
    name: 'Flycast',
    exeNames: ['flycast.exe'],
    args: ['{rom}'],
    homepage: 'https://github.com/flyinghead/flycast'
  }
]

/** A way to run a platform: an emulator, plus a libretro core when the emulator is RetroArch. */
export interface Runner {
  emulatorId: string
  core?: string
}

export interface PlatformDef {
  id: string
  name: string
  /** Short label used on icons. */
  short: string
  family: PlatformFamily
  support: SupportLevel
  kind: GameKind
  /** Lower-case extensions including the dot. Empty for folder/titleId platforms. */
  extensions: string[]
  /** Extensions that only this platform uses; safe for automatic platform guessing. */
  uniqueExtensions?: string[]
  /** Words that, when found in a folder name, hint this platform during auto scans. */
  folderHints?: string[]
  runners: Runner[]
  /** libretro-thumbnails system folder name, used for automatic cover lookup. */
  thumbnails?: string
  note?: string
  /** Theme hue (0-360) used by the animated background when this platform is focused. */
  hue: number
}

const disc = ['.cue', '.chd', '.iso', '.m3u']

export const PLATFORMS: PlatformDef[] = [
  // ── PlayStation ──────────────────────────────────────────────
  {
    id: 'ps1', name: 'PlayStation', short: 'PS1', family: 'playstation', support: 'stable', kind: 'file',
    extensions: [...disc, '.pbp', '.ecm', '.img', '.mds'], folderHints: ['ps1', 'psx', 'playstation 1'],
    runners: [{ emulatorId: 'duckstation' }, { emulatorId: 'retroarch', core: 'swanstation' }, { emulatorId: 'retroarch', core: 'mednafen_psx_hw' }],
    thumbnails: 'Sony - PlayStation', hue: 215
  },
  {
    id: 'ps2', name: 'PlayStation 2', short: 'PS2', family: 'playstation', support: 'stable', kind: 'file',
    extensions: ['.iso', '.chd', '.cso', '.zso', '.gz', '.bin', '.mdf', '.elf'], folderHints: ['ps2', 'playstation 2'],
    runners: [{ emulatorId: 'pcsx2' }], thumbnails: 'Sony - PlayStation 2', hue: 225
  },
  {
    id: 'ps3', name: 'PlayStation 3', short: 'PS3', family: 'playstation', support: 'experimental', kind: 'folder',
    extensions: [], folderHints: ['ps3', 'playstation 3'],
    runners: [{ emulatorId: 'rpcs3' }], thumbnails: 'Sony - PlayStation 3', hue: 235,
    note: 'Oyun klasörleri (PS3_GAME/USRDIR/EBOOT.BIN) veya kurulu oyunlar (dev_hdd0/game). Her oyun çalışmaz.'
  },
  {
    id: 'ps4', name: 'PlayStation 4', short: 'PS4', family: 'playstation', support: 'experimental', kind: 'folder',
    extensions: [], folderHints: ['ps4', 'playstation 4'],
    runners: [{ emulatorId: 'shadps4' }], hue: 210,
    note: 'Emülasyon erken aşamada. Oyunların çoğu henüz çalışmıyor.'
  },
  {
    id: 'ps5', name: 'PlayStation 5', short: 'PS5', family: 'playstation', support: 'unsupported', kind: 'file',
    extensions: [], runners: [], hue: 220,
    note: 'Çalışan bir PS5 emülatörü yok. PC sürümleri PC Oyunları altına eklenebilir.'
  },
  {
    id: 'psp', name: 'PlayStation Portable', short: 'PSP', family: 'playstation', support: 'stable', kind: 'file',
    extensions: ['.iso', '.cso', '.pbp', '.chd'], folderHints: ['psp'],
    runners: [{ emulatorId: 'ppsspp' }, { emulatorId: 'retroarch', core: 'ppsspp' }],
    thumbnails: 'Sony - PlayStation Portable', hue: 200
  },
  {
    id: 'vita', name: 'PlayStation Vita', short: 'VITA', family: 'playstation', support: 'experimental', kind: 'titleId',
    extensions: [], folderHints: ['vita', 'ux0'],
    runners: [{ emulatorId: 'vita3k' }], thumbnails: 'Sony - PlayStation Vita', hue: 195,
    note: 'Tarayıcıyı Vita3K ux0/app klasörüne yönlendirin. Oyunlar Title ID ile başlatılır.'
  },
  // ── Xbox ─────────────────────────────────────────────────────
  {
    id: 'xbox', name: 'Xbox', short: 'XBOX', family: 'xbox', support: 'experimental', kind: 'file',
    extensions: ['.iso', '.xiso'], uniqueExtensions: ['.xiso'], folderHints: ['xbox', 'original xbox'],
    runners: [{ emulatorId: 'xemu' }], thumbnails: 'Microsoft - Xbox', hue: 110
  },
  {
    id: 'x360', name: 'Xbox 360', short: '360', family: 'xbox', support: 'experimental', kind: 'file',
    extensions: ['.iso', '.xex', '.zar'], uniqueExtensions: ['.xex', '.zar'], folderHints: ['360', 'x360', 'xbox 360'],
    runners: [{ emulatorId: 'xenia' }], thumbnails: 'Microsoft - Xbox 360', hue: 100
  },
  {
    id: 'xone', name: 'Xbox One', short: 'ONE', family: 'xbox', support: 'unsupported', kind: 'file',
    extensions: [], runners: [], hue: 120,
    note: 'Xbox One emülatörü yok. Birçok oyunun PC sürümü var; PC Oyunları altına ekleyin.'
  },
  {
    id: 'xseries', name: 'Xbox Series X|S', short: 'XS', family: 'xbox', support: 'unsupported', kind: 'file',
    extensions: [], runners: [], hue: 125,
    note: 'Xbox Series emülatörü yok. Birçok oyunun PC sürümü var; PC Oyunları altına ekleyin.'
  },
  // ── Nintendo ─────────────────────────────────────────────────
  {
    id: 'nes', name: 'NES', short: 'NES', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.nes', '.fds', '.unf', '.zip', '.7z'], uniqueExtensions: ['.nes', '.fds', '.unf'], folderHints: ['nes', 'famicom'],
    runners: [{ emulatorId: 'retroarch', core: 'mesen' }, { emulatorId: 'retroarch', core: 'fceumm' }],
    thumbnails: 'Nintendo - Nintendo Entertainment System', hue: 0
  },
  {
    id: 'snes', name: 'Super Nintendo', short: 'SNES', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.sfc', '.smc', '.fig', '.zip', '.7z'], uniqueExtensions: ['.sfc', '.smc', '.fig'], folderHints: ['snes', 'super nintendo', 'super famicom'],
    runners: [{ emulatorId: 'retroarch', core: 'snes9x' }],
    thumbnails: 'Nintendo - Super Nintendo Entertainment System', hue: 275
  },
  {
    id: 'n64', name: 'Nintendo 64', short: 'N64', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.n64', '.z64', '.v64', '.zip', '.7z'], uniqueExtensions: ['.n64', '.z64', '.v64'], folderHints: ['n64', 'nintendo 64'],
    runners: [{ emulatorId: 'retroarch', core: 'mupen64plus_next' }],
    thumbnails: 'Nintendo - Nintendo 64', hue: 140
  },
  {
    id: 'gc', name: 'GameCube', short: 'GC', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.rvz', '.gcm', '.gcz', '.iso', '.ciso'], uniqueExtensions: ['.gcm', '.gcz'], folderHints: ['gamecube', 'gc', 'ngc'],
    runners: [{ emulatorId: 'dolphin' }], thumbnails: 'Nintendo - GameCube', hue: 265
  },
  {
    id: 'wii', name: 'Wii', short: 'WII', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.wbfs', '.rvz', '.iso', '.wad'], uniqueExtensions: ['.wbfs', '.wad'], folderHints: ['wii'],
    runners: [{ emulatorId: 'dolphin' }], thumbnails: 'Nintendo - Wii', hue: 190
  },
  {
    id: 'wiiu', name: 'Wii U', short: 'WIIU', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.wua', '.wud', '.wux', '.rpx'], uniqueExtensions: ['.wua', '.wud', '.wux', '.rpx'], folderHints: ['wii u', 'wiiu'],
    runners: [{ emulatorId: 'cemu' }], thumbnails: 'Nintendo - Wii U', hue: 185
  },
  {
    id: 'switch', name: 'Nintendo Switch', short: 'NSW', family: 'nintendo', support: 'experimental', kind: 'file',
    extensions: ['.nsp', '.xci'], uniqueExtensions: ['.nsp', '.xci'], folderHints: ['switch'],
    runners: [{ emulatorId: 'ryujinx' }], hue: 355,
    note: 'Kendi konsolunuzdan alınmış anahtar ve sistem yazılımı gerektirir.'
  },
  {
    id: 'gb', name: 'Game Boy', short: 'GB', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.gb', '.zip', '.7z'], uniqueExtensions: ['.gb'], folderHints: ['game boy', 'gameboy', 'gb'],
    runners: [{ emulatorId: 'retroarch', core: 'gambatte' }, { emulatorId: 'retroarch', core: 'mgba' }],
    thumbnails: 'Nintendo - Game Boy', hue: 80
  },
  {
    id: 'gbc', name: 'Game Boy Color', short: 'GBC', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.gbc', '.zip', '.7z'], uniqueExtensions: ['.gbc'], folderHints: ['gbc', 'game boy color'],
    runners: [{ emulatorId: 'retroarch', core: 'gambatte' }, { emulatorId: 'retroarch', core: 'mgba' }],
    thumbnails: 'Nintendo - Game Boy Color', hue: 300
  },
  {
    id: 'gba', name: 'Game Boy Advance', short: 'GBA', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.gba', '.zip', '.7z'], uniqueExtensions: ['.gba'], folderHints: ['gba', 'game boy advance'],
    runners: [{ emulatorId: 'retroarch', core: 'mgba' }],
    thumbnails: 'Nintendo - Game Boy Advance', hue: 250
  },
  {
    id: 'nds', name: 'Nintendo DS', short: 'NDS', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.nds', '.zip', '.7z'], uniqueExtensions: ['.nds'], folderHints: ['nds', 'nintendo ds'],
    runners: [{ emulatorId: 'melonds' }, { emulatorId: 'retroarch', core: 'melondsds' }],
    thumbnails: 'Nintendo - Nintendo DS', hue: 210
  },
  {
    id: '3ds', name: 'Nintendo 3DS', short: '3DS', family: 'nintendo', support: 'stable', kind: 'file',
    extensions: ['.3ds', '.cci', '.cxi', '.3dsx', '.cia'], uniqueExtensions: ['.3ds', '.cci', '.cxi', '.3dsx', '.cia'], folderHints: ['3ds'],
    runners: [{ emulatorId: 'azahar' }], thumbnails: 'Nintendo - Nintendo 3DS', hue: 5
  },
  // ── Sega ─────────────────────────────────────────────────────
  {
    id: 'sms', name: 'Master System', short: 'SMS', family: 'sega', support: 'stable', kind: 'file',
    extensions: ['.sms', '.zip', '.7z'], uniqueExtensions: ['.sms'], folderHints: ['master system', 'sms'],
    runners: [{ emulatorId: 'retroarch', core: 'genesis_plus_gx' }],
    thumbnails: 'Sega - Master System - Mark III', hue: 205
  },
  {
    id: 'md', name: 'Mega Drive', short: 'MD', family: 'sega', support: 'stable', kind: 'file',
    extensions: ['.md', '.gen', '.smd', '.bin', '.zip', '.7z'], uniqueExtensions: ['.md', '.gen', '.smd'], folderHints: ['mega drive', 'megadrive', 'genesis'],
    runners: [{ emulatorId: 'retroarch', core: 'genesis_plus_gx' }],
    thumbnails: 'Sega - Mega Drive - Genesis', hue: 230
  },
  {
    id: 'saturn', name: 'Saturn', short: 'SAT', family: 'sega', support: 'stable', kind: 'file',
    extensions: [...disc], folderHints: ['saturn'],
    runners: [{ emulatorId: 'retroarch', core: 'mednafen_saturn' }],
    thumbnails: 'Sega - Saturn', hue: 240, note: 'Kendi konsolunuzdan alınmış bir Saturn BIOS gerektirir.'
  },
  {
    id: 'dc', name: 'Dreamcast', short: 'DC', family: 'sega', support: 'stable', kind: 'file',
    extensions: ['.gdi', '.cdi', '.chd'], uniqueExtensions: ['.gdi', '.cdi'], folderHints: ['dreamcast', 'dc'],
    runners: [{ emulatorId: 'flycast' }, { emulatorId: 'retroarch', core: 'flycast' }],
    thumbnails: 'Sega - Dreamcast', hue: 25
  },
  {
    id: 'gg', name: 'Game Gear', short: 'GG', family: 'sega', support: 'stable', kind: 'file',
    extensions: ['.gg', '.zip', '.7z'], uniqueExtensions: ['.gg'], folderHints: ['game gear', 'gamegear'],
    runners: [{ emulatorId: 'retroarch', core: 'genesis_plus_gx' }],
    thumbnails: 'Sega - Game Gear', hue: 45
  },
  // ── Other platforms ──────────────────────────────────────────
  {
    id: 'arcade', name: 'Arcade', short: 'ARC', family: 'other', support: 'stable', kind: 'file',
    extensions: ['.zip', '.7z'], folderHints: ['arcade', 'mame', 'fbneo'],
    runners: [{ emulatorId: 'retroarch', core: 'fbneo' }, { emulatorId: 'retroarch', core: 'mame' }],
    thumbnails: 'FBNeo - Arcade Games', hue: 330, note: 'ROM setleri çekirdek sürümüyle eşleşmelidir.'
  },
  {
    id: 'neogeo', name: 'Neo Geo', short: 'NEO', family: 'other', support: 'stable', kind: 'file',
    extensions: ['.zip', '.7z'], folderHints: ['neo geo', 'neogeo'],
    runners: [{ emulatorId: 'retroarch', core: 'fbneo' }],
    thumbnails: 'SNK - Neo Geo', hue: 40, note: 'Oyunların yanında neogeo.zip BIOS dosyası gerekir.'
  },
  {
    id: 'a2600', name: 'Atari 2600', short: '2600', family: 'other', support: 'stable', kind: 'file',
    extensions: ['.a26', '.zip', '.7z'], uniqueExtensions: ['.a26'], folderHints: ['2600'],
    runners: [{ emulatorId: 'retroarch', core: 'stella' }], thumbnails: 'Atari - 2600', hue: 15
  },
  {
    id: 'a7800', name: 'Atari 7800', short: '7800', family: 'other', support: 'stable', kind: 'file',
    extensions: ['.a78', '.zip', '.7z'], uniqueExtensions: ['.a78'], folderHints: ['7800'],
    runners: [{ emulatorId: 'retroarch', core: 'prosystem' }], thumbnails: 'Atari - 7800', hue: 20
  },
  {
    id: 'lynx', name: 'Atari Lynx', short: 'LYNX', family: 'other', support: 'stable', kind: 'file',
    extensions: ['.lnx', '.zip', '.7z'], uniqueExtensions: ['.lnx'], folderHints: ['lynx'],
    runners: [{ emulatorId: 'retroarch', core: 'handy' }], thumbnails: 'Atari - Lynx', hue: 50
  },
  {
    id: 'msx', name: 'MSX', short: 'MSX', family: 'other', support: 'stable', kind: 'file',
    extensions: ['.rom', '.mx1', '.mx2', '.dsk', '.cas', '.zip'], uniqueExtensions: ['.mx1', '.mx2'], folderHints: ['msx'],
    runners: [{ emulatorId: 'retroarch', core: 'bluemsx' }], thumbnails: 'Microsoft - MSX', hue: 180
  },
  {
    id: 'c64', name: 'Commodore 64', short: 'C64', family: 'other', support: 'stable', kind: 'file',
    extensions: ['.d64', '.t64', '.prg', '.crt', '.tap', '.zip'], uniqueExtensions: ['.d64', '.t64'], folderHints: ['c64', 'commodore'],
    runners: [{ emulatorId: 'retroarch', core: 'vice_x64sc' }], thumbnails: 'Commodore - 64', hue: 170
  },
  // ── PC ───────────────────────────────────────────────────────
  {
    id: 'pc', name: 'PC Oyunları', short: 'PC', family: 'pc', support: 'stable', kind: 'exe',
    extensions: ['.exe', '.lnk', '.url', '.bat'], runners: [], hue: 260
  }
]

export interface FamilyDef {
  id: PlatformFamily
  name: string
  hue: number
}

export const FAMILIES: FamilyDef[] = [
  { id: 'playstation', name: 'PlayStation', hue: 220 },
  { id: 'xbox', name: 'Xbox', hue: 110 },
  { id: 'nintendo', name: 'Nintendo', hue: 355 },
  { id: 'sega', name: 'Sega', hue: 215 },
  { id: 'other', name: 'Diğer Platformlar', hue: 30 },
  { id: 'pc', name: 'PC Oyunları', hue: 265 }
]

const platformMap = new Map(PLATFORMS.map((p) => [p.id, p]))
const emulatorMap = new Map(EMULATORS.map((e) => [e.id, e]))

export function getPlatform(id: string): PlatformDef | undefined {
  return platformMap.get(id)
}

export function getEmulator(id: string): EmulatorDef | undefined {
  return emulatorMap.get(id)
}

/** The runner a platform will use, honouring the user's choice when it is valid. */
export function resolveRunner(platform: PlatformDef, chosen?: string): Runner | undefined {
  if (chosen) {
    const match = platform.runners.find((r) => runnerKey(r) === chosen)
    if (match) return match
  }
  return platform.runners[0]
}

export function runnerKey(r: Runner): string {
  return r.core ? `${r.emulatorId}:${r.core}` : r.emulatorId
}

export function runnerLabel(r: Runner): string {
  const emu = getEmulator(r.emulatorId)?.name ?? r.emulatorId
  return r.core ? `${emu} (${r.core})` : emu
}
