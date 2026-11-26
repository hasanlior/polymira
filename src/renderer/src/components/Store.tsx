import { useEffect, useMemo, useRef, useState } from 'react'
import { BRAND } from '@shared/brand'
import { PLATFORMS, getEmulator } from '@shared/platforms'
import type { StoreItem, ThemeDef } from '@shared/catalog'
import { appIconUrl, downloadLine, drmInConsole, formatBytes, formatEta } from '../api'
import { Badge, Icon, type IconName } from '../icons'
import { Glyph, Hint } from '../glyphs'
import { useInput } from '../input'
import type { MenuCtx } from '../menu'
import { sfx } from '../sound'

export type StoreTab = 'featured' | 'apps' | 'emulators' | 'themes' | 'mods' | 'downloads'

const TABS: { id: StoreTab; title: string }[] = [
  { id: 'featured', title: 'Öne Çıkanlar' },
  { id: 'apps', title: 'Uygulamalar' },
  { id: 'emulators', title: 'Emülatörler' },
  { id: 'themes', title: 'Temalar' },
  { id: 'mods', title: 'Modlar' },
  { id: 'downloads', title: 'İndirmeler' }
]

const COLS = 5

interface StoreAction {
  label: string
  icon: IconName
  danger?: boolean
  disabled?: boolean
  /** Runs through the download → install → done flow on the store page. */
  install?: () => Promise<unknown>
  run?: () => void
}

interface Entry {
  key: string
  kind: string
  name: string
  short: string
  hue: number
  icon?: string
  glyph?: IconName
  author?: string
  description: string
  details: string[]
  info: { k: string; v: string }[]
  status: string
  installed: boolean
  /** Wide image for the store page. */
  banner?: string
  shots?: string[]
  theme?: ThemeDef
  /** Real progress of a running download (emulators). */
  progress?: number
  stage?: string
  actions: StoreAction[]
}

/** Install flow shown on the primary button. */
interface Flow {
  stage: 'download' | 'install' | 'done' | 'error'
  text: string
}

function hueOf(s: string): number {
  let h = 0
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) % 360
  return h
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms))

function TileIcon({ e, big }: { e: Entry; big?: boolean }) {
  const [broken, setBroken] = useState(false)
  if (e.icon && !broken) {
    return (
      <div className={`store-icon img ${big ? 'big' : ''}`} style={{ ['--h' as string]: e.hue }}>
        <img src={e.icon} alt="" draggable={false} onError={() => setBroken(true)} />
      </div>
    )
  }
  return (
    <div className={`store-icon ${big ? 'big' : ''}`}>
      {e.glyph ? (
        <div className="badge" style={{ ['--h' as string]: e.hue }}>
          <Icon name={e.glyph} />
        </div>
      ) : (
        <Badge text={e.short} hue={e.hue} />
      )}
    </div>
  )
}

