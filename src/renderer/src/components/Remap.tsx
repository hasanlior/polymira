import { useEffect, useState } from 'react'
import type { Settings } from '@shared/types'
import { ButtonGlyph, Hint, STYLE_NAMES } from '../glyphs'
import { DEFAULT_MAP, MAPPABLE, input, usePadStyle, useInput, type Action } from '../input'
import { sfx } from '../sound'

export const ACTION_NAMES: Record<string, string> = {
  confirm: 'Seç / Onayla',
  back: 'Geri',
  view: 'Galeri / Alternatif',
  options: 'Seçenekler',
  l1: 'Önceki kategori (sol omuz)',
  r1: 'Sonraki kategori (sağ omuz)',
  l2: 'Sol tetik',
  r2: 'Sağ tetik',
  select: 'Select / Geri tuşu',
  start: 'Start / Menü'
}

/** Controller button remapping: pick an action, press the new button. Conflicting buttons are swapped. */
export function RemapScreen({ settings, onClose }: { settings: Settings; onClose: () => void }) {
  const [idx, setIdx] = useState(0)
  const [waiting, setWaiting] = useState<Action | null>(null)
  const style = usePadStyle()
  const shown = style === 'keyboard' ? 'xbox' : style
  const map: Record<string, number> = { ...DEFAULT_MAP, ...settings.padMap }
  const rows: (Action | 'reset')[] = [...MAPPABLE, 'reset']

  const save = (next: Record<string, number>) => {
    const diff: Record<string, number> = {}
    for (const [a, b] of Object.entries(next)) if (DEFAULT_MAP[a] !== b) diff[a] = b
    void window.bridge.invoke('settings:set', { padMap: diff })
  }

  useEffect(() => {
    if (!waiting) return
    const t = setTimeout(() => {
      input.capture = null
      setWaiting(null)
    }, 6000)
    input.capture = (button) => {
      const next = { ...map }
      const other = Object.keys(next).find((a) => next[a] === button && a !== waiting)
      if (other) next[other] = next[waiting]
      next[waiting] = button
      save(next)
      sfx('confirm')
      setWaiting(null)
    }
    return () => {
      clearTimeout(t)
      input.capture = null
    }
  }, [waiting])

  useInput((a) => {
    if (waiting) {
      if (a === 'back') setWaiting(null) // keyboard Esc cancels
      return
    }
    if (a === 'up' && idx > 0) {
      sfx('move')
      setIdx(idx - 1)
    } else if (a === 'down' && idx < rows.length - 1) {
      sfx('move')
      setIdx(idx + 1)
    } else if (a === 'confirm') {
      const r = rows[idx]
      sfx('confirm')
      if (r === 'reset') save({ ...DEFAULT_MAP })
      else setWaiting(r)
    } else if (a === 'back') {
      sfx('back')
      onClose()
    }
  })

  return (
    <div className="modal-layer center">
      <div className="dialog remap">
        <div className="dialog-text">Tuş ataması</div>
        <div className="remap-sub">{`Simgeler: ${STYLE_NAMES[shown]} · Değiştirmek istediğin işlemi seç, sonra yeni tuşa bas.`}</div>
        <div className="remap-list">
          {rows.map((r, i) => (
            <div key={r} className={`remap-row ${i === idx ? 'sel' : ''}`}>
              <span>{r === 'reset' ? 'Varsayılan tuşlara dön' : ACTION_NAMES[r]}</span>
              {r !== 'reset' && (waiting === r ? <span className="remap-wait">Bir tuşa bas…</span> : <ButtonGlyph button={map[r]} style={shown} />)}
            </div>
          ))}
        </div>
        <div className="x-hints static">
          <Hint action="confirm">Değiştir</Hint>
          <Hint action="back">Kapat</Hint>
        </div>
      </div>
    </div>
  )
}
