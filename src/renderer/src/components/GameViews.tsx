import { useEffect, useRef, useState } from 'react'
import { getPlatform, resolveRunner, runnerLabel } from '@shared/platforms'
import type { Game, Screenshot, TrophyGame } from '@shared/types'
import { call, coverUrl, formatDate, formatPlayTime } from '../api'
import { Badge, Icon, type IconName } from '../icons'
import { Hint } from '../glyphs'
import { useInput } from '../input'
import { gameOptions, SUPPORT_LABEL, type MenuCtx } from '../menu'
import { sfx } from '../sound'

function Cover({ game, className }: { game: Game; className?: string }) {
  const p = getPlatform(game.platformId)
  return (
    <div className={`cover-box ${className ?? ''}`}>
      {game.hasCover && !game.coverIsIcon ? (
        <img src={coverUrl(game.id, game.coverRev)} alt="" draggable={false} />
      ) : (
        <div className="cover-fallback">
          {game.hasCover ? <img className="cover-fallback-icon" src={coverUrl(game.id, game.coverRev)} alt="" /> : p && <Badge text={p.short} hue={p.hue} />}
          <div className="cover-fallback-title">{game.title}</div>
        </div>
      )}
    </div>
  )
}

function compatibility(game: Game, ctx: MenuCtx): { text: string; tone: 'ok' | 'warn' | 'bad' } {
  const p = getPlatform(game.platformId)
  if (!p) return { text: 'Bilinmiyor', tone: 'warn' }
  if (game.missing) return { text: 'Oyun dosyası bulunamadı', tone: 'bad' }
  if (p.support === 'unsupported') return { text: 'Bu platform çalıştırılamıyor', tone: 'bad' }
  if (game.lastLaunch?.result === 'crashed' || game.lastLaunch?.result === 'error') {
    return { text: game.lastLaunch.message ?? 'Son açılışta sorun oluştu', tone: 'bad' }
  }
  if (p.id !== 'pc') {
    const runner = resolveRunner(p, ctx.state.settings.platformEmulator[p.id])
    const st = runner && ctx.state.emulators.find((e) => e.id === runner.emulatorId)
    if (!st?.found) return { text: `${runner ? runnerLabel(runner) : 'Emülatör'} ayarlanmadı`, tone: 'warn' }
  }
  if (game.lastLaunch?.result === 'ok') return { text: `Çalıştı · Platform: ${SUPPORT_LABEL[p.support]}`, tone: 'ok' }
  return { text: `Henüz denenmedi · Platform: ${SUPPORT_LABEL[p.support]}`, tone: p.support === 'stable' ? 'ok' : 'warn' }
}

const COMPAT_LABEL: Record<string, string> = {
  playable: 'Oynanabilir',
  ingame: 'Oyun içi (sorunlu)',
  menus: 'Menülere kadar',
  boots: 'Açılıyor, oynanmıyor',
  nothing: 'Açılmıyor'
}

// ── Game detail screen ───────────────────────────────────────────────

