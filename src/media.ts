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

/** Copy an image to the clipboard. ClipboardItem only takes PNG, so transcode. */
export async function copyImage(src: string): Promise<void> {
  const blob = await fetch(src).then(r => r.blob())
  if (blob.type === 'image/png') {
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })])
    return
  }
  const bmp = await createImageBitmap(blob)
  const canvas = document.createElement('canvas')
  canvas.width = bmp.width
  canvas.height = bmp.height
  canvas.getContext('2d')!.drawImage(bmp, 0, 0)
  const png = await new Promise<Blob>((resolve, reject) =>
    canvas.toBlob(b => (b ? resolve(b) : reject(new Error('PNG encode failed'))), 'image/png'),
  )
  await navigator.clipboard.write([new ClipboardItem({ 'image/png': png })])
}

/** "870" / "12.4k" / "1.2M" */
export function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`
  return String(n)
}
