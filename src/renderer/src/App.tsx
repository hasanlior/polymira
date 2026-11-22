import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { BRAND } from '@shared/brand'
import type { AppState, Game, Toast } from '@shared/types'
import { call, coverUrl, useAppState } from './api'
import { WaveBackground } from './background'
import { Icon } from './icons'
import { input, useInput } from './input'
import { buildMenu, THEMES, type BrowseRequest, type MenuCtx, type OptItem } from './menu'
import { configureSound, sfx } from './sound'
import { Xmb, type XmbHandle } from './components/Xmb'
import { ConfirmDialog, FileBrowser, OptionsPanel, Toasts } from './components/Modals'
import { GameDetail, Gallery, Screenshots } from './components/GameViews'

type Modal =
  | { type: 'detail'; gameId: string }
  | { type: 'gallery'; title: string; gameIds: string[] }
  | { type: 'options'; title: string; items: OptItem[] }
  | { type: 'browse'; req: BrowseRequest }
  | { type: 'confirm'; text: string; onYes: () => void }
  | { type: 'shots'; gameId?: string }

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
  const keyRef = useRef(1)

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
    (hue: number) => {
      const theme = THEMES.find((t) => t.value === state?.settings.theme)
      if (!theme || theme.hue === null) bgRef.current?.setHue(hue)
      else bgRef.current?.setHue(theme.hue, theme.value === 'night')
    },
    [state?.settings.theme]
  )

  const ctx: MenuCtx | null = useMemo(() => {
    if (!state) return null
    const c: MenuCtx = {
      state,
      pads,
      back: () => xmbHandle.current?.back(),
      openDetail: (gameId) => openModal({ type: 'detail', gameId }),
      openGallery: (title, games) => openModal({ type: 'gallery', title, gameIds: games.map((g) => g.id) }),
      openOptions: (title, items) => openModal({ type: 'options', title, items }),
      browse: (req) => openModal({ type: 'browse', req }),
      confirm: (text, onYes) => openModal({ type: 'confirm', text, onYes }),
      openScreenshots: (gameId) => openModal({ type: 'shots', gameId }),
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
  }, [state, pads, openModal, pushToast])

  const cats = useMemo(() => (ctx ? buildMenu(ctx) : []), [ctx])

  return (
    <div className="app">
      <canvas ref={canvasRef} className="bg" />
      {state && ctx && (
        <>
          <Clock state={state} pads={pads.length} />
          <Xmb cats={cats} handle={xmbHandle} onFocus={onFocusHue} />
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
            }
          })}
          {launching && <LaunchSplash game={launching} />}
        </>
      )}
      {!state && <div className="boot">{BRAND.name}</div>}
      <Toasts toasts={toasts} />
    </div>
  )
}