export function GameDetail({ gameId, ctx, onClose }: { gameId: string; ctx: MenuCtx; onClose: () => void }) {
  const game = ctx.state.games.find((g) => g.id === gameId)
  const [btn, setBtn] = useState(0)
  const [shots, setShots] = useState<Screenshot[]>([])

  useEffect(() => {
    void call('media:screenshots', gameId).then(setShots)
  }, [gameId, ctx.state.session])

  useEffect(() => {
    if (!game) onClose()
  }, [game, onClose])

  // Game information (description, year, genre…) and trophy progress, looked up once.
  const [loadingInfo, setLoadingInfo] = useState(false)
  const [trophy, setTrophy] = useState<TrophyGame | null>(null)
  useEffect(() => {
    setLoadingInfo(true)
    void call('game:info', gameId)
      .catch(() => null)
      .finally(() => setLoadingInfo(false))
    void call('game:compat', gameId).catch(() => null)
    void call('trophies:games')
      .then((list) => setTrophy(list.find((t) => t.gameId === gameId) ?? null))
      .catch(() => {})
  }, [gameId])

  const buttons: { label: string; icon: IconName; run: () => void }[] = game
    ? [
        { label: 'Başlat', icon: 'play', run: () => ctx.launch(game.id) },
        {
          label: game.favorite ? 'Favorilerden çıkar' : 'Favorilere ekle',
          icon: 'star',
          run: () => ctx.run(() => call('library:favorite', game.id, !game.favorite))
        },
        { label: `Ekran görüntüleri (${shots.length})`, icon: 'camera', run: () => ctx.openScreenshots(game.id) },
        ...(trophy ? [{ label: `Kupalar ${trophy.unlocked}/${trophy.total}`, icon: 'star' as IconName, run: () => ctx.openTrophies(trophy.key) }] : []),
        { label: 'Seçenekler', icon: 'grid', run: () => ctx.openOptions(game.title, gameOptions(game, ctx)) }
      ]
    : []

  useInput((a) => {
    if (a === 'left' && btn > 0) {
      sfx('move')
      setBtn(btn - 1)
    } else if (a === 'right' && btn < buttons.length - 1) {
      sfx('move')
      setBtn(btn + 1)
    } else if (a === 'confirm') {
      sfx('confirm')
      buttons[btn]?.run()
    } else if (a === 'back') {
      sfx('back')
      onClose()
    } else if (a === 'options' && game) {
      sfx('confirm')
      ctx.openOptions(game.title, gameOptions(game, ctx))
    }
  })

  if (!game) return null
  const p = getPlatform(game.platformId)
  const compat = compatibility(game, ctx)
  const m = game.meta
  const facts: [string, string | undefined][] = [
    ['Platform', p?.name],
    ['Yıl', m.year ? String(m.year) : undefined],
    ['Tür', m.genre],
    ['Geliştirici', m.developer],
    ['Yayıncı', m.publisher],
    ['Bölge', m.region],
    ['Title ID', m.titleId],
    ['Oynama süresi', formatPlayTime(game.playTimeSec)],
    ['Son oynanma', formatDate(game.lastPlayedAt)],
    ['Açılma sayısı', String(game.playCount)]
  ]

  return (
    <div className="modal-layer detail">
      {game.hasCover && <div className="detail-bg" style={{ backgroundImage: `url("${coverUrl(game.id, game.coverRev)}")` }} />}
      <div className="detail-body">
        <Cover game={game} className="detail-cover" />
        <div className="detail-info">
          <div className="detail-platform">{p?.name}</div>
          <h1 className="detail-title">
            {game.title}
            {game.favorite && <Icon name="star" className="fav-star" />}
          </h1>
          <div className={`compat ${compat.tone}`}>
            <span className="dot" />
            {compat.text}
          </div>
          {game.compat && (
            <div className={`compat ${game.compat.level === 'playable' ? 'ok' : game.compat.level === 'ingame' ? 'warn' : 'bad'}`}>
              <span className="dot" />
              {`${game.compat.source} uyumluluğu: ${COMPAT_LABEL[game.compat.level] ?? game.compat.label}${game.compat.updated ? ` · ${game.compat.updated}` : ''}`}
            </div>
          )}
          {m.description ? <p className="detail-desc">{m.description}</p> : loadingInfo ? <p className="detail-desc dim">Oyun bilgisi aranıyor…</p> : null}
          <div className="facts">
            {facts
              .filter(([, v]) => v)
              .map(([k, v]) => (
                <div key={k} className="fact">
                  <span>{k}</span>
                  <b>{v}</b>
                </div>
              ))}
          </div>
          <div className="detail-path">{game.path}</div>
          <div className="detail-buttons">
            {buttons.map((b, i) => (
              <div key={b.label} className={`btn ${i === btn ? 'sel' : ''} ${i === 0 ? 'primary' : ''}`}>
                <Icon name={b.icon} />
                {b.label}
              </div>
            ))}
          </div>
          {shots.length > 0 && (
            <div className="detail-shots">
              {shots.slice(0, 5).map((s) => (
                <img key={s.file} src={s.url} alt="" />
              ))}
            </div>
          )}
          {shots.length === 0 && game.onlineShots && game.onlineShots.length > 0 && (
            <div className="detail-shots">
              {game.onlineShots.slice(0, 5).map((u) => (
                <img key={u} src={u} alt="" />
              ))}
            </div>
          )}
        </div>
      </div>
      <div className="x-hints">
        <Hint action="confirm">Seç</Hint>
        <Hint action="back">Geri</Hint>
        <Hint action="options">Seçenekler</Hint>
      </div>
    </div>
  )
}

// ── Cover gallery ────────────────────────────────────────────────────

const COLS = 6

