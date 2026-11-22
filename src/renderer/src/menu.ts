import { BRAND } from '@shared/brand'
import { EMULATORS, FAMILIES, PLATFORMS, getEmulator, getPlatform, resolveRunner, runnerKey, runnerLabel, type PlatformDef } from '@shared/platforms'
import type { AppState, Game, OverlayHotkey, Settings, SupportLevel, WindowMode } from '@shared/types'
import { appIconUrl, coverUrl, downloadLine, formatBytes, formatPlayTime } from './api'
import type { IconName } from './icons'
import type { StoreItem } from '@shared/catalog'

export interface XItem {
  id: string
  title: string
  subtitle?: string
  icon: IconName
  badge?: { text: string; hue: number }
  cover?: string
  coverIsIcon?: boolean
  /** App icon shown on a rounded tile. */
  tile?: string
  value?: string
  checked?: boolean
  game?: Game
  hue?: number
  /** Folder: entering shows these. */
  children?: XItem[]
  /** Shown when a folder is empty. */
  emptyText?: string
  onActivate?: () => void
  /** Triangle / Y. */
  onOptions?: () => void
  /** Square / X: alternate view (gallery) for game folders. */
  onView?: () => void
}

export interface XCategory {
  id: string
  title: string
  icon: IconName
  hue: number
  items: XItem[]
}

export interface OptItem {
  label: string
  icon?: IconName
  checked?: boolean
  danger?: boolean
  children?: OptItem[]
  onSelect?: () => void
}

export interface BrowseRequest {
  title: string
  mode: 'folder' | 'file'
  extensions?: string[]
  onPick: (path: string) => void
}

export interface MenuCtx {
  state: AppState
  catalog: StoreItem[]
  refreshCatalog: () => void
  pads: string[]
  back: () => void
  openDetail: (gameId: string) => void
  openGallery: (title: string, games: Game[]) => void
  openOptions: (title: string, items: OptItem[]) => void
  browse: (req: BrowseRequest) => void
  confirm: (text: string, onYes: () => void) => void
  launch: (gameId: string) => void
  run: (fn: () => Promise<unknown>, success?: string | ((r: unknown) => string | null)) => void
  openScreenshots: (gameId?: string) => void
  openScan: () => void
  openRemap: () => void
  openStats: () => void
  openTrophies: (key?: string) => void
  keyboard: (title: string, initial: string, onDone: (text: string) => void) => void
  openStore: (tab?: 'featured' | 'apps' | 'emulators' | 'themes' | 'mods' | 'downloads') => void
  setSettings: (patch: Partial<Settings>) => void
}

export const SUPPORT_LABEL: Record<SupportLevel, string> = {
  stable: 'Kararlı',
  experimental: 'Deneysel',
  unsupported: 'Desteklenmiyor'
}

const byTitle = (a: Game, b: Game) => a.title.localeCompare(b.title, 'tr', { numeric: true })

export function platformGames(state: AppState, platformId: string): Game[] {
  return state.games.filter((g) => g.platformId === platformId).sort(byTitle)
}

const SOURCE_LABEL: Record<string, string> = { steam: 'Steam', epic: 'Epic Games', gog: 'GOG', xbox: 'Xbox uygulaması', riot: 'Riot Games', ubisoft: 'Ubisoft Connect', ea: 'EA', battlenet: 'Battle.net', other: 'PC' }

export function gameSubtitle(g: Game): string {
  const p = getPlatform(g.platformId)
  const parts = [(g.meta.source && SOURCE_LABEL[g.meta.source]) || p?.name || g.platformId]
  if (g.missing) parts.push('Dosya bulunamadı')
  else if (g.lastLaunch?.result === 'crashed') parts.push('Son açılışta sorun')
  else if (g.playTimeSec > 0) parts.push(formatPlayTime(g.playTimeSec))
  return parts.join(' · ')
}

export function gameItem(g: Game, ctx: MenuCtx, prefix = ''): XItem {
  const p = getPlatform(g.platformId)
  if (g.platformId === 'apps') {
    const item = ctx.catalog.find((i) => i.id === g.meta.storeId)
    const iconKey = (g.meta.storeId ?? '').replace(/[^a-z0-9_-]/gi, '_')
    const rev = ctx.state.appIcons[iconKey]
    return {
      id: `${prefix}app:${g.id}`,
      title: g.title,
      subtitle: item?.description,
      icon: 'apps',
      tile: rev ? appIconUrl(g.meta.storeId ?? '', rev) : item?.icon,
      badge: { text: item?.short ?? 'APP', hue: item?.hue ?? 200 },
      hue: item?.hue,
      game: g,
      onActivate: () => ctx.launch(g.id),
      onOptions: () =>
        ctx.openOptions(g.title, [
          { label: 'Aç', icon: 'play', onSelect: () => ctx.launch(g.id) },
          {
            label: g.favorite ? 'Favorilerden çıkar' : 'Favorilere ekle',
            icon: 'star',
            onSelect: () => ctx.run(() => window.bridge.invoke('library:favorite', g.id, !g.favorite))
          },
          {
            label: 'Kaldır',
            icon: 'trash',
            danger: true,
            onSelect: () => ctx.run(() => window.bridge.invoke('store:uninstall', g.meta.storeId ?? ''), `${g.title} kaldırıldı.`)
          }
        ])
    }
  }
  return {
    id: `${prefix}game:${g.id}`,
    title: g.title,
    subtitle: gameSubtitle(g),
    icon: 'game',
    badge: p ? { text: p.short, hue: p.hue } : undefined,
    cover: g.hasCover ? coverUrl(g.id, g.coverRev) : undefined,
    coverIsIcon: g.coverIsIcon,
    game: g,
    hue: p?.hue,
    onActivate: () => ctx.openDetail(g.id),
    onOptions: () => ctx.openOptions(g.title, gameOptions(g, ctx))
  }
}

