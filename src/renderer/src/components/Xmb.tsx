import { useEffect, useMemo, useState, type MutableRefObject } from 'react'
import { Badge, Icon } from '../icons'
import { Hint } from '../glyphs'
import { useInput, type Action } from '../input'
import type { XCategory, XItem } from '../menu'
import { sfx } from '../sound'

export interface XmbHandle {
  back: () => void
}

interface Props {
  cats: XCategory[]
  handle: MutableRefObject<XmbHandle | null>
  onFocus: (hue: number, gameId: string | null) => void
}

const ROW = 8.2 // vertical distance between unselected rows (u)
const BAR_Y = 24 // category row top (u)
const SEL_Y = 37 // selected item row (u)
const WINDOW = 9 // rows rendered above/below the selection

export function ItemIcon({ item, big }: { item: XItem; big?: boolean }) {
  if (item.tile) {
    return (
      <div className={`x-icon app-tile ${big ? 'big' : ''}`} style={{ ['--h' as string]: item.hue ?? 210 }}>
        <img src={item.tile} alt="" draggable={false} />
      </div>
    )
  }
  if (item.cover) {
    return (
      <div className={`x-icon cover ${big ? 'big' : ''} ${item.coverIsIcon ? 'icon-only' : ''}`}>
        <img src={item.cover} alt="" draggable={false} />
      </div>
    )
  }
  if (item.badge) {
    return (
      <div className={`x-icon ${big ? 'big' : ''}`}>
        <Badge text={item.badge.text} hue={item.badge.hue} />
      </div>
    )
  }
  if (item.checked !== undefined) {
    return (
      <div className={`x-icon ${big ? 'big' : ''}`}>
        {item.checked ? <Icon name="check" /> : <span className="x-radio" />}
      </div>
    )
  }
  return (
    <div className={`x-icon ${big ? 'big' : ''}`}>
      <Icon name={item.icon} />
    </div>
  )
}

export function Xmb({ cats, handle, onFocus }: Props) {
  const [catId, setCatId] = useState(cats[0]?.id ?? 'home')
  const [path, setPath] = useState<string[]>([])
  const [sel, setSel] = useState<Record<string, number>>({})

  const catIdx = Math.max(0, cats.findIndex((c) => c.id === catId))
  const cat = cats[catIdx]

  // Resolve the folder path into levels; drop path parts that no longer exist.
  const { levels, validPath } = useMemo(() => {
    const lv: XItem[][] = [cat?.items ?? []]
    const ok: string[] = []
    for (const id of path) {
      const it = lv[lv.length - 1].find((x) => x.id === id)
      if (!it?.children) break
      lv.push(it.children)
      ok.push(id)
    }
    return { levels: lv, validPath: ok }
  }, [cat, path])

  const depth = validPath.length
  const list = levels[depth]
  const key = (d: number) => [cat?.id, ...validPath.slice(0, d)].join('/')
  const idxAt = (d: number) => Math.min(Math.max(0, sel[key(d)] ?? 0), Math.max(0, levels[d].length - 1))
  const idx = idxAt(depth)
  const item: XItem | undefined = list[idx]
  const parent: XItem | undefined = depth > 0 ? levels[depth - 1][idxAt(depth - 1)] : undefined

  useEffect(() => {
    if (validPath.length !== path.length) setPath(validPath)
  }, [validPath, path])

  const hue = item?.hue ?? parent?.hue ?? cat?.hue ?? 220
  const focusedGameId = item?.game?.id ?? null
  useEffect(() => onFocus(hue, focusedGameId), [hue, focusedGameId, onFocus])

  const setIdx = (n: number) => setSel((s) => ({ ...s, [key(depth)]: n }))

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

  const viewTarget = (): (() => void) | undefined => {
    if (item?.onView) return item.onView
    for (let d = depth - 1; d >= 0; d--) {
      const it = levels[d][idxAt(d)]
      if (it?.onView) return it.onView
    }
    return undefined
  }

  useInput((a) => {
    switch (a) {
      case 'up':
        if (idx > 0) {
          sfx('move')
          setIdx(idx - 1)
        }
        break
      case 'down':
        if (idx < list.length - 1) {
          sfx('move')
          setIdx(idx + 1)
        }
        break
      case 'left':
        if (depth > 0) back()
        else switchCat(-1)
        break
      case 'right':
        if (depth === 0) switchCat(1)
        else if (item?.children) {
          sfx('confirm')
          setPath([...validPath, item.id])
        }
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
        } else {
          sfx('error')
        }
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
        if (list.length) setIdx(Math.max(0, idx - 8))
        break
      case 'r2':
        if (list.length) setIdx(Math.min(list.length - 1, idx + 8))
        break
    }
  })

  // Vertical position of row `o` (offset from selection) in u.
  const rowY = (o: number) => (o === 0 ? SEL_Y : o < 0 ? BAR_Y - 2 + o * ROW : SEL_Y + 6 + o * ROW)

  const hints: [Action, string][] = []
  if (item?.children || item?.onActivate) hints.push(['confirm', 'Seç'])
  if (depth > 0) hints.push(['back', 'Geri'])
  if (item?.onOptions) hints.push(['options', 'Seçenekler'])
  if (viewTarget()) hints.push(['view', 'Galeri'])

  return (
    <div className={`xmb depth-${Math.min(depth, 2)}`}>
      <div className="x-cats">
        {cats.map((c, i) => {
          const o = i - catIdx
          return (
            <div
              key={c.id}
              className={`x-cat ${o === 0 ? 'sel' : ''}`}
              style={{ transform: `translate(calc(${o === 0 ? 0 : o * 15 + (o > 0 ? 3 : -3)} * var(--u)), calc(${BAR_Y} * var(--u)))` }}
            >
              <Icon name={c.icon} />
              <div className="x-cat-label">{c.title}</div>
            </div>
          )
        })}
      </div>

      {parent && (
        <div className="x-crumb" style={{ transform: `translate(0, calc(${SEL_Y} * var(--u)))` }}>
          <ItemIcon item={parent} big />
          <div className="x-crumb-title">{parent.title}</div>
        </div>
      )}

      <div className="x-col" key={key(depth)}>
        {list.length === 0 && (
          <div className="x-item sel empty" style={{ transform: `translate(0, calc(${SEL_Y} * var(--u)))` }}>
            <div className="x-icon big">
              <Icon name="info" />
            </div>
            <div className="x-text">
              <div className="x-title">{parent?.emptyText ?? 'Burada henüz bir şey yok.'}</div>
            </div>
          </div>
        )}
        {list.map((it, i) => {
          const o = i - idx
          if (Math.abs(o) > WINDOW) return null
          const selected = o === 0
          return (
            <div
              key={it.id}
              className={`x-item ${selected ? 'sel' : ''} ${o < 0 ? 'above' : ''}`}
              style={{ transform: `translate(0, calc(${rowY(o)} * var(--u)))` }}
            >
              <ItemIcon item={it} big={selected} />
              <div className="x-text">
                <div className="x-title">
                  <span>{it.title}</span>
                  {it.value && <span className="x-value">{it.value}</span>}
                </div>
                {selected && it.subtitle && <div className="x-sub">{it.subtitle}</div>}
              </div>
            </div>
          )
        })}
      </div>

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
