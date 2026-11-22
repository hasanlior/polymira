import { useEffect, useState } from 'react'
import type { PlayStats } from '@shared/types'
import { call, formatPlayTime } from '../api'
import { Hint } from '../glyphs'
import { Icon } from '../icons'
import { useInput } from '../input'
import { sfx } from '../sound'

function hours(sec: number): string {
  if (sec < 60) return sec ? '<1 dk' : '0'
  const h = Math.floor(sec / 3600)
  const m = Math.round((sec % 3600) / 60)
  return h ? `${h} sa ${m} dk` : `${m} dk`
}

/** Play statistics: last 7 days as bars, totals, and the most played games. */
export function StatsScreen({ onClose }: { onClose: () => void }) {
  const [stats, setStats] = useState<PlayStats | null>(null)
  useEffect(() => {
    void call('stats:get').then(setStats)
  }, [])
  useInput((a) => {
    if (a === 'back' || a === 'confirm') {
      sfx('back')
      onClose()
    }
  })
  const max = Math.max(1, ...(stats?.days.map((d) => d.sec) ?? [1]))
  return (
    <div className="modal-layer center stats-layer">
      <div className="stats">
        <div className="stats-head">
          <Icon name="chart" /> Oynama istatistikleri
        </div>
        {!stats ? (
          <div className="stats-empty">Yükleniyor…</div>
        ) : (
          <>
            <div className="stats-tiles">
              <div className="stats-tile">
                <b>{hours(stats.weekSec)}</b>
                <span>Bu hafta</span>
              </div>
              <div className="stats-tile">
                <b>{hours(stats.totalSec)}</b>
                <span>Toplam</span>
              </div>
              <div className="stats-tile">
                <b>{stats.gamesPlayed}</b>
                <span>Oynanan oyun</span>
              </div>
              <div className="stats-tile">
                <b>{stats.sessions}</b>
                <span>Oturum</span>
              </div>
            </div>
            <div className="stats-chart" role="img" aria-label="Son 7 günün oynama süresi">
              {stats.days.map((d) => (
                <div className="stats-col" key={d.day}>
                  <div className="stats-val">{d.sec ? hours(d.sec) : ''}</div>
                  <div className="stats-bar-wrap">
                    <div className="stats-bar" style={{ height: `${(d.sec / max) * 100}%` }} />
                  </div>
                  <div className="stats-day">{new Date(d.day).toLocaleDateString('tr-TR', { weekday: 'short' })}</div>
                </div>
              ))}
            </div>
            <div className="stats-top-title">En çok oynananlar</div>
            {stats.top.length === 0 && <div className="stats-empty">Henüz oynama kaydı yok.</div>}
            {stats.top.map((g, i) => (
              <div className="stats-row" key={g.gameId}>
                <span className="stats-rank">{i + 1}</span>
                <span className="stats-name">{g.title}</span>
                <span className="stats-week">{g.weekSec ? `bu hafta ${hours(g.weekSec)}` : ''}</span>
                <span className="stats-total">{formatPlayTime(g.sec)}</span>
              </div>
            ))}
          </>
        )}
        <div className="x-hints static">
          <Hint action="back">Kapat</Hint>
        </div>
      </div>
    </div>
  )
}