/** Per-game emulator choice, RetroArch shader and extra command line arguments. */
function emulatorOptions(g: Game, ctx: MenuCtx): OptItem {
  const p = getPlatform(g.platformId)!
  const opts = g.emuOptions ?? {}
  const runner = resolveRunner(p, g.emulator ?? ctx.state.settings.platformEmulator[p.id])
  const save = (runnerKeyValue: string | null, o = opts) => ctx.run(() => window.bridge.invoke('library:setEmulator', g.id, runnerKeyValue, o), 'Oyun ayarı kaydedildi.')
  const shaderNames: Record<string, string> = { none: 'Yok', crt: 'CRT (tüplü TV)', lcd: 'LCD ızgara (el konsolu)', smooth: 'Keskin yumuşatma' }
  return {
    label: 'Emülatör ayarları (bu oyun)',
    icon: 'chip',
    children: [
      {
        label: `Emülatör: ${g.emulator ? runnerLabel(runner!) : 'Platform varsayılanı'}`,
        icon: 'chip',
        children: [
          { label: 'Platform varsayılanı', checked: !g.emulator, onSelect: () => save(null) },
          ...p.runners.map((r) => ({ label: runnerLabel(r), checked: g.emulator === runnerKey(r), onSelect: () => save(runnerKey(r)) }))
        ]
      },
      ...(runner?.emulatorId === 'retroarch'
        ? [
            {
              label: `Görüntü filtresi: ${shaderNames[opts.shader ?? 'none']}`,
              icon: 'display' as IconName,
              children: (['none', 'crt', 'lcd', 'smooth'] as const).map((sh) => ({
                label: shaderNames[sh],
                checked: (opts.shader ?? 'none') === sh,
                onSelect: () => save(g.emulator ?? null, { ...opts, shader: sh })
              }))
            }
          ]
        : []),
      {
        label: opts.extraArgs ? `Ek komut satırı: ${opts.extraArgs}` : 'Ek komut satırı ekle',
        icon: 'file',
        onSelect: () => ctx.keyboard('Bu oyun için ek komut satırı argümanları', opts.extraArgs ?? '', (t) => save(g.emulator ?? null, { ...opts, extraArgs: t.trim() || undefined }))
      }
    ]
  }
}

export function gameOptions(g: Game, ctx: MenuCtx): OptItem[] {
  const current = getPlatform(g.platformId)
  const moveTargets = PLATFORMS.filter((p) => p.id !== g.platformId && (current?.kind === p.kind || p.kind === 'file'))
  return [
    { label: 'Başlat', icon: 'play', onSelect: () => ctx.launch(g.id) },
    { label: 'Bilgi', icon: 'info', onSelect: () => ctx.openDetail(g.id) },
    {
      label: g.favorite ? 'Favorilerden çıkar' : 'Favorilere ekle',
      icon: 'star',
      onSelect: () => ctx.run(() => window.bridge.invoke('library:favorite', g.id, !g.favorite))
    },
    { label: 'Ekran görüntüleri', icon: 'camera', onSelect: () => ctx.openScreenshots(g.id) },
    ...(current && current.runners.length ? [emulatorOptions(g, ctx)] : []),
    {
      label: 'Kapak görseli seç',
      icon: 'photo',
      onSelect: () =>
        ctx.browse({
          title: 'Kapak görseli seç',
          mode: 'file',
          extensions: ['.png', '.jpg', '.jpeg', '.webp'],
          onPick: (file) => ctx.run(() => window.bridge.invoke('library:setCover', g.id, file), 'Kapak güncellendi.')
        })
    },
    {
      label: 'Arka plan görseli seç',
      icon: 'display',
      onSelect: () =>
        ctx.browse({
          title: 'Arka plan görseli seç',
          mode: 'file',
          extensions: ['.png', '.jpg', '.jpeg', '.webp'],
          onPick: (file) => ctx.run(() => window.bridge.invoke('library:setBackground', g.id, file), 'Arka plan güncellendi.')
        })
    },
    g.musicPath
      ? {
          label: 'Tema müziğini kaldır',
          icon: 'sound',
          onSelect: () => ctx.run(() => window.bridge.invoke('library:setMusic', g.id, null), 'Tema müziği kaldırıldı.')
        }
      : {
          label: 'Tema müziği seç',
          icon: 'sound',
          onSelect: () =>
            ctx.browse({
              title: 'Tema müziği seç (mp3, ogg, wav, flac, m4a)',
              mode: 'file',
              extensions: ['.mp3', '.ogg', '.wav', '.flac', '.m4a', '.opus'],
              onPick: (file) => ctx.run(() => window.bridge.invoke('library:setMusic', g.id, file), 'Tema müziği ayarlandı.')
            })
        },
    {
      label: 'Platformu değiştir',
      icon: 'folder',
      children: moveTargets.map((p) => ({
        label: p.name,
        onSelect: () => ctx.run(() => window.bridge.invoke('library:move', g.id, p.id), `${p.name} kütüphanesine taşındı.`)
      }))
    },
    {
      label: 'Kütüphaneden kaldır',
      icon: 'trash',
      danger: true,
      onSelect: () =>
        ctx.confirm(`"${g.title}" kütüphaneden kaldırılsın mı? Oyun dosyası silinmez.`, () =>
          ctx.run(() => window.bridge.invoke('library:remove', g.id), 'Kaldırıldı.')
        )
    }
  ]
}

function choice<T extends string | number | boolean>(
  id: string,
  title: string,
  icon: IconName,
  current: T,
  options: { value: T; label: string }[],
  set: (v: T) => void,
  ctx: MenuCtx,
  subtitle?: string
): XItem {
  return {
    id,
    title,
    icon,
    subtitle,
    value: options.find((o) => o.value === current)?.label,
    children: options.map((o) => ({
      id: `${id}:${String(o.value)}`,
      title: o.label,
      icon: 'check',
      checked: o.value === current,
      onActivate: () => {
        set(o.value)
        ctx.back()
      }
    }))
  }
}

function toggle(id: string, title: string, icon: IconName, on: boolean, set: (v: boolean) => void, subtitle?: string): XItem {
  return { id, title, icon, subtitle, value: on ? 'Açık' : 'Kapalı', onActivate: () => set(!on) }
}

export function addFolderFlow(ctx: MenuCtx, platformId?: string): void {
  ctx.browse({
    title: platformId ? `${getPlatform(platformId)?.name} oyun klasörü seç` : 'Oyun klasörü seç',
    mode: 'folder',
    onPick: (folder) => {
      const scan = (pid: string) =>
        ctx.run(
          () => window.bridge.invoke('library:addFolder', folder, pid),
          (r) => {
            const rep = r as { added: number; skipped: number }
            return rep.added ? `${rep.added} oyun eklendi.` : rep.skipped ? 'Yeni oyun bulunamadı (hepsi zaten ekli).' : 'Bu klasörde oyun bulunamadı.'
          }
        )
      if (platformId) return scan(platformId)
      ctx.openOptions('Bu klasör hangi platforma ait?', [
        { label: 'Otomatik algıla', icon: 'search', onSelect: () => scan('auto') },
        ...FAMILIES.filter((f) => f.id !== 'apps').map((f) => ({
          label: f.name,
          icon: 'folder' as IconName,
          children: PLATFORMS.filter((p) => p.family === f.id && p.support !== 'unsupported').map((p) => ({
            label: p.name,
            onSelect: () => scan(p.id)
          }))
        }))
      ])
    }
  })
}