/** A miniature console screen in the theme's colours and home screen layout. */
function ThemePreview({ theme, hue }: { theme?: ThemeDef; hue: number }) {
  const h = theme?.hue ?? hue
  const style = theme?.uiStyle ?? 'xmb'
  const css = theme?.css ?? ''
  const filter = /filter:\s*([^;}]+)/.exec(css)?.[1]
  return (
    <div
      className={`tp tp-${style} ${theme?.night ? 'night' : ''}`}
      style={{ ['--h' as string]: h, filter: filter && !/invert/.test(filter) ? filter : undefined }}
    >
      <div className="tp-wave" />
      {style === 'xmb' && (
        <>
          <div className="tp-row">
            {Array.from({ length: 7 }, (_, i) => (
              <i key={i} className={i === 2 ? 'on' : ''} />
            ))}
          </div>
          <div className="tp-list">
            {Array.from({ length: 5 }, (_, i) => (
              <b key={i} className={i === 0 ? 'on' : ''} />
            ))}
          </div>
        </>
      )}
      {style === 'aurora' && (
        <>
          <div className="tp-tabs">
            {Array.from({ length: 5 }, (_, i) => (
              <i key={i} className={i === 0 ? 'on' : ''} />
            ))}
          </div>
          <div className="tp-hero" />
          <div className="tp-cards">
            {Array.from({ length: 6 }, (_, i) => (
              <i key={i} className={i === 0 ? 'on' : ''} />
            ))}
          </div>
        </>
      )}
      {style === 'tiles' && (
        <div className="tp-grid">
          {Array.from({ length: 12 }, (_, i) => (
            <i key={i} className={i === 0 ? 'on' : i % 5 === 3 ? 'wide' : ''} />
          ))}
        </div>
      )}
      {style === 'shelf' && (
        <>
          <div className="tp-side">
            {Array.from({ length: 6 }, (_, i) => (
              <b key={i} className={i === 1 ? 'on' : ''} />
            ))}
          </div>
          <div className="tp-shelf">
            {Array.from({ length: 5 }, (_, i) => (
              <i key={i} className={i === 0 ? 'on' : ''} />
            ))}
          </div>
        </>
      )}
      {/crt|scanline|repeating-linear/i.test(css) && <div className="tp-scan" />}
    </div>
  )
}

/** Left side of the store page: banner, screenshots, theme preview or generated art. */
function Media({ e, index }: { e: Entry; index: number }) {
  const [broken, setBroken] = useState<string | null>(null)
  const images = [e.banner, ...(e.shots ?? [])].filter((x): x is string => Boolean(x) && x !== broken)
  if (e.theme) return <ThemePreview theme={e.theme} hue={e.hue} />
  const src = images[index % Math.max(1, images.length)]
  if (src) {
    return (
      <div className="sp-shot" style={{ ['--h' as string]: e.hue }}>
        <img key={src} src={src} alt="" draggable={false} onError={() => setBroken(src)} />
      </div>
    )
  }
  return (
    <div className="sp-art" style={{ ['--h' as string]: e.hue }}>
      <div className="sp-art-ring" />
      <TileIcon e={e} big />
      <div className="sp-art-name">{e.name}</div>
    </div>
  )
}

