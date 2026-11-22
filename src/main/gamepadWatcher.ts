import { EventEmitter } from 'events'
import type { OverlayHotkey } from '@shared/types'
import { readXInput, XINPUT } from './native/win32'

/**
 * Polls XInput controllers from the main process. Chromium only delivers gamepad input to a
 * focused window, so this is what lets the in-game menu open while the emulator has focus.
 *
 * Emits 'menu' when the configured hotkey is pressed on any controller.
 */
export class GamepadWatcher extends EventEmitter {
  private timer: NodeJS.Timeout | null = null
  private prev = [0, 0, 0, 0]
  private idleSlots = new Set<number>()
  private tick = 0

  constructor(public hotkey: OverlayHotkey) {
    super()
  }

  start(): void {
    if (this.timer) return
    this.prev = [0, 0, 0, 0]
    this.timer = setInterval(() => this.poll(), 16)
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer)
    this.timer = null
  }

  private poll(): void {
    this.tick++
    for (let i = 0; i < 4; i++) {
      // Polling a disconnected slot is slow on some systems; re-check empty slots once a second.
      if (this.idleSlots.has(i) && this.tick % 60 !== 0) continue
      const s = readXInput(i)
      if (!s) {
        this.idleSlots.add(i)
        this.prev[i] = 0
        continue
      }
      this.idleSlots.delete(i)
      const now = s.buttons
      const pressed = now & ~this.prev[i]
      this.prev[i] = now

      const guide = (pressed & XINPUT.GUIDE) !== 0
      const combo =
        (now & XINPUT.BACK) !== 0 && (now & XINPUT.START) !== 0 && (pressed & (XINPUT.BACK | XINPUT.START)) !== 0
      if ((this.hotkey !== 'back-start' && guide) || (this.hotkey !== 'guide' && combo)) {
        this.emit('menu', i)
      }
    }
  }
}
