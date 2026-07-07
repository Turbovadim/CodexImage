import type { Board, BoardNode, BoardSummary, NewNodesPayload } from './types.ts'

async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  })
  const data = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error((data as { error?: string }).error || `HTTP ${res.status}`)
  return data as T
}

export const api = {
  listBoards: () => request<BoardSummary[]>('GET', '/api/boards'),
  createBoard: () => request<Board>('POST', '/api/boards'),
  getBoard: (id: string) => request<Board>('GET', `/api/boards/${id}`),
  deleteBoard: (id: string) => request<{ ok: boolean }>('DELETE', `/api/boards/${id}`),
  addNodes: (boardId: string, payload: NewNodesPayload) =>
    request<{ nodes: BoardNode[] }>('POST', `/api/boards/${boardId}/nodes`, payload),
  regenerateNode: (boardId: string, nodeId: string, edits?: { prompt?: string; aspect?: string }) =>
    request<{ node: BoardNode }>('POST', `/api/boards/${boardId}/nodes/${nodeId}/regenerate`, edits ?? {}),
  stopNode: (boardId: string, nodeId: string) =>
    request<{ ok: boolean }>('POST', `/api/boards/${boardId}/nodes/${nodeId}/stop`),
  deleteNode: (boardId: string, nodeId: string) =>
    request<{ deleted: string[] }>('DELETE', `/api/boards/${boardId}/nodes/${nodeId}`),
  moveNode: (boardId: string, nodeId: string, x: number, y: number) =>
    request<{ node: BoardNode }>('PATCH', `/api/boards/${boardId}/nodes/${nodeId}`, { x, y }),
}
