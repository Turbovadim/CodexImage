export const MAX_ATTACHMENTS = 8
export const MAX_ATTACHMENT_BYTES = 8 * 1024 * 1024
export const MAX_ATTACHMENT_TOTAL_BYTES = 32 * 1024 * 1024

export type NodeStatus = 'running' | 'done' | 'error' | 'stopped'
export type StopReason = 'user' | 'app-quit' | 'deleted'

export interface BoardNode {
  id: string
  /** parent node this one branches from; null for root nodes */
  parentId: string | null
  prompt: string
  aspect: string
  /** snapshot of the parent images this node was generated from (server /images/... urls) */
  sourceImages: string[]
  /** user-uploaded reference images for this node (server /images/... urls) */
  attachments: string[]
  /** Codex-selected final outputs, in semantic order */
  images: string[]
  /** semantic labels aligned by index with images */
  imageLabels: string[]
  /** all generated candidates from the current run; never presented as final outputs */
  attempts: string[]
  /** Codex's concise description of the selected final output set */
  text: string
  status: NodeStatus
  error?: string
  /** why a run entered the stopped state; timeouts are errors, not stops */
  stopReason?: StopReason
  /** manual canvas position; auto-layout applies when absent */
  x?: number
  y?: number
  createdAt: number
  /** start of the current run; differs from createdAt after regeneration */
  runStartedAt?: number
  finishedAt?: number
  usage?: Record<string, number>
}

export interface Board {
  id: string
  title: string
  createdAt: number
  nodes: BoardNode[]
  /** ids of nodes currently generating (populated on GET) */
  generating?: string[]
}

export interface BoardSummary {
  id: string
  title: string
  createdAt: number
  updatedAt: number
  imageCount: number
  lastImage: string | null
  generating: boolean
  /** input+output tokens summed over all nodes */
  totalTokens: number
}

export interface AttachmentUpload {
  name: string
  /** base64-encoded file content */
  data: string
}

export interface NewNodesPayload {
  prompt: string
  parentId?: string | null
  /** explicit source images to branch from (defaults to all parent images) */
  sourceImages?: string[]
  aspect: string
  count: number
  attachments?: AttachmentUpload[]
  /** existing board image urls to reuse as attachments (duplicate) */
  attachmentUrls?: string[]
}

export type ServerEvent =
  | { type: 'hello'; generating: string[] }
  | { type: 'node'; node: BoardNode }
  | { type: 'nodesDeleted'; ids: string[] }
  | { type: 'activity'; nodeId: string; text: string }
  | { type: 'done'; nodeId: string }
