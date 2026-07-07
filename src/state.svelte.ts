import { api } from './api.ts'
import type { Board, BoardNode, BoardSummary, ServerEvent } from './types.ts'

export interface PendingAttachment {
  name: string
  data: string
  previewUrl: string
}

export interface ComposerTarget {
  nodeId: string
  prompt: string
  /** specific image being branched from, if picked in the lightbox */
  sourceImage?: string
  thumb?: string
}

/** BoardNode keys that may disappear between server snapshots of a node. */
const OPTIONAL_NODE_KEYS = ['error', 'x', 'y', 'finishedAt', 'usage'] as const

class AppState {
  boards = $state<BoardSummary[]>([])
  board = $state<Board | null>(null)
  draft = $state('')
  activity = $state<Record<string, string>>({})
  target = $state<ComposerTarget | null>(null)
  lightbox = $state<{ src: string; node: BoardNode } | null>(null)
  /** node whose prompt is being edited in the modal */
  editing = $state<BoardNode | null>(null)

  #es: EventSource | null = null
  #activeBoardId: string | null = null

  async init() {
    const list = await api.listBoards()
    this.boards = list
    if (list.length) await this.openBoard(list[0].id)
  }

  async refreshBoards() {
    this.boards = await api.listBoards()
  }

  #connectEvents(boardId: string) {
    this.#es?.close()
    const es = new EventSource(`/api/boards/${boardId}/events`)
    this.#es = es
    es.onmessage = e => {
      if (this.#activeBoardId !== boardId) return
      const ev: ServerEvent = JSON.parse(e.data)
      const board = this.board
      if (ev.type === 'node') {
        if (!board || board.id !== boardId) return
        const existing = board.nodes.find(n => n.id === ev.node.id)
        if (existing) {
          // Update in place so canvas cards holding this node keep their
          // reactive reference; only the changed fields re-render.
          for (const k of OPTIONAL_NODE_KEYS) if (!(k in ev.node)) delete existing[k]
          Object.assign(existing, ev.node)
        } else {
          board.nodes.push(ev.node)
        }
      } else if (ev.type === 'nodesDeleted') {
        const gone = new Set(ev.ids)
        if (board && board.id === boardId) {
          board.nodes = board.nodes.filter(n => !gone.has(n.id))
        }
        if (this.target && gone.has(this.target.nodeId)) this.target = null
        if (this.editing && gone.has(this.editing.id)) this.editing = null
      } else if (ev.type === 'activity') {
        this.activity[ev.nodeId] = ev.text
      } else if (ev.type === 'done') {
        delete this.activity[ev.nodeId]
        void this.refreshBoards()
      }
    }
  }

