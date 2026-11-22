import type { ReactNode } from 'react'
import { input, usePadStyle, type Action, type PadStyle } from './input'

/**
 * Button symbols for the controller in use (Xbox, PlayStation, Nintendo) or the keyboard.
 * Drawn as simple shapes and letters; no vendor artwork.
 */

type Face = { label: string; color: string; shape?: 'cross' | 'circle' | 'square' | 'triangle' }

const FACE: Record<Exclude<PadStyle, 'keyboard'>, Face[]> = {
  // standard mapping order: bottom, right, left, top
  xbox: [
    { label: 'A', color: '#5fd068' },
    { label: 'B', color: '#ff5b5b' },
    { label: 'X', color: '#4aa3ff' },
    { label: 'Y', color: '#ffcf3a' }
  ],
  ps: [
    { label: '', color: '#7fb2ff', shape: 'cross' },
    { label: '', color: '#ff6b7c', shape: 'circle' },
    { label: '', color: '#ff8ad8', shape: 'square' },
    { label: '', color: '#3fe0b5', shape: 'triangle' }
  ],
  switch: [
    { label: 'B', color: '#d8dbe6' },
    { label: 'A', color: '#d8dbe6' },
    { label: 'Y', color: '#d8dbe6' },
    { label: 'X', color: '#d8dbe6' }
  ]
}

const OTHER: Record<Exclude<PadStyle, 'keyboard'>, Record<number, string>> = {
  xbox: { 4: 'LB', 5: 'RB', 6: 'LT', 7: 'RT', 8: 'View', 9: 'Menu', 10: 'LS', 11: 'RS', 16: 'Guide' },
  ps: { 4: 'L1', 5: 'R1', 6: 'L2', 7: 'R2', 8: 'Create', 9: 'Options', 10: 'L3', 11: 'R3', 16: 'PS' },
  switch: { 4: 'L', 5: 'R', 6: 'ZL', 7: 'ZR', 8: '−', 9: '+', 10: 'LS', 11: 'RS', 16: 'Home' }
}

const KEYS: Partial<Record<Action, string>> = {
  confirm: 'Enter',
  back: 'Esc',
  options: 'T',
  view: 'G',
  l1: 'Q',
  r1: 'E',
  l2: 'PgUp',
  r2: 'PgDn',
  start: 'Tab',
  select: '—',
  home: 'Ctrl+Shift+Home'
}

function Shape({ shape }: { shape: NonNullable<Face['shape']> }) {
  const common = { fill: 'none', stroke: 'currentColor', strokeWidth: 2.4, strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const }
  return (
    <svg viewBox="0 0 16 16" className="glyph-shape" aria-hidden>
      {shape === 'cross' && <path d="M4 4l8 8M12 4l-8 8" {...common} />}
      {shape === 'circle' && <circle cx="8" cy="8" r="4.6" {...common} />}
      {shape === 'square' && <rect x="3.6" y="3.6" width="8.8" height="8.8" rx="0.8" {...common} />}
      {shape === 'triangle' && <path d="M8 3.2 13 12H3z" {...common} />}
    </svg>
  )
}

/** Symbol for a physical button index in a given style. */
export function ButtonGlyph({ button, style }: { button: number; style: PadStyle }) {
  if (style === 'keyboard') return <span className="glyph key">?</span>
  if (button <= 3) {
    const f = FACE[style][button]
    return (
      <span className={`glyph face ${style}`} style={{ color: f.color }}>
        {f.shape ? <Shape shape={f.shape} /> : f.label}
      </span>
    )
  }
  if (button >= 12 && button <= 15) return <span className="glyph pill">{['↑', '↓', '←', '→'][button - 12]}</span>
  return <span className="glyph pill">{OTHER[style][button] ?? `#${button}`}</span>
}

/** Symbol for the button that triggers `action` with the current controller / keyboard. */
export function Glyph({ action }: { action: Action }) {
  const style = usePadStyle()
  if (style === 'keyboard') return <span className="glyph key">{KEYS[action] ?? action}</span>
  if (action === 'home') return <ButtonGlyph button={16} style={style} />
  const button = input.map[action]
  if (button === undefined) return null
  return <ButtonGlyph button={button} style={style} />
}

/** A button hint: symbol + label, e.g. [A] Seç. */
export function Hint({ action, children }: { action: Action; children: ReactNode }) {
  return (
    <span className="hint">
      <Glyph action={action} /> {children}
    </span>
  )
}

export const STYLE_NAMES: Record<PadStyle, string> = {
  xbox: 'Xbox',
  ps: 'PlayStation',
  switch: 'Nintendo',
  keyboard: 'Klavye'
}
