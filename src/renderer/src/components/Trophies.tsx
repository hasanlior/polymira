import { locale } from '../i18n'
import { useEffect, useRef, useState } from 'react'
import type { Trophy, TrophyGame } from '@shared/types'
import { call } from '../api'
import { Hint } from '../glyphs'
import { Icon } from '../icons'
import { useInput } from '../input'
import { sfx } from '../sound'

function Pic({ src, locked }: { src?: string; locked?: boolean }) {
  const [broken, setBroken] = useState(false)
  if (!src || broken)
    return (
      <div className="trophy-ph">
        <Icon name={locked ? 'warning' : 'star'} />
      </div>
    )
  return <img src={src} alt="" onError={() => setBroken(true)} />
}

/** Trophy room: games with trophies (Steam + RetroAchievements), then one game's trophies. */
export function TrophyScreen({ initialKey, onClose }: { initialKey?: string; onClose: () => void }) {
  const [games, setGames] = useState<TrophyGame[] | null>(null)
  const [open, setOpen] = useState<TrophyGame | null>(null)
  const [list, setList] = useState<Trophy[] | null>(null)
  const [idx, setIdx] = useState(0)
  const [gameIdx, setGameIdx] = useState(0)
  const listRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    void call('trophies:games')
      .then((g) => {
        setGames(g)
        const start = initialKey ? g.find((x) => x.key === initialKey) : undefined
        if (start) setOpen(start)
      })
      .catch(() => setGames([]))
  }, [])

  useEffect(() => {
    if (!open) return
    setList(null)
    setIdx(0)
    void call('trophies:list', open.key)
      .then(setList)
      .catch(() => setList([]))
  }, [open])

  useEffect(() => {
    listRef.current?.querySelector('.sel')?.scrollIntoView({ block: 'nearest' })
  }, [idx, gameIdx, open])

  const rows = open ? list ?? [] : games ?? []
  const cur = open ? idx : gameIdx
  const setCur = open ? setIdx : setGameIdx

  useInput((a) => {
    if (a === 'up' && cur > 0) {
      sfx('move')
      setCur(cur - 1)
    } else if (a === 'down' && cur < rows.length - 1) {
      sfx('move')
      setCur(cur + 1)
    } else if (a === 'confirm' && !open && games?.[gameIdx]) {
      sfx('confirm')
      setOpen(games[gameIdx])
    } else if (a === 'back') {
      sfx('back')
      if (open && !initialKey) setOpen(null)
      else onClose()
    }
  })

  const done = list?.filter((t) => t.unlocked).length ?? 0
  return (
    <div className="modal-layer center">
      <div className="trophies">
        <div className="trophies-head">
          <Icon name="star" /> {open ? `${open.title} · ${done}/${list?.length ?? open.total}` : 'Kupalar'}
        </div>
        <div className="trophies-list" ref={listRef}>
          {!open && games === null && <div className="stats-empty">Yükleniyor…</div>}
          {!open && games?.length === 0 && (
            <div className="stats-empty">
              Henüz kupa bulunamadı. Steam oyunlarının başarımları otomatik okunur; emülatör oyunları için Ayarlar → Oyun Modu ve Sistem → RetroAchievements hesabı.
            </div>
          )}
          {!open &&
            games?.map((g, i) => (
              <div key={g.key} className={`trophy-row ${i === gameIdx ? 'sel' : ''}`}>
                <Pic src={g.icon} />
                <div className="trophy-main">
                  <div className="trophy-title">{g.title}</div>
                  <div className="trophy-desc">{g.subtitle}</div>
                </div>
                <div className="trophy-side">
                  {g.unlocked}/{g.total} · %{g.total ? Math.round((g.unlocked / g.total) * 100) : 0}
                  <div className="trophy-bar">
                    <div style={{ width: `${g.total ? (g.unlocked / g.total) * 100 : 0}%` }} />
                  </div>
                </div>
              </div>
            ))}
          {open && list === null && <div className="stats-empty">Yükleniyor…</div>}
          {open &&
            list?.map((t, i) => (
              <div key={t.id} className={`trophy-row ${i === idx ? 'sel' : ''} ${t.unlocked ? '' : 'locked'}`}>
                <Pic src={t.icon} locked={!t.unlocked} />
                <div className="trophy-main">
                  <div className="trophy-title">{t.hidden && !t.unlocked ? 'Gizli kupa' : t.title}</div>
                  <div className="trophy-desc">{t.hidden && !t.unlocked ? 'Açıldığında görünecek.' : t.description}</div>
                </div>
                <div className="trophy-side">
                  {t.unlocked ? (t.unlockedAt ? new Date(t.unlockedAt).toLocaleDateString(locale()) : 'Açıldı') : 'Kilitli'}
                </div>
              </div>
            ))}
        </div>
        <div className="x-hints static">
          {!open && <Hint action="confirm">Aç</Hint>}
          <Hint action="back">{open && !initialKey ? 'Geri' : 'Kapat'}</Hint>
        </div>
      </div>
    </div>
  )
}
