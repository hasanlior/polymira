import { useEffect, useRef, useState } from 'react'

export type Action =
  | 'up'
  | 'down'
  | 'left'
  | 'right'
  | 'confirm'
  | 'back'
  | 'options'
  | 'view'
  | 'l1'
  | 'r1'
  | 'l2'
  | 'r2'
  | 'start'
  | 'select'
  | 'home'

/** Actions that can be put on any controller button. */
export const MAPPABLE: Action[] = ['confirm', 'back', 'view', 'options', 'l1', 'r1', 'l2', 'r2', 'select', 'start']

/** W3C "standard" gamepad layout: button index for each action. */
export const DEFAULT_MAP: Record<string, number> = {
  confirm: 0,
  back: 1,
  view: 2,
  options: 3,
  l1: 4,
  r1: 5,
  l2: 6,
  r2: 7,
  select: 8,
  start: 9
}

/** Which button labels to show. */
export type PadStyle = 'xbox' | 'ps' | 'switch' | 'keyboard'

type Handler = (a: Action) => boolean | void

export function padStyleOf(id: string): PadStyle {
  const s = id.toLowerCase()
  if (/054c|dualsense|dualshock|wireless controller|playstation|ps[345]/.test(s)) return 'ps'
  if (/057e|pro controller|joy-con|nintendo|switch/.test(s)) return 'switch'
  return 'xbox' // XInput and most PC pads use the Xbox layout
}

/**
 * Unified input: gamepads (W3C standard mapping), keyboard and mouse wheel all become Actions.
 * Handlers form a stack; the most recently mounted handler receives input (modals on top).
 * Buttons can be remapped; the last used device decides which button symbols are shown.
 */
class InputManager {
  private stack: { id: number; fn: () => Handler | null; wantsStart: boolean }[] = []
  private nextId = 1
  private prevButtons = new Map<number, boolean[]>()
  private held = new Map<string, { since: number; last: number }>()
  private running = false
  private styleListeners = new Set<(s: PadStyle) => void>()
  onPadChange: ((connected: boolean, name: string) => void) | null = null
  /** OS focus of the console window (reported by the main process; works while a webview has focus). */
  windowFocused = document.hasFocus()
  /** When false the left stick is not turned into directions (the browser uses it as a pointer). */
  stickAsDpad = true
  /** action → button index */
  map: Record<string, number> = { ...DEFAULT_MAP }
  /** Forced symbol style, or null to follow the last used device. */
  forcedStyle: PadStyle | null = null
  rumble = true
  private lastStyle: PadStyle = 'keyboard'
  /** While set, the next pressed button is reported here instead of being dispatched. */
  capture: ((button: number) => void) | null = null

  get style(): PadStyle {
    return this.forcedStyle ?? this.lastStyle
  }

  onStyle(fn: (s: PadStyle) => void): () => void {
    this.styleListeners.add(fn)
    return () => this.styleListeners.delete(fn)
  }

  private setLastStyle(s: PadStyle): void {
    if (s === this.lastStyle) return
    this.lastStyle = s
    for (const fn of this.styleListeners) fn(this.style)
  }

  setForcedStyle(s: PadStyle | null): void {
    this.forcedStyle = s
    for (const fn of this.styleListeners) fn(this.style)
  }

  setMap(overrides: Record<string, number> | undefined): void {
    this.map = { ...DEFAULT_MAP, ...(overrides ?? {}) }
  }

  push(fn: () => Handler | null, wantsStart = false): () => void {
    const id = this.nextId++
    this.stack.push({ id, fn, wantsStart })
    return () => {
      this.stack = this.stack.filter((h) => h.id !== id)
    }
  }

  /** The handler that would receive input right now (the top of the stack). */
  current(): Handler | null {
    for (let i = this.stack.length - 1; i >= 0; i--) {
      const h = this.stack[i].fn()
      if (h) return h
    }
    return null
  }

  dispatch(a: Action): void {
    for (let i = this.stack.length - 1; i >= 0; i--) {
      const entry = this.stack[i]
      const h = entry.fn()
      if (h) {
        // Screens that have no use for Start treat it like the options button.
        h(a === 'start' && !entry.wantsStart ? 'options' : a)
        return
      }
    }
  }

  /** Short vibration on the controllers that support it. */
  buzz(ms = 35, strength = 0.35): void {
    if (!this.rumble) return
    for (const p of this.connectedPads()) {
      const act = (p as Gamepad & { vibrationActuator?: { playEffect: (t: string, o: object) => Promise<unknown> } }).vibrationActuator
      void act?.playEffect('dual-rumble', { duration: ms, strongMagnitude: strength * 0.6, weakMagnitude: strength }).catch(() => {})
    }
  }

