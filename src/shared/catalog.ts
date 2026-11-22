/**
 * Store catalog. Built-in entries live here; more can come from a remote catalog URL
 * (Settings → storeCatalogUrl) that serves a JSON array of the same `StoreItem` shape,
 * so new apps, themes and partner content can be published without an app update.
 */

export interface ThemeDef {
  id: string
  name: string
  /** Background hue (0-360), or null to follow the focused category. */
  hue: number | null
  night?: boolean
  /** Optional extra CSS (custom properties etc.) applied while the theme is active. */
  css?: string
  /** Home screen layout this theme switches to. */
  uiStyle?: 'xmb' | 'aurora' | 'tiles' | 'shelf'
}

export interface StoreApp {
  /** Page opened full screen. */
  url: string
  /**
   * Needs DRM (Widevine/PlayReady). The built-in browser has no DRM module, so these open in
   * Microsoft Edge full screen, with the in-game menu on top like a game.
   */
  drm?: boolean
  /** Full-screen kiosk (no address bar). False for the general web browser. */
  kiosk?: boolean
  /** Custom user agent, e.g. a TV user agent for YouTube's TV interface. */
  userAgent?: string
}

/** A community mod: a CSS file that restyles the console. */
export interface StoreMod {
  /** URL of the mod's style.css (https). */
  css: string
  version?: string
}

export interface StoreItem {
  id: string
  kind: 'app' | 'theme' | 'mod'
  name: string
  description: string
  /** Short badge text and hue used as the item's icon. */
  short: string
  hue: number
  author?: string
  /** Optional icon image URL (PNG/JPEG). Without it the icon is read from the app's website. */
  icon?: string
  /** Store section the item is listed in (defaults by kind). */
  section?: string
  app?: StoreApp
  theme?: ThemeDef
  mod?: StoreMod
}

const TV_UA = 'Mozilla/5.0 (SMART-TV; Linux; Tizen 6.5) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0 TV Safari/537.36'

