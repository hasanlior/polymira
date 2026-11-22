import { useEffect, useRef } from 'react'

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

type Handler = (a: Action) => boolean | void

/**
 * Unified input: gamepads (W3C standard mapping), keyboard and mouse wheel all become Actions.
 * Handlers form a stack; the most recently mounted handler receives input (modals on top).
 */
class InputManager {
  private stack: { id: number; fn: () => Handler | null }[] = []
  private nextId = 1
  private prevButtons = new Map<number, boolean[]>()
  private held = new Map<string, { since: number; last: number }>()
  private running = false
  onPadChange: ((connected: boolean, name: string) => void) | null = null

  push(fn: () => Handler | null): () => void {
    const id = this.nextId++
    this.stack.push({ id, fn })
    return () => {
      this.stack = this.stack.filter((h) => h.id !== id)
    }
  }

  dispatch(a: Action): void {
    for (let i = this.stack.length - 1; i >= 0; i--) {
      const h = this.stack[i].fn()
      if (h) {
        h(a)
        return
      }
    }
  }

  start(): void {
    if (this.running) return
    this.running = true
    window.addEventListener('keydown', this.onKey)
    window.addEventListener('wheel', this.onWheel, { passive: true })
    window.addEventListener('gamepadconnected', (e) => this.onPadChange?.(true, (e as GamepadEvent).gamepad.id))
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
    if (!document.hasFocus()) return
    const now = performance.now()
    const dirs = { up: false, down: false, left: false, right: false }
    for (const pad of this.connectedPads()) {
      const b = pad.buttons.map((x) => x.pressed)
      const prev = this.prevButtons.get(pad.index) ?? []
      this.prevButtons.set(pad.index, b)
      const edge = (i: number) => b[i] && !prev[i]
      // Standard mapping: 0 A/✕ 1 B/○ 2 X/□ 3 Y/△ 4 LB 5 RB 6 LT 7 RT 8 Back 9 Start 12-15 D-pad
      if (edge(0)) this.dispatch('confirm')
      if (edge(1)) this.dispatch('back')
      if (edge(2)) this.dispatch('view')
      if (edge(3)) this.dispatch('options')
      if (edge(4)) this.dispatch('l1')
      if (edge(5)) this.dispatch('r1')
      if (edge(6)) this.dispatch('l2')
      if (edge(7)) this.dispatch('r2')
      if (edge(8)) this.dispatch('select')
      if (edge(9)) this.dispatch('options')
      const ax = pad.axes[0] ?? 0
      const ay = pad.axes[1] ?? 0
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

/** Subscribes `handler` to input while mounted (and while `enabled`). */
export function useInput(handler: Handler, enabled = true): void {
  const ref = useRef(handler)
  const en = useRef(enabled)
  ref.current = handler
  en.current = enabled
  useEffect(() => input.push(() => (en.current ? ref.current : null)), [])
}