  start(): void {
    if (this.running) return
    this.running = true
    window.addEventListener('keydown', this.onKey)
    window.bridge?.on('window:focus', (f) => {
      this.windowFocused = f
    })
    window.addEventListener('wheel', this.onWheel, { passive: true })
    window.addEventListener('gamepadconnected', (e) => {
      const pad = (e as GamepadEvent).gamepad
      this.setLastStyle(padStyleOf(pad.id))
      this.onPadChange?.(true, pad.id)
    })
    window.addEventListener('gamepaddisconnected', (e) => this.onPadChange?.(false, (e as GamepadEvent).gamepad.id))
    const loop = () => {
      this.pollPads()
      requestAnimationFrame(loop)
    }
    requestAnimationFrame(loop)
  }

  connectedPads(): Gamepad[] {
    return (navigator.getGamepads?.() ?? []).filter((p): p is Gamepad => Boolean(p))
  }

  private onKey = (e: KeyboardEvent) => {
    const map: Record<string, Action> = {
      ArrowUp: 'up',
      ArrowDown: 'down',
      ArrowLeft: 'left',
      ArrowRight: 'right',
      Enter: 'confirm',
      ' ': 'confirm',
      Escape: 'back',
      Backspace: 'back',
      t: 'options',
      T: 'options',
      ContextMenu: 'options',
      g: 'view',
      G: 'view',
      q: 'l1',
      Q: 'l1',
      e: 'r1',
      E: 'r1',
      PageUp: 'l2',
      PageDown: 'r2',
      Tab: 'start'
    }
    const a = map[e.key]
    if (!a) return
    if ((e.target as HTMLElement)?.tagName === 'INPUT') return
    e.preventDefault()
    if (e.isTrusted) this.setLastStyle('keyboard')
    this.dispatch(a)
  }

  private lastWheel = 0
  private onWheel = (e: WheelEvent) => {
    const now = performance.now()
    if (now - this.lastWheel < 70) return
    this.lastWheel = now
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) this.dispatch(e.deltaX > 0 ? 'right' : 'left')
    else this.dispatch(e.deltaY > 0 ? 'down' : 'up')
  }

  private repeat(key: string, active: boolean, action: Action, now: number): void {
    const h = this.held.get(key)
    if (!active) {
      this.held.delete(key)
      return
    }
    if (!h) {
      this.held.set(key, { since: now, last: now })
      this.dispatch(action)
    } else if (now - h.since > 380 && now - h.last > 75) {
      h.last = now
      this.dispatch(action)
    }
  }

  private pollPads(): void {
    if (!this.windowFocused && !document.hasFocus()) return
    const now = performance.now()
    const dirs = { up: false, down: false, left: false, right: false }
    const byButton = new Map<number, Action>()
    for (const [action, button] of Object.entries(this.map)) byButton.set(button, action as Action)
    for (const pad of this.connectedPads()) {
      const b = pad.buttons.map((x) => x.pressed)
      const prev = this.prevButtons.get(pad.index) ?? []
      this.prevButtons.set(pad.index, b)
      const edge = (i: number) => b[i] && !prev[i]
      let pressed = false
      for (let i = 0; i < b.length; i++) {
        if (!edge(i)) continue
        pressed = true
        if (this.capture) {
          if (i >= 12 && i <= 15) continue // D-pad stays navigation
          const cb = this.capture
          this.capture = null
          cb(i)
          continue
        }
        if (i === 16) this.dispatch('home')
        const action = byButton.get(i)
        if (action) {
          if (action === 'confirm') this.buzz()
          this.dispatch(action)
        }
      }
      if (pressed || b.some(Boolean)) this.setLastStyle(padStyleOf(pad.id))
      if (this.capture) continue
      const ax = this.stickAsDpad ? pad.axes[0] ?? 0 : 0
      const ay = this.stickAsDpad ? pad.axes[1] ?? 0 : 0
      dirs.up ||= b[12] || ay < -0.55
      dirs.down ||= b[13] || ay > 0.55
      dirs.left ||= b[14] || ax < -0.55
      dirs.right ||= b[15] || ax > 0.55
    }
    this.repeat('up', dirs.up, 'up', now)
    this.repeat('down', dirs.down, 'down', now)
    this.repeat('left', dirs.left, 'left', now)
    this.repeat('right', dirs.right, 'right', now)
  }
}

export const input = new InputManager()

/**
 * Subscribes `handler` to input while mounted (and while `enabled`).
 * Screens that do not ask for Start (`wantsStart`) receive it as 'options'.
 */
export function useInput(handler: Handler, enabled = true, wantsStart = false): void {
  const ref = useRef(handler)
  const en = useRef(enabled)
  ref.current = handler
  en.current = enabled
  useEffect(() => input.push(() => (en.current ? ref.current : null), wantsStart), [])
}

/** The button style currently shown (follows the last used controller or the keyboard). */
export function usePadStyle(): PadStyle {
  const [s, setS] = useState(input.style)
  useEffect(() => input.onStyle(setS), [])
  return s
}