export const BUILTIN_CATALOG: StoreItem[] = [
  {
    id: 'youtube',
    kind: 'app',
    name: 'YouTube',
    short: 'YT',
    hue: 0,
    description: 'YouTube TV arayüzü: kumanda ve klavyeyle gezinmeye uygun.',
    app: { url: 'https://www.youtube.com/tv', kiosk: true, userAgent: TV_UA }
  },
  {
    id: 'netflix',
    kind: 'app',
    name: 'Netflix',
    short: 'N',
    hue: 357,
    description: 'Netflix, tam ekran. Korumalı (DRM) içerik için Microsoft Edge ile açılır.',
    app: { url: 'https://www.netflix.com', kiosk: true, drm: true }
  },
  {
    id: 'disneyplus',
    kind: 'app',
    name: 'Disney+',
    short: 'D+',
    hue: 220,
    description: 'Disney+, tam ekran.',
    app: { url: 'https://www.disneyplus.com', kiosk: true, drm: true }
  },
  {
    id: 'primevideo',
    kind: 'app',
    name: 'Prime Video',
    short: 'PV',
    hue: 195,
    description: 'Amazon Prime Video, tam ekran.',
    app: { url: 'https://www.primevideo.com', kiosk: true, drm: true }
  },
  {
    id: 'spotify',
    kind: 'app',
    name: 'Spotify',
    short: 'SP',
    hue: 140,
    description: 'Spotify web oynatıcı.',
    app: { url: 'https://open.spotify.com', kiosk: true, drm: true }
  },
  {
    id: 'twitch',
    kind: 'app',
    name: 'Twitch',
    short: 'TW',
    hue: 265,
    description: 'Canlı yayınlar.',
    app: { url: 'https://www.twitch.tv', kiosk: true }
  },
  {
    id: 'browser',
    kind: 'app',
    name: 'İnternet Tarayıcısı',
    short: 'WEB',
    hue: 205,
    description: 'Konsolun kendi tarayıcısı: kumandayla imleç, sanal klavye, sekmesiz tam ekran.',
    app: { url: 'https://www.google.com', kiosk: false }
  },
  // ── Interface styles: change the whole home screen ──
  { id: 'style-classic', kind: 'theme', name: 'Klasik XMB', short: 'XMB', hue: 220, description: 'Yatay kategori çubuğu ve dikey listeler; POLYMIRA\'nın varsayılan arayüzü.', theme: { id: 'style-classic', name: 'Klasik XMB', hue: null, uiStyle: 'xmb' } },
  { id: 'style-aurora', kind: 'theme', name: 'Aurora', short: 'AU', hue: 230, description: 'Modern konsol arayüzü: üstte sekmeler, yatay kare kartlar, odaktaki oyun büyük ve arka planda.', theme: { id: 'style-aurora', name: 'Aurora', hue: 232, night: true, uiStyle: 'aurora' } },
  { id: 'style-tiles', kind: 'theme', name: 'Tiles', short: 'TL', hue: 130, description: 'Büyük döşemelerden oluşan ızgara, üstte bölümler. Tek bakışta çok oyun.', theme: { id: 'style-tiles', name: 'Tiles', hue: 140, night: true, uiStyle: 'tiles' } },
  { id: 'style-shelf', kind: 'theme', name: 'Shelf', short: 'SH', hue: 205, description: 'Solda kategori menüsü, sağda dikey kapaklardan bir raf. El konsolu ve TV için.', theme: { id: 'style-shelf', name: 'Shelf', hue: 210, night: true, uiStyle: 'shelf' } },
  // ── Unusual themes ──
  {
    id: 'theme-crt',
    kind: 'theme',
    name: 'CRT Terminal',
    short: 'CRT',
    hue: 120,
    description: 'Yeşil fosforlu tüp monitör: tarama çizgileri, hafif titreşim, daktilo yazısı.',
    theme: {
      id: 'theme-crt',
      name: 'CRT Terminal',
      hue: 120,
      night: true,
      css: `.app { filter: grayscale(1) sepia(1) hue-rotate(65deg) saturate(4) brightness(.95) contrast(1.15); }
.app * { font-family: 'Cascadia Mono', Consolas, 'Courier New', monospace !important; letter-spacing: .02em; }
.app::after { content: ''; position: fixed; inset: 0; pointer-events: none; z-index: 999;
  background: repeating-linear-gradient(0deg, rgba(0,0,0,.28) 0 1px, transparent 1px 3px), radial-gradient(ellipse at center, transparent 60%, rgba(0,0,0,.55));
  animation: crt-flicker 3s steps(2) infinite; }
@keyframes crt-flicker { 50% { opacity: .92; } }`
    }
  },
  {
    id: 'theme-paper',
    kind: 'theme',
    name: 'Kağıt',
    short: 'KĞ',
    hue: 40,
    description: 'Açık renkli, kâğıt gibi sakin bir tema. Gündüz ve aydınlık odalar için.',
    theme: {
      id: 'theme-paper',
      name: 'Kağıt',
      hue: 40,
      css: `.app { filter: invert(.92) hue-rotate(180deg) sepia(.15); }
.app img, .app video, .app .backdrop, .app .launch-cover { filter: invert(1) hue-rotate(180deg); }`
    }
  },
  {
    id: 'theme-vapor',
    kind: 'theme',
    name: 'Vaporwave',
    short: 'VW',
    hue: 300,
    description: 'Pembe ve turkuaz, 80\'ler bilgisayar estetiği, eğik yazılar.',
    theme: {
      id: 'theme-vapor',
      name: 'Vaporwave',
      hue: 300,
      css: `.app { filter: saturate(1.4) hue-rotate(-10deg); }
.x-title, .shell-title, .x-cat-label { font-style: italic; text-shadow: 2px 2px 0 #00e5ff, -1px -1px 0 #ff2bd6 !important; }
.app::after { content: ''; position: fixed; left: 0; right: 0; bottom: 0; height: 38vh; pointer-events: none; z-index: 1; opacity: .35;
  background: linear-gradient(transparent, rgba(255,43,214,.25)), repeating-linear-gradient(90deg, rgba(0,229,255,.5) 0 1px, transparent 1px 6vw), repeating-linear-gradient(0deg, rgba(0,229,255,.5) 0 1px, transparent 1px 5vh);
  transform: perspective(40vh) rotateX(55deg); transform-origin: bottom; }`
    }
  },
  // ── Themes ──
  { id: 'theme-ocean', kind: 'theme', name: 'Okyanus', short: 'OK', hue: 190, description: 'Turkuaz dalgalar.', theme: { id: 'theme-ocean', name: 'Okyanus', hue: 188 } },
  { id: 'theme-sunset', kind: 'theme', name: 'Gün Batımı', short: 'GB', hue: 20, description: 'Turuncu-pembe sıcak tonlar.', theme: { id: 'theme-sunset', name: 'Gün Batımı', hue: 18 } },
  { id: 'theme-forest', kind: 'theme', name: 'Orman', short: 'OR', hue: 120, description: 'Derin yeşil.', theme: { id: 'theme-forest', name: 'Orman', hue: 125 } },
  { id: 'theme-neon', kind: 'theme', name: 'Neon', short: 'NE', hue: 300, description: 'Mor-pembe neon.', theme: { id: 'theme-neon', name: 'Neon', hue: 305 } },
  { id: 'theme-sakura', kind: 'theme', name: 'Sakura', short: 'SA', hue: 335, description: 'Yumuşak pembe.', theme: { id: 'theme-sakura', name: 'Sakura', hue: 335 } },
  { id: 'theme-midnight', kind: 'theme', name: 'Gece Yarısı', short: 'GY', hue: 240, description: 'Neredeyse siyah, sade.', theme: { id: 'theme-midnight', name: 'Gece Yarısı', hue: 240, night: true } }
]
