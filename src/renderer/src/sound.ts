/** Menu sounds synthesized with WebAudio — no audio assets needed. */
type Sfx = 'move' | 'confirm' | 'back' | 'error' | 'category'

let ctx: AudioContext | null = null
let enabled = true
let volume = 0.5

export function configureSound(on: boolean, vol: number): void {
  enabled = on
  volume = vol
}

function tone(freq: number, start: number, dur: number, gain: number, type: OscillatorType = 'sine', endFreq?: number) {
  const c = ctx!
  const o = c.createOscillator()
  const g = c.createGain()
  o.type = type
  o.frequency.setValueAtTime(freq, c.currentTime + start)
  if (endFreq) o.frequency.exponentialRampToValueAtTime(endFreq, c.currentTime + start + dur)
  g.gain.setValueAtTime(0, c.currentTime + start)
  g.gain.linearRampToValueAtTime(gain * volume, c.currentTime + start + 0.008)
  g.gain.exponentialRampToValueAtTime(0.0001, c.currentTime + start + dur)
  o.connect(g).connect(c.destination)
  o.start(c.currentTime + start)
  o.stop(c.currentTime + start + dur + 0.02)
}

export function sfx(kind: Sfx): void {
  if (!enabled || volume <= 0) return
  try {
    ctx ??= new AudioContext()
    if (ctx.state === 'suspended') void ctx.resume()
    switch (kind) {
      case 'move':
        tone(1320, 0, 0.05, 0.08)
        break
      case 'category':
        tone(990, 0, 0.07, 0.09)
        tone(1480, 0.015, 0.06, 0.04)
        break
      case 'confirm':
        tone(880, 0, 0.08, 0.12)
        tone(1320, 0.06, 0.12, 0.1)
        break
      case 'back':
        tone(1100, 0, 0.1, 0.1, 'sine', 620)
        break
      case 'error':
        tone(220, 0, 0.18, 0.14, 'triangle')
        tone(180, 0.09, 0.2, 0.12, 'triangle')
        break
    }
  } catch {
    // audio not available
  }
}
