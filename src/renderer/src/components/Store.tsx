import { useEffect, useMemo, useRef, useState } from 'react'
import { BRAND } from '@shared/brand'
import { PLATFORMS, getEmulator } from '@shared/platforms'
import type { StoreItem } from '@shared/catalog'
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
  run: () => void
}

interface Entry {
  key: string
  name: string
  short: string
  hue: number
  icon?: string
  glyph?: IconName
  author?: string
  description: string
  details: string[]
  status: string
  installed: boolean
  progress?: number
  actions: StoreAction[]
}

function hueOf(s: string): number {
  let h = 0
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) % 360
  return h
}

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
  const [idx, setIdx] = useState(0)
  const [detail, setDetail] = useState<string | null>(null)
  const [btn, setBtn] = useState(0)
  const [closing, setClosing] = useState(false)
  const gridRef = useRef<HTMLDivElement>(null)

  useEffect(() => ctx.refreshCatalog(), [])

  const entries = useMemo(() => {
    const installedApps = new Map(state.games.filter((g) => g.platformId === 'apps').map((g) => [g.meta.storeId, g]))
    const installedThemes = new Set(state.settings.installedThemes)

    const appEntry = (i: StoreItem): Entry => {
      const inst = installedApps.has(i.id)
      const open = () => openApp(i)
      return {
        key: `app:${i.id}`,
        name: i.name,
        short: i.short,
        hue: i.hue,
        icon: state.appIcons[i.id.replace(/[^a-z0-9_-]/gi, '_')] ? appIconUrl(i.id, state.appIcons[i.id.replace(/[^a-z0-9_-]/gi, '_')]) : i.icon,
        author: i.author,
        description: i.description,
        details: i.app?.drm && !drmInConsole(state)
          ? ['Korumalı (DRM) içerik: Microsoft Edge ile tam ekran açılır, oyun içi menü üstünde çalışır.', `Konsol içi DRM: ${state.drm.detail}`]
          : [i.app?.drm ? 'Konsolun içinde açılır (Widevine).' : 'Konsolun içinde açılır.', i.app?.kiosk ? 'TV arayüzü: yön tuşları, seç ve geri tuşları' : 'Sol çubuk imleç, sağ çubuk kaydırma, sanal klavye'],
        status: inst ? 'Yüklü' : 'Ücretsiz',
        installed: inst,
        actions: inst
          ? [
              { label: 'Aç', icon: 'play', run: open },
              { label: 'Kaldır', icon: 'trash', danger: true, run: () => ctx.run(() => window.bridge.invoke('store:uninstall', i.id), `${i.name} kaldırıldı.`) }
            ]
          : [
              { label: 'Ekle', icon: 'plus', run: () => ctx.run(() => window.bridge.invoke('store:install', i.id), `${i.name} Uygulamalar'a eklendi.`) },
              { label: 'Ekle ve aç', icon: 'play', run: () => ctx.run(async () => { await window.bridge.invoke('store:install', i.id); open() }) }
            ]
      }
    }

    const themeEntry = (i: StoreItem): Entry => {
      const inst = installedThemes.has(i.id)
      const active = state.settings.theme === i.id
      return {
        key: `theme:${i.id}`,
        name: i.name,
        short: i.short,
        hue: i.theme?.hue ?? i.hue,
        glyph: 'palette',
        author: i.author,
        description: i.description,
        details: ['Arka plan ve renk teması'],
        status: active ? 'Kullanılıyor' : inst ? 'Yüklü' : 'Ücretsiz',
        installed: inst,
        actions: [
          {
            label: active ? 'Kullanılıyor' : 'Uygula',
            icon: 'check',
            run: () =>
              ctx.run(async () => {
                if (!inst) await window.bridge.invoke('store:install', i.id)
                await window.bridge.invoke('settings:set', { theme: i.id })
              }, `${i.name} teması uygulandı.`)
          },
          ...(inst ? [{ label: 'Kaldır', icon: 'trash' as IconName, danger: true, run: () => ctx.run(() => window.bridge.invoke('store:uninstall', i.id), 'Tema kaldırıldı.') }] : [])
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
        name,
        short: name.replace(/[^A-Za-z0-9]/g, '').slice(0, 3).toUpperCase(),
        hue: hueOf(id),
        glyph: 'chip',
        author: emu?.homepage.replace(/^https?:\/\/(www\.)?/, '').replace(/\/.*$/, ''),
        description: emu?.notes ?? `${name} emülatörü. Resmi sürümü kaynağından indirilir ve kurulur.`,
        details: [platforms.length ? `Platformlar: ${platforms.join(', ')}` : '', job ? downloadLine(job) : '', 'BIOS / firmware dosyaları dahil değildir.'].filter(Boolean),
        status: busy ? (progress !== undefined ? `%${Math.floor(progress * 100)}` : 'Sırada') : inst ? 'Kurulu' : 'İndir',
        installed: inst,
        progress: busy ? progress ?? 0 : undefined,
        actions: busy
          ? [{ label: 'İndirmeyi iptal et', icon: 'trash', danger: true, run: () => void window.bridge.invoke('emu:cancelDownload', id) }]
          : [{ label: inst ? 'Yeniden indir' : 'İndir ve kur', icon: 'up', run: () => ctx.run(() => window.bridge.invoke('emu:download', [id]), `${name} indirme sırasına eklendi. Arka planda iner.`) }]
      }
    })

    const modEntries: Entry[] = [
      ...state.mods.map((m) => ({
        key: `mod:${m.id}`,
        name: m.name,
        short: m.name.slice(0, 2).toUpperCase(),
        hue: hueOf(m.id),
        glyph: 'brush' as IconName,
        description: m.description ?? 'Görünüm modu (CSS).',
        details: [`Klasör: mods/${m.id}`],
        status: m.enabled ? 'Açık' : 'Kapalı',
        installed: m.enabled,
        actions: [
          { label: m.enabled ? 'Kapat' : 'Aç', icon: (m.enabled ? 'power' : 'check') as IconName, run: () => ctx.run(() => window.bridge.invoke('mods:toggle', m.id, !m.enabled)) },
          ...(catalog.some((i) => i.kind === 'mod' && i.id === m.id)
            ? [{ label: 'Kaldır', icon: 'trash' as IconName, danger: true, run: () => ctx.run(() => window.bridge.invoke('store:uninstall', m.id), `${m.name} kaldırıldı.`) }]
            : [])
        ]
      })),
      // Community mods from the store catalog that are not installed yet.
      ...catalog
        .filter((i) => i.kind === 'mod' && !state.mods.some((m) => m.id === i.id))
        .map((i) => ({
          key: `cmod:${i.id}`,
          name: i.name,
          short: i.short,
          hue: i.hue,
          glyph: 'brush' as IconName,
          author: i.author,
          description: i.description,
          details: ['Topluluk modu · görünümü değiştirir, kod çalıştırmaz', i.mod?.version ? `Sürüm ${i.mod.version}` : ''].filter(Boolean),
          status: 'Ücretsiz',
          installed: false,
          actions: [{ label: 'İndir ve aç', icon: 'up' as IconName, run: () => ctx.run(() => window.bridge.invoke('store:install', i.id), `${i.name} kuruldu.`) }]
        })),
      {
        key: 'tool:mods',
        name: 'Mod klasörü',
        short: 'MOD',
        hue: 30,
        glyph: 'folder',
        description: 'Her mod bir klasördür: mod.json ({ "name", "description" }) ve style.css. Modlar görünümü değiştirir, kod çalıştıramaz.',
        details: [],
        status: 'Aç',
        installed: false,
        actions: [
          { label: 'Klasörü aç', icon: 'folder', run: () => void window.bridge.invoke('folder:open', 'mods') },
          { label: 'Yeniden yükle', icon: 'refresh', run: () => ctx.run(() => window.bridge.invoke('state:get'), 'Yenilendi.') }
        ]
      },
      {
        key: 'tool:themes',
        name: 'Tema klasörü',
        short: 'TEMA',
        hue: 285,
        glyph: 'folder',
        description: 'Kendi temanı ekle: { "name": "…", "hue": 210, "night": false, "css": "…" } içeren bir .json dosyası.',
        details: [],
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
        name: j.name,
        short: j.name.slice(0, 3).toUpperCase(),
        hue: hueOf(j.id),
        glyph: busy ? 'up' : j.status === 'error' ? 'warning' : 'check',
        description: downloadLine(j),
        details: [j.total ? `${formatBytes(j.received)} / ${formatBytes(j.total)}` : '', j.version ? `Sürüm ${j.version}` : '', j.error ?? ''].filter(Boolean),
        status: j.status === 'downloading' ? [`%${Math.floor((progress ?? 0) * 100)}`, formatEta(j.etaSec)].filter(Boolean).join(' · ') : downloadLine(j),
        installed: j.status === 'done',
        progress: busy ? progress ?? 0 : undefined,
        actions: busy ? [{ label: 'İptal et', icon: 'trash', danger: true, run: () => void window.bridge.invoke('emu:cancelDownload', j.id) }] : []
      }
    })

    const byTab: Record<StoreTab, Entry[]> = {
      featured: [...apps.slice(0, 5), ...emuEntries.filter((e) => !e.installed).slice(0, 5), ...themes.slice(0, 5)],
      apps,
      emulators: emuEntries,
      themes,
      mods: modEntries,
      downloads: jobs
    }
    return byTab
  }, [state, catalog])

  const list = entries[tab]
  const cur = list[Math.min(idx, list.length - 1)]
  const open = detail ? [...Object.values(entries).flat()].find((e) => e.key === detail) ?? null : null

  useEffect(() => {
    gridRef.current?.querySelector<HTMLElement>('.store-tile.sel')?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
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

  useInput((a) => {
    if (closing) return
    if (open) {
      if (a === 'left' || a === 'up') {
        if (btn > 0) {
          sfx('move')
          setBtn(btn - 1)
        }
      } else if (a === 'right' || a === 'down') {
        if (btn < open.actions.length - 1) {
          sfx('move')
          setBtn(btn + 1)
        }
      } else if (a === 'confirm') {
        const act = open.actions[btn]
        if (!act) return
        sfx('confirm')
        act.run()
      } else if (a === 'back') {
        sfx('back')
        setDetail(null)
      }
      return
    }
    const n = list.length
    if (a === 'l1' || a === 'l2') switchTab(-1)
    else if (a === 'r1' || a === 'r2') switchTab(1)
    else if (a === 'left' && idx % COLS > 0) {
      sfx('move')
      setIdx(idx - 1)
    } else if (a === 'right' && idx % COLS < COLS - 1 && idx < n - 1) {
      sfx('move')
      setIdx(idx + 1)
    } else if (a === 'up' && idx >= COLS) {
      sfx('move')
      setIdx(idx - COLS)
    } else if (a === 'down' && idx + COLS < n) {
      sfx('move')
      setIdx(idx + COLS)
    } else if (a === 'down' && Math.floor(idx / COLS) < Math.floor((n - 1) / COLS)) {
      sfx('move')
      setIdx(n - 1)
    } else if (a === 'confirm' && cur) {
      sfx('confirm')
      setDetail(cur.key)
      setBtn(0)
    } else if (a === 'options') {
      ctx.refreshCatalog()
    } else if (a === 'back' || a === 'home') close()
  })

  return (
    <div className={`store ${closing ? 'closing' : ''}`}>
      <div className="store-head">
        <div className="store-brand">
          <Icon name="bag" /> {BRAND.name} Mağaza
        </div>
        <div className="store-tabs">
          <span className="store-trigger"><Glyph action="l1" /></span>
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
          <span className="store-trigger"><Glyph action="r1" /></span>
        </div>
      </div>

      <div className="store-grid" ref={gridRef}>
        {list.length === 0 && (
          <div className="store-empty">{tab === 'downloads' ? 'Şu an indirme yok. Emülatörler bölümünden indirme başlatabilirsin.' : 'Burada henüz bir şey yok.'}</div>
        )}
        {list.map((e, i) => (
          <div
            key={e.key}
            className={`store-tile ${i === idx ? 'sel' : ''}`}
            onClick={() => {
              setIdx(i)
              setDetail(e.key)
              setBtn(0)
            }}
          >
            <TileIcon e={e} />
            <div className="store-tile-name">{e.name}</div>
            <div className={`store-tile-status ${e.installed ? 'ok' : ''}`}>{e.status}</div>
            {e.progress !== undefined && (
              <div className="store-bar">
                <div style={{ width: `${Math.round(e.progress * 100)}%` }} />
              </div>
            )}
          </div>
        ))}
      </div>

      <div className="store-foot">
        <Hint action="confirm">Seç</Hint>
        <Hint action="back">{open ? 'Geri' : 'Mağazadan çık'}</Hint>
        <span className="hint"><Glyph action="l1" /> <Glyph action="r1" /> Bölüm</span>
        <Hint action="options">Yenile</Hint>
      </div>

      {open && (
        <div className="store-detail">
          <div className="store-detail-card">
            <TileIcon e={open} big />
            <div className="store-detail-body">
              <div className="store-detail-name">{open.name}</div>
              {open.author && <div className="store-detail-author">{open.author}</div>}
              <div className="store-detail-status">{open.status}</div>
              <p className="store-detail-desc">{open.description}</p>
              {open.details.map((d) => (
                <div className="store-detail-line" key={d}>
                  {d}
                </div>
              ))}
              {open.progress !== undefined && (
                <div className="store-bar big">
                  <div style={{ width: `${Math.round(open.progress * 100)}%` }} />
                </div>
              )}
              <div className="store-detail-actions">
                {open.actions.map((a, i) => (
                  <div
                    key={a.label}
                    className={`btn ${i === btn ? 'sel' : ''} ${a.danger ? 'danger' : ''}`}
                    onClick={() => {
                      setBtn(i)
                      a.run()
                    }}
                  >
                    <Icon name={a.icon} /> {a.label}
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
