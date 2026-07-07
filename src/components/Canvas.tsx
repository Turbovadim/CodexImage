import { createContext, memo, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react'
import {
  ReactFlow, ReactFlowProvider, Background, BackgroundVariant, MiniMap,
  Handle, Position, applyNodeChanges, useReactFlow,
  type Node as RFNode, type Edge as RFEdge, type NodeChange, type NodeProps,
} from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import type { Board, BoardNode } from '../types.ts'
import { thumbUrl, thumbFallback } from '../media.ts'

export interface BoardActions {
  branch: (node: BoardNode, sourceImage?: string) => void
  regenerate: (node: BoardNode) => void
  /** update the prompt and regenerate in a fresh session */
  edit: (node: BoardNode, prompt: string) => void
  stop: (node: BoardNode) => void
  remove: (node: BoardNode) => void
  openImage: (src: string, node: BoardNode) => void
  move: (nodeId: string, x: number, y: number) => void
}

const ActionsContext = createContext<BoardActions | null>(null)

type GenNodeType = RFNode<{ bn: BoardNode; activity: string; isTarget: boolean }, 'gen'>

// ---------------------------------------------------------------------------
// Auto layout: tidy tree per root, roots side by side. Level Y positions use
// the tallest measured card on each level; nodes the user dragged keep their
// manual position.
// ---------------------------------------------------------------------------

const CARD_W = 340
const H_GAP = 64
const V_GAP = 96
const ROOT_GAP = 140
const EST_H = 460

function computeLayout(bns: BoardNode[], heights: Map<string, number>): Map<string, { x: number; y: number }> {
  const ids = new Set(bns.map(n => n.id))
  const children = new Map<string, BoardNode[]>()
  const roots: BoardNode[] = []
  const sorted = [...bns].sort((a, b) => a.createdAt - b.createdAt || a.id.localeCompare(b.id))
  for (const n of sorted) {
    if (n.parentId && ids.has(n.parentId)) {
      const list = children.get(n.parentId) || []
      list.push(n)
      children.set(n.parentId, list)
    } else {
      roots.push(n)
    }
  }

  const depth = new Map<string, number>()
  const levelMax: number[] = []
  const assignDepth = (n: BoardNode, d: number) => {
    depth.set(n.id, d)
    levelMax[d] = Math.max(levelMax[d] || 0, heights.get(n.id) || EST_H)
    for (const c of children.get(n.id) || []) assignDepth(c, d + 1)
  }
  for (const r of roots) assignDepth(r, 0)

  const levelY: number[] = []
  let y = 0
  for (let d = 0; d < levelMax.length; d++) {
    levelY[d] = y
    y += (levelMax[d] || EST_H) + V_GAP
  }

  const widths = new Map<string, number>()
  const subtreeWidth = (n: BoardNode): number => {
    const kids = children.get(n.id) || []
    const w = kids.length
      ? Math.max(CARD_W, kids.reduce((s, k) => s + subtreeWidth(k), 0) + H_GAP * (kids.length - 1))
      : CARD_W
    widths.set(n.id, w)
    return w
  }

  const pos = new Map<string, { x: number; y: number }>()
  const place = (n: BoardNode, x0: number) => {
    const w = widths.get(n.id)!
    pos.set(n.id, { x: x0 + w / 2 - CARD_W / 2, y: levelY[depth.get(n.id)!] })
    const kids = children.get(n.id) || []
    const kidsW = kids.reduce((s, k) => s + widths.get(k.id)!, 0) + H_GAP * Math.max(0, kids.length - 1)
    let cx = x0 + (w - kidsW) / 2
    for (const k of kids) {
      place(k, cx)
      cx += widths.get(k.id)! + H_GAP
    }
  }
  for (const r of roots) subtreeWidth(r)
  let rootX = 0
  for (const r of roots) {
    place(r, rootX)
    rootX += widths.get(r.id)! + ROOT_GAP
  }
  return pos
}

// ---------------------------------------------------------------------------
// Node card
// ---------------------------------------------------------------------------

function Elapsed({ since }: { since: number }) {
  const [, force] = useState(0)
  useEffect(() => {
    const t = setInterval(() => force(n => n + 1), 1000)
    return () => clearInterval(t)
  }, [])
  return <>{Math.max(0, Math.round((Date.now() - since) / 1000))}s</>
}

function ToolButton(props: { title: string; onClick: () => void; danger?: boolean; children: React.ReactNode }) {
  return (
    <button
      title={props.title}
      onClick={e => { e.stopPropagation(); props.onClick() }}
      className={`nodrag flex h-7 min-w-7 items-center justify-center rounded-lg border border-line bg-raised px-1.5
        text-[12.5px] text-dim shadow-md transition-colors
        ${props.danger ? 'hover:border-danger/50 hover:text-danger' : 'hover:border-faint hover:text-ink'}`}
    >
      {props.children}
    </button>
  )
}

const GenCard = memo(function GenCard({ data }: NodeProps<GenNodeType>) {
  const { bn, activity, isTarget } = data
  const actions = useContext(ActionsContext)!
  const running = bn.status === 'running'
  const date = new Date(bn.createdAt).toLocaleDateString(undefined, { day: '2-digit', month: '2-digit', year: '2-digit' })
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')

  const startEdit = () => {
    setDraft(bn.prompt)
    setEditing(true)
  }
  const [copied, setCopied] = useState(false)
  const copyPrompt = () => {
    navigator.clipboard.writeText(bn.prompt).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1200)
    }).catch(() => {})
  }
  const saveEdit = () => {
    const next = draft.trim()
    setEditing(false)
    if (next && next !== bn.prompt) actions.edit(bn, next)
  }

  return (
    <div className="group relative" style={{ width: CARD_W }}>
      <Handle type="target" position={Position.Top} className="!pointer-events-none !top-0 !opacity-0" />
      <Handle type="source" position={Position.Bottom} className="!pointer-events-none !bottom-0 !opacity-0" />

      {/* hover toolbar */}
      <div className="absolute -top-9 right-1 z-10 flex gap-1 opacity-0 transition-opacity group-hover:opacity-100">
        {running ? (
          <ToolButton title="Stop generation" onClick={() => actions.stop(bn)} danger>◼ Stop</ToolButton>
        ) : (
          <>
            <ToolButton title="Branch from this node" onClick={() => actions.branch(bn)}>＋ Branch</ToolButton>
            <ToolButton title="Edit prompt & regenerate" onClick={startEdit}>✎</ToolButton>
            <ToolButton title="Regenerate (new session, same prompt)" onClick={() => actions.regenerate(bn)}>↻</ToolButton>
          </>
        )}
        <ToolButton title="Copy prompt" onClick={copyPrompt}>{copied ? '✓' : '⧉'}</ToolButton>
        {bn.images.length > 0 && (
          <a
            href={bn.images[bn.images.length - 1]}
            download
            onClick={e => e.stopPropagation()}
            title="Download image"
            className="nodrag flex h-7 min-w-7 items-center justify-center rounded-lg border border-line bg-raised px-1.5 text-[12.5px] text-dim shadow-md hover:border-faint hover:text-ink"
          >
            ⬇
          </a>
        )}
        <ToolButton title="Delete node and its branches" onClick={() => actions.remove(bn)} danger>✕</ToolButton>
      </div>

      <div
        className={`overflow-hidden rounded-[20px] border bg-raised shadow-[0_10px_36px_rgba(0,0,0,.4)] transition-[border-color,box-shadow]
          ${isTarget ? 'border-accent shadow-[0_0_0_3px_rgba(124,140,255,.18),0_10px_36px_rgba(0,0,0,.4)]' : 'border-line'}`}
      >
        {/* prompt */}
        <div className="px-4 pt-3.5 pb-3">
          {editing ? (
            <div>
              <textarea
                autoFocus
                value={draft}
                rows={5}
                onChange={e => setDraft(e.target.value)}
                onKeyDown={e => {
                  e.stopPropagation()
                  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); saveEdit() }
                  if (e.key === 'Escape') setEditing(false)
                }}
                className="nodrag nowheel w-full resize-none rounded-lg border border-line bg-bg px-2.5 py-2
                  text-[12.5px] leading-relaxed text-ink outline-none focus:border-accent"
              />
              <div className="mt-1.5 flex justify-end gap-1.5">
                <button
                  onClick={e => { e.stopPropagation(); setEditing(false) }}
                  className="nodrag rounded-lg border border-line px-2.5 py-1 text-[11.5px] text-dim hover:border-faint hover:text-ink"
                >
                  Cancel
                </button>
                <button
                  onClick={e => { e.stopPropagation(); saveEdit() }}
                  className="nodrag rounded-lg border border-accent-strong bg-accent-strong px-2.5 py-1 text-[11.5px] font-medium text-white hover:bg-accent"
                >
                  ↻ Save & regenerate
                </button>
              </div>
            </div>
          ) : (
            <div className="nodrag line-clamp-6 cursor-text text-[12.5px] leading-relaxed break-words whitespace-pre-wrap text-ink/90 select-text">
              {bn.prompt}
            </div>
          )}
          {bn.attachments.length > 0 && (
            <div className="mt-2 flex gap-1.5">
              {bn.attachments.map(src => (
                <img
                  key={src}
                  src={thumbUrl(src)}
                  onError={thumbFallback(src)}
                  alt=""
                  loading="lazy"
                  decoding="async"
                  className="h-9 w-9 rounded-md border border-line object-cover"
                />
              ))}
            </div>
          )}
          <div className="mt-2.5 flex items-center gap-2 text-[10.5px] text-faint">
            <span className="flex items-center gap-1"><span className="text-accent">❖</span> Me</span>
            {bn.aspect !== 'auto' && (
              <span className="rounded-full border border-line px-1.5 py-px">{bn.aspect}</span>
            )}
            <span className="ml-auto">{date}</span>
          </div>
        </div>

        <div className="border-t border-dashed border-line/80" />

        {/* result */}
        {bn.images.length > 0 && (
          <div className={bn.images.length > 1 ? 'grid grid-cols-2 gap-px bg-line/50' : ''}>
            {bn.images.map(src => (
              <img
                key={src}
                src={thumbUrl(src)}
                onError={thumbFallback(src)}
                alt=""
                loading="lazy"
                decoding="async"
                onClick={() => actions.openImage(src, bn)}
                className="nodrag block w-full cursor-zoom-in"
                draggable={false}
              />
            ))}
          </div>
        )}

        {running && (
          <div className="relative">
            {bn.images.length === 0 && (
              <div
                className="aspect-square w-full animate-shimmer
                  bg-[linear-gradient(100deg,var(--color-raised)_40%,var(--color-hover)_50%,var(--color-raised)_60%)] bg-[length:200%_100%]"
              />
            )}
            <div className={`flex items-center gap-2 px-4 py-2.5 text-[11.5px] text-dim ${bn.images.length === 0 ? 'absolute inset-x-0 bottom-0' : ''}`}>
              <div className="size-3.5 shrink-0 animate-spin rounded-full border-2 border-line border-t-accent" />
              <span className="shrink-0">Generating · <Elapsed since={bn.createdAt} /></span>
              {activity && <span className="min-w-0 truncate text-faint">{activity}</span>}
            </div>
          </div>
        )}

        {bn.status === 'error' && (
          <div className="flex flex-col items-center gap-2.5 px-5 py-8 text-center">
            <span className="text-[22px] text-danger">▲</span>
            <div className="line-clamp-4 max-w-full text-[12px] break-words text-danger/90" title={bn.error}>
              {bn.error || 'Generation failed'}
            </div>
            <button
              onClick={e => { e.stopPropagation(); actions.regenerate(bn) }}
              className="nodrag rounded-lg border border-line px-3 py-1.5 text-[12px] text-dim hover:border-faint hover:text-ink"
            >
              ↻ Retry
            </button>
          </div>
        )}

        {bn.status === 'stopped' && (
          <div className="flex items-center justify-between px-4 py-3.5 text-[12px] text-faint">
            <span>Stopped.</span>
            <button
              onClick={e => { e.stopPropagation(); actions.regenerate(bn) }}
              className="nodrag rounded-lg border border-line px-2.5 py-1 text-[12px] text-dim hover:border-faint hover:text-ink"
            >
              ↻ Retry
            </button>
          </div>
        )}

        {!running && bn.status === 'done' && (
          <div className="flex items-center gap-2 px-4 py-2.5 text-[10.5px] text-faint">
            <span className="min-w-0 flex-1 truncate" title={bn.text}>{bn.text || 'Done'}</span>
            <span className="shrink-0">
              codex{bn.finishedAt ? ` · ${Math.round((bn.finishedAt - bn.createdAt) / 1000)}s` : ''}
            </span>
          </div>
        )}
      </div>
    </div>
  )
})

