import type { Node } from '@xyflow/svelte'
import type { BoardNode } from '../types.ts'

export interface GenNodeData extends Record<string, unknown> {
  bn: BoardNode
  onheight: (id: string, height: number) => void
}

export type GenNode = Node<GenNodeData, 'gen'>

// ---------------------------------------------------------------------------
// Auto layout anchored to reality: children flow directly beneath their
// parent's *actual* position (manual or auto), so branching a dragged or
// shorter-than-estimated node spawns right under it instead of at some
// global grid slot. Manually placed nodes are anchors — they keep their
// position, don't occupy a slot in their parent's row, and their own
// subtree flows beneath them.
// ---------------------------------------------------------------------------

export const CARD_W = 340
const H_GAP = 64
const V_GAP = 96
const ROOT_GAP = 140
const EST_H = 460

export function computeLayout(
  bns: BoardNode[],
  heights: Map<string, number>,
  manual: Map<string, { x: number; y: number }> = new Map(),
): Map<string, { x: number; y: number }> {
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

  // Subtree width counts only auto-placed descendants; manual nodes anchor
  // their own subtree and take no slot in the parent's row.
  const widths = new Map<string, number>()
  const subtreeWidth = (n: BoardNode): number => {
    let sum = 0
    let count = 0
    for (const k of children.get(n.id) || []) {
      const kw = subtreeWidth(k)
      if (!manual.has(k.id)) {
        sum += kw
        count++
      }
    }
    const w = count ? Math.max(CARD_W, sum + H_GAP * (count - 1)) : CARD_W
    widths.set(n.id, w)
    return w
  }

  const pos = new Map<string, { x: number; y: number }>()
  const place = (n: BoardNode, bandX: number, y: number) => {
    const w = widths.get(n.id)!
    const p = manual.get(n.id) ?? { x: bandX + w / 2 - CARD_W / 2, y }
    pos.set(n.id, p)
    const kids = children.get(n.id) || []
    const autoKids = kids.filter(k => !manual.has(k.id))
    const rowW =
      autoKids.reduce((s, k) => s + widths.get(k.id)!, 0) + H_GAP * Math.max(0, autoKids.length - 1)
    const rowY = p.y + (heights.get(n.id) || EST_H) + V_GAP
    let cx = p.x + CARD_W / 2 - rowW / 2
    for (const k of kids) {
      if (manual.has(k.id)) {
        place(k, 0, 0) // anchored — band coords unused
      } else {
        place(k, cx, rowY)
        cx += widths.get(k.id)! + H_GAP
      }
    }
  }

  for (const r of roots) subtreeWidth(r)
  let rootX = 0
  for (const r of roots) {
    if (manual.has(r.id)) {
      place(r, 0, 0)
    } else {
      place(r, rootX, 0)
      rootX += widths.get(r.id)! + ROOT_GAP
    }
  }

  // Collision pass: an auto-placed node must not land on occupied space
  // (manual nodes, or nodes already resolved). On overlap the node shifts
  // right together with its auto subtree until the spot is free. Pure
  // function of the inputs, so repeated relayouts stay stable.
  const MARGIN = 24
  interface Rect { x: number; y: number; r: number; b: number }
  const rectOf = (id: string): Rect => {
    const p = pos.get(id)!
    return { x: p.x, y: p.y, r: p.x + CARD_W, b: p.y + (heights.get(id) || EST_H) }
  }
  const overlaps = (a: Rect, b: Rect) =>
    a.x < b.r + MARGIN && a.r > b.x - MARGIN && a.y < b.b + MARGIN && a.b > b.y - MARGIN

  const shiftSubtree = (n: BoardNode, dx: number) => {
    if (manual.has(n.id)) return
    const p = pos.get(n.id)!
    pos.set(n.id, { x: p.x + dx, y: p.y })
    for (const k of children.get(n.id) || []) shiftSubtree(k, dx)
  }

  const occupied: Rect[] = bns.filter(n => manual.has(n.id)).map(n => rectOf(n.id))
  const resolve = (n: BoardNode) => {
    if (!manual.has(n.id)) {
      for (let guard = 0; guard < 100; guard++) {
        const r = rectOf(n.id)
        const hit = occupied.find(o => overlaps(r, o))
        if (!hit) break
        shiftSubtree(n, hit.r + H_GAP - r.x)
      }
      occupied.push(rectOf(n.id))
    }
    for (const k of children.get(n.id) || []) resolve(k)
  }
  for (const r of roots) resolve(r)

  return pos
}
