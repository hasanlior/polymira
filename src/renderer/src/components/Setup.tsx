import { useEffect, useState } from 'react'
import { BRAND } from '@shared/brand'
import type { DiscoverResult, Settings } from '@shared/types'
import { call, downloadLine } from '../api'
import { EMULATORS, PLATFORMS } from '@shared/platforms'
import { Icon, type IconName } from '../icons'
import { input, useInput } from '../input'
import type { MenuCtx } from '../menu'
import { sfx } from '../sound'

type Step = 'welcome' | 'pads' | 'scan' | 'results' | 'emulators' | 'optimize' | 'done'
const STEPS: Step[] = ['welcome', 'pads', 'scan', 'results', 'emulators', 'optimize', 'done']

interface Props {
  ctx: MenuCtx
  /** 'setup' = full first-run wizard, 'scan' = only "scan this PC". */
  mode: 'setup' | 'scan'
  onClose: () => void
}

type Toggle = { key: keyof Settings; label: string; desc: string; icon: IconName }

const OPTIMIZE: Toggle[] = [
  { key: 'gmPriority', label: 'Oyuna yüksek işlemci önceliği ver', desc: 'Oyun, arka plandaki programlardan önce işlemci zamanı alır.', icon: 'chip' },
  { key: 'gmPowerPlan', label: 'Oyun sırasında yüksek performans güç planı', desc: 'Dengeli veya Güç tasarrufu planındaysan oyun sırasında Yüksek performansa geçer, oyun bitince geri döner. Zaten performans planındaysan dokunmaz.', icon: 'power' },
  {
    key: 'gmStopExplorer',
    label: 'Oyun sırasında Windows masaüstünü kapat',
    desc: 'Görev çubuğu ve masaüstü (explorer.exe) oyun boyunca kapatılır, oyun bitince geri açılır. Açık Dosya Gezgini pencereleri kapanır.',
    icon: 'monitor'
  },
  { key: 'autoRestart', label: 'Kapanırsa otomatik yeniden başlat', desc: `${BRAND.name} beklenmedik şekilde kapanırsa kendini yeniden açar ve masaüstünü geri getirir.`, icon: 'refresh' },
  { key: 'launchOnStartup', label: 'Windows açılınca başlat', desc: 'Bilgisayar açıldığında doğrudan konsol arayüzü gelir.', icon: 'home' }
]

