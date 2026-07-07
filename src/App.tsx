import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { Board, BoardNode, BoardSummary, ServerEvent } from './types.ts'
import { api } from './api.ts'
import { BoardSwitcher } from './components/BoardSwitcher.tsx'
import { Canvas, type BoardActions } from './components/Canvas.tsx'
import { Composer, type ComposerTarget, type PendingAttachment } from './components/Composer.tsx'
import { Lightbox } from './components/Lightbox.tsx'

const SAMPLES = [
  'A cozy cabin in a snowy forest at dusk, warm light in the windows',
  'Isometric illustration of a tiny home office, pastel palette',
  'Logo concept for a coffee brand called "Ember", minimal, flat',
  'Studio photo of a perfume bottle on black marble, dramatic lighting',
]

export default function App() {
  const [boards, setBoards] = useState<BoardSummary[]>([])
  const [board, setBoard] = useState<Board | null>(null)
  const [draft, setDraft] = useState('')
  const [activity, setActivity] = useState<Record<string, string>>({})
  const [target, setTarget] = useState<ComposerTarget | null>(null)
  const [lightbox, setLightbox] = useState<{ src: string; node: BoardNode } | null>(null)
  const eventSourceRef = useRef<EventSource | null>(null)
  const activeBoardIdRef = useRef<string | null>(null)

  const refreshBoards = useCallback(async () => {
    setBoards(await api.listBoards())
  }, [])

  const connectEvents = useCallback((boardId: string) => {
    eventSourceRef.current?.close()
    const es = new EventSource(`/api/boards/${boardId}/events`)
    eventSourceRef.current = es
    es.onmessage = e => {
      if (activeBoardIdRef.current !== boardId) return
      const ev: ServerEvent = JSON.parse(e.data)
      if (ev.type === 'node') {
        setBoard(prev => {
          if (!prev || prev.id !== boardId) return prev
          const idx = prev.nodes.findIndex(n => n.id === ev.node.id)
          const nodes = idx >= 0
            ? prev.nodes.map((n, i) => (i === idx ? ev.node : n))
            : [...prev.nodes, ev.node]
          return { ...prev, nodes }
        })
      } else if (ev.type === 'nodesDeleted') {
        const gone = new Set(ev.ids)
        setBoard(prev => (prev && prev.id === boardId
          ? { ...prev, nodes: prev.nodes.filter(n => !gone.has(n.id)) }
          : prev))
        setTarget(prev => (prev && gone.has(prev.nodeId) ? null : prev))
      } else if (ev.type === 'activity') {
        setActivity(prev => ({ ...prev, [ev.nodeId]: ev.text }))
      } else if (ev.type === 'done') {
        setActivity(prev => {
          const next = { ...prev }
          delete next[ev.nodeId]
          return next
        })
        void refreshBoards()
      }
    }
  }, [refreshBoards])

  const openBoard = useCallback(async (id: string) => {
    activeBoardIdRef.current = id
    const full = await api.getBoard(id)
    if (activeBoardIdRef.current !== id) return
    setBoard(full)
    setActivity({})
    setTarget(null)
    setLightbox(null)
    connectEvents(id)
  }, [connectEvents])

  const newBoard = useCallback(async () => {
    const created = await api.createBoard()
    await refreshBoards()
    await openBoard(created.id)
  }, [openBoard, refreshBoards])

  const deleteBoard = useCallback(async (id: string) => {
    if (!confirm('Delete this board and its images?')) return
    await api.deleteBoard(id)
    if (activeBoardIdRef.current === id) {
      activeBoardIdRef.current = null
      eventSourceRef.current?.close()
      setBoard(null)
      setTarget(null)
    }
    await refreshBoards()
  }, [refreshBoards])

  const send = useCallback(async (
    text: string,
    options: { aspect: string; count: number },
    attachments: PendingAttachment[],
  ) => {
    let boardId = activeBoardIdRef.current
    if (!boardId) {
      const created = await api.createBoard()
      boardId = created.id
      activeBoardIdRef.current = boardId
      setBoard(created)
      connectEvents(boardId)
    }
    const { nodes } = await api.addNodes(boardId, {
      prompt: text,
      parentId: target?.nodeId ?? null,
      sourceImages: target?.sourceImage ? [target.sourceImage] : undefined,
      aspect: options.aspect,
      count: options.count,
      attachments: attachments.map(a => ({ name: a.name, data: a.data })),
    })
    setBoard(prev => (prev && prev.id === boardId
      ? { ...prev, title: prev.nodes.length ? prev.title : text.slice(0, 60), nodes: [...prev.nodes, ...nodes] }
      : prev))
    setTarget(null)
    void refreshBoards()
  }, [connectEvents, refreshBoards, target])

  const actions = useMemo<BoardActions>(() => ({
    branch: (node, sourceImage) => {
      setTarget({
        nodeId: node.id,
        prompt: node.prompt,
        sourceImage,
        thumb: node.images[node.images.length - 1],
      })
    },
    regenerate: node => {
      const boardId = activeBoardIdRef.current
      if (!boardId) return
      api.regenerateNode(boardId, node.id).catch(err => alert((err as Error).message))
    },
    edit: (node, prompt) => {
      const boardId = activeBoardIdRef.current
      if (!boardId) return
      api.regenerateNode(boardId, node.id, { prompt }).catch(err => alert((err as Error).message))
    },
    stop: node => {
      const boardId = activeBoardIdRef.current
      if (!boardId) return
      void api.stopNode(boardId, node.id)
    },
    remove: node => {
      const boardId = activeBoardIdRef.current
      if (!boardId) return
      if (!confirm('Delete this node and all branches under it?')) return
      api.deleteNode(boardId, node.id).catch(err => alert((err as Error).message))
    },
    openImage: (src, node) => setLightbox({ src, node }),
    move: (nodeId, x, y) => {
      const boardId = activeBoardIdRef.current
      if (!boardId) return
      void api.moveNode(boardId, nodeId, x, y).catch(() => {})
    },
  }), [])

  useEffect(() => {
    void (async () => {
      const list = await api.listBoards()
      setBoards(list)
      if (list.length) await openBoard(list[0].id)
    })()
    const interval = setInterval(refreshBoards, 15000)
    return () => {
      clearInterval(interval)
      eventSourceRef.current?.close()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const empty = !board || board.nodes.length === 0

  return (
    <div className="flex h-screen">
      <main className="relative min-w-0 flex-1">
        <BoardSwitcher
          boards={boards}
          activeBoardId={board?.id ?? null}
          onSelect={openBoard}
          onNew={newBoard}
          onDelete={deleteBoard}
        />
        {board && !empty && (
          <Canvas board={board} activity={activity} targetNodeId={target?.nodeId ?? null} actions={actions} />
        )}

        {empty && (
          <div className="flex h-full flex-col items-center justify-center gap-4 px-6 pb-40 text-center text-dim">
            <div className="text-[42px] text-accent">❖</div>
            <h1 className="text-[26px] font-semibold text-ink">What should we create?</h1>
            <p className="max-w-lg">
              Prompt once — or ×4 for four takes side by side. Every generation becomes a node
              you can branch, continue, and regenerate on an infinite canvas.
            </p>
            <div className="flex max-w-[560px] flex-wrap justify-center gap-2">
              {SAMPLES.map(s => (
                <button
                  key={s}
                  onClick={() => setDraft(s)}
                  className="rounded-full border border-line px-3 py-1.5 text-[12.5px] text-dim hover:border-faint hover:text-ink"
                >
                  {s}
                </button>
              ))}
            </div>
          </div>
        )}

        <Composer
          target={target}
          onClearTarget={() => setTarget(null)}
          draft={draft}
          onDraftChange={setDraft}
          onSend={send}
        />
      </main>
      {lightbox && (
        <Lightbox
          src={lightbox.src}
          onClose={() => setLightbox(null)}
          onBranch={() => actions.branch(lightbox.node, lightbox.src)}
        />
      )}
    </div>
  )
}
