import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { BRAND } from '@shared/brand'
import type { AppState, Game, Toast } from '@shared/types'
import { call, coverUrl, downloadLine, drmInConsole, formatEta, useAppState } from './api'
import { WaveBackground } from './background'
import { Icon } from './icons'
import { input, useInput } from './input'
import { allThemes, buildMenu, type BrowseRequest, type MenuCtx, type OptItem } from './menu'
import { BUILTIN_CATALOG, type StoreItem } from '@shared/catalog'
import { configureSound, sfx } from './sound'
import { Xmb, type XmbHandle } from './components/Xmb'
import { ConfirmDialog, FileBrowser, OptionsPanel, Toasts } from './components/Modals'
import { GameDetail, Gallery, Screenshots } from './components/GameViews'
import { Setup } from './components/Setup'
import { Backdrop, ThemeMusic, Trailer } from './components/Ambience'
import { Store, type StoreTab } from './components/Store'
import { WebApp } from './components/WebApp'
import { OnScreenKeyboard } from './components/Keyboard'

type Modal =
  | { type: 'detail'; gameId: string }
  | { type: 'gallery'; title: string; gameIds: string[] }
  | { type: 'options'; title: string; items: OptItem[] }
  | { type: 'browse'; req: BrowseRequest }
  | { type: 'confirm'; text: string; onYes: () => void }
  | { type: 'shots'; gameId?: string }
  | { type: 'scan' }
  | { type: 'store'; tab?: StoreTab }
  | { type: 'web'; item: StoreItem; gameId?: string }
  | { type: 'keyboard'; title: string; initial: string; onDone: (text: string) => void }

type ModalEntry = Modal & { key: number }

function Clock({ state, pads }: { state: AppState; pads: number }) {
  const [now, setNow] = useState(() => new Date())
  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 1000 * 15)
    return () => clearInterval(t)
  }, [])
  return (
    <div className="status">
      {state.scanning && (
        <span className="status-chip">
          <Icon name="refresh" className="spin" /> Taranıyor
        </span>
      )}
      {(() => {
        const j = state.downloads.find((d) => d.status === 'downloading' || d.status === 'extracting')
        const waiting = state.downloads.filter((d) => d.status === 'queued').length
        return j ? (
          <span className="status-chip">
            <Icon name="up" className="rot180" /> {j.name}{' '}
            {j.status === 'downloading'
              ? [`%${j.total ? Math.floor((j.received / j.total) * 100) : 0}`, formatEta(j.etaSec)].filter(Boolean).join(' · ')
              : downloadLine(j)}
            {waiting ? ` (+${waiting})` : ''}
          </span>
        ) : null
      })()}
      {state.session && (
        <span className="status-chip">
          <Icon name="play" /> Oyun {state.session.paused ? 'duraklatıldı' : 'çalışıyor'}
        </span>
      )}
      <span className="status-chip" title="Bağlı kontrolcüler">
        <Icon name="pad" /> {pads}
      </span>
      <span className="status-time">
        {now.toLocaleDateString('tr-TR', { day: 'numeric', month: 'numeric' })}{' '}
        <b>{now.toLocaleTimeString('tr-TR', { hour: '2-digit', minute: '2-digit' })}</b>
      </span>
    </div>
  )
}

function ShutdownScreen({ text, done }: { text: string; done: boolean }) {
  useInput(() => true) // nothing to navigate while shutting down
  return (
    <div className={`shutdown ${done ? 'done' : ''}`}>
      <div className="shutdown-brand">{BRAND.name}</div>
      <div className="shutdown-text" key={text}>
        {text}
      </div>
      {!done && <div className="launch-spinner" />}
    </div>
  )
}

/** Shown behind a running game: the console stays open, so the desktop never flashes. */
function PlayingCurtain({ game }: { game: Game | undefined }) {
  return (
    <div className="curtain">
      {game?.hasCover && <img className="curtain-cover" src={coverUrl(game.id, game.coverRev)} alt="" />}
      <div className="curtain-title">{game?.title ?? 'Oyun'}</div>
      <div className="curtain-hint">Oyun çalışıyor · Menü için kumandanın orta tuşu veya Ctrl+Shift+Home</div>
    </div>
  )
}

function LaunchSplash({ game }: { game: Game }) {
  useInput(() => true) // swallow input while the game starts
  return (
    <div className="modal-layer center launch">
      {game.hasCover && <img className="launch-cover" src={coverUrl(game.id, game.coverRev)} alt="" />}
      <div className="launch-title">{game.title}</div>
      <div className="launch-spinner" />
    </div>
  )
}

