/**
 * Menu sounds. They are synthesized once into small WAV files in memory and played with
 * ordinary <audio> elements. (A live WebAudio context fails on some audio devices and can take
 * the page down with it; plain media playback does not.)
 */
type Sfx = 'move' | 'confirm' | 'back' | 'error' | 'category' | 'notify'

interface Tone {
  freq: number
  start: number
  dur: number
  gain: number
  wave?: 'sine' | 'triangle'
  endFreq?: number
}

const RATE = 44100
let enabled = true
let volume = 0.5
const urls = new Map<Sfx, string>()

export function configureSound(on: boolean, vol: number): void {
  enabled = on
  volume = vol
}

function render(tones: Tone[]): string {
  const length = Math.ceil(RATE * Math.max(...tones.map((t) => t.start + t.dur + 0.02)))
  const data = new Float32Array(length)
  for (const t of tones) {
    const from = Math.floor(t.start * RATE)
    const n = Math.floor(t.dur * RATE)
    let phase = 0
    for (let i = 0; i < n && from + i < length; i++) {
      const k = i / n
      const f = t.endFreq ? t.freq * Math.pow(t.endFreq / t.freq, k) : t.freq
      phase += (2 * Math.PI * f) / RATE
      const osc = t.wave === 'triangle' ? (2 / Math.PI) * Math.asin(Math.sin(phase)) : Math.sin(phase)
      // 8 ms attack, exponential decay
      const env = Math.min(1, i / (0.008 * RATE)) * Math.exp(-5 * k)
      data[from + i] += osc * env * t.gain
    }
  }
  // 16-bit PCM WAV
  const buf = new ArrayBuffer(44 + length * 2)
  const v = new DataView(buf)
  const str = (o: number, s: string) => [...s].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)))
  str(0, 'RIFF')
  v.setUint32(4, 36 + length * 2, true)
  str(8, 'WAVEfmt ')
  v.setUint32(16, 16, true)
  v.setUint16(20, 1, true)
  v.setUint16(22, 1, true)
  v.setUint32(24, RATE, true)
  v.setUint32(28, RATE * 2, true)
  v.setUint16(32, 2, true)
  v.setUint16(34, 16, true)
  str(36, 'data')
  v.setUint32(40, length * 2, true)
  for (let i = 0; i < length; i++) v.setInt16(44 + i * 2, Math.max(-1, Math.min(1, data[i])) * 0x7fff, true)
  return URL.createObjectURL(new Blob([buf], { type: 'audio/wav' }))
}

const DEFS: Record<Sfx, Tone[]> = {
  move: [{ freq: 1320, start: 0, dur: 0.06, gain: 0.35 }],
  category: [
    { freq: 990, start: 0, dur: 0.08, gain: 0.35 },
    { freq: 1480, start: 0.015, dur: 0.07, gain: 0.18 }
  ],
  confirm: [
    { freq: 880, start: 0, dur: 0.09, gain: 0.4 },
    { freq: 1320, start: 0.06, dur: 0.14, gain: 0.35 }
  ],
  back: [{ freq: 1100, start: 0, dur: 0.12, gain: 0.35, endFreq: 620 }],
  notify: [
    { freq: 1046, start: 0, dur: 0.12, gain: 0.3 },
    { freq: 1568, start: 0.09, dur: 0.22, gain: 0.26 }
  ],
  error: [
    { freq: 220, start: 0, dur: 0.2, gain: 0.45, wave: 'triangle' },
    { freq: 180, start: 0.09, dur: 0.22, gain: 0.4, wave: 'triangle' }
  ]
}

export function sfx(kind: Sfx): void {
  if (!enabled || volume <= 0) return
  try {
    let url = urls.get(kind)
    if (!url) {
      url = render(DEFS[kind])
      urls.set(kind, url)
    }
    const a = new Audio(url)
    a.volume = Math.min(1, volume)
    void a.play().catch(() => {})
  } catch {
    // audio not available
  }
}
