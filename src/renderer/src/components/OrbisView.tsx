import { useEffect, useRef, useState } from 'react'
import type { OrbisReport } from '@shared/types'
import { call } from '../api'
import { Hint } from '../glyphs'
import { Icon } from '../icons'
import { useInput } from '../input'
import { sfx } from '../sound'

const KIND: Record<string, string> = { executable: 'çalıştırılabilir dosya', 'dynamic executable': 'dinamik çalıştırılabilir', 'system module': 'sistem modülü', other: 'diğer' }

/**
 * POLYMIRA Orbis report for a PS4/PS5 game: which system libraries the executable needs and
 * how much of them Orbis already implements. "mode: run" starts it and shows what it printed.
 */
export function OrbisView({ gameId, mode, onClose }: { gameId?: string; mode: 'analyze' | 'run' | 'selftest'; onClose: () => void }) {
  const [report, setReport] = useState<OrbisReport | null>(null)
  const [log, setLog] = useState<{ code: number; log: string } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const box = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const p =
      mode === 'analyze'
        ? call('orbis:analyze', gameId!).then(setReport)
        : mode === 'run'
          ? call('orbis:run', gameId!).then(setLog)
          : call('orbis:selftest').then(setLog)
    void p.catch((e: Error) => setError(e.message))
  }, [gameId, mode])

  useInput((a) => {
    if (a === 'down' || a === 'up') box.current?.scrollBy({ top: a === 'down' ? 140 : -140, behavior: 'smooth' })
    else if (a === 'back' || a === 'confirm') {
      sfx('back')
      onClose()
    }
  })

  const title = mode === 'analyze' ? 'Orbis analizi' : mode === 'run' ? 'Orbis ile deneme' : 'Orbis kendi testi'
  return (
    <div className="modal-layer center">
      <div className="whatsnew orbis">
        <div className="stats-head">
          <Icon name="chip" /> {title}
          <span className="orbis-tag">deneysel</span>
        </div>
        <div className="whatsnew-body" ref={box}>
          {error && <p className="orbis-error">{error}</p>}
          {!error && !report && !log && <p>Çalışıyor…</p>}
          {report && (
            <>
              <div className="orbis-score">
                <b>%{report.readiness}</b>
                <span>
                  {`${report.imports} sistem fonksiyonundan ${report.implemented} tanesi hazır`}
                  {report.from_game_modules ? ` (${report.from_game_modules} tanesi oyunun kendi modüllerinden)` : ''}
                </span>
              </div>
              <div className="trophy-bar orbis-bar">
                <div style={{ width: `${report.readiness}%` }} />
              </div>
              <p className="orbis-meta">{`${report.container} · ${KIND[report.kind] ?? report.kind} · ${report.segments} segment${report.uses_tls ? ' · TLS' : ''}`}</p>
              <h3>Sistem kütüphaneleri</h3>
              <table className="orbis-table">
                <tbody>
                  {report.libraries.map((l) => (
                    <tr key={l.name}>
                      <td>{l.name}</td>
                      <td>{`${l.implemented}/${l.functions}`}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {report.missing_examples.length > 0 && (
                <>
                  <h3>Henüz olmayan fonksiyonlardan örnekler</h3>
                  <p className="orbis-mono">{report.missing_examples.join(', ')}</p>
                </>
              )}
            </>
          )}
          {log && (
            <>
              <p>{log.code === 0 ? 'Çıkış kodu 0.' : `Çıkış kodu ${log.code}.`}</p>
              <pre className="orbis-log">{log.log || '(çıktı yok)'}</pre>
            </>
          )}
        </div>
        <div className="x-hints static">
          <Hint action="back">Kapat</Hint>
        </div>
      </div>
    </div>
  )
}