  async openBoard(id: string) {
    this.#activeBoardId = id
    const full = await api.getBoard(id)
    if (this.#activeBoardId !== id) return
    this.board = full
    this.activity = {}
    this.target = null
    this.lightbox = null
    this.editing = null
    this.#connectEvents(id)
  }

  async newBoard() {
    const created = await api.createBoard()
    await this.refreshBoards()
    await this.openBoard(created.id)
  }

  async deleteBoard(id: string) {
    if (!confirm('Delete this board and its images?')) return
    await api.deleteBoard(id)
    if (this.#activeBoardId === id) {
      this.#activeBoardId = null
      this.#es?.close()
      this.board = null
      this.target = null
    }
    await this.refreshBoards()
  }

  async send(
    text: string,
    options: { aspect: string; count: number },
    attachments: PendingAttachment[],
  ) {
    let boardId = this.#activeBoardId
    if (!boardId) {
      const created = await api.createBoard()
      boardId = created.id
      this.#activeBoardId = boardId
      this.board = created
      this.#connectEvents(boardId)
    }
    const target = this.target
    const { nodes } = await api.addNodes(boardId, {
      prompt: text,
      parentId: target?.nodeId ?? null,
      sourceImages: target?.sourceImage ? [target.sourceImage] : undefined,
      aspect: options.aspect,
      count: options.count,
      attachments: attachments.map(a => ({ name: a.name, data: a.data })),
    })
    if (this.board && this.board.id === boardId) {
      if (!this.board.nodes.length) this.board.title = text.slice(0, 60)
      this.board.nodes.push(...nodes)
    }
    this.target = null
    void this.refreshBoards()
  }

  // -------------------------------------------------------------------------
  // Node actions
  // -------------------------------------------------------------------------

  branch(node: BoardNode, sourceImage?: string) {
    this.target = {
      nodeId: node.id,
      prompt: node.prompt,
      sourceImage,
      thumb: node.images[node.images.length - 1],
    }
  }

  regenerate(node: BoardNode) {
    const boardId = this.#activeBoardId
    if (!boardId) return
    api.regenerateNode(boardId, node.id).catch(err => alert((err as Error).message))
  }

  /** update the prompt and regenerate in a fresh session */
  edit(node: BoardNode, prompt: string) {
    const boardId = this.#activeBoardId
    if (!boardId) return
    api.regenerateNode(boardId, node.id, { prompt }).catch(err => alert((err as Error).message))
  }

  stop(node: BoardNode) {
    const boardId = this.#activeBoardId
    if (!boardId) return
    void api.stopNode(boardId, node.id)
  }

  remove(node: BoardNode) {
    const boardId = this.#activeBoardId
    if (!boardId) return
    if (!confirm('Delete this node and all branches under it?')) return
    api.deleteNode(boardId, node.id).catch(err => alert((err as Error).message))
  }

  openImage(src: string, node: BoardNode) {
    this.lightbox = { src, node }
  }

  /**
   * Arrow-key navigation in the lightbox. Left/right step through the current
   * node's images, then continue into the prev/next sibling; up/down move to
   * the nearest ancestor/descendant that has images.
   */
  navigateLightbox(dir: 'up' | 'down' | 'left' | 'right') {
    const lb = this.lightbox
    const nodes = this.board?.nodes
    if (!lb || !nodes) return
    const byCreated = (a: BoardNode, b: BoardNode) =>
      a.createdAt - b.createdAt || a.id.localeCompare(b.id)
    const node = lb.node

    if (dir === 'up') {
      let p = nodes.find(n => n.id === node.parentId)
      while (p && !p.images.length) {
        const pid = p.parentId
        p = nodes.find(n => n.id === pid)
      }
      if (p) this.lightbox = { src: p.images[p.images.length - 1], node: p }
    } else if (dir === 'down') {
      const queue = nodes.filter(n => n.parentId === node.id).sort(byCreated)
      while (queue.length) {
        const c = queue.shift()!
        if (c.images.length) {
          this.lightbox = { src: c.images[c.images.length - 1], node: c }
          return
        }
        queue.push(...nodes.filter(n => n.parentId === c.id).sort(byCreated))
      }
    } else {
      const delta = dir === 'right' ? 1 : -1
      const ni = node.images.indexOf(lb.src) + delta
      if (ni >= 0 && ni < node.images.length) {
        this.lightbox = { src: node.images[ni], node }
        return
      }
      // roots are unrelated generations — sideways only moves between true siblings
      if (!node.parentId) return
      const siblings = nodes
        .filter(n => n.parentId === node.parentId && n.images.length > 0)
        .sort(byCreated)
      const next = siblings[siblings.findIndex(n => n.id === node.id) + delta]
      if (next) {
        this.lightbox = {
          src: delta > 0 ? next.images[0] : next.images[next.images.length - 1],
          node: next,
        }
      }
    }
  }

  move(nodeId: string, x: number, y: number) {
    const boardId = this.#activeBoardId
    if (!boardId) return
    void api.moveNode(boardId, nodeId, x, y).catch(() => {})
  }
}

export const app = new AppState()
