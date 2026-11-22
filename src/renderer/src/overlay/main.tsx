import { createRoot } from 'react-dom/client'
import { useEffect, useState } from 'react'
import { getPlatform } from '@shared/platforms'
import type { PerfSnapshot, Toast } from '@shared/types'
import { call, coverUrl, formatPlayTime, useAppState } from '../api'
import { Icon, type IconName } from '../icons'
import { Hint } from '../glyphs'
import { input, useInput } from '../input'
import { configureSound, sfx } from '../sound'
import '../styles.css'
import { NoticeStack } from '../components/Notices'

/**
 * In-game menu. Runs in a transparent always-on-top window above the emulator and is
 * opened from the main process by the controller Guide button, Back+Start, or Ctrl+Shift+Home.
 */
function Overlay() {
  const state = useAppState()
  const [idx, setIdx] = useState(0)
  const [perf, setPerf] = useState<PerfSnapshot | null>(null)
  const [confirmQuit, setConfirmQuit] = useState(false)
  const [toast, setToast] = useState<Toast | null>(null)
  const [visible, setVisible] = useState(false)

  useEffect(() => {
    input.start()
    const offOpen = window.bridge.on('overlay:opened', () => {
      setIdx(0)
      setConfirmQuit(false)
      setVisible(true)
    })
    const offToast = window.bridge.on('toast', (t) => {
      setToast(t)
      setTimeout(() => setToast(null), 2500)
    })
    return () => {
      offOpen()
      offToast()
    }
  }, [])

  useEffect(() => {
    if (!state) return
    configureSound(state.settings.menuSounds, state.settings.soundVolume)
    input.setMap(state.settings.padMap)
    input.setForcedStyle(state.settings.padStyle === 'auto' ? null : state.settings.padStyle)
  }, [state?.settings])

  useEffect(() => {
    if (!visible) return
    const tick = () => void call('session:perf').then(setPerf).catch(() => {})
    tick()
    const t = setInterval(tick, 1000)
    return () => clearInterval(t)
  }, [visible])

  const session = state?.session
  const game = session ? state?.games.find((g) => g.id === session.gameId) : undefined
  const platform = game ? getPlatform(game.platformId) : undefined

  const close = () => {
    setVisible(false)
    void call('overlay:close')
  }

  const items: { label: string; icon: IconName; run: () => void }[] = [
    { label: 'Oyuna devam et', icon: 'play', run: close },
    {
      label: session?.paused ? 'Oyunu sürdür' : 'Oyunu duraklat',
      icon: session?.paused ? 'play' : 'pause',
      run: () => void call('session:pause', !session?.paused)
    },
    { label: 'Ekran görüntüsü al', icon: 'camera', run: () => void call('session:screenshot') },
    {
      label: 'Ana menüye dön',
      icon: 'home',
      run: () => {
        setVisible(false)
        void call('session:home')
      }
    },
    { label: 'Oyunu kapat', icon: 'exit', run: () => setConfirmQuit(true) }
  ]

  useInput((a) => {
    if (confirmQuit) {
      if (a === 'confirm') {
        sfx('confirm')
        setVisible(false)
        void call('session:stop')
      } else if (a === 'back') {
        sfx('back')
        setConfirmQuit(false)
      }
      return
    }
    if (a === 'up' && idx > 0) {
      sfx('move')
      setIdx(idx - 1)
    } else if (a === 'down' && idx < items.length - 1) {
      sfx('move')
      setIdx(idx + 1)
    } else if (a === 'confirm') {
      sfx('confirm')
      items[idx].run()
    } else if (a === 'back') {
      sfx('back')
      close()
    }
  })

  if (!state || !session || !game) return <NoticeStack />

  return (
    <>
    <NoticeStack />
    <div className={`ov ${visible ? 'show' : ''}`}>
      <div className="ov-panel">
        <div className="ov-game">
          {game.hasCover ? <img src={coverUrl(game.id, game.coverRev)} alt="" /> : <Icon name="game" />}
          <div>
            <div className="ov-title">{game.title}</div>
            <div className="ov-sub">
              {platform?.name} · {session.paused ? 'Duraklatıldı' : 'Çalışıyor'}
            </div>
          </div>
        </div>
        <div className="ov-list">
          {items.map((it, i) => (
            <div key={it.label} className={`ov-item ${i === idx ? 'sel' : ''}`}>
              <Icon name={it.icon} />
              <span>{it.label}</span>
            </div>
          ))}
        </div>
        {confirmQuit && (
          <div className="ov-confirm">
            Oyun kapatılsın mı? Kaydedilmemiş ilerleme kaybolur.
            <div className="ov-confirm-keys"><Hint action="confirm">Evet</Hint> <Hint action="back">Hayır</Hint></div>
          </div>
        )}
        <div className="x-hints static">
          <Hint action="confirm">Seç</Hint>
          <Hint action="back">Oyuna dön</Hint>
        </div>
      </div>

      <div className="ov-perf">
        <div className="ov-perf-title">
          <Icon name="chart" /> Performans
        </div>
        <PerfRow label="Oturum süresi" value={perf ? formatPlayTime(perf.sessionSec) : '—'} />
        <PerfRow label="Oyun CPU" value={perf?.gameCpu != null ? `%${perf.gameCpu}` : '—'} bar={perf?.gameCpu ?? undefined} />
        <PerfRow label="Oyun RAM" value={perf?.gameMemMb != null ? `${perf.gameMemMb} MB` : '—'} />
        <PerfRow label="Sistem CPU" value={perf ? `%${perf.sysCpu}` : '—'} bar={perf?.sysCpu} />
        <PerfRow
          label="Sistem RAM"
          value={perf ? `${(perf.sysMemUsedMb / 1024).toFixed(1)} / ${(perf.sysMemTotalMb / 1024).toFixed(1)} GB` : '—'}
          bar={perf ? (perf.sysMemUsedMb / perf.sysMemTotalMb) * 100 : undefined}
        />
        {perf?.gpu ? (
          <>
            <PerfRow label="GPU" value={`%${perf.gpu.util}`} bar={perf.gpu.util} />
            <PerfRow label="VRAM" value={`${perf.gpu.memUsedMb} MB`} />
            <div className="ov-perf-note">{perf.gpu.name}</div>
          </>
        ) : (
          <div className="ov-perf-note">GPU bilgisi şu an yalnızca NVIDIA kartlarda gösteriliyor.</div>
        )}
      </div>

      {toast && <div className={`toast ov-toast ${toast.kind}`}>{toast.text}</div>}
    </div>
    </>
  )
}

function PerfRow({ label, value, bar }: { label: string; value: string; bar?: number }) {
  return (
    <div className="ov-perf-row">
      <div className="ov-perf-line">
        <span>{label}</span>
        <b>{value}</b>
      </div>
      {bar !== undefined && (
        <div className="ov-bar">
          <div style={{ width: `${Math.min(100, Math.max(0, bar))}%` }} />
        </div>
      )}
    </div>
  )
}

createRoot(document.getElementById('root')!).render(<Overlay />)