const nodeTypes = { gen: GenCard }
const EDGE_STYLE = { stroke: 'var(--color-line)', strokeWidth: 1.6, strokeDasharray: '7 5' }

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

function CanvasInner(props: {
  board: Board
  activity: Record<string, string>
  targetNodeId: string | null
  actions: BoardActions
}) {
  const { board, activity, targetNodeId, actions } = props
  const [nodes, setNodes] = useState<GenNodeType[]>([])
  const heightsRef = useRef<Map<string, number>>(new Map())
  const manualRef = useRef<Set<string>>(new Set())
  const layoutRaf = useRef<number>(0)
  const { fitView } = useReactFlow()
  const prevCount = useRef(0)
  const prevBoardId = useRef<string | null>(null)

  const relayout = useCallback(() => {
    cancelAnimationFrame(layoutRaf.current)
    layoutRaf.current = requestAnimationFrame(() => {
      setNodes(prev => {
        const bns = prev.map(n => n.data.bn)
        const pos = computeLayout(bns, heightsRef.current)
        return prev.map(n => {
          if (manualRef.current.has(n.id) || n.dragging) return n
          const p = pos.get(n.id)
          if (!p || (p.x === n.position.x && p.y === n.position.y)) return n
          return { ...n, position: p }
        })
      })
    })
  }, [])

  // Tree shape only — layout and edges must not recompute on content updates
  // (images arriving, activity ticks), just on adds/removes/reparenting.
  const structureKey = useMemo(
    () => board.nodes.map(n => `${n.id}:${n.parentId || ''}`).join('|'),
    [board.nodes],
  )

  // Sync board data into RF nodes. Node and data object identities are
  // preserved for anything unchanged, so on a large board an update to one
  // node re-renders one card, not all of them.
  useEffect(() => {
    if (prevBoardId.current !== board.id) {
      prevBoardId.current = board.id
      heightsRef.current = new Map()
      manualRef.current = new Set()
      prevCount.current = 0
      setNodes([])
    }
    for (const bn of board.nodes) {
      if (bn.x != null && bn.y != null) manualRef.current.add(bn.id)
    }
    setNodes(prev => {
      const prevMap = new Map(prev.map(n => [n.id, n]))
      let pos: Map<string, { x: number; y: number }> | null = null
      let changed = prev.length !== board.nodes.length
      const next = board.nodes.map(bn => {
        const existing = prevMap.get(bn.id)
        const act = activity[bn.id] || ''
        const isTarget = bn.id === targetNodeId
        if (existing) {
          const d = existing.data
          if (d.bn === bn && d.activity === act && d.isTarget === isTarget) return existing
          changed = true
          return { ...existing, data: { bn, activity: act, isTarget } }
        }
        changed = true
        // layout is only computed when genuinely new nodes appear
        if (!pos) pos = computeLayout(board.nodes, heightsRef.current)
        const manual = bn.x != null && bn.y != null ? { x: bn.x, y: bn.y } : null
        return {
          id: bn.id,
          type: 'gen' as const,
          position: manual ?? pos.get(bn.id) ?? { x: 0, y: 0 },
          data: { bn, activity: act, isTarget },
        }
      })
      return changed ? next : prev
    })
  }, [board.id, board.nodes, activity, targetNodeId])

  // Re-flow auto positions only when the tree shape changes
  useEffect(() => {
    relayout()
  }, [structureKey, relayout])

  // Fit view when nodes appear (first load or new generations)
  useEffect(() => {
    const count = board.nodes.length
    if (count > prevCount.current) {
      const t = setTimeout(() => fitView({ padding: 0.15, duration: 420, maxZoom: 1 }), 90)
      prevCount.current = count
      return () => clearTimeout(t)
    }
    prevCount.current = count
  }, [board.nodes.length, board.id, fitView])

  const onNodesChange = useCallback((changes: NodeChange<GenNodeType>[]) => {
    let dims = false
    for (const c of changes) {
      if (c.type === 'dimensions' && c.dimensions?.height) {
        const prev = heightsRef.current.get(c.id)
        if (prev !== c.dimensions.height) {
          heightsRef.current.set(c.id, c.dimensions.height)
          dims = true
        }
      }
    }
    setNodes(nds => applyNodeChanges(changes, nds))
    if (dims) relayout()
  }, [relayout])

  const boardNodesRef = useRef(board.nodes)
  boardNodesRef.current = board.nodes

  // Edge identity is stable across content updates: rebuilt only when the
  // tree shape changes, so React Flow skips edge reconciliation on SSE bursts.
  const edges = useMemo<RFEdge[]>(() => {
    const bns = boardNodesRef.current
    const ids = new Set(bns.map(n => n.id))
    return bns
      .filter(n => n.parentId && ids.has(n.parentId))
      .map(n => ({
        id: `e-${n.id}`,
        source: n.parentId!,
        target: n.id,
        type: 'smoothstep',
        style: EDGE_STYLE,
      }))
  }, [structureKey])

  return (
    <ActionsContext.Provider value={actions}>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        onNodeDragStop={(_, n) => {
          manualRef.current.add(n.id)
          actions.move(n.id, n.position.x, n.position.y)
        }}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.15, maxZoom: 1 }}
        minZoom={0.08}
        maxZoom={2}
        onlyRenderVisibleElements
        nodesConnectable={false}
        nodesFocusable={false}
        edgesFocusable={false}
        elementsSelectable={false}
        deleteKeyCode={null}
        selectionKeyCode={null}
        zoomOnDoubleClick={false}
        proOptions={{ hideAttribution: true }}
        className="!bg-bg"
      >
        <Background variant={BackgroundVariant.Dots} gap={28} size={1.4} color="#1e222d" />
        <MiniMap
          pannable
          zoomable
          className="!border !border-line !bg-raised"
          maskColor="rgba(13,14,18,.72)"
          nodeColor="#232734"
          nodeStrokeColor="transparent"
        />
      </ReactFlow>
    </ActionsContext.Provider>
  )
}

export function Canvas(props: {
  board: Board
  activity: Record<string, string>
  targetNodeId: string | null
  actions: BoardActions
}) {
  return (
    <ReactFlowProvider>
      <CanvasInner {...props} />
    </ReactFlowProvider>
  )
}
