import { useEffect, useState } from 'react'
import type { Notice } from '@shared/types'
import { Icon, type IconName } from '../icons'
import { sfx } from '../sound'

const KIND_ICON: Record<Notice['kind'], IconName> = {
  download: 'up',
  trophy: 'star',
  update: 'refresh',
  save: 'storage',
  info: 'info'
}

/** PS3-style notification cards that slide in at the top right and leave after a few seconds. */
export function NoticeStack({ sound = true }: { sound?: boolean }) {
  const [items, setItems] = useState<(Notice & { leaving?: boolean })[]>([])
  useEffect(
    () =>
      window.bridge.on('notice', (n) => {
        if (sound) sfx('notify')
        setItems((xs) => [n, ...xs].slice(0, 3))
        setTimeout(() => setItems((xs) => xs.map((x) => (x.id === n.id ? { ...x, leaving: true } : x))), 5200)
        setTimeout(() => setItems((xs) => xs.filter((x) => x.id !== n.id)), 5700)
      }),
    [sound]
  )
  return (
    <div className="notices">
      {items.map((n) => (
        <div key={n.id} className={`notice ${n.kind} ${n.leaving ? 'leaving' : ''}`}>
          <div className="notice-icon">{n.icon ? <img src={n.icon} alt="" /> : <Icon name={KIND_ICON[n.kind]} />}</div>
          <div className="notice-body">
            <div className="notice-title">{n.title}</div>
            {n.text && <div className="notice-text">{n.text}</div>}
          </div>
        </div>
      ))}
    </div>
  )
}