function addFileFlow(ctx: MenuCtx, p: PlatformDef): void {
  ctx.browse({
    title: `${p.name} oyunu seç`,
    mode: p.kind === 'folder' || p.kind === 'titleId' ? 'folder' : 'file',
    extensions: p.extensions.length ? p.extensions : undefined,
    onPick: (file) => ctx.run(() => window.bridge.invoke('library:addFile', file, p.id), 'Oyun eklendi.')
  })
}

/** One row per downloadable emulator, with live progress. */
export function downloadItems(ctx: MenuCtx): XItem[] {
  return ctx.state.downloadable.map((id) => {
    const emu = getEmulator(id)
    const job = ctx.state.downloads.find((j) => j.id === id)
    const installed = ctx.state.emulators.find((e) => e.id === id)?.found
    const busy = job && (job.status === 'queued' || job.status === 'downloading' || job.status === 'extracting')
    const usedBy = PLATFORMS.filter((p) => p.runners.some((r) => r.emulatorId === id)).map((p) => p.short)
    return {
      id: `dl:${id}`,
      title: emu?.name ?? id,
      icon: busy ? 'up' : installed ? 'check' : 'chip',
      value: busy ? (job!.status === 'downloading' ? `%${job!.total ? Math.floor((job!.received / job!.total) * 100) : 0}` : '…') : installed ? 'Kurulu' : 'İndir',
      subtitle: job ? downloadLine(job) : usedBy.join(', '),
      onActivate: () =>
        busy
          ? ctx.confirm(`${emu?.name} indirmesi iptal edilsin mi?`, () => void window.bridge.invoke('emu:cancelDownload', id))
          : installed
            ? ctx.confirm(`${emu?.name} zaten kurulu. Yeniden indirilsin mi?`, () => void window.bridge.invoke('emu:download', [id]))
            : ctx.run(() => window.bridge.invoke('emu:download', [id]), `${emu?.name} indirme sırasına eklendi.`)
    } satisfies XItem
  })
}

function emulatorItemFor(p: PlatformDef, ctx: MenuCtx): XItem | null {
  if (p.id === 'pc') return null
  if (p.runners.length === 0) {
    return { id: `emu:${p.id}`, title: 'Emülatör', icon: 'warning', value: 'Yok', subtitle: p.note }
  }
  const { settings, emulators } = ctx.state
  const runner = resolveRunner(p, settings.platformEmulator[p.id])!
  const status = emulators.find((e) => e.id === runner.emulatorId)
  return {
    id: `emu:${p.id}`,
    title: `Emülatör: ${runnerLabel(runner)}`,
    icon: 'chip',
    value: status?.found ? 'Hazır' : 'Ayarlanmadı',
    subtitle: status?.found ? status.path ?? undefined : 'Ayarlar → Emülasyon bölümünden yolunu seçin.',
    children: emulatorChildren(runner.emulatorId, ctx, p)
  }
}

