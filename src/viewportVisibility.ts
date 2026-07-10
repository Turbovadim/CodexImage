type VisibilityListener = (visible: boolean) => void

const listeners = new Map<Element, VisibilityListener>()
let observer: IntersectionObserver | null = null

function getObserver(): IntersectionObserver {
  observer ??= new IntersectionObserver(
    entries => {
      for (const entry of entries) listeners.get(entry.target)?.(entry.isIntersecting)
    },
    // Start decoding just before a card enters the window so the high-res swap
    // is ready when the user reaches it, without decoding the whole canvas.
    { rootMargin: '240px' },
  )
  return observer
}

/** Observe a transformed canvas card with one shared IntersectionObserver. */
export function observeViewportVisibility(
  element: Element,
  listener: VisibilityListener,
): () => void {
  if (typeof IntersectionObserver === 'undefined') {
    listener(true)
    return () => {}
  }

  listeners.set(element, listener)
  getObserver().observe(element)
  return () => {
    listeners.delete(element)
    observer?.unobserve(element)
    if (listeners.size === 0) {
      observer?.disconnect()
      observer = null
    }
  }
}
