<script lang="ts">
  import { untrack } from 'svelte'
  import {
    SvelteFlow, Background, BackgroundVariant, MiniMap, useSvelteFlow,
    type Edge, type NodeTypes,
  } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { app } from '../state.svelte.ts'
  import { isKey, isTyping } from '../hotkeys.ts'
  import { computeLayout, type GenNode } from './layout.ts'
  import GenCard from './GenCard.svelte'

  const nodeTypes = { gen: GenCard } as NodeTypes
  const EDGE_STYLE = 'stroke: var(--color-line); stroke-width: 1.6; stroke-dasharray: 7 5;'

  const { fitView } = useSvelteFlow()

  let nodes = $state.raw<GenNode[]>([])
  let edges = $state.raw<Edge[]>([])

  const heights = new Map<string, number>()
  const manual = new Set<string>()
  let layoutRaf = 0
  let prevBoardId: string | null = null
  let prevCount = 0

  // Current positions of manually placed nodes — passed to computeLayout so
  // auto-placed children flow beneath where their parent actually sits.
  function manualPositions(): Map<string, { x: number; y: number }> {
    const m = new Map<string, { x: number; y: number }>()
    const byId = new Map(nodes.map(n => [n.id, n]))
    for (const bn of app.board?.nodes ?? []) {
      if (!manual.has(bn.id)) continue
      const p = byId.get(bn.id)?.position ?? (bn.x != null && bn.y != null ? { x: bn.x, y: bn.y } : null)
      if (p) m.set(bn.id, p)
    }
    return m
  }

  // Re-flow auto positions; nodes the user dragged keep their manual position.
  function relayout() {
    cancelAnimationFrame(layoutRaf)
    layoutRaf = requestAnimationFrame(() => {
      const bns = nodes.map(n => n.data.bn)
      const pos = computeLayout(bns, heights, manualPositions())
      let changed = false
      const next = nodes.map(n => {
        if (manual.has(n.id) || n.dragging) return n
        const p = pos.get(n.id)
        if (!p || (p.x === n.position.x && p.y === n.position.y)) return n
        changed = true
        return { ...n, position: p }
      })
      if (changed) nodes = next
    })
  }

  function reportHeight(id: string, height: number) {
    if (heights.get(id) !== height) {
      heights.set(id, height)
      relayout()
    }
  }

  // Tree shape only — the flow's node/edge arrays must not rebuild on content
  // updates (images arriving, activity ticks); those flow straight into the
  // cards through the reactive board nodes. Rebuild only on adds/removes/
  // reparenting or when a different board object is loaded.
  const structureKey = $derived(
    app.board ? `${app.board.id}|${app.board.nodes.map(n => `${n.id}:${n.parentId || ''}`).join('|')}` : '',
  )

  $effect(() => {
    const board = app.board
    void structureKey
    untrack(() => sync(board))
  })

  // Gallery/lightbox "show on canvas": glide the camera to the requested node
  $effect(() => {
    const id = app.focusNodeId
    if (!id) return
    untrack(() => (app.focusNodeId = null))
    void fitView({ nodes: [{ id }], padding: 0.35, duration: 420, maxZoom: 1 })
  })

  function sync(board: typeof app.board) {
    if (!board) {
      nodes = []
      edges = []
      prevBoardId = null
      prevCount = 0
      return
    }
    if (board.id !== prevBoardId) {
      prevBoardId = board.id
      heights.clear()
      manual.clear()
      prevCount = 0
      nodes = []
    }
    const bns = board.nodes
    for (const bn of bns) {
      if (bn.x != null && bn.y != null) manual.add(bn.id)
    }

    const prevMap = new Map(nodes.map(n => [n.id, n]))
    let pos: Map<string, { x: number; y: number }> | null = null
    nodes = bns.map(bn => {
      const existing = prevMap.get(bn.id)
      if (existing) {
        // board object replaced (e.g. reopened): rebind the card to the new
        // reactive node while keeping position
        return existing.data.bn === bn ? existing : { ...existing, data: { ...existing.data, bn } }
      }
      // layout is only computed when genuinely new nodes appear
      if (!pos) pos = computeLayout(bns, heights, manualPositions())
      const manualPos = bn.x != null && bn.y != null ? { x: bn.x, y: bn.y } : null
      return {
        id: bn.id,
        type: 'gen' as const,
        position: manualPos ?? pos.get(bn.id) ?? { x: 0, y: 0 },
        data: { bn, onheight: reportHeight },
      }
    })

    const ids = new Set(bns.map(n => n.id))
    edges = bns
      .filter(n => n.parentId && ids.has(n.parentId))
      .map(n => ({
        id: `e-${n.id}`,
        source: n.parentId!,
        target: n.id,
        type: 'smoothstep',
        style: EDGE_STYLE,
      }))

    relayout()

    // Camera: fit everything on first load; on new generations, glide to just
    // the fresh nodes and their parents instead of zooming out to the world.
    if (bns.length > prevCount) {
      const fresh = bns.filter(bn => !prevMap.has(bn.id))
      const focusIds =
        prevCount > 0 && fresh.length
          ? [...new Set(fresh.flatMap(bn => (bn.parentId ? [bn.parentId, bn.id] : [bn.id])))]
          : null
      setTimeout(() => {
        void fitView(
          focusIds
            ? { nodes: focusIds.map(id => ({ id })), padding: 0.25, duration: 420, maxZoom: 1 }
            : { padding: 0.15, duration: 420, maxZoom: 1 },
        )
      }, 90)
    }
    prevCount = bns.length
  }
</script>

<svelte:window
  onkeydown={e => {
    if (e.metaKey || e.ctrlKey || e.altKey || isTyping(e)) return
    if (isKey(e, 'f')) {
      e.preventDefault()
      void fitView({ padding: 0.15, duration: 320, maxZoom: 1 })
    }
  }}
/>

<SvelteFlow
  bind:nodes
  bind:edges
  {nodeTypes}
  fitView
  fitViewOptions={{ padding: 0.15, maxZoom: 1 }}
  minZoom={0.08}
  maxZoom={2}
  panOnScroll
  zoomOnScroll={false}
  zoomOnPinch
  nodesConnectable={false}
  nodesFocusable={false}
  edgesFocusable={false}
  elementsSelectable={false}
  deleteKey={null}
  selectionKey={null}
  zoomOnDoubleClick={false}
  colorMode="dark"
  proOptions={{ hideAttribution: true }}
  class="!bg-bg"
  onnodedragstop={({ targetNode }) => {
    if (!targetNode) return
    manual.add(targetNode.id)
    app.move(targetNode.id, targetNode.position.x, targetNode.position.y)
  }}
>
  <Background variant={BackgroundVariant.Dots} gap={28} size={1.4} patternColor="#1e222d" bgColor="var(--color-bg)" />
  <MiniMap
    pannable
    zoomable
    class="!border !border-line !bg-raised"
    maskColor="rgba(13,14,18,.72)"
    nodeColor="#232734"
    nodeStrokeColor="transparent"
  />
</SvelteFlow>