function emulatorChildren(emuId: string, ctx: MenuCtx, platform?: PlatformDef): XItem[] {
  const emu = getEmulator(emuId)!
  const status = ctx.state.emulators.find((e) => e.id === emuId)
  const items: XItem[] = [
    {
      id: `emu:${emuId}:pick`,
      title: 'Çalıştırılabilir dosyayı seç',
      subtitle: status?.path ?? `${emu.exeNames[0]} dosyasını seçin`,
      icon: 'folder',
      onActivate: () =>
        ctx.browse({
          title: `${emu.name} (${emu.exeNames[0]})`,
          mode: 'file',
          extensions: ['.exe'],
          onPick: (file) => ctx.run(() => window.bridge.invoke('emu:setPath', emuId, file), `${emu.name} ayarlandı.`)
        })
    }
  ]
  if (status?.path) {
    items.push({
      id: `emu:${emuId}:clear`,
      title: 'Yolu temizle',
      icon: 'trash',
      onActivate: () => ctx.run(() => window.bridge.invoke('emu:setPath', emuId, null))
    })
  }
  if (platform && platform.runners.length > 1) {
    const current = runnerKey(resolveRunner(platform, ctx.state.settings.platformEmulator[platform.id])!)
    items.push(
      choice(
        `emu:${platform.id}:runner`,
        'Bu platform için emülatör',
        'chip',
        current,
        platform.runners.map((r) => ({ value: runnerKey(r), label: runnerLabel(r) })),
        (v) => ctx.setSettings({ platformEmulator: { ...ctx.state.settings.platformEmulator, [platform.id]: v } }),
        ctx
      )
    )
  }
  items.push({ id: `emu:${emuId}:info`, title: emu.name, subtitle: emu.notes ?? emu.homepage, icon: 'info', value: emu.homepage.replace(/^https?:\/\//, '') })
  return items
}

function platformItem(p: PlatformDef, ctx: MenuCtx): XItem {
  const games = platformGames(ctx.state, p.id)
  const children: XItem[] = []
  if (games.length) {
    children.push({
      id: `gallery:${p.id}`,
      title: 'Galeri görünümü',
      subtitle: `${games.length} oyun`,
      icon: 'grid',
      onActivate: () => ctx.openGallery(p.name, games)
    })
    children.push(...games.map((g) => gameItem(g, ctx)))
  }
  if (p.support !== 'unsupported') {
    children.push({
      id: `addfolder:${p.id}`,
      title: 'Oyun klasörü ekle',
      subtitle: 'Klasördeki tüm oyunları tarar ve ekler',
      icon: 'plus',
      onActivate: () => addFolderFlow(ctx, p.id)
    })
    children.push({
      id: `addfile:${p.id}`,
      title: p.kind === 'exe' ? 'PC oyunu ekle' : 'Tek oyun ekle',
      subtitle: p.kind === 'exe' ? '.exe, .lnk veya .url dosyası seçin' : undefined,
      icon: 'file',
      onActivate: () => addFileFlow(ctx, p)
    })
  }
  const emu = emulatorItemFor(p, ctx)
  if (emu) children.push(emu)
  if (p.note) children.push({ id: `note:${p.id}`, title: 'Bilgi', subtitle: p.note, icon: 'info' })

  return {
    id: `platform:${p.id}`,
    title: p.name,
    subtitle: `${games.length} oyun · ${SUPPORT_LABEL[p.support]}`,
    icon: 'folder',
    badge: { text: p.short, hue: p.hue },
    hue: p.hue,
    children,
    onView: games.length ? () => ctx.openGallery(p.name, games) : undefined
  }
}

const WINDOW_MODES: { value: WindowMode; label: string }[] = [
  { value: 'fullscreen', label: 'Tam ekran' },
  { value: 'borderless', label: 'Kenarlıksız pencere' },
  { value: 'windowed', label: 'Pencere' }
]

export const THEMES: { value: string; label: string; hue: number | null }[] = [
  { value: 'auto', label: 'Otomatik (kategoriye göre)', hue: null },
  { value: 'classic', label: 'Klasik mavi', hue: 218 },
  { value: 'crimson', label: 'Kızıl', hue: 352 },
  { value: 'emerald', label: 'Zümrüt', hue: 150 },
  { value: 'violet', label: 'Mor', hue: 275 },
  { value: 'gold', label: 'Altın', hue: 40 },
  { value: 'night', label: 'Gece', hue: 0 }
]

/** Built-in themes + themes added from the store or the themes folder. */
export function allThemes(ctx: { state: AppState }): { value: string; label: string; hue: number | null; night?: boolean; css?: string }[] {
  return [
    ...THEMES,
    ...ctx.state.themes.map((t) => ({ value: t.id, label: t.name, hue: t.hue, night: t.night, css: t.css }))
  ]
}

/** Save management shared by every emulator. */
function savesSection(ctx: MenuCtx): XItem {
  const s = ctx.state.settings
  const set = ctx.setSettings
  const when = (t: number) => new Date(t).toLocaleString('tr-TR', { day: 'numeric', month: 'short', year: 'numeric', hour: '2-digit', minute: '2-digit' })
  const perEmu: XItem[] = ctx.state.saves.map((sv) => ({
    id: `saves:${sv.emuId}`,
    title: sv.name,
    icon: 'storage' as IconName,
    value: sv.backups.length ? `${sv.backups.length} yedek` : sv.locations.length ? 'Yedek yok' : 'Kayıt bulunamadı',
    subtitle: sv.locations.length ? sv.locations.join(' · ') : 'Bu emülatörün kayıt klasörü henüz oluşmamış (oyun oynayınca oluşur).',
    children: [
      {
        id: `saves:${sv.emuId}:backup`,
        title: 'Şimdi yedekle',
        icon: 'up' as IconName,
        onActivate: () => ctx.run(() => window.bridge.invoke('saves:backup', [sv.emuId]), (n) => (n ? null : 'Yedeklenecek kayıt bulunamadı.'))
      },
      {
        id: `saves:${sv.emuId}:restore`,
        title: 'Yedekten geri yükle',
        icon: 'refresh' as IconName,
        emptyText: 'Henüz yedek yok.',
        children: sv.backups.map((b) => ({
          id: `saves:${sv.emuId}:${b.name}`,
          title: when(b.at),
          subtitle: `${formatBytes(b.size)}${b.synced ? ' · eşitleme klasöründe de var' : ''}`,
          icon: 'storage' as IconName,
          onActivate: () =>
            ctx.confirm(`${sv.name} kayıtları ${when(b.at)} tarihli yedeğe döndürülsün mü?\nŞu anki kayıtlar önce ayrıca yedeklenir.`, () =>
              ctx.run(() => window.bridge.invoke('saves:restore', sv.emuId, b.file))
            )
        }))
      },
      { id: `saves:${sv.emuId}:open`, title: 'Kayıt klasörünü aç', icon: 'folder' as IconName, onActivate: () => void window.bridge.invoke('saves:openFolder', sv.emuId) }
    ]
  }))
  return {
    id: 'set:saves',
    title: 'Kayıt Yönetimi',
    icon: 'storage',
    subtitle: 'Tüm emülatörlerin oyun kayıtları tek yerde: yedekle, geri yükle, eşitle',
    children: [
      {
        id: 'saves:all',
        title: 'Tüm kayıtları şimdi yedekle',
        icon: 'up',
        onActivate: () => ctx.run(() => window.bridge.invoke('saves:backup'), (n) => (n ? null : 'Yedeklenecek kayıt bulunamadı.'))
      },
      toggle('saves:auto', 'Oyundan sonra otomatik yedekle', 'refresh', s.saveAutoBackup, (v) => set({ saveAutoBackup: v }), 'Her oyun oturumu bitince o emülatörün kayıtları yedeklenir (son 10 yedek saklanır)'),
      {
        id: 'saves:sync',
        title: 'Eşitleme klasörü',
        icon: 'folder',
        value: s.saveSyncFolder ? 'Açık' : 'Kapalı',
        subtitle: s.saveSyncFolder || 'Yedeklerin bir kopyası OneDrive, Google Drive veya bir USB diskteki klasöre de yazılır',
        onActivate: () =>
          s.saveSyncFolder
            ? ctx.confirm('Eşitleme klasörü kapatılsın mı?', () => set({ saveSyncFolder: '' }))
            : ctx.browse({ title: 'Eşitleme klasörü seç', mode: 'folder', onPick: (dir) => set({ saveSyncFolder: dir }) })
      },
      {
        id: 'saves:refresh',
        title: 'Kayıt konumlarını yeniden tara',
        icon: 'search',
        onActivate: () => ctx.run(() => window.bridge.invoke('saves:refresh'), 'Kayıt konumları güncellendi.')
      },
      ...perEmu
    ]
  }
}

const HOTKEYS: { value: OverlayHotkey; label: string }[] = [
  { value: 'both', label: 'Guide tuşu veya Back + Start' },
  { value: 'guide', label: 'Yalnızca Guide (Xbox / PS) tuşu' },
  { value: 'back-start', label: 'Yalnızca Back + Start' }
]

function settingsCategory(ctx: MenuCtx): XItem[] {
  const s = ctx.state.settings
  const set = ctx.setSettings
  const configured = ctx.state.emulators.filter((e) => e.found).length

  const display: XItem = {
    id: 'set:display',
    title: 'Görüntü Ayarları',
    icon: 'display',
    children: [
      choice('set:windowMode', 'Ekran modu', 'display', s.windowMode, WINDOW_MODES, (v) => set({ windowMode: v }), ctx),
      toggle('set:animations', 'Menü animasyonları', 'refresh', s.animations, (v) => set({ animations: v }), 'Kapatmak düşük donanımda akıcılığı artırır'),
      toggle('set:gameBg', 'Oyun arka planları', 'photo', s.gameBackgrounds, (v) => set({ gameBackgrounds: v }), 'Bir oyunun üstüne gelince görseli arka planda gösterilir'),
      choice('set:theme', 'Arka plan teması', 'palette', s.theme, allThemes(ctx).map((t) => ({ value: t.value, label: t.label })), (v) => set({ theme: v }), ctx)
    ]
  }
  const sound: XItem = {
    id: 'set:sound',
    title: 'Ses Ayarları',
    icon: 'sound',
    children: [
      toggle('set:menuSounds', 'Menü sesleri', 'sound', s.menuSounds, (v) => set({ menuSounds: v })),
      toggle('set:trailers', 'Oyun fragmanları', 'play', s.gameTrailers, (v) => set({ gameTrailers: v }), 'Bir oyunun üstünde durunca resmi fragmanı sesiyle arka planda oynar (Steam)'),
      toggle('set:gameMusic', 'Oyun tema müzikleri', 'sound', s.gameMusic, (v) => set({ gameMusic: v }), 'Bir oyunun üstüne gelince tema müziği çalar (oyun seçeneklerinden müzik atanabilir)'),
      choice(
        'set:musicVolume',
        'Tema müziği ses seviyesi',
        'sound',
        s.musicVolume,
        [0.2, 0.4, 0.6, 0.8, 1].map((v) => ({ value: v, label: `%${Math.round(v * 100)}` })),
        (v) => set({ musicVolume: v }),
        ctx
      ),
      choice(
        'set:volume',
        'Menü ses seviyesi',
        'sound',
        s.soundVolume,
        [0.25, 0.5, 0.75, 1].map((v) => ({ value: v, label: `%${Math.round(v * 100)}` })),
        (v) => set({ soundVolume: v }),
        ctx
      )
    ]
  }
  const controllers: XItem = {
    id: 'set:pads',
    title: 'Kontrolcü Ayarları',
    icon: 'pad',
    children: [
      {
        id: 'set:pads:list',
        title: 'Bağlı kontrolcüler',
        icon: 'pad',
        value: String(ctx.pads.length),
        children: ctx.pads.map((name, i) => ({ id: `pad:${i}`, title: `Kontrolcü ${i + 1}`, subtitle: name, icon: 'pad' })),
        emptyText: 'Kontrolcü algılanmadı. Bağladıktan sonra herhangi bir tuşa basın.'
      },
      choice(
        'set:pads:style',
        'Tuş simgeleri',
        'pad',
        s.padStyle,
        [
          { value: 'auto', label: 'Kullanılan kumandaya göre' },
          { value: 'xbox', label: 'Xbox (A B X Y)' },
          { value: 'ps', label: 'PlayStation (✕ ○ □ △)' },
          { value: 'switch', label: 'Nintendo (B A Y X)' },
          { value: 'keyboard', label: 'Klavye' }
        ],
        (v) => set({ padStyle: v }),
        ctx,
        'Ekranın altındaki tuş ipuçları bu simgelerle gösterilir'
      ),
      { id: 'set:pads:remap', title: 'Tuş ataması', icon: 'grid', subtitle: 'Her işlemi istediğin tuşa ata', value: Object.keys(s.padMap).length ? 'Özel' : 'Varsayılan', onActivate: () => ctx.openRemap() },
      toggle(
        'set:pads:auto',
        'Emülatörlerde otomatik kol ayarı',
        'chip',
        s.autoPadConfig,
        (v) => set({ autoPadConfig: v }),
        'Oyun başlamadan önce emülatörün kol ayarları yazılır; emülatörü açıp ayarlamana gerek kalmaz'
      ),
      {
        id: 'set:pads:reapply',
        title: 'Kol ayarlarını yeniden uygula',
        icon: 'refresh',
        subtitle: 'Emülatörlerde değiştirdiğin kol ayarlarını varsayılana döndürür (bir sonraki oyunda yazılır)',
        onActivate: () => ctx.confirm('Emülatörlerin kol ayarları bir sonraki oyunda yeniden yazılsın mı?', () => set({ padConfigured: {} }))
      },
      toggle('set:pads:rumble', 'Titreşim', 'pad', s.padRumble, (v) => set({ padRumble: v }), 'Seçim yaparken kısa titreşim (destekleyen kumandalarda)'),
      choice('set:hotkey', 'Oyun içi menü tuşu', 'game', s.overlayHotkey, HOTKEYS, (v) => set({ overlayHotkey: v }), ctx, 'Klavye: Ctrl + Shift + Home'),
      {
        id: 'set:pads:map',
        title: 'Tuş düzeni',
        icon: 'info',
        subtitle: 'Kumanda: seç, geri, seçenekler, galeri, omuz tuşlarıyla kategori · Klavye: oklar, Enter, Esc, T, G, Q/E'
      }
    ]
  }
  const gameSettings: XItem = {
    id: 'set:game',
    title: 'Oyun Modu ve Sistem',
    icon: 'game',
    children: [
      {
        id: 'set:ra',
        title: 'RetroAchievements hesabı',
        icon: 'star',
        value: s.raUser ? s.raUser : 'Bağlı değil',
        subtitle: 'Emülatör oyunlarında kupalar için. Web API anahtarını retroachievements.org → Settings sayfasından alırsın.',
        children: [
          {
            id: 'set:ra:user',
            title: 'Kullanıcı adı',
            icon: 'user',
            value: s.raUser || '—',
            onActivate: () => ctx.keyboard('RetroAchievements kullanıcı adı', s.raUser, (t) => set({ raUser: t.trim() }))
          },
          {
            id: 'set:ra:key',
            title: 'Web API anahtarı',
            icon: 'file',
            value: s.raKey ? 'Girildi' : '—',
            onActivate: () => ctx.keyboard('RetroAchievements Web API anahtarı', '', (t) => set({ raKey: t.trim() }))
          },
          { id: 'set:ra:off', title: 'Hesabı kaldır', icon: 'trash', onActivate: () => set({ raUser: '', raKey: '' }) }
        ]
      },
      toggle('set:notify', 'Oyun sırasında bildirimler', 'info', s.notifyInGame, (v) => set({ notifyInGame: v }), 'İndirme, kupa ve yedek bildirimleri oyunun üstünde kısa süre görünür'),
      toggle('set:pause', 'Menü açılınca oyunu duraklat', 'pause', s.pauseOnOverlay, (v) => set({ pauseOnOverlay: v }), 'Oyun içi menü açıkken oyun işlemi askıya alınır'),
      toggle('set:gm:prio', 'Oyuna yüksek işlemci önceliği', 'chip', s.gmPriority, (v) => set({ gmPriority: v }), 'Oyun, arka plandaki programlardan önce işlemci zamanı alır'),
      toggle('set:gm:power', 'Yüksek performans güç planı', 'power', s.gmPowerPlan, (v) => set({ gmPowerPlan: v }), 'Dengeli planda oyun sırasında Yüksek performansa geçer, sonra geri döner'),
      {
        ...toggle(
          'set:gm:explorer',
          'Oyun sırasında Windows masaüstünü kapat',
          'monitor',
          s.gmStopExplorer,
          (v) => set({ gmStopExplorer: v }),
          'Görev çubuğu ve masaüstü oyun boyunca kapanır, oyun bitince geri açılır. Açık Dosya Gezgini pencereleri kapanır.'
        ),
        onActivate: () =>
          s.gmStopExplorer
            ? set({ gmStopExplorer: false })
            : ctx.confirm(
                'Oyun sırasında Windows masaüstü (görev çubuğu, Dosya Gezgini) kapatılsın mı?\nOyun bitince veya uygulama kapanınca otomatik geri açılır.',
                () => set({ gmStopExplorer: true })
              )
      },
      choice(
        'set:sys:drm',
        'Korumalı uygulamalar (Netflix vb.)',
        'apps',
        s.drmMode,
        [
          { value: 'auto', label: 'Otomatik' },
          { value: 'console', label: 'Konsolun içinde' },
          { value: 'edge', label: 'Microsoft Edge' }
        ],
        (v) => set({ drmMode: v }),
        ctx,
        ctx.state.drm.detail
      ),
      toggle('set:sys:restart', 'Kapanırsa otomatik yeniden başlat', 'refresh', s.autoRestart, (v) => set({ autoRestart: v }), 'Beklenmedik kapanmada uygulama kendini yeniden açar ve masaüstünü geri getirir'),
      toggle('set:sys:startup', 'Windows açılınca başlat', 'home', s.launchOnStartup, (v) => set({ launchOnStartup: v }), 'Bilgisayar açılınca doğrudan konsol arayüzü gelir'),
      {
        id: 'set:sys:wizard',
        title: 'Kurulum sihirbazını yeniden çalıştır',
        icon: 'refresh',
        onActivate: () => set({ setupDone: false })
      }
    ]
  }
  const emulation: XItem = {
    id: 'set:emu',
    title: 'Emülasyon Ayarları',
    icon: 'chip',
    value: `${configured}/${EMULATORS.length}`,
    children: [
      {
        id: 'set:emu:detect',
        title: 'Emülatörleri otomatik bul',
        subtitle: 'Program Files, Masaüstü, İndirilenler, C:\\Emulators gibi yaygın konumları tarar',
        icon: 'search',
        onActivate: () => ctx.run(() => window.bridge.invoke('emu:detect'), (n) => (n ? `${n} emülatör bulundu.` : 'Yeni emülatör bulunamadı.'))
      },
      {
        id: 'set:emu:download',
        title: 'Emülatör indir',
        subtitle: 'Resmi sürümleri indirip kurar. İndirme arka planda sürer; bu sırada gezinebilir, oyun oynayabilirsin.',
        icon: 'up',
        children: downloadItems(ctx)
      },
      {
        id: 'set:emu:folder',
        title: 'Emülatör klasörünü tara',
        subtitle: 'Emülatörlerin kurulu olduğu klasörü seçin',
        icon: 'folder',
        onActivate: () =>
          ctx.browse({
            title: 'Emülatör klasörü seç',
            mode: 'folder',
            onPick: (dir) =>
              ctx.run(() => window.bridge.invoke('emu:detect', dir), (n) => (n ? `${n} emülatör bulundu.` : 'Bu klasörde emülatör bulunamadı.'))
          })
      },
      ...EMULATORS.map((e) => {
        const st = ctx.state.emulators.find((x) => x.id === e.id)
        return {
          id: `set:emu:${e.id}`,
          title: e.name,
          subtitle: st?.path ?? 'Ayarlanmadı',
          value: st?.found ? 'Hazır' : st?.path ? 'Bulunamadı' : '—',
          icon: 'chip' as IconName,
          children: emulatorChildren(e.id, ctx)
        }
      }),
      {
        id: 'set:emu:platforms',
        title: 'Platform emülatör seçimleri',
        icon: 'grid',
        children: PLATFORMS.filter((p) => p.runners.length > 1).map((p) => {
          const current = runnerKey(resolveRunner(p, s.platformEmulator[p.id])!)
          return choice(
            `set:runner:${p.id}`,
            p.name,
            'chip',
            current,
            p.runners.map((r) => ({ value: runnerKey(r), label: runnerLabel(r) })),
            (v) => set({ platformEmulator: { ...s.platformEmulator, [p.id]: v } }),
            ctx
          )
        })
      },
      {
        id: 'set:emu:bios',
        title: 'BIOS ve sistem dosyaları',
        icon: 'info',
        subtitle: 'BIOS, firmware ve anahtarlar her emülatörün kendi klasörüne, kendi cihazınızdan aldığınız yasal kopyalardan yerleştirilmelidir.'
      }
    ]
  }
  const storage: XItem = {
    id: 'set:storage',
    title: 'Kütüphane ve Depolama',
    icon: 'storage',
    children: [
      { id: 'set:lib:add', title: 'Oyun klasörü ekle', icon: 'plus', onActivate: () => addFolderFlow(ctx) },
      {
        id: 'set:lib:folders',
        title: 'Oyun klasörleri',
        icon: 'folder',
        value: String(s.watchedFolders.length),
        emptyText: 'Henüz klasör eklenmedi.',
        children: s.watchedFolders.map((f) => ({
          id: `folder:${f.path}`,
          title: f.path,
          subtitle: f.platformId === 'auto' ? 'Otomatik algılama' : getPlatform(f.platformId)?.name,
          icon: 'folder',
          onActivate: () =>
            ctx.confirm(`Bu klasör ve içinden eklenen oyunlar kütüphaneden kaldırılsın mı?\n${f.path}\n(Dosyalar silinmez.)`, () =>
              ctx.run(() => window.bridge.invoke('library:removeFolder', f.path), (n) => `Klasör kaldırıldı (${n} oyun).`)
            )
        }))
      },
      {
        id: 'set:lib:rescan',
        title: 'Tüm klasörleri yeniden tara',
        icon: 'refresh',
        onActivate: () => ctx.run(() => window.bridge.invoke('library:rescan'), (r) => `${(r as { added: number }).added} yeni oyun eklendi.`)
      },
      toggle('set:lib:startup', 'Açılışta yeni oyunları tara', 'refresh', s.scanOnStartup, (v) => set({ scanOnStartup: v })),
      toggle('set:lib:covers', 'Kapakları otomatik indir', 'photo', s.downloadCovers, (v) => set({ downloadCovers: v }), 'libretro-thumbnails arşivinden'),
      {
        id: 'set:lib:fetch',
        title: 'Eksik kapak ve arka planları şimdi indir',
        icon: 'photo',
        onActivate: () => ctx.run(() => window.bridge.invoke('library:fetchCovers'), (n) => (n ? `${n} oyun için kapak aranıyor…` : 'Eksik kapak yok ya da indirme kapalı.'))
      }
    ]
  }
  const ui: XItem = {
    id: 'set:ui',
    title: 'Arayüz Kişiselleştirme',
    icon: 'palette',
    children: [
      {
        id: 'set:ui:hidden',
        title: 'Kategorileri göster / gizle',
        icon: 'grid',
        children: CATEGORY_DEFS.filter((c) => c.id !== 'settings' && c.id !== 'home').map((c) =>
          toggle(`set:ui:cat:${c.id}`, c.title, c.icon, !s.hiddenCategories.includes(c.id), (v) =>
            set({ hiddenCategories: v ? s.hiddenCategories.filter((x) => x !== c.id) : [...s.hiddenCategories, c.id] })
          )
        )
      }
    ]
  }
  const about: XItem = {
    id: 'set:about',
    title: 'Hakkında',
    icon: 'info',
    children: [
      { id: 'about:name', title: BRAND.name, subtitle: BRAND.tagline, icon: 'info', value: `v${ctx.state.version}` },
      { id: 'about:games', title: 'Kütüphanedeki oyunlar', icon: 'game', value: String(ctx.state.games.length) },
      (() => {
        const u = ctx.state.update
        const value =
          u.status === 'ready' ? `v${u.version} hazır` : u.status === 'downloading' ? `%${Math.round((u.progress ?? 0) * 100)}` : u.status === 'checking' ? 'Denetleniyor…' : u.status === 'none' ? 'Güncel' : u.status === 'error' ? 'Hata' : u.status === 'disabled' ? 'Kapalı' : ''
        return u.status === 'ready'
          ? {
              id: 'about:update',
              title: `Güncellemeyi kur (v${u.version})`,
              icon: 'up' as IconName,
              value,
              subtitle: u.notes ?? 'Kurulumdan sonra uygulama yeniden açılır.',
              onActivate: () => ctx.confirm(`${BRAND.name} v${u.version} kurulsun mu? Uygulama kapanıp yeniden açılır.`, () => ctx.run(() => window.bridge.invoke('update:install')))
            }
          : {
              id: 'about:update',
              title: 'Güncellemeleri denetle',
              icon: 'refresh' as IconName,
              value,
              subtitle: u.message ?? 'Yeni sürümler GitHub Releases üzerinden indirilir ve doğrulanır.',
              onActivate: () => ctx.run(() => window.bridge.invoke('update:check'))
            }
      })(),
      toggle('about:autoupdate', 'Açılışta güncellemeleri denetle', 'refresh', ctx.state.settings.autoUpdate, (v) => ctx.setSettings({ autoUpdate: v })),
      {
        id: 'about:legal',
        title: 'Yasal',
        icon: 'info',
        subtitle: `${BRAND.name} oyun, BIOS veya firmware içermez ve dağıtmaz. Yalnızca sahip olduğunuz yazılımların yedeklerini kullanın.`
      }
    ]
  }
  const quit: XItem = {
    id: 'set:quit',
    title: `${BRAND.name} uygulamasından çık`,
    icon: 'power',
    onActivate: () => ctx.confirm('Uygulamadan çıkılsın mı?', () => void window.bridge.invoke('app:quit'))
  }
  return [display, sound, controllers, gameSettings, emulation, savesSection(ctx), storage, ui, about, quit]
}

const CATEGORY_DEFS: { id: string; title: string; icon: IconName }[] = [
  { id: 'home', title: 'Ana Sayfa', icon: 'home' },
  { id: 'playstation', title: 'PlayStation', icon: 'disc' },
  { id: 'xbox', title: 'Xbox', icon: 'sphere' },
  { id: 'nintendo', title: 'Nintendo', icon: 'cartridge' },
  { id: 'sega', title: 'Sega', icon: 'ring' },
  { id: 'other', title: 'Diğer Platformlar', icon: 'joystick' },
  { id: 'pc', title: 'PC Oyunları', icon: 'monitor' },
  { id: 'apps', title: 'Uygulamalar', icon: 'apps' },
  { id: 'media', title: 'Medya', icon: 'photo' },
  { id: 'store', title: 'Mağaza', icon: 'bag' },
  { id: 'settings', title: 'Ayarlar', icon: 'gear' }
]

function homeItems(ctx: MenuCtx): XItem[] {
  const { games, session } = ctx.state
  const items: XItem[] = []
  if (session) {
    const g = games.find((x) => x.id === session.gameId)
    items.push({
      id: 'home:resume',
      title: `Oyuna dön: ${g?.title ?? ''}`,
      subtitle: session.paused ? 'Duraklatıldı' : 'Çalışıyor',
      icon: 'play',
      cover: g?.hasCover ? coverUrl(g.id, g.coverRev) : undefined,
      onActivate: () => void window.bridge.invoke('session:returnToGame')
    })
    items.push({
      id: 'home:quitgame',
      title: 'Oyunu kapat',
      icon: 'exit',
      onActivate: () => ctx.confirm('Çalışan oyun kapatılsın mı? Kaydedilmemiş ilerleme kaybolur.', () => void window.bridge.invoke('session:stop'))
    })
  }
  const activeDownloads = ctx.state.downloads.filter((d) => d.status !== 'done')
  if (activeDownloads.length) {
    items.push({ id: 'home:downloads', title: 'İndirmeler', icon: 'up', value: String(activeDownloads.length), children: downloadItems(ctx) })
  }
  if (ctx.state.notices.length) {
    items.push({
      id: 'home:notices',
      title: 'Bildirimler',
      icon: 'info',
      value: String(ctx.state.notices.length),
      children: [
        ...ctx.state.notices.map((n) => ({
          id: `notice:${n.id}`,
          title: n.title,
          subtitle: [n.text, new Date(n.at).toLocaleString('tr-TR', { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' })].filter(Boolean).join(' · '),
          icon: (n.kind === 'trophy' ? 'star' : n.kind === 'download' ? 'up' : n.kind === 'update' ? 'refresh' : n.kind === 'save' ? 'storage' : 'info') as IconName
        })),
        { id: 'notices:clear', title: 'Bildirimleri temizle', icon: 'trash', onActivate: () => void window.bridge.invoke('notices:clear') }
      ]
    })
  }
  items.push({ id: 'home:trophies', title: 'Kupalar', icon: 'star', subtitle: 'Steam ve RetroAchievements başarımların', onActivate: () => ctx.openTrophies() })
  items.push({ id: 'home:stats', title: 'Oynama istatistikleri', icon: 'chart', subtitle: 'Haftalık süre ve en çok oynanan oyunlar', onActivate: () => ctx.openStats() })
  const recent = games.filter((g) => g.lastPlayedAt).sort((a, b) => b.lastPlayedAt! - a.lastPlayedAt!)
  items.push(...recent.slice(0, 4).map((g) => gameItem(g, ctx, 'home:')))
  if (recent.length) {
    items.push({ id: 'home:recent', title: 'Son oynananlar', icon: 'clock', value: String(recent.length), children: recent.slice(0, 30).map((g) => gameItem(g, ctx, 'recent:')) })
  }
  const favs = games.filter((g) => g.favorite).sort(byTitle)
  items.push({
    id: 'home:favs',
    title: 'Favoriler',
    icon: 'star',
    value: String(favs.length),
    children: favs.map((g) => gameItem(g, ctx, 'fav:')),
    emptyText: 'Bir oyunda seçenekler tuşuna basıp favorilere ekleyin.',
    onView: favs.length ? () => ctx.openGallery('Favoriler', favs) : undefined
  })
  const added = [...games].sort((a, b) => b.addedAt - a.addedAt).slice(0, 30)
  items.push({
    id: 'home:added',
    title: 'Son eklenenler',
    icon: 'plus',
    value: String(added.length),
    children: added.map((g) => gameItem(g, ctx, 'added:')),
    emptyText: 'Henüz oyun eklenmedi.'
  })
  const unplayed = games.filter((g) => g.playCount === 0 && !g.missing)
  if (unplayed.length) {
    // Deterministic daily shuffle so the list changes but is stable within a day.
    const seed = Math.floor(Date.now() / 86400000)
    const pick = [...unplayed].sort((a, b) => ((parseInt(a.id.slice(0, 6), 16) ^ seed) % 997) - ((parseInt(b.id.slice(0, 6), 16) ^ seed) % 997)).slice(0, 15)
    items.push({ id: 'home:discover', title: 'Keşfet', subtitle: 'Henüz hiç oynamadığın oyunlar', icon: 'search', children: pick.map((g) => gameItem(g, ctx, 'disc:')) })
  }
  const problems = games.filter((g) => g.missing || g.lastLaunch?.result === 'crashed' || g.lastLaunch?.result === 'error')
  if (problems.length) {
    items.push({
      id: 'home:problems',
      title: 'Dikkat gerektirenler',
      subtitle: 'Dosyası bulunamayan veya açılamayan oyunlar',
      icon: 'warning',
      value: String(problems.length),
      children: problems.map((g) => ({ ...gameItem(g, ctx, 'prob:'), subtitle: g.missing ? 'Dosya bulunamadı' : g.lastLaunch?.message }))
    })
  }
  items.push({
    id: 'home:add',
    title: 'Oyun ekle',
    subtitle: 'Klasör tara veya tek oyun ekle',
    icon: 'plus',
    children: [
      {
        id: 'home:add:scan',
        title: 'Bilgisayarı tara',
        subtitle: 'Steam, Epic Games, GOG oyunlarını, emülatörleri ve oyun klasörlerini otomatik bulur',
        icon: 'search',
        onActivate: () => ctx.openScan()
      },
      { id: 'home:add:folder', title: 'Oyun klasörü ekle', subtitle: 'Platformu otomatik algıla veya seç', icon: 'folder', onActivate: () => addFolderFlow(ctx) },
      {
        id: 'home:add:rescan',
        title: 'Tüm klasörleri yeniden tara',
        icon: 'refresh',
        onActivate: () => ctx.run(() => window.bridge.invoke('library:rescan'), (r) => `${(r as { added: number }).added} yeni oyun eklendi.`)
      }
    ]
  })
  return items
}

export function buildMenu(ctx: MenuCtx): XCategory[] {
  const hidden = new Set(ctx.state.settings.hiddenCategories)
  const cats: XCategory[] = []
  for (const def of CATEGORY_DEFS) {
    if (hidden.has(def.id)) continue
    const fam = FAMILIES.find((f) => f.id === def.id)
    let items: XItem[]
    if (def.id === 'home') items = homeItems(ctx)
    else if (def.id === 'settings') items = settingsCategory(ctx)
    else if (def.id === 'media') {
      items = [
        { id: 'media:shots', title: 'Ekran Görüntüleri', subtitle: 'Oyun içi menüden aldığın görüntüler', icon: 'camera', onActivate: () => ctx.openScreenshots() }
      ]
    } else if (def.id === 'apps') {
      items = [
        ...platformGames(ctx.state, 'apps').map((g) => gameItem(g, ctx)),
        { id: 'apps:store', title: 'Mağazadan uygulama ekle', icon: 'bag', subtitle: 'YouTube, Netflix, Disney+, Spotify, Twitch…', onActivate: () => ctx.openStore('apps') }
      ]
    } else if (def.id === 'store') {
      items = [
        { id: 'store:open', title: `${BRAND.name} Mağaza`, subtitle: 'Uygulamalar, emülatörler, temalar ve modlar', icon: 'bag', onActivate: () => ctx.openStore() },
        {
          id: 'store:downloads',
          title: 'İndirmeler',
          subtitle: 'İndirmeler arka planda sürer; bu sırada gezinip oyun oynayabilirsin.',
          icon: 'up',
          value: String(ctx.state.downloads.filter((j) => j.status === 'queued' || j.status === 'downloading' || j.status === 'extracting').length || ''),
          onActivate: () => ctx.openStore('downloads')
        }
      ]
    } else if (def.id === 'pc') {
      const pc = getPlatform('pc')!
      items = platformItem(pc, ctx).children!
    } else {
      items = PLATFORMS.filter((p) => p.family === def.id).map((p) => platformItem(p, ctx))
    }
    const hue = fam?.hue ?? ({ home: 218, media: 190, store: 285, apps: 200 } as Record<string, number>)[def.id] ?? 230
    cats.push({ id: def.id, title: def.title, icon: def.icon, hue, items })
  }
  return cats
}
