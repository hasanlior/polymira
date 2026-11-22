import { useEffect, useRef, useState } from 'react'
import type { DirEntry, Toast } from '@shared/types'
import { call } from '../api'
import { Icon } from '../icons'
import { useInput } from '../input'
import type { BrowseRequest, OptItem } from '../menu'
import { sfx } from '../sound'

function useScrollIntoView(index: number) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = ref.current?.querySelector<HTMLElement>(`[data-i="${index}"]`)
    el?.scrollIntoView({ block: 'nearest' })
  }, [index])
  return ref
}

// ── Options side panel (PS3-style, slides in from the right) ─────────

export function OptionsPanel({ title, items, onClose }: { title: string; items: OptItem[]; onClose: () => void }) {
  const [stack, setStack] = useState<{ title: string; items: OptItem[]; idx: number }[]>([{ title, items, idx: 0 }])
  const top = stack[stack.length - 1]
  const listRef = useScrollIntoView(top.idx)

  const setIdx = (idx: number) => setStack((s) => [...s.slice(0, -1), { ...s[s.length - 1], idx }])

  useInput((a) => {
    if (a === 'up' && top.idx > 0) {
      sfx('move')
      setIdx(top.idx - 1)
    } else if (a === 'down' && top.idx < top.items.length - 1) {
      sfx('move')
      setIdx(top.idx + 1)
    } else if (a === 'confirm' || a === 'right') {
      const it = top.items[top.idx]
      if (!it) return
      if (it.children) {
        sfx('confirm')
        setStack((s) => [...s, { title: it.label, items: it.children!, idx: 0 }])
      } else if (a === 'confirm' && it.onSelect) {
        sfx('confirm')
        onClose()
        it.onSelect()
      }
    } else if (a === 'back' || a === 'left' || a === 'options') {
      sfx('back')
      if (stack.length > 1 && a !== 'options') setStack((s) => s.slice(0, -1))
      else onClose()
    }
  })

  return (
    <div className="modal-layer">
      <div className="opt-panel">
        <div className="opt-title">{top.title}</div>
        <div className="opt-list" ref={listRef}>
          {top.items.map((it, i) => (
            <div key={`${it.label}-${i}`} data-i={i} className={`opt-item ${i === top.idx ? 'sel' : ''} ${it.danger ? 'danger' : ''}`}>
              {it.icon && <Icon name={it.icon} />}
              <span>{it.label}</span>
              {it.children && <span className="opt-more">›</span>}
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}

// ── Yes / No dialog ──────────────────────────────────────────────────

export function ConfirmDialog({ text, onYes, onClose }: { text: string; onYes: () => void; onClose: () => void }) {
  const [yes, setYes] = useState(false)
  useInput((a) => {
    if (a === 'left' || a === 'right' || a === 'up' || a === 'down') {
      sfx('move')
      setYes((v) => !v)
    } else if (a === 'confirm') {
      sfx(yes ? 'confirm' : 'back')
      onClose()
      if (yes) onYes()
    } else if (a === 'back') {
      sfx('back')
      onClose()
    }
  })
  return (
    <div className="modal-layer center">
      <div className="dialog">
        <div className="dialog-text">{text}</div>
        <div className="dialog-buttons">
          <div className={`btn ${yes ? 'sel' : ''}`}>Evet</div>
          <div className={`btn ${!yes ? 'sel' : ''}`}>Hayır</div>
        </div>
      </div>
    </div>
  )
}

// ── Controller-friendly file / folder picker ─────────────────────────

type Row = { kind: 'pick-here' | 'system' | 'up' | 'entry'; label: string; entry?: DirEntry }

export function FileBrowser({ req, onClose }: { req: BrowseRequest; onClose: () => void }) {
  const storageKey = `browse:${req.mode}`
  const [dir, setDir] = useState<string | null>(() => localStorage.getItem(storageKey))
  const [data, setData] = useState<{ path: string | null; parent: string | null; entries: DirEntry[] } | null>(null)
  const [idx, setIdx] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const listRef = useScrollIntoView(idx)

  useEffect(() => {
    let alive = true
    call('fs:list', dir)
      .then((d) => {
        if (!alive) return
        setData(d)
        setError(null)
        setIdx(0)
        if (d.path) localStorage.setItem(storageKey, d.path)
      })
      .catch((err) => {
        if (!alive) return
        setError((err as Error).message)
        if (dir) setDir(null)
      })
    return () => {
      alive = false
    }
  }, [dir, storageKey])

  const exts = req.extensions?.map((e) => e.toLowerCase())
  const rows: Row[] = []
  rows.push({ kind: 'system', label: 'Sistem penceresiyle seç…' })
  if (data?.path && req.mode === 'folder') rows.push({ kind: 'pick-here', label: 'Bu klasörü seç' })
  if (data?.path) rows.push({ kind: 'up', label: '..' })
  for (const e of data?.entries ?? []) {
    if (!e.isDir) {
      if (req.mode === 'folder') continue
      if (exts && !exts.some((x) => e.name.toLowerCase().endsWith(x))) continue
    }
    rows.push({ kind: 'entry', label: e.name, entry: e })
  }

  const pick = (p: string) => {
    onClose()
    req.onPick(p)
  }

  const goUp = () => setDir(data?.parent ?? null)

  useInput((a) => {
    if (a === 'up' && idx > 0) {
      sfx('move')
      setIdx(idx - 1)
    } else if (a === 'down' && idx < rows.length - 1) {
      sfx('move')
      setIdx(idx + 1)
    } else if (a === 'l2') setIdx(Math.max(0, idx - 10))
    else if (a === 'r2') setIdx(Math.min(rows.length - 1, idx + 10))
    else if (a === 'confirm' || a === 'right') {
      const r = rows[idx]
      if (!r) return
      sfx('confirm')
      if (r.kind === 'system') {
        if (a !== 'confirm') return
        void (req.mode === 'folder' ? call('dialog:folder') : call('dialog:file', req.extensions?.includes('.exe') ? 'exe' : req.extensions?.includes('.png') ? 'image' : 'any')).then(
          (p) => p && pick(p)
        )
      } else if (r.kind === 'pick-here' && data?.path) {
        if (a === 'confirm') pick(data.path)
      } else if (r.kind === 'up') goUp()
      else if (r.entry?.isDir) setDir(r.entry.path)
      else if (r.entry && a === 'confirm') pick(r.entry.path)
    } else if (a === 'back' || a === 'left') {
      sfx('back')
      if (data?.path && a === 'left') goUp()
      else if (data?.path && a === 'back') goUp()
      else onClose()
    } else if (a === 'options') {
      sfx('back')
      onClose()
    }
  })

  return (
    <div className="modal-layer center">
      <div className="browser">
        <div className="browser-head">
          <div className="browser-title">{req.title}</div>
          <div className="browser-path">{data?.path ?? 'Bu bilgisayar'}</div>
        </div>
        {error && <div className="browser-error">{error}</div>}
        <div className="browser-list" ref={listRef}>
          {rows.map((r, i) => (
            <div key={`${r.kind}-${r.label}-${i}`} data-i={i} className={`browser-row ${i === idx ? 'sel' : ''} ${r.kind}`}>
              <Icon
                name={r.kind === 'system' ? 'search' : r.kind === 'pick-here' ? 'check' : r.kind === 'up' ? 'up' : r.entry?.isDir ? (data?.path ? 'folder' : 'drive') : 'file'}
              />
              <span>{r.label}</span>
            </div>
          ))}
        </div>
        <div className="x-hints static">
          <span>A/✕ Aç / Seç</span>
          <span>B/○ Üst klasör</span>
          <span>△ Kapat</span>
        </div>
      </div>
    </div>
  )
}

// ── Toasts ───────────────────────────────────────────────────────────

export function Toasts({ toasts }: { toasts: (Toast & { id: number })[] }) {
  return (
    <div className="toasts">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`}>
          <Icon name={t.kind === 'error' ? 'warning' : t.kind === 'success' ? 'check' : 'info'} />
          <span>{t.text}</span>
        </div>
      ))}
    </div>
  )
}