export function Setup({ ctx, mode, onClose }: Props) {
  const [step, setStep] = useState<Step>(mode === 'scan' ? 'scan' : 'welcome')
  const [progress, setProgress] = useState('Hazırlanıyor…')
  const [result, setResult] = useState<DiscoverResult | null>(null)
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [idx, setIdx] = useState(0)
  const [imported, setImported] = useState<{ games: number; emulators: number; folders: number } | null>(null)
  const [pads, setPads] = useState<string[]>([])
  const [emuPick, setEmuPick] = useState<Set<string>>(new Set())
  const s = ctx.state.settings

  useEffect(() => window.bridge.on('progress', setProgress), [])

  useEffect(() => {
    if (step !== 'pads') return
    const t = setInterval(() => setPads(input.connectedPads().map((p) => p.id.replace(/\s*\(.*\)$/, ''))), 500)
    return () => clearInterval(t)
  }, [step])

  useEffect(() => {
    if (step !== 'scan') return
    let alive = true
    call('discover:run')
      .then((r) => {
        if (!alive) return
        setResult(r)
        setPicked(new Set(r.groups.map((g) => g.key)))
        setIdx(0)
        setStep('results')
      })
      .catch((err: Error) => {
        if (!alive) return
        setProgress(`Tarama başarısız: ${err.message}`)
      })
    return () => {
      alive = false
    }
  }, [step])

  const go = (next: Step) => {
    sfx('confirm')
    setIdx(0)
    setStep(next)
  }

  const finish = () => {
    sfx('confirm')
    if (mode === 'setup') ctx.setSettings({ setupDone: true })
    onClose()
  }

  const importSelected = async () => {
    const r = await call('discover:apply', [...picked])
    setImported(r)
    if (mode === 'setup') {
      // Pre-select emulators for platforms that now have games and no emulator yet.
      const st = await call('state:get')
      const have = new Set(st.emulators.filter((e) => e.found).map((e) => e.id))
      const needed = new Set<string>()
      for (const p of PLATFORMS) {
        if (!st.games.some((g) => g.platformId === p.id)) continue
        const r = p.runners[0]
        if (r && !have.has(r.emulatorId) && st.downloadable.includes(r.emulatorId)) needed.add(r.emulatorId)
      }
      setEmuPick(needed)
      go('emulators')
    } else go('done')
  }

  const groups = result?.groups ?? []
  // Rows on the results screen: one per group, then the action button.
  const resultRows = groups.length + 1
  const optimizeRows = OPTIMIZE.length + 1
  const installed = new Set(ctx.state.emulators.filter((e) => e.found).map((e) => e.id))
  const emuList = ctx.state.downloadable.filter((id) => !installed.has(id))
  const emulatorRows = emuList.length + 1

  useInput((a) => {
    const rows = step === 'results' ? resultRows : step === 'optimize' ? optimizeRows : step === 'emulators' ? emulatorRows : 0
    if (rows && (a === 'up' || a === 'down')) {
      const n = idx + (a === 'up' ? -1 : 1)
      if (n >= 0 && n < rows) {
        sfx('move')
        setIdx(n)
      }
      return
    }
    if (a === 'back') {
      if (step === 'scan') return
      sfx('back')
      if (mode === 'scan' || step === 'welcome') {
        if (mode === 'scan') onClose()
        return
      }
      const order: Step[] = STEPS
      const prev = order[Math.max(0, order.indexOf(step) - 1)]
      setStep(prev === 'scan' ? 'pads' : prev)
      setIdx(0)
      return
    }
    if (a !== 'confirm') return
    switch (step) {
      case 'welcome':
        go('pads')
        break
      case 'pads':
        go('scan')
        break
      case 'results':
        if (idx < groups.length) {
          sfx('move')
          const key = groups[idx].key
          setPicked((p) => {
            const n = new Set(p)
            if (n.has(key)) n.delete(key)
            else n.add(key)
            return n
          })
        } else {
          void importSelected()
        }
        break
      case 'emulators':
        if (idx < emuList.length) {
          sfx('move')
          const id = emuList[idx]
          setEmuPick((p) => {
            const n = new Set(p)
            if (n.has(id)) n.delete(id)
            else n.add(id)
            return n
          })
        } else {
          if (emuPick.size) void call('emu:download', [...emuPick])
          go('optimize')
        }
        break
      case 'optimize':
        if (idx < OPTIMIZE.length) {
          sfx('move')
          const t = OPTIMIZE[idx]
          ctx.setSettings({ [t.key]: !s[t.key] } as Partial<Settings>)
        } else go('done')
        break
      case 'done':
        finish()
        break
    }
  })

  const total = imported ? imported.games : 0

  return (
    <div className="modal-layer setup">
      <div className="setup-card">
        {step === 'welcome' && (
          <div className="setup-hero">
            <div className="setup-welcome">Hoş geldin</div>
            <div className="setup-brand">{BRAND.name}</div>
            <div className="setup-tagline">{BRAND.tagline}</div>
            <p className="setup-text">
              Birkaç adımda bilgisayarındaki oyunları bulup hepsini tek bir konsol arayüzünde toplayacağız. Her şeyi kontrolcüyle yapabilirsin.
            </p>
            <div className="btn primary sel">A/✕ Başla</div>
          </div>
        )}

        {step === 'pads' && (
          <div className="setup-section">
            <h2>Kontrolcü</h2>
            <p className="setup-text">Kontrolcünü bağla ve herhangi bir tuşa bas. Klavye ile de devam edebilirsin (Enter).</p>
            <div className="setup-list">
              {pads.length === 0 && (
                <div className="setup-row">
                  <Icon name="pad" />
                  <span>Henüz kontrolcü algılanmadı</span>
                </div>
              )}
              {pads.map((p, i) => (
                <div key={`${p}-${i}`} className="setup-row on">
                  <Icon name="check" />
                  <span>
                    Kontrolcü {i + 1}: {p}
                  </span>
                </div>
              ))}
            </div>
            <div className="btn primary sel">A/✕ Bilgisayarı tara</div>
          </div>
        )}

        {step === 'scan' && (
          <div className="setup-section center">
            <div className="launch-spinner" />
            <h2>Bilgisayar taranıyor</h2>
            <p className="setup-text">Steam, Epic Games ve GOG kütüphaneleri, kurulu emülatörler ve oyun klasörleri aranıyor.</p>
            <div className="setup-progress">{progress}</div>
          </div>
        )}

        {step === 'results' && (
          <div className="setup-section">
            <h2>Bulunanlar</h2>
            <p className="setup-text">
              {groups.length
                ? 'Eklemek istediklerini seç. A/✕ ile işaretini kaldırabilirsin.'
                : 'Bilinen konumlarda yeni oyun bulunamadı. Oyun klasörlerini daha sonra Ana Sayfa → Oyun ekle bölümünden ekleyebilirsin.'}
            </p>
            <div className="setup-list">
              {groups.map((g, i) => (
                <div key={g.key} className={`setup-row ${i === idx ? 'sel' : ''} ${picked.has(g.key) ? 'on' : ''}`}>
                  {picked.has(g.key) ? <Icon name="check" /> : <span className="x-radio" />}
                  <div className="setup-row-text">
                    <b>
                      {g.label} <span className="setup-count">{g.count}</span>
                    </b>
                    <small>{g.sample.join(' · ')}</small>
                  </div>
                </div>
              ))}
            </div>
            <div className={`btn primary ${idx === groups.length ? 'sel' : ''}`}>{groups.length ? 'Seçilenleri ekle' : 'Devam'}</div>
            {result && <div className="setup-foot">Tarama {Math.max(1, Math.round(result.durationMs / 1000))} sn sürdü · {result.romRoots.length} klasör tarandı</div>}
          </div>
        )}

        {step === 'emulators' && (
          <div className="setup-section">
            <h2>Emülatörler</h2>
            <p className="setup-text">
              İstersen emülatörleri resmi kaynaklarından otomatik indirip kurabilirim. İndirme arka planda sürer; bu sırada konsolda gezinebilir, oyun oynayabilirsin.
              BIOS ve sistem dosyaları indirilmez, onları kendi cihazından sağlaman gerekir.
            </p>
            <div className="setup-list">
              {emuList.length === 0 && (
                <div className="setup-row on">
                  <Icon name="check" />
                  <span>İndirilebilen tüm emülatörler zaten kurulu.</span>
                </div>
              )}
              {emuList.map((id, i) => {
                const emu = EMULATORS.find((e) => e.id === id)
                const job = ctx.state.downloads.find((j) => j.id === id)
                const usedBy = PLATFORMS.filter((p) => p.runners.some((r) => r.emulatorId === id)).map((p) => p.short)
                return (
                  <div key={id} className={`setup-row ${i === idx ? 'sel' : ''} ${emuPick.has(id) ? 'on' : ''}`}>
                    {emuPick.has(id) ? <Icon name="check" /> : <span className="x-radio" />}
                    <div className="setup-row-text">
                      <b>{emu?.name}</b>
                      <small>{job ? downloadLine(job) : usedBy.join(' · ')}</small>
                    </div>
                  </div>
                )
              })}
            </div>
            <div className={`btn primary ${idx === emuList.length ? 'sel' : ''}`}>
              {emuPick.size ? `${emuPick.size} emülatörü arka planda indir` : 'Atla'}
            </div>
          </div>
        )}

        {step === 'optimize' && (
          <div className="setup-section">
            <h2>Oyun modu ve sistem</h2>
            <p className="setup-text">Oyun başladığında bilgisayarı oyuna odaklamak için. Bunları daha sonra Ayarlar → Oyun Modu ve Sistem bölümünden değiştirebilirsin.</p>
            <div className="setup-list">
              {OPTIMIZE.map((t, i) => (
                <div key={t.key} className={`setup-row ${i === idx ? 'sel' : ''} ${s[t.key] ? 'on' : ''}`}>
                  <Icon name={t.icon} />
                  <div className="setup-row-text">
                    <b>{t.label}</b>
                    <small>{t.desc}</small>
                  </div>
                  <span className="setup-value">{s[t.key] ? 'Açık' : 'Kapalı'}</span>
                </div>
              ))}
            </div>
            <div className={`btn primary ${idx === OPTIMIZE.length ? 'sel' : ''}`}>Devam</div>
          </div>
        )}

        {step === 'done' && (
          <div className="setup-hero">
            <Icon name="check" className="setup-done-icon" />
            <h2>Hazır!</h2>
            <p className="setup-text">
              {total ? `${total} oyun kütüphanene eklendi. ` : ''}
              {imported?.emulators ? `${imported.emulators} emülatör ayarlandı. ` : ''}
              {imported?.folders ? `${imported.folders} klasör yeni oyunlar için izlenecek.` : ''}
            </p>
            <div className="btn primary sel">A/✕ {mode === 'setup' ? 'Konsola geç' : 'Tamam'}</div>
          </div>
        )}
      </div>
      {mode === 'setup' && (
        <div className="setup-steps">
          {STEPS.map((st, i) => (
            <span key={st} className={st === step ? 'cur' : i < STEPS.indexOf(step) ? 'done' : ''} />
          ))}
        </div>
      )}
      <div className="x-hints">
        <span>A/✕ Seç</span>
        {step !== 'welcome' && step !== 'scan' && <span>B/○ Geri</span>}
      </div>
    </div>
  )
}
