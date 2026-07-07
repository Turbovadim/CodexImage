/**
 * Card-size thumbnail convention: the server writes `t_<name>` next to each
 * original in data/images. Cards load the thumb and fall back to the original
 * when it doesn't exist (pre-thumbnail images, sips failure).
 */
export function thumbUrl(src: string): string {
  const i = src.lastIndexOf('/')
  return `${src.slice(0, i + 1)}t_${src.slice(i + 1)}`
}

/** onerror handler that swaps a failed thumb for the original, exactly once. */
export function thumbFallback(original: string) {
  return (e: Event) => {
    const img = e.currentTarget as HTMLImageElement
    if (img.dataset.fb !== '1') {
      img.dataset.fb = '1'
      img.src = original
    }
  }
}