export function Gallery({ title, gameIds, ctx, onClose }: { title: string; gameIds: string[]; ctx: MenuCtx; onClose: () => void }) {
  const games = gameIds.map((id) => ctx.state.games.find((g) => g.id === id)).filter((g): g is Game => Boolean(g))
  const [idx, setIdx] = useState(0)
  const gridRef = useRef<HTMLDivElement>(null)
  const cur = games[Math.min(idx, games.length - 1)]

  useEffect(() => {
    gridRef.current?.querySelector<HTMLElement>(`[data-i="${idx}"]`)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
  }, [idx])

  const move = (d: number) => {
    const n = idx + d
    if (n < 0 || n >= games.length) return
    sfx('move')
    setIdx(n)
  }

  useInput((a) => {
    if (a === 'left') move(-1)
    else if (a === 'right') move(1)
    else if (a === 'up') move(-COLS)
    else if (a === 'down') move(COLS)
    else if (a === 'l2') move(-COLS * 3)
    else if (a === 'r2') move(COLS * 3)
    else if (a === 'confirm' && cur) {
      sfx('confirm')
      ctx.openDetail(cur.id)
    } else if (a === 'options' && cur) {
      sfx('confirm')
      ctx.openOptions(cur.title, gameOptions(cur, ctx))
    } else if (a === 'back' || a === 'view') {
      sfx('back')
      onClose()
    }
  })

  return (
    <div className="modal-layer gallery">
      <div className="gallery-head">
        <div className="gallery-title">{title}</div>
        <div className="gallery-sub">
          {cur ? `${cur.title} · ${getPlatform(cur.platformId)?.name ?? ''}` : 'Oyun yok'} — {games.length ? idx + 1 : 0}/{games.length}
        </div>
      </div>
      <div className="gallery-grid" ref={gridRef} style={{ ['--cols' as string]: COLS }}>
        {games.map((g, i) => (
          <div key={g.id} data-i={i} className={`gallery-cell ${i === idx ? 'sel' : ''}`}>
            <Cover game={g} />
            {g.favorite && <Icon name="star" className="fav-star" />}
          </div>
        ))}
      </div>
      <div className="x-hints">
        <Hint action="confirm">Detay</Hint>
        <Hint action="options">Seçenekler</Hint>
        <Hint action="back">Geri</Hint>
      </div>
    </div>
  )
}

// ── Screenshot viewer ────────────────────────────────────────────────

export function Screenshots({ gameId, ctx, onClose }: { gameId?: string; ctx: MenuCtx; onClose: () => void }) {
  const [shots, setShots] = useState<Screenshot[] | null>(null)
  const [idx, setIdx] = useState(0)
  const [full, setFull] = useState(false)

  const reload = () => void call('media:screenshots', gameId).then(setShots)
  useEffect(reload, [gameId])

  const list = shots ?? []
  const cur = list[Math.min(idx, list.length - 1)]
  const title = gameId ? ctx.state.games.find((g) => g.id === gameId)?.title ?? 'Ekran görüntüleri' : 'Tüm ekran görüntüleri'

  useInput((a) => {
    const move = (d: number) => {
      const n = idx + d
      if (n >= 0 && n < list.length) {
        sfx('move')
        setIdx(n)
      }
    }
    if (a === 'left') move(-1)
    else if (a === 'right') move(1)
    else if (a === 'up' && !full) move(-4)
    else if (a === 'down' && !full) move(4)
    else if (a === 'confirm' && cur) {
      sfx('confirm')
      setFull((f) => !f)
    } else if (a === 'options' && cur) {
      ctx.openOptions('Ekran görüntüsü', [
        {
          label: 'Sil',
          icon: 'trash',
          danger: true,
          onSelect: () =>
            ctx.confirm('Bu ekran görüntüsü silinsin mi?', () =>
              ctx.run(async () => {
                await call('media:deleteScreenshot', cur.file)
                setFull(false)
                reload()
              }, 'Silindi.')
            )
        }
      ])
    } else if (a === 'back') {
      sfx('back')
      if (full) setFull(false)
      else onClose()
    }
  })

  return (
    <div className="modal-layer gallery">
      <div className="gallery-head">
        <div className="gallery-title">{title}</div>
        <div className="gallery-sub">{cur ? `${formatDate(cur.takenAt)} — ${idx + 1}/${list.length}` : ''}</div>
      </div>
      {shots && list.length === 0 && (
        <div className="empty-note">
          <Icon name="camera" />
          Henüz ekran görüntüsü yok. Oyun oynarken menü tuşuna basıp “Ekran görüntüsü al”ı seçin.
        </div>
      )}
      {full && cur ? (
        <div className="shot-full">
          <img src={cur.url} alt="" />
        </div>
      ) : (
        <div className="shot-grid">
          {list.map((s, i) => (
            <div key={s.file} className={`shot-cell ${i === idx ? 'sel' : ''}`} ref={(el) => i === idx && el?.scrollIntoView({ block: 'nearest' })}>
              <img src={s.url} alt="" loading="lazy" />
              {!gameId && <div className="shot-label">{ctx.state.games.find((g) => g.id === s.gameId)?.title}</div>}
            </div>
          ))}
        </div>
      )}
      <div className="x-hints">
        <Hint action="confirm">Büyüt</Hint>
        <Hint action="options">Sil</Hint>
        <Hint action="back">Geri</Hint>
      </div>
    </div>
  )
}