export function App() {
  const state = useAppState()
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const bgRef = useRef<WaveBackground | null>(null)
  const xmbHandle = useRef<XmbHandle | null>(null)
  const [modals, setModals] = useState<ModalEntry[]>([])
  const [toasts, setToasts] = useState<(Toast & { id: number })[]>([])
  const [pads, setPads] = useState<string[]>([])
  const [launching, setLaunching] = useState<Game | null>(null)
  const [focusId, setFocusId] = useState<string | null>(null)
  const [catalog, setCatalog] = useState<StoreItem[]>(BUILTIN_CATALOG)
  const [modCss, setModCss] = useState('')
  const refreshCatalog = useCallback((refresh = false) => {
    void call('store:catalog', refresh).then(setCatalog).catch(() => {})
  }, [])
  useEffect(() => refreshCatalog(), [refreshCatalog])
  const [shutdown, setShutdown] = useState<{ text: string; done?: boolean } | null>(null)
  useEffect(() => window.bridge.on('shutdown', setShutdown), [])
  const keyRef = useRef(1)
  const [focused, setFocused] = useState(() => document.hasFocus())
  useEffect(() => window.bridge.on('window:focus', setFocused), [])

  // Background
  useEffect(() => {
    if (canvasRef.current && !bgRef.current) bgRef.current = new WaveBackground(canvasRef.current)
    return () => {
      bgRef.current?.destroy()
      bgRef.current = null
    }
  }, [])

  const pushToast = useCallback((t: Toast) => {
    const id = keyRef.current++
    setToasts((ts) => [...ts.slice(-3), { ...t, id }])
    setTimeout(() => setToasts((ts) => ts.filter((x) => x.id !== id)), t.kind === 'error' ? 7000 : 4000)
  }, [])

  // Input + controller notifications
  useEffect(() => {
    input.start()
    const refresh = () => setPads(input.connectedPads().map((p) => p.id))
    input.onPadChange = (connected, name) => {
      refresh()
      pushToast({ kind: connected ? 'success' : 'warning', text: `${connected ? 'Kontrolcü bağlandı' : 'Kontrolcü bağlantısı kesildi'}: ${name.replace(/\s*\(.*\)$/, '')}` })
    }
    refresh()
    const off = window.bridge.on('toast', pushToast)
    return off
  }, [pushToast])

  // Settings side-effects
  useEffect(() => {
    if (!state) return
    configureSound(state.settings.menuSounds, state.settings.soundVolume)
    document.documentElement.classList.toggle('no-anim', !state.settings.animations)
    if (bgRef.current) bgRef.current.animate = state.settings.animations
  }, [state?.settings])

  // The XMB is visible again once a game session ends or we come back home.
  useEffect(() => {
    if (state) setLaunching(null)
  }, [state?.session?.gameId, state?.session?.paused])

  const openModal = useCallback((m: Modal) => setModals((ms) => [...ms, { ...m, key: keyRef.current++ } as ModalEntry]), [])
  const closeModal = (key: number) => setModals((ms) => ms.filter((m) => m.key !== key))

  const onFocusHue = useCallback(
    (hue: number, gameId: string | null) => {
      setFocusId(gameId)
      const theme = state ? allThemes({ state }).find((t) => t.value === state.settings.theme) : undefined
      if (!theme || theme.hue === null) bgRef.current?.setHue(hue)
      else bgRef.current?.setHue(theme.hue, theme.value === 'night')
    },
    [state?.settings.theme, state?.themes]
  )

  // CSS mods + the active theme's CSS.
  const modsKey = state?.mods.map((m) => `${m.id}:${m.enabled}`).join(',')
  useEffect(() => {
    void call('mods:css').then(setModCss).catch(() => setModCss(''))
  }, [modsKey])
  const themeCss = state ? allThemes({ state }).find((t) => t.value === state.settings.theme)?.css ?? '' : ''

  const ctx: MenuCtx | null = useMemo(() => {
    if (!state) return null
    const c: MenuCtx = {
      state,
      pads,
      catalog,
      refreshCatalog: () => refreshCatalog(true),
      back: () => xmbHandle.current?.back(),
      openDetail: (gameId) => openModal({ type: 'detail', gameId }),
      openGallery: (title, games) => openModal({ type: 'gallery', title, gameIds: games.map((g) => g.id) }),
      openOptions: (title, items) => openModal({ type: 'options', title, items }),
      browse: (req) => openModal({ type: 'browse', req }),
      confirm: (text, onYes) => openModal({ type: 'confirm', text, onYes }),
      openScreenshots: (gameId) => openModal({ type: 'shots', gameId }),
      openScan: () => openModal({ type: 'scan' }),
      openStore: (tab) => openModal({ type: 'store', tab }),
      setSettings: (patch) => void call('settings:set', patch),
      run: (fn, success) => {
        fn()
          .then((r) => {
            const text = typeof success === 'function' ? success(r) : success
            if (text) pushToast({ kind: 'success', text })
          })
          .catch((err: Error) => {
            sfx('error')
            pushToast({ kind: 'error', text: err.message })
          })
      },
      launch: (gameId) => {
        const g = state.games.find((x) => x.id === gameId)
        if (!g) return
        if (g.platformId === 'apps') {
          // Web apps open inside the console; DRM apps (Netflix…) go through Edge like a game.
          const item = catalog.find((i) => i.id === g.meta.storeId)
          if (item?.app && (!item.app.drm || drmInConsole(state))) {
            sfx('confirm')
            openModal({ type: 'web', item, gameId: g.id })
            return
          }
        }
        setLaunching(g)
        call('game:launch', gameId)
          .then(() => {
            // Window is hidden by the main process; keep the splash until then.
            setTimeout(() => setLaunching(null), 1500)
          })
          .catch((err: Error) => {
            setLaunching(null)
            sfx('error')
            pushToast({ kind: 'error', text: err.message })
          })
      }
    }
    return c
  }, [state, pads, openModal, pushToast, catalog, refreshCatalog])

  const cats = useMemo(() => (ctx ? buildMenu(ctx) : []), [ctx])

  // A full-screen app (store, browser) covers the XMB: hide it and silence the ambience.
  const inApp = modals.some((m) => m.type === 'web' || m.type === 'store')
  const inWeb = modals.some((m) => m.type === 'web')
  // A game has focus and the console is waiting behind it: draw nothing that costs GPU time.
  const playing = Boolean(state?.session) && !focused
  useEffect(() => {
    if (bgRef.current) bgRef.current.animate = !inWeb && !playing && (state?.settings.animations ?? true)
  }, [inWeb, playing, state?.settings.animations])

  return (
    <div className={`app ${inApp ? 'in-app' : ''}`}>
      <canvas ref={canvasRef} className="bg" />
      <style>{themeCss + '\n' + modCss}</style>
      {state && (() => {
        // The detail screen keeps its game's ambience; otherwise follow the XMB focus.
        const detail = [...modals].reverse().find((m) => m.type === 'detail') as { gameId: string } | undefined
        const id = detail?.gameId ?? (state.settings.setupDone ? focusId : null)
        const g = id && !inApp ? state.games.find((x) => x.id === id) ?? null : null
        return (
          <>
            {state.settings.gameBackgrounds && <Backdrop game={g} />}
            <div className="backdrop trailer-layer">
              <Trailer
                game={state.session ? null : g}
                enabled={state.settings.gameTrailers}
                volume={state.settings.musicVolume}
                muted={Boolean(g?.musicPath && state.settings.gameMusic)}
              />
              <div className="backdrop-shade show" style={{ opacity: g?.trailerUrl && state.settings.gameTrailers ? undefined : 0 }} />
            </div>
            <ThemeMusic game={state.session ? null : g} enabled={state.settings.gameMusic} volume={state.settings.musicVolume} />
          </>
        )
      })()}
      {state && ctx && (
        <>
          {state.settings.setupDone && (
            <>
              <Clock state={state} pads={pads.length} />
              <Xmb cats={cats} handle={xmbHandle} onFocus={onFocusHue} />
            </>
          )}
          {modals.map((m) => {
            const close = () => closeModal(m.key)
            switch (m.type) {
              case 'detail':
                return <GameDetail key={m.key} gameId={m.gameId} ctx={ctx} onClose={close} />
              case 'gallery':
                return <Gallery key={m.key} title={m.title} gameIds={m.gameIds} ctx={ctx} onClose={close} />
              case 'options':
                return <OptionsPanel key={m.key} title={m.title} items={m.items} onClose={close} />
              case 'browse':
                return <FileBrowser key={m.key} req={m.req} onClose={close} />
              case 'confirm':
                return <ConfirmDialog key={m.key} text={m.text} onYes={m.onYes} onClose={close} />
              case 'shots':
                return <Screenshots key={m.key} gameId={m.gameId} ctx={ctx} onClose={close} />
              case 'scan':
                return <Setup key={m.key} mode="scan" ctx={ctx} onClose={close} />
              case 'store':
                return (
                  <Store
                    key={m.key}
                    ctx={ctx}
                    initialTab={m.tab}
                    onClose={close}
                    openApp={(item) => {
                      const g = state.games.find((x) => x.platformId === 'apps' && x.meta.storeId === item.id)
                      if (g) ctx.launch(g.id)
                      else if (item.app && (!item.app.drm || drmInConsole(state))) openModal({ type: 'web', item })
                    }}
                  />
                )
              case 'web':
                return (
                  <WebApp
                    key={m.key}
                    item={m.item}
                    gameId={m.gameId}
                    ctx={ctx}
                    onClose={close}
                    openKeyboard={(title, initial, onDone) => openModal({ type: 'keyboard', title, initial, onDone })}
                  />
                )
              case 'keyboard':
                return <OnScreenKeyboard key={m.key} title={m.title} initial={m.initial} onDone={m.onDone} onClose={close} />
            }
          })}
          {!state.settings.setupDone && <Setup mode="setup" ctx={ctx} onClose={() => {}} />}
          {launching && <LaunchSplash game={launching} />}
        </>
      )}
      {!state && <div className="boot">{BRAND.name}</div>}
      <Toasts toasts={toasts} />
      {state && playing && !shutdown && <PlayingCurtain game={state.games.find((g) => g.id === state.session?.gameId)} />}
      {shutdown && <ShutdownScreen text={shutdown.text} done={Boolean(shutdown.done)} />}
    </div>
  )
}
