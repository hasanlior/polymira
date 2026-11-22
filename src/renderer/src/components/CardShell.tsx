import { useEffect, useMemo, useRef, useState, type MutableRefObject } from 'react'
import { coverUrl } from '../api'
import { Hint } from '../glyphs'
import { Icon } from '../icons'
import { useInput, type Action } from '../input'
import type { XCategory, XItem } from '../menu'
import { sfx } from '../sound'
import { ItemIcon, type XmbHandle } from './Xmb'

/**
 * Alternative home screens built on the same menu data as the XMB:
 *  - aurora: tabs on top, a row of square cards, the focused item large below
 *  - tiles:  a grid of large tiles under a big section title
 *  - shelf:  categories in a left rail, a shelf of portrait covers on the right
 */
export type ShellVariant = 'aurora' | 'tiles' | 'shelf'

interface Props {
  variant: ShellVariant
  cats: XCategory[]
  handle: MutableRefObject<XmbHandle | null>
  onFocus: (hue: number, gameId: string | null) => void
}

const TILE_COLS = 5

function Art({ item, big }: { item: XItem; big?: boolean }) {
  const g = item.game
  if (g?.hasCover) return <img className="card-cover" src={item.cover ?? coverUrl(g.id, g.coverRev)} alt="" draggable={false} />
  if (item.cover) return <img className="card-cover" src={item.cover} alt="" draggable={false} />
  return (
    <div className="card-glyph">
      <ItemIcon item={item} big={big} />
    </div>
  )
}

export function CardShell({ variant, cats, handle, onFocus }: Props) {
  const [catId, setCatId] = useState(cats[0]?.id ?? 'home')
  const [path, setPath] = useState<string[]>([])
  const [sel, setSel] = useState<Record<string, number>>({})
  const rowRef = useRef<HTMLDivElement>(null)

  const catIdx = Math.max(0, cats.findIndex((c) => c.id === catId))
  const cat = cats[catIdx]

  const { levels, validPath, trail } = useMemo(() => {
    const lv: XItem[][] = [cat?.items ?? []]
    const ok: string[] = []
    const names: string[] = []
    for (const id of path) {
      const it = lv[lv.length - 1].find((x) => x.id === id)
      if (!it?.children) break
      lv.push(it.children)
      ok.push(id)
      names.push(it.title)
    }
    return { levels: lv, validPath: ok, trail: names }
  }, [cat, path])

  const depth = validPath.length
  const list = levels[depth]
  const key = [cat?.id, ...validPath].join('/')
  const idx = Math.min(Math.max(0, sel[key] ?? 0), Math.max(0, list.length - 1))
  const item: XItem | undefined = list[idx]

  useEffect(() => {
    if (validPath.length !== path.length) setPath(validPath)
  }, [validPath, path])

  const hue = item?.hue ?? cat?.hue ?? 220
  const focusedGameId = item?.game?.id ?? null
  useEffect(() => onFocus(hue, focusedGameId), [hue, focusedGameId, onFocus])

  useEffect(() => {
    rowRef.current?.querySelector('.card.sel')?.scrollIntoView({ block: 'nearest', inline: 'center', behavior: 'smooth' })
  }, [idx, key])

  const setIdx = (n: number) => setSel((s) => ({ ...s, [key]: n }))
  const back = () => {
    if (depth > 0) {
      sfx('back')
      setPath(validPath.slice(0, -1))
    }
  }
  handle.current = { back }

  const switchCat = (dir: number) => {
    const n = catIdx + dir
    if (n < 0 || n >= cats.length) return
    sfx('category')
    setCatId(cats[n].id)
    setPath([])
  }
  const move = (d: number) => {
    const n = idx + d
    if (n < 0 || n >= list.length) return
    sfx('move')
    setIdx(n)
  }
  const viewTarget = (): (() => void) | undefined => {
    if (item?.onView) return item.onView
    for (let d = depth - 1; d >= 0; d--) {
      const parentKey = [cat?.id, ...validPath.slice(0, d)].join('/')
      const it = levels[d][Math.max(0, sel[parentKey] ?? 0)]
      if (it?.onView) return it.onView
    }
    return undefined
  }

  useInput((a) => {
    const grid = variant === 'tiles'
    switch (a) {
      case 'left':
        move(-1)
        break
      case 'right':
        move(1)
        break
      case 'up':
        if (grid && idx >= TILE_COLS) move(-TILE_COLS)
        else if (depth > 0) back()
        else switchCat(-1)
        break
      case 'down':
        if (grid && idx + TILE_COLS < list.length) move(TILE_COLS)
        else if (!grid) switchCat(1)
        break
      case 'l1':
        switchCat(-1)
        break
      case 'r1':
        switchCat(1)
        break
      case 'confirm':
        if (!item) return
        if (item.children) {
          sfx('confirm')
          setPath([...validPath, item.id])
        } else if (item.onActivate) {
          sfx('confirm')
          item.onActivate()
        } else sfx('error')
        break
      case 'back':
        back()
        break
      case 'options':
        if (item?.onOptions) {
          sfx('confirm')
          item.onOptions()
        }
        break
      case 'view': {
        const v = viewTarget()
        if (v) {
          sfx('confirm')
          v()
        }
        break
      }
      case 'l2':
        move(-Math.min(idx, 8))
        break
      case 'r2':
        move(Math.min(list.length - 1 - idx, 8))
        break
    }
  })

  const hints: [Action, string][] = []
  if (item?.children || item?.onActivate) hints.push(['confirm', 'Seç'])
  if (depth > 0) hints.push(['back', 'Geri'])
  if (item?.onOptions) hints.push(['options', 'Seçenekler'])
  if (viewTarget()) hints.push(['view', 'Galeri'])

  const heading = depth > 0 ? trail[trail.length - 1] : cat?.title
  const cards = (
    <div className={`cards cards-${variant}`} ref={rowRef}>
      {list.length === 0 && <div className="cards-empty">Burada henüz bir şey yok.</div>}
      {list.map((it, i) => (
        <div key={it.id} className={`card ${i === idx ? 'sel' : ''} ${it.game ? 'is-game' : ''}`}>
          <div className="card-art">
            <Art item={it} big={i === idx} />
            {it.value && <span className="card-value">{it.value}</span>}
          </div>
          <div className="card-name">{it.title}</div>
        </div>
      ))}
    </div>
  )

  return (
    <div className={`shell shell-${variant}`}>
      {variant === 'shelf' ? (
        <aside className="rail">
          {cats.map((c, i) => (
            <div key={c.id} className={`rail-item ${i === catIdx ? 'sel' : ''}`}>
              <Icon name={c.icon} />
              <span>{c.title}</span>
            </div>
          ))}
        </aside>
      ) : (
        <div className="tabs">
          {cats.map((c, i) => (
            <div key={c.id} className={`tab ${i === catIdx ? 'sel' : ''}`}>
              {variant === 'tiles' && <Icon name={c.icon} />}
              <span>{c.title}</span>
            </div>
          ))}
        </div>
      )}

      <main className="shell-main">
        <div className="shell-heading">
          {depth > 0 && <div className="shell-trail">{[cat?.title, ...trail.slice(0, -1)].join(' / ')}</div>}
          <h1>{heading}</h1>
        </div>
        {cards}
        {item && (
          <div className="shell-detail">
            <div className="shell-title">{item.title}</div>
            {item.subtitle && <div className="shell-sub">{item.subtitle}</div>}
          </div>
        )}
      </main>

      <div className="x-hints">
        {hints.map(([a, t]) => (
          <Hint key={a} action={a}>
            {t}
          </Hint>
        ))}
      </div>
    </div>
  )
}
