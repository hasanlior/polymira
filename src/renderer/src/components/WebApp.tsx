/// <reference types="electron" />
import { createElement, useCallback, useEffect, useRef, useState } from 'react'
import type { StoreItem } from '@shared/catalog'
import { appIconUrl } from '../api'
import { BRAND } from '@shared/brand'
import { Icon } from '../icons'
import { Hint } from '../glyphs'
import { input, useInput, type Action } from '../input'
import type { MenuCtx, OptItem } from '../menu'
import { sfx } from '../sound'

type WebviewEl = HTMLElement & Electron.WebviewTag

/** Chromium user agent without the Electron/app tokens (some sites refuse "Electron"). */
const CHROME_UA = navigator.userAgent
  .split(' ')
  .filter((t) => !new RegExp(`^(Electron|xport|${BRAND.slug})/`, 'i').test(t))
  .join(' ')

const DEADZONE = 0.18

function toUrl(text: string): string {
  const t = text.trim()
  if (/^https?:\/\//i.test(t)) return t
  if (!/\s/.test(t) && /^[^\s/]+\.[a-z]{2,}(\/.*)?$/i.test(t)) return `https://${t}`
  return `https://www.google.com/search?q=${encodeURIComponent(t)}`
}

/**
 * Built-in browser / web app, full screen inside the console.
 * TV-style apps (kiosk) get arrow keys from the D-pad and stick; the general browser gets a
 * pointer on the left stick, scrolling on the right stick and an on-screen keyboard.
 */
export function WebApp({
  item,
  gameId,
  ctx,
  onClose,
  openKeyboard
}: {
  item: StoreItem
  gameId?: string
  ctx: MenuCtx
  onClose: () => void
  openKeyboard: (title: string, initial: string, onDone: (text: string) => void) => void
}) {
  const app = item.app!
  const kiosk = Boolean(app.kiosk)
  const viewRef = useRef<WebviewEl | null>(null)
  const areaRef = useRef<HTMLDivElement>(null)
  const [ready, setReady] = useState(false)
  const [loading, setLoading] = useState(true)
  const [title, setTitle] = useState(item.name)
  const [url, setUrl] = useState(app.url)
  const [error, setError] = useState<string | null>(null)
  const [textFocus, setTextFocus] = useState(false)
  const [hint, setHint] = useState(true)
  const cursor = useRef({ x: 0, y: 0, shown: false })
  const cursorEl = useRef<HTMLDivElement>(null)
  const [closing, setClosing] = useState(false)

  const view = () => (ready ? viewRef.current : null)

  // Play time for apps in the library.
  useEffect(() => {
    if (!gameId) return
    const started = Date.now()
    void window.bridge.invoke('app:opened', gameId)
    return () => void window.bridge.invoke('app:closed', gameId, (Date.now() - started) / 1000)
  }, [gameId])

  useEffect(() => {
    const t = setTimeout(() => setHint(false), 5000)
    return () => clearTimeout(t)
  }, [])

  // Webview events.
  useEffect(() => {
    const v = viewRef.current
    if (!v) return
    const onReady = () => {
      setReady(true)
      v.focus()
    }
    const onStart = () => {
      setLoading(true)
      setError(null)
    }
    const onStop = () => setLoading(false)
    const onTitle = (e: Electron.PageTitleUpdatedEvent) => setTitle(e.title)
    const onNav = (e: Electron.DidNavigateEvent | Electron.DidNavigateInPageEvent) => setUrl(e.url)
    const onFail = (e: Electron.DidFailLoadEvent) => {
      if (e.errorCode === -3 || !e.isMainFrame) return // aborted / sub frame
      setError(`${e.errorDescription || 'Bağlantı hatası'} (${e.errorCode})`)
    }
    const onIpc = (e: Electron.IpcMessageEvent) => {
      if (e.channel === 'text-focus') setTextFocus(Boolean(e.args[0]))
      else if (e.channel === 'menu') openMenu()
    }
    v.addEventListener('dom-ready', onReady, { once: true })
    v.addEventListener('did-start-loading', onStart)
    v.addEventListener('did-stop-loading', onStop)
    v.addEventListener('page-title-updated', onTitle as EventListener)
    v.addEventListener('did-navigate', onNav as EventListener)
    v.addEventListener('did-navigate-in-page', onNav as EventListener)
    v.addEventListener('did-fail-load', onFail as EventListener)
    v.addEventListener('ipc-message', onIpc as EventListener)
    return () => {
      v.removeEventListener('did-start-loading', onStart)
      v.removeEventListener('did-stop-loading', onStop)
      v.removeEventListener('page-title-updated', onTitle as EventListener)
      v.removeEventListener('did-navigate', onNav as EventListener)
      v.removeEventListener('did-navigate-in-page', onNav as EventListener)
      v.removeEventListener('did-fail-load', onFail as EventListener)
      v.removeEventListener('ipc-message', onIpc as EventListener)
    }
  }, [])

  const key = (keyCode: string) => {
    const v = view()
    if (!v) return
    v.focus()
    v.sendInputEvent({ type: 'keyDown', keyCode })
    if (keyCode === 'Enter') v.sendInputEvent({ type: 'char', keyCode: '\r' })
    v.sendInputEvent({ type: 'keyUp', keyCode })
  }

  const typeText = (text: string) => {
    const v = view()
    if (!v || !text) return
    v.focus()
    void v.insertText(text)
  }

  const keyboard = (forPage = true) => {
    viewRef.current?.blur()
    if (forPage) openKeyboard('Metin gir', '', typeText)
    else openKeyboard('Adres veya arama', url, (t) => t.trim() && view()?.loadURL(toUrl(t)))
  }

  const close = useCallback(() => {
    if (closing) return
    setClosing(true)
    setTimeout(onClose, 250)
  }, [closing, onClose])

  const openMenu = () => {
    sfx('confirm')
    viewRef.current?.blur()
    const v = view()
    const zoom = (d: number) => v && v.setZoomLevel(d === 0 ? 0 : v.getZoomLevel() + d)
    const items: OptItem[] = [
      ...(!kiosk ? [{ label: 'Adres veya arama gir', icon: 'search' as const, onSelect: () => keyboard(false) }] : []),
      { label: 'Klavye (metin yaz)', icon: 'file', onSelect: () => keyboard(true) },
      { label: 'Geri', icon: 'exit', onSelect: () => v?.canGoBack() && v.goBack() },
      { label: 'İleri', icon: 'play', onSelect: () => v?.canGoForward() && v.goForward() },
      { label: 'Yenile', icon: 'refresh', onSelect: () => v?.reload() },
      { label: 'Başlangıç sayfası', icon: 'home', onSelect: () => v?.loadURL(app.url) },
      {
        label: 'Yakınlaştırma',
        icon: 'search',
        children: [
          { label: 'Yakınlaştır', onSelect: () => zoom(0.5) },
          { label: 'Uzaklaştır', onSelect: () => zoom(-0.5) },
          { label: 'Varsayılan', onSelect: () => zoom(0) }
        ]
      },
      { label: `${item.name} uygulamasını kapat`, icon: 'power', danger: true, onSelect: close }
    ]
    ctx.openOptions(item.name, items)
  }

  // Pointer + scrolling from the sticks (browser mode only).
  const handler = (a: Action) => {
    const v = view()
    if (hint) setHint(false)
    if (document.activeElement !== viewRef.current) viewRef.current?.focus()
    if (a === 'options' || a === 'select' || a === 'home') return openMenu()
    if (a === 'view') return keyboard(true)
    if (!v) return
    if (kiosk) {
      const map: Partial<Record<Action, string>> = { up: 'Up', down: 'Down', left: 'Left', right: 'Right', confirm: 'Enter', back: 'Escape' }
      if (map[a]) key(map[a]!)
      else if (a === 'start') keyboard(true)
      return
    }
    switch (a) {
      case 'up':
      case 'down':
      case 'left':
      case 'right':
        key(a[0].toUpperCase() + a.slice(1))
        break
      case 'confirm': {
        const { x, y } = cursor.current
        v.sendInputEvent({ type: 'mouseDown', x: Math.round(x), y: Math.round(y), button: 'left', clickCount: 1 })
        v.sendInputEvent({ type: 'mouseUp', x: Math.round(x), y: Math.round(y), button: 'left', clickCount: 1 })
        break
      }
      case 'back':
        if (v.canGoBack()) v.goBack()
        else ctx.confirm(`${item.name} kapatılsın mı?`, close)
        break
      case 'l1':
        if (v.canGoBack()) v.goBack()
        break
      case 'r1':
        if (v.canGoForward()) v.goForward()
        break
      case 'l2':
        v.setZoomLevel(v.getZoomLevel() - 0.5)
        break
      case 'r2':
        v.setZoomLevel(v.getZoomLevel() + 0.5)
        break
      case 'start':
        keyboard(false)
        break
    }
  }
  const handlerRef = useRef(handler)
  handlerRef.current = handler
  useInput(handler, true, true)

  useEffect(() => {
    if (kiosk) return
    input.stickAsDpad = false
    const area = areaRef.current!
    const r0 = area.getBoundingClientRect()
    cursor.current = { x: r0.width / 2, y: r0.height / 2, shown: true }
    let raf = 0
    let last = performance.now()
    let scrollAcc = 0
    const loop = (now: number) => {
      raf = requestAnimationFrame(loop)
      const dt = Math.min(50, now - last) / 16.7
      last = now
      // Only while this view is the one receiving input (no menu or keyboard on top).
      if (input.current() !== handlerRef.current) return
      const v = viewRef.current
      let mx = 0
      let my = 0
      let sy = 0
      let sx = 0
      for (const p of input.connectedPads()) {
        const f = (n: number) => (Math.abs(n) > DEADZONE ? n : 0)
        mx += f(p.axes[0] ?? 0)
        my += f(p.axes[1] ?? 0)
        sx += f(p.axes[2] ?? 0)
        sy += f(p.axes[3] ?? 0)
      }
      const rect = area.getBoundingClientRect()
      if (mx || my) {
        const speed = 16 * dt
        const c = cursor.current
        c.x = Math.max(0, Math.min(rect.width - 1, c.x + mx * Math.abs(mx) * speed))
        c.y = Math.max(0, Math.min(rect.height - 1, c.y + my * Math.abs(my) * speed))
        if (cursorEl.current) cursorEl.current.style.transform = `translate(${c.x}px, ${c.y}px)`
        if (ready && v) v.sendInputEvent({ type: 'mouseMove', x: Math.round(c.x), y: Math.round(c.y) })
      }
      if ((sx || sy) && ready && v) {
        scrollAcc += dt
        if (scrollAcc >= 1) {
          scrollAcc = 0
          const c = cursor.current
          v.sendInputEvent({ type: 'mouseWheel', x: Math.round(c.x), y: Math.round(c.y), deltaX: -sx * 40, deltaY: -sy * 40, canScroll: true } as Electron.MouseWheelInputEvent)
        }
      }
    }
    raf = requestAnimationFrame(loop)
    if (cursorEl.current) cursorEl.current.style.transform = `translate(${cursor.current.x}px, ${cursor.current.y}px)`
    return () => {
      cancelAnimationFrame(raf)
      input.stickAsDpad = true
    }
  }, [kiosk, ready])

  const host = (() => {
    try {
      return new URL(url).host
    } catch {
      return url
    }
  })()

  return (
    <div className={`webapp ${closing ? 'closing' : ''} ${kiosk ? 'kiosk' : ''}`}>
      {!kiosk && (
        <div className="web-bar">
          <img className="web-bar-icon" src={appIconUrl(item.id)} alt="" onError={(e) => ((e.target as HTMLImageElement).style.visibility = 'hidden')} />
          <div className="web-bar-text">
            <div className="web-bar-title">{title}</div>
            <div className="web-bar-url">{host}</div>
          </div>
          <div className="web-bar-hints">
            <span>Sol çubuk: imleç</span>
            <span>Sağ çubuk: kaydır</span>
            <Hint action="confirm">Tıkla</Hint>
            <Hint action="back">Geri</Hint>
            <Hint action="view">Klavye{textFocus ? ' •' : ''}</Hint>
            <Hint action="start">Adres</Hint>
            <Hint action="options">Menü</Hint>
          </div>
        </div>
      )}
      <div className={`web-progress ${loading ? 'on' : ''}`} />
      <div className="web-area" ref={areaRef}>
        {createElement('webview', {
          ref: viewRef,
          src: app.url,
          partition: item.id === 'browser' ? 'persist:web' : `persist:app-${item.id.replace(/[^a-z0-9_-]/gi, '_')}`,
          useragent: app.userAgent ?? CHROME_UA,
          allowpopups: 'true',
          className: 'web-view'
        })}
        {!kiosk && <div className="web-cursor" ref={cursorEl} />}
        {error && (
          <div className="web-error">
            <Icon name="warning" />
            <div>Sayfa açılamadı</div>
            <small>{error}</small>
            <small><Hint action="options">Menü</Hint> → Yenile</small>
          </div>
        )}
        {kiosk && hint && (
          <div className="web-hint">
            <Hint action="options">Menü</Hint> <Hint action="view">Klavye</Hint>
          </div>
        )}
      </div>
    </div>
  )
}
