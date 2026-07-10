const MAX_CONCURRENT_DECODES = 3

interface DecodeEntry {
  src: string
  consumers: number
  started: boolean
  settled: boolean
  image: HTMLImageElement | null
  promise: Promise<void>
  resolve: () => void
  reject: (error: unknown) => void
}

export interface ImageDecodeRequest {
  promise: Promise<void>
  cancel: () => void
}

const queue: DecodeEntry[] = []
const pending = new Map<string, DecodeEntry>()
let active = 0
let draining = false

function settle(entry: DecodeEntry, error?: unknown): void {
  if (entry.settled) return
  entry.settled = true
  entry.image = null
  pending.delete(entry.src)
  if (entry.started) active--
  if (error) entry.reject(error)
  else entry.resolve()
  drainQueue()
}

function cancelConsumer(entry: DecodeEntry): void {
  if (entry.settled || entry.consumers === 0) return
  entry.consumers--
  if (entry.consumers > 0) return

  // Dropping src aborts a decode that no visible card needs anymore. A future
  // viewport entry will create a fresh request against Chromium's byte cache.
  if (entry.image) entry.image.src = ''
  settle(entry)
}

function drainQueue(): void {
  if (draining) return
  draining = true
  try {
    while (active < MAX_CONCURRENT_DECODES && queue.length > 0) {
      const entry = queue.shift()!
      if (entry.settled || entry.consumers === 0) continue

      entry.started = true
      active++
      const image = new Image()
      entry.image = image
      image.src = entry.src
      try {
        image.decode().then(
          () => settle(entry),
          error => settle(entry, error),
        )
      } catch (error) {
        settle(entry, error)
      }
    }
  } finally {
    draining = false
  }
}

/**
 * Decode an image before swapping it into a visible card. Requests for the
 * same source share work, at most three images decode concurrently, and the
 * work is cancelled when no viewport-visible card still needs it.
 */
export function requestImageDecode(src: string): ImageDecodeRequest {
  let entry = pending.get(src)
  if (entry) {
    entry.consumers++
  } else {
    let resolve!: () => void
    let reject!: (error: unknown) => void
    const promise = new Promise<void>((res, rej) => {
      resolve = res
      reject = rej
    })
    entry = {
      src,
      consumers: 1,
      started: false,
      settled: false,
      image: null,
      promise,
      resolve,
      reject,
    }
    pending.set(src, entry)
    queue.push(entry)
    drainQueue()
  }

  let cancelled = false
  return {
    promise: entry.promise,
    cancel: () => {
      if (cancelled) return
      cancelled = true
      cancelConsumer(entry)
    },
  }
}
