import { useEffect, useRef, useState, type ReactNode } from 'react'
import { call } from '../api'
import { Hint } from '../glyphs'
import { Icon } from '../icons'
import { useInput } from '../input'
import { sfx } from '../sound'

/** Inline Markdown subset used in release notes: **bold** and `code`. */
function inline(text: string): ReactNode[] {
  return text.split(/(\*\*[^*]+\*\*|`[^`]+`)/g).map((part, i) =>
    part.startsWith('**') ? <b key={i}>{part.slice(2, -2)}</b> : part.startsWith('`') ? <code key={i}>{part.slice(1, -1)}</code> : part
  )
}

/** Renders release notes (headings, bullet lists, paragraphs). */
function Notes({ md }: { md: string }) {
  const out: ReactNode[] = []
  let list: ReactNode[] = []
  const flush = () => {
    if (list.length) out.push(<ul key={`u${out.length}`}>{list}</ul>)
    list = []
  }
  md.split(/\r?\n/).forEach((raw, i) => {
    const line = raw.trim()
    if (!line) return flush()
    const h = line.match(/^#{1,4}\s+(.*)$/)
    const li = line.match(/^[-*]\s+(.*)$/)
    if (h) {
      flush()
      out.push(<h3 key={i}>{inline(h[1])}</h3>)
    } else if (li) list.push(<li key={i}>{inline(li[1])}</li>)
    else {
      flush()
      out.push(<p key={i}>{inline(line)}</p>)
    }
  })
  flush()
  return <>{out}</>
}

/** "What's new": the release notes of the installed version, from GitHub Releases. */
export function WhatsNew({ onClose }: { onClose: () => void }) {
  const [notes, setNotes] = useState<{ version: string; body: string } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const box = useRef<HTMLDivElement>(null)
  useEffect(() => {
    void call('app:notes')
      .then(setNotes)
      .catch((e: Error) => setError(e.message))
  }, [])
  useInput((a) => {
    if (a === 'down' || a === 'up') box.current?.scrollBy({ top: a === 'down' ? 120 : -120, behavior: 'smooth' })
    else if (a === 'back' || a === 'confirm') {
      sfx('back')
      onClose()
    }
  })
  return (
    <div className="modal-layer center">
      <div className="whatsnew">
        <div className="stats-head">
          <Icon name="info" /> {notes ? `Yenilikler · v${notes.version}` : 'Yenilikler'}
        </div>
        <div className="whatsnew-body" ref={box}>
          {error ? <p>{error}</p> : notes ? <Notes md={notes.body} /> : <p>Yükleniyor…</p>}
        </div>
        <div className="x-hints static">
          <Hint action="back">Kapat</Hint>
        </div>
      </div>
    </div>
  )
}
