import { useEffect, useRef, useState } from 'react'
import { Icon } from '../icons'
import { Hint } from '../glyphs'
import { useInput } from '../input'
import { sfx } from '../sound'

type Key = { label: string; value?: string; action?: 'shift' | 'back' | 'space' | 'done' | 'clear'; wide?: number }

const ROWS: Key[][] = [
  '1234567890'.split('').map((c) => ({ label: c })),
  'qwertyuıopğü'.split('').map((c) => ({ label: c })),
  'asdfghjklşi'.split('').map((c) => ({ label: c })),
  ['z', 'x', 'c', 'v', 'b', 'n', 'm', 'ö', 'ç', '.', '-'].map((c) => ({ label: c })),
  [
    { label: '⇧', action: 'shift', wide: 1.5 },
    { label: '@' },
    { label: '/' },
    { label: ':' },
    { label: '.com', value: '.com', wide: 1.5 },
    { label: 'Boşluk', action: 'space', wide: 3 },
    { label: '⌫', action: 'back', wide: 1.5 },
    { label: 'Tamam', action: 'done', wide: 2 }
  ]
]

/**
 * Controller on-screen keyboard. ✕/A types the key, □/X deletes, △/Y adds a space, L1 is shift,
 * ○/B cancels. A physical keyboard can type straight into the field.
 */
export function OnScreenKeyboard({
  title,
  initial = '',
  placeholder,
  onDone,
  onClose
}: {
  title: string
  initial?: string
  placeholder?: string
  onDone: (text: string) => void
  onClose: () => void
}) {
  const [text, setText] = useState(initial)
  const [shift, setShift] = useState(false)
  const [pos, setPos] = useState({ r: 1, c: 0 })
  const field = useRef<HTMLInputElement>(null)
  useEffect(() => field.current?.focus(), [])

  const finish = () => {
    sfx('confirm')
    onClose()
    onDone(text)
  }
  const press = (k: Key) => {
    sfx('move')
    if (k.action === 'shift') setShift((s) => !s)
    else if (k.action === 'back') setText((t) => t.slice(0, -1))
    else if (k.action === 'space') setText((t) => t + ' ')
    else if (k.action === 'done') finish()
    else {
      const v = k.value ?? k.label
      setText((t) => t + (shift ? v.toLocaleUpperCase('tr') : v))
      if (shift) setShift(false)
    }
  }
  const moveRow = (dr: number) =>
    setPos((p) => {
      const r = (p.r + dr + ROWS.length) % ROWS.length
      const frac = (p.c + 0.5) / ROWS[p.r].length
      return { r, c: Math.min(ROWS[r].length - 1, Math.floor(frac * ROWS[r].length)) }
    })

  useInput((a) => {
    if (a === 'left') setPos((p) => ({ ...p, c: (p.c - 1 + ROWS[p.r].length) % ROWS[p.r].length }))
    else if (a === 'right') setPos((p) => ({ ...p, c: (p.c + 1) % ROWS[p.r].length }))
    else if (a === 'up') moveRow(-1)
    else if (a === 'down') moveRow(1)
    else if (a === 'confirm') press(ROWS[pos.r][pos.c])
    else if (a === 'view') press({ label: '⌫', action: 'back' })
    else if (a === 'options') press({ label: ' ', action: 'space' })
    else if (a === 'l1') setShift((s) => !s)
    else if (a === 'start' || a === 'r2') finish()
    else if (a === 'back') {
      sfx('back')
      onClose()
    }
    if (a === 'left' || a === 'right' || a === 'up' || a === 'down') sfx('move')
  }, true, true)

  return (
    <div className="modal-layer center osk-layer">
      <div className="osk">
        <div className="osk-title">{title}</div>
        <input
          ref={field}
          className="osk-field"
          value={text}
          placeholder={placeholder}
          spellCheck={false}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') finish()
            else if (e.key === 'Escape') onClose()
          }}
        />
        <div className="osk-keys">
          {ROWS.map((row, r) => (
            <div className="osk-row" key={r}>
              {row.map((k, c) => (
                <div
                  key={k.label}
                  className={`osk-key ${pos.r === r && pos.c === c ? 'sel' : ''} ${k.action === 'shift' && shift ? 'on' : ''} ${k.action === 'done' ? 'done' : ''}`}
                  style={{ flexGrow: k.wide ?? 1 }}
                  onMouseDown={(e) => {
                    e.preventDefault()
                    setPos({ r, c })
                    press(k)
                  }}
                >
                  {k.action || k.value ? k.label : shift ? k.label.toLocaleUpperCase('tr') : k.label}
                </div>
              ))}
            </div>
          ))}
        </div>
        <div className="osk-hints">
          <Hint action="confirm">Yaz</Hint>
          <Hint action="view">Sil</Hint>
          <Hint action="options">Boşluk</Hint>
          <Hint action="l1">Büyük harf</Hint>
          <Hint action="start">Tamam</Hint>
          <Hint action="back">İptal</Hint>
        </div>
      </div>
    </div>
  )
}