/** Full-screen store app: browse with the D-pad, L1/R1 switch sections, ✕ opens an item. */
export function Store({
  ctx,
  initialTab = 'featured',
  onClose,
  openApp
}: {
  ctx: MenuCtx
  initialTab?: StoreTab
  onClose: () => void
  openApp: (item: StoreItem) => void
}) {
  const { state, catalog } = ctx
  const [tab, setTab] = useState<StoreTab>(initialTab)
  // -1: the featured banner is focused.
  const [idx, setIdx] = useState(0)
  const [detail, setDetail] = useState<string | null>(null)
  const [btn, setBtn] = useState(0)
  const [media, setMedia] = useState(0)
  const [hero, setHero] = useState(0)
  const [flows, setFlows] = useState<Record<string, Flow>>({})
  const [closing, setClosing] = useState(false)
  const gridRef = useRef<HTMLDivElement>(null)

  useEffect(() => ctx.refreshCatalog(), [])

  const iconRev = (id: string) => state.appIcons[id.replace(/[^a-z0-9_-]/gi, '_')]

  const entries = useMemo(() => {
    const installedApps = new Map(state.games.filter((g) => g.platformId === 'apps').map((g) => [g.meta.storeId, g]))
    const installedThemes = new Set(state.settings.installedThemes)

    const appEntry = (i: StoreItem): Entry => {
      const inst = installedApps.has(i.id)
      const icon = iconRev(i.id) ? appIconUrl(i.id, iconRev(i.id)) : i.icon
      const bannerRev = iconRev(`${i.id}__banner`)
      return {
        key: `app:${i.id}`,
        kind: 'Uygulama',
        name: i.name,
        short: i.short,
        hue: i.hue,
        icon,
        banner: i.banner ?? (bannerRev ? appIconUrl(`${i.id}__banner`, bannerRev) : undefined),
        shots: i.screenshots,
        author: i.author ?? (i.app ? new URL(i.app.url).hostname.replace(/^www\./, '') : undefined),
        description: i.description,
        details:
          i.app?.drm && !drmInConsole(state)
            ? ['Korumalı (DRM) içerik: Microsoft Edge ile tam ekran açılır, oyun içi menü üstünde çalışır.']
            : [i.app?.kiosk ? 'TV arayüzü: yön tuşları, seç ve geri tuşlarıyla gezilir.' : 'Sol çubuk imleç, sağ çubuk kaydırma, sanal klavye.'],
        info: [
          { k: 'Tür', v: 'Uygulama' },
          { k: 'Açılış', v: i.app?.drm && !drmInConsole(state) ? 'Microsoft Edge (DRM)' : 'Konsolun içinde' },
          { k: 'Kumanda', v: i.app?.kiosk ? 'TV arayüzü' : 'İmleç ve sanal klavye' },
          ...(i.version ? [{ k: 'Sürüm', v: i.version }] : []),
          { k: 'Fiyat', v: 'Ücretsiz' }
        ],
        status: inst ? 'Yüklü' : 'Ücretsiz',
        installed: inst,
        actions: inst
          ? [
              { label: 'Aç', icon: 'play', run: () => openApp(i) },
              { label: 'Kaldır', icon: 'trash', danger: true, run: () => ctx.run(() => window.bridge.invoke('store:uninstall', i.id), `${i.name} kaldırıldı.`) }
            ]
          : [{ label: 'İndir', icon: 'up', install: () => window.bridge.invoke('store:install', i.id) }]
      }
    }

    const themeEntry = (i: StoreItem): Entry => {
      const inst = installedThemes.has(i.id)
      const active = state.settings.theme === i.id
      const apply = () =>
        ctx.run(() => window.bridge.invoke('settings:set', i.theme?.uiStyle ? { theme: i.id, uiStyle: i.theme.uiStyle } : { theme: i.id }), `${i.name} teması uygulandı.`)
      return {
        key: `theme:${i.id}`,
        kind: i.theme?.uiStyle ? 'Arayüz stili' : 'Tema',
        name: i.name,
        short: i.short,
        hue: i.theme?.hue ?? i.hue,
        glyph: 'palette',
        theme: i.theme,
        author: i.author ?? BRAND.name,
        description: i.description,
        details: [i.theme?.uiStyle ? 'Ana ekranın düzenini değiştirir.' : i.theme?.css ? 'Renkleri ve yazı stilini değiştirir.' : 'Arka plan rengini değiştirir.'],
        info: [
          { k: 'Tür', v: i.theme?.uiStyle ? 'Arayüz stili' : 'Tema' },
          { k: 'Düzen', v: { xmb: 'Klasik XMB', aurora: 'Aurora', tiles: 'Tiles', shelf: 'Shelf' }[i.theme?.uiStyle ?? 'xmb'] },
          { k: 'Görünüm', v: i.theme?.night ? 'Koyu' : 'Renkli' },
          { k: 'Fiyat', v: 'Ücretsiz' }
        ],
        status: active ? 'Kullanılıyor' : inst ? 'Yüklü' : 'Ücretsiz',
        installed: inst,
        actions: !inst
          ? [{ label: 'İndir', icon: 'up', install: () => window.bridge.invoke('store:install', i.id) }]
          : [
              { label: active ? 'Kullanılıyor' : 'Uygula', icon: 'check', disabled: active, run: apply },
              { label: 'Kaldır', icon: 'trash', danger: true, run: () => ctx.run(() => window.bridge.invoke('store:uninstall', i.id), 'Tema kaldırıldı.') }
            ]
      }
    }

    const emuEntries: Entry[] = state.downloadable.map((id) => {
      const emu = getEmulator(id)
      const name = emu?.name ?? id
      const job = state.downloads.find((j) => j.id === id)
      const busy = Boolean(job && (job.status === 'queued' || job.status === 'downloading' || job.status === 'extracting'))
      const inst = Boolean(state.emulators.find((e) => e.id === id)?.found)
      const platforms = PLATFORMS.filter((p) => p.runners.some((r) => r.emulatorId === id)).map((p) => p.name)
      const progress = job && job.status === 'downloading' && job.total ? job.received / job.total : undefined
      return {
        key: `emu:${id}`,
        kind: 'Emülatör',
        name,
        short: name.replace(/[^A-Za-z0-9]/g, '').slice(0, 3).toUpperCase(),
        hue: hueOf(id),
        glyph: 'chip',
        author: emu?.homepage.replace(/^https?:\/\/(www\.)?/, '').replace(/\/.*$/, ''),
        description: emu?.notes ?? `${name} emülatörü. Resmi sürümü kaynağından indirilir ve kurulur.`,
        details: ['BIOS ve firmware dosyaları dahil değildir; resmi sürüm kaynağından indirilir.'],
        info: [
          { k: 'Tür', v: 'Emülatör' },
          ...(platforms.length ? [{ k: 'Platformlar', v: platforms.join(', ') }] : []),
          ...(job?.version ? [{ k: 'Sürüm', v: job.version }] : []),
          ...(job?.total ? [{ k: 'Boyut', v: formatBytes(job.total) }] : []),
          { k: 'Durum', v: inst ? 'Kurulu' : 'Kurulu değil' }
        ],
        status: busy ? (progress !== undefined ? `%${Math.floor(progress * 100)}` : 'Sırada') : inst ? 'Kurulu' : 'İndir',
        installed: inst,
        progress: busy ? progress ?? 0 : undefined,
        stage: busy && job ? downloadLine(job) : undefined,
        actions: busy
          ? [{ label: 'İndirmeyi iptal et', icon: 'trash', danger: true, run: () => void window.bridge.invoke('emu:cancelDownload', id) }]
          : [
              {
                label: inst ? 'Yeniden indir' : 'İndir ve kur',
                icon: 'up',
                run: () => ctx.run(() => window.bridge.invoke('emu:download', [id]))
              }
            ]
      }
    })

    const modEntries: Entry[] = [
      ...state.mods.map((m): Entry => {
        const item = catalog.find((i) => i.kind === 'mod' && i.id === m.id)
        return {
          key: `mod:${m.id}`,
          kind: 'Mod',
          name: m.name,
          short: m.name.slice(0, 2).toUpperCase(),
          hue: item?.hue ?? hueOf(m.id),
          glyph: 'brush',
          author: item?.author,
          description: m.description ?? item?.description ?? 'Görünüm modu (CSS).',
          details: ['Modlar yalnızca görünümü değiştirir, kod çalıştıramaz.'],
          info: [
            { k: 'Tür', v: 'Görünüm modu' },
            ...(item?.mod?.version ? [{ k: 'Sürüm', v: item.mod.version }] : []),
            { k: 'Klasör', v: `mods/${m.id}` },
            { k: 'Durum', v: m.enabled ? 'Açık' : 'Kapalı' }
          ],
          status: m.enabled ? 'Açık' : 'Kapalı',
          installed: m.enabled,
          actions: [
            { label: m.enabled ? 'Kapat' : 'Aç', icon: m.enabled ? 'power' : 'check', run: () => ctx.run(() => window.bridge.invoke('mods:toggle', m.id, !m.enabled)) },
            ...(item ? [{ label: 'Kaldır', icon: 'trash' as IconName, danger: true, run: () => ctx.run(() => window.bridge.invoke('store:uninstall', m.id), `${m.name} kaldırıldı.`) }] : [])
          ]
        }
      }),
      // Community mods from the store catalog that are not installed yet.
      ...catalog
        .filter((i) => i.kind === 'mod' && !state.mods.some((m) => m.id === i.id))
        .map(
          (i): Entry => ({
            key: `cmod:${i.id}`,
            kind: 'Mod',
            name: i.name,
            short: i.short,
            hue: i.hue,
            glyph: 'brush',
            shots: i.screenshots,
            banner: i.banner,
            author: i.author,
            description: i.description,
            details: ['Topluluk modu: görünümü değiştirir, kod çalıştırmaz.'],
            info: [{ k: 'Tür', v: 'Görünüm modu' }, ...(i.mod?.version ? [{ k: 'Sürüm', v: i.mod.version }] : []), { k: 'Fiyat', v: 'Ücretsiz' }],
            status: 'Ücretsiz',
            installed: false,
            actions: [{ label: 'İndir', icon: 'up', install: () => window.bridge.invoke('store:install', i.id) }]
          })
        ),
      {
        key: 'tool:mods',
        kind: 'Araç',
        name: 'Mod klasörü',
        short: 'MOD',
        hue: 30,
        glyph: 'folder',
        description: 'Kendi modunu ekle: her mod bir klasördür, içinde mod.json ({ "name", "description" }) ve style.css bulunur. Modlar görünümü değiştirir, kod çalıştıramaz.',
        details: [],
        info: [{ k: 'Tür', v: 'Klasör' }],
        status: 'Aç',
        installed: false,
        actions: [
          { label: 'Klasörü aç', icon: 'folder', run: () => void window.bridge.invoke('folder:open', 'mods') },
          { label: 'Yeniden yükle', icon: 'refresh', run: () => ctx.run(() => window.bridge.invoke('state:get'), 'Yenilendi.') }
        ]
      },
      {
        key: 'tool:themes',
        kind: 'Araç',
        name: 'Tema klasörü',
        short: 'TEMA',
        hue: 285,
        glyph: 'folder',
        description: 'Kendi temanı ekle: { "name": "…", "hue": 210, "night": false, "css": "…" } içeren bir .json dosyası.',
        details: [],
        info: [{ k: 'Tür', v: 'Klasör' }],
        status: 'Aç',
        installed: false,
        actions: [{ label: 'Klasörü aç', icon: 'folder', run: () => void window.bridge.invoke('folder:open', 'themes') }]
      }
    ]

    const apps = catalog.filter((i) => i.kind === 'app').map(appEntry)
    const themes = catalog.filter((i) => i.kind === 'theme').map(themeEntry)
    const jobs: Entry[] = state.downloads.map((j) => {
      const busy = j.status === 'queued' || j.status === 'downloading' || j.status === 'extracting'
      const progress = j.total ? j.received / j.total : undefined
      return {
        key: `job:${j.id}`,
        kind: 'İndirme',
        name: j.name,
        short: j.name.slice(0, 3).toUpperCase(),
        hue: hueOf(j.id),
        glyph: busy ? 'up' : j.status === 'error' ? 'warning' : 'check',
        description: downloadLine(j),
        details: [j.error ?? ''].filter(Boolean),
        info: [
          ...(j.total ? [{ k: 'Boyut', v: `${formatBytes(j.received)} / ${formatBytes(j.total)}` }] : []),
          ...(j.version ? [{ k: 'Sürüm', v: j.version }] : []),
          { k: 'Durum', v: downloadLine(j) }
        ],
        status: j.status === 'downloading' ? [`%${Math.floor((progress ?? 0) * 100)}`, formatEta(j.etaSec)].filter(Boolean).join(' · ') : downloadLine(j),
        installed: j.status === 'done',
        progress: busy ? progress ?? 0 : undefined,
        stage: busy ? downloadLine(j) : undefined,
        actions: busy ? [{ label: 'İptal et', icon: 'trash', danger: true, run: () => void window.bridge.invoke('emu:cancelDownload', j.id) }] : []
      }
    })

    const byTab: Record<StoreTab, Entry[]> = {
      featured: [...apps.slice(0, 5), ...themes.filter((t) => t.theme?.uiStyle || t.theme?.css).slice(0, 5), ...emuEntries.filter((e) => !e.installed).slice(0, 5)],
      apps,
      emulators: emuEntries,
      themes,
      mods: modEntries,
      downloads: jobs
    }
    return byTab
  }, [state, catalog])

  const list = entries[tab]
  const heroes = useMemo(
    () => (tab === 'featured' ? entries.featured.filter((e) => e.banner || e.theme?.uiStyle).slice(0, 6) : []),
    [entries, tab]
  )
  const heroEntry = heroes.length ? heroes[hero % heroes.length] : null
  const cur = idx < 0 ? heroEntry : list[Math.min(idx, list.length - 1)]
  const all = useMemo(() => Object.values(entries).flat(), [entries])
  const open = detail ? all.find((e) => e.key === detail) ?? null : null

  // Featured banner turns every few seconds while it is not focused.
  useEffect(() => {
    if (heroes.length < 2 || idx < 0 || open) return
    const t = setInterval(() => setHero((h) => h + 1), 7000)
    return () => clearInterval(t)
  }, [heroes.length, idx, open])

  useEffect(() => {
    gridRef.current?.querySelector<HTMLElement>('.store-tile.sel, .store-hero.sel')?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
  }, [idx, tab])

  const close = () => {
    sfx('back')
    setClosing(true)
    setTimeout(onClose, 250)
  }

  const switchTab = (d: number) => {
    const i = TABS.findIndex((t) => t.id === tab)
    const next = TABS[(i + d + TABS.length) % TABS.length].id
    sfx('move')
    setTab(next)
    setIdx(0)
  }

  const openPage = (e: Entry) => {
    sfx('confirm')
    setDetail(e.key)
    setBtn(0)
    setMedia(0)
  }

  /** Download → install → done, shown on the primary button. Errors stay readable for a moment. */
  const runInstall = async (e: Entry, act: StoreAction) => {
    if (!act.install || flows[e.key]) return
    const set = (f: Flow | null) =>
      setFlows((cur) => {
        const next = { ...cur }
        if (f) next[e.key] = f
        else delete next[e.key]
        return next
      })
    set({ stage: 'download', text: 'İndiriliyor…' })
    const started = Date.now()
    try {
      await act.install()
      await wait(Math.max(0, 900 - (Date.now() - started)))
      set({ stage: 'install', text: 'Kuruluyor…' })
      await wait(700)
      sfx('confirm')
      set({ stage: 'done', text: 'Kuruldu' })
      await wait(1400)
      set(null)
      setBtn(0)
    } catch (err) {
      sfx('error')
      set({ stage: 'error', text: (err as Error).message || 'Kurulamadı' })
      await wait(3500)
      set(null)
    }
  }

  const runAction = (e: Entry, act: StoreAction) => {
    if (act.disabled) return
    if (act.install) void runInstall(e, act)
    else act.run?.()
  }

  useInput((a) => {
    if (closing) return
    if (open) {
      const pageList = list.some((x) => x.key === open.key) ? list : all
      if (a === 'up') {
        if (btn > 0) {
          sfx('move')
          setBtn(btn - 1)
        }
      } else if (a === 'down') {
        if (btn < open.actions.length - 1) {
          sfx('move')
          setBtn(btn + 1)
        }
      } else if (a === 'left' || a === 'right') {
        const n = 1 + (open.shots?.length ?? 0)
        if (n > 1) {
          sfx('move')
          setMedia((m) => (m + (a === 'right' ? 1 : n - 1)) % n)
        }
      } else if (a === 'l1' || a === 'r1') {
        // Previous / next item without going back to the grid.
        const i = pageList.findIndex((x) => x.key === open.key)
        const next = pageList[(i + (a === 'r1' ? 1 : pageList.length - 1)) % pageList.length]
        if (next && next.key !== open.key) {
          sfx('move')
          setDetail(next.key)
          setBtn(0)
          setMedia(0)
          if (tab !== 'featured' || list.includes(next)) setIdx(list.indexOf(next))
        }
      } else if (a === 'confirm') {
        if (flows[open.key]) return
        const act = open.actions[btn]
        if (!act || act.disabled) return
        sfx('confirm')
        runAction(open, act)
      } else if (a === 'back') {
        sfx('back')
        setDetail(null)
      }
      return
    }
    const n = list.length
    if (a === 'l1' || a === 'l2') switchTab(-1)
    else if (a === 'r1' || a === 'r2') switchTab(1)
    else if (idx < 0) {
      if (a === 'down' && n > 0) {
        sfx('move')
        setIdx(0)
      } else if ((a === 'left' || a === 'right') && heroes.length > 1) {
        sfx('move')
        setHero((h) => h + (a === 'right' ? 1 : heroes.length - 1))
      } else if (a === 'confirm' && heroEntry) openPage(heroEntry)
      else if (a === 'back' || a === 'home') close()
    } else if (a === 'left' && idx % COLS > 0) {
      sfx('move')
      setIdx(idx - 1)
    } else if (a === 'right' && idx % COLS < COLS - 1 && idx < n - 1) {
      sfx('move')
      setIdx(idx + 1)
    } else if (a === 'up' && idx >= COLS) {
      sfx('move')
      setIdx(idx - COLS)
    } else if (a === 'up' && heroes.length) {
      sfx('move')
      setIdx(-1)
    } else if (a === 'down' && idx + COLS < n) {
      sfx('move')
      setIdx(idx + COLS)
    } else if (a === 'down' && Math.floor(idx / COLS) < Math.floor((n - 1) / COLS)) {
      sfx('move')
      setIdx(n - 1)
    } else if (a === 'confirm' && cur) openPage(cur)
    else if (a === 'options') ctx.refreshCatalog()
    else if (a === 'back' || a === 'home') close()
  })

  const flow = open ? flows[open.key] : undefined

  return (
    <div className={`store ${closing ? 'closing' : ''}`}>
      <div className="store-head">
        <div className="store-brand">
          <Icon name="bag" /> {BRAND.name} Mağaza
        </div>
        <div className="store-tabs">
          <span className="store-trigger">
            <Glyph action="l1" />
          </span>
          {TABS.map((t) => (
            <div
              key={t.id}
              className={`store-tab ${t.id === tab ? 'sel' : ''}`}
              onClick={() => {
                setTab(t.id)
                setIdx(0)
              }}
            >
              {t.title}
              {t.id === 'downloads' && state.downloads.some((j) => j.status === 'downloading' || j.status === 'queued') ? ' •' : ''}
            </div>
          ))}
          <span className="store-trigger">
            <Glyph action="r1" />
          </span>
        </div>
      </div>

      <div className="store-grid" ref={gridRef}>
        {heroEntry && (
          <div className={`store-hero ${idx < 0 ? 'sel' : ''}`} onClick={() => openPage(heroEntry)}>
            <div className="store-hero-media">
              <Media e={heroEntry} index={0} />
            </div>
            <div className="store-hero-text">
              <div className="store-hero-kind">{heroEntry.kind}</div>
              <div className="store-hero-name">{heroEntry.name}</div>
              <div className="store-hero-desc">{heroEntry.description}</div>
              <div className="store-hero-dots">
                {heroes.map((h, i) => (
                  <i key={h.key} className={i === hero % heroes.length ? 'on' : ''} />
                ))}
              </div>
            </div>
          </div>
        )}
        {list.length === 0 && (
          <div className="store-empty">{tab === 'downloads' ? 'Şu an indirme yok. Emülatörler bölümünden indirme başlatabilirsin.' : 'Burada henüz bir şey yok.'}</div>
        )}
        {list.map((e, i) => (
          <div
            key={e.key}
            className={`store-tile ${i === idx ? 'sel' : ''}`}
            onClick={() => {
              setIdx(i)
              openPage(e)
            }}
          >
            <TileIcon e={e} />
            <div className="store-tile-name">{e.name}</div>
            <div className={`store-tile-status ${e.installed ? 'ok' : ''}`}>{flows[e.key]?.text ?? e.status}</div>
            {(e.progress !== undefined || (flows[e.key] && flows[e.key].stage !== 'done')) && (
              <div className={`store-bar ${e.progress === undefined ? 'busy' : ''}`}>
                <div style={{ width: `${Math.round((e.progress ?? 1) * 100)}%` }} />
              </div>
            )}
          </div>
        ))}
      </div>

      <div className="store-foot">
        {open ? (
          <>
            <Hint action="confirm">Seç</Hint>
            <span className="hint">
              <Glyph action="l1" /> <Glyph action="r1" /> Önceki / sonraki
            </span>
            <Hint action="back">Geri</Hint>
          </>
        ) : (
          <>
            <Hint action="confirm">Aç</Hint>
            <span className="hint">
              <Glyph action="l1" /> <Glyph action="r1" /> Bölüm
            </span>
            <Hint action="options">Yenile</Hint>
            <Hint action="back">Mağazadan çık</Hint>
          </>
        )}
      </div>

      {open && (
        <div className="sp" key={open.key} style={{ ['--h' as string]: open.hue }}>
          <div className="sp-bg" />
          <div className="sp-media">
            <Media e={open} index={media} />
            {(open.shots?.length ?? 0) > 0 && (
              <div className="sp-dots">
                {[open.banner ?? '', ...(open.shots ?? [])].map((s, i) => (
                  <i key={`${s}-${i}`} className={i === media ? 'on' : ''} />
                ))}
              </div>
            )}
          </div>
          <div className="sp-info">
            <div className="sp-head">
              <TileIcon e={open} />
              <div>
                <div className="sp-kind">{open.kind}</div>
                <div className="sp-name">{open.name}</div>
                {open.author && <div className="sp-author">{open.author}</div>}
              </div>
            </div>
            <div className="sp-chips">
              <span className={`sp-chip ${open.installed ? 'ok' : ''}`}>{open.status}</span>
            </div>
            <p className="sp-desc">{open.description}</p>
            {open.details.map((d) => (
              <div className="sp-note" key={d}>
                {d}
              </div>
            ))}
            {open.progress !== undefined && (
              <div className="sp-progress">
                <div className="store-bar big">
                  <div style={{ width: `${Math.round(open.progress * 100)}%` }} />
                </div>
                {open.stage && <div className="sp-stage">{open.stage}</div>}
              </div>
            )}
            <div className="sp-actions">
              {flow && (
                <div className={`sp-btn primary sel flow ${flow.stage}`}>
                  <div className={`sp-fill ${flow.stage === 'done' || flow.stage === 'error' ? '' : 'busy'}`} style={flow.stage === 'done' ? { width: '100%' } : undefined} />
                  <span className="sp-btn-label">
                    <Icon name={flow.stage === 'done' ? 'check' : flow.stage === 'error' ? 'warning' : 'up'} />
                    {flow.text}
                  </span>
                </div>
              )}
              {!flow && open.actions.map((a, i) => {
                const primary = i === 0
                return (
                  <div
                    key={a.label}
                    className={`sp-btn ${primary ? 'primary' : ''} ${i === btn ? 'sel' : ''} ${a.danger ? 'danger' : ''} ${a.disabled ? 'off' : ''}`}
                    onClick={() => {
                      setBtn(i)
                      runAction(open, a)
                    }}
                  >
                    <span className="sp-btn-label">
                      <Icon name={a.icon} />
                      {a.label}
                    </span>
                  </div>
                )
              })}
            </div>
            {open.info.length > 0 && (
              <div className="sp-table">
                {open.info.map((r) => (
                  <div className="sp-row" key={r.k}>
                    <span>{r.k}</span>
                    <b>{r.v}</b>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
