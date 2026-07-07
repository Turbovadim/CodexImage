import { useEffect, useMemo, useRef, useState } from 'react'
import type { BoardSummary } from '../types.ts'
import { thumbUrl, thumbFallback } from '../media.ts'

function timeAgo(ts: number): string {
  const s = Math.floor((Date.now() - ts) / 1000)
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.floor(s / 60)}m ago`
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`
  return new Date(ts).toLocaleDateString()
}

export function BoardSwitcher(props: {
  boards: BoardSummary[]
  activeBoardId: string | null
  onSelect: (id: string) => void
  onNew: () => void
  onDelete: (id: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const rootRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)

  const active = props.boards.find(b => b.id === props.activeBoardId)
  const anyGenerating = props.boards.some(b => b.generating)

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    return q ? props.boards.filter(b => b.title.toLowerCase().includes(q)) : props.boards
  }, [props.boards, query])

  useEffect(() => {
    if (!open) return
    setQuery('')
    searchRef.current?.focus()
    const onDown = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onDown)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onDown)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  return (
    <div ref={rootRef} className="absolute top-4 left-4 z-30">
      <button
        onClick={() => setOpen(o => !o)}
        title="Switch board"
        className={`flex items-center gap-2.5 rounded-xl border px-3.5 py-2 shadow-[0_8px_30px_rgba(0,0,0,.35)] backdrop-blur-md
          ${open ? 'border-faint bg-hover' : 'border-line bg-raised/95 hover:border-faint'}`}
      >
        <span className="text-[15px] text-accent">❖</span>
        <span className="max-w-[220px] truncate text-[13.5px] font-medium text-ink">
          {active?.title ?? 'CodexImage'}
        </span>
        {anyGenerating && (
          <span className="size-[13px] shrink-0 animate-spin rounded-full border-2 border-line border-t-accent" />
        )}
        <span className={`text-[10px] text-faint transition-transform ${open ? 'rotate-180' : ''}`}>▼</span>
      </button>

      {open && (
        <div className="mt-2 w-[320px] overflow-hidden rounded-2xl border border-line bg-raised/95 shadow-[0_18px_60px_rgba(0,0,0,.55)] backdrop-blur-md">
          <div className="p-2.5 pb-1.5">
            <input
              ref={searchRef}
              value={query}
              onChange={e => setQuery(e.target.value)}
              placeholder="Search boards…"
              className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] text-ink outline-none placeholder:text-faint focus:border-accent"
            />
          </div>

          <div className="max-h-[52vh] overflow-y-auto px-1.5 py-1">
            {filtered.length === 0 && (
              <div className="px-3 py-4 text-center text-[12.5px] text-faint">No boards found</div>
            )}
            {filtered.map(b => (
              <div
                key={b.id}
                onClick={() => { props.onSelect(b.id); setOpen(false) }}
                className={`group flex cursor-pointer items-center gap-2.5 rounded-[10px] px-2.5 py-2
                  ${b.id === props.activeBoardId ? 'bg-hover text-ink' : 'text-dim hover:bg-hover hover:text-ink'}`}
              >
                {b.lastImage ? (
                  <img
                    src={thumbUrl(b.lastImage)}
                    onError={thumbFallback(b.lastImage)}
                    alt=""
                    loading="lazy"
                    className="h-[30px] w-[30px] shrink-0 rounded-md border border-line object-cover"
                  />
                ) : (
                  <div className="flex h-[30px] w-[30px] shrink-0 items-center justify-center rounded-md border border-line bg-bg text-[13px] text-faint">❖</div>
                )}
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13px]">{b.title}</div>
                  <div className="text-[11px] text-faint">
                    {b.imageCount} image{b.imageCount === 1 ? '' : 's'} · {timeAgo(b.updatedAt)}
                  </div>
                </div>
                {b.generating && (
                  <div className="size-[13px] shrink-0 animate-spin rounded-full border-2 border-line border-t-accent" />
                )}
                <button
                  onClick={e => { e.stopPropagation(); props.onDelete(b.id) }}
                  title="Delete board"
                  className="shrink-0 rounded-md p-1 text-[13px] text-faint opacity-0 group-hover:opacity-100 hover:bg-danger/10 hover:text-danger"
                >
                  ✕
                </button>
              </div>
            ))}
          </div>

          <button
            onClick={() => { props.onNew(); setOpen(false) }}
            className="flex w-full items-center gap-2 border-t border-line px-4 py-2.5 text-[13px] font-medium text-accent hover:bg-hover"
          >
            ＋ New board
          </button>
        </div>
      )}
    </div>
  )
}
