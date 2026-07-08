<script lang="ts">
  import { app } from '../state.svelte.ts'
  import { isKey, isTyping } from '../hotkeys.ts'
  import { copyImage } from '../media.ts'
  import type { BoardNode } from '../types.ts'
  import Icon, { type IconName } from './Icon.svelte'

  const ARROWS: Record<string, 'up' | 'down' | 'left' | 'right'> = {
    ArrowUp: 'up',
    ArrowDown: 'down',
    ArrowLeft: 'left',
    ArrowRight: 'right',
  }

  let { src, node, onClose, onBranch }: {
    src: string
    node: BoardNode
    onClose: () => void
    onBranch?: () => void
  } = $props()

  let copied = $state(false)
  let continueText = $state('')
  let ctxMenu = $state<{ x: number; y: number } | null>(null)

  // ---------------------------------------------------------------------------
  // Zoom & pan: scroll to zoom toward the cursor, drag to pan when zoomed,
  // double-click toggles 1x <-> 2.5x. Resets whenever the image changes.
  // ---------------------------------------------------------------------------
  const MIN_ZOOM = 1
  const MAX_ZOOM = 8
  let scale = $state(1)
  let tx = $state(0)
  let ty = $state(0)
  let stageEl: HTMLDivElement | undefined = $state()
  let panning = $state(false)
  let moved = false
  let lastX = 0
  let lastY = 0

  $effect(() => {
    void src
    scale = 1
    tx = 0
    ty = 0
    ctxMenu = null
  })

  // (px, py) = cursor offset from the stage center; keep that image point fixed
  function zoomAt(px: number, py: number, next: number) {
    const s = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, next))
    tx = s === 1 ? 0 : px - ((px - tx) * s) / scale
    ty = s === 1 ? 0 : py - ((py - ty) * s) / scale
    scale = s
  }

  function cursorOffset(e: MouseEvent): [number, number] {
    const r = stageEl!.getBoundingClientRect()
    return [e.clientX - r.left - r.width / 2, e.clientY - r.top - r.height / 2]
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault()
    const [px, py] = cursorOffset(e)
    zoomAt(px, py, scale * Math.exp(-e.deltaY * 0.0022))
  }

  function onDblClick(e: MouseEvent) {
    const [px, py] = cursorOffset(e)
    zoomAt(px, py, scale > 1 ? 1 : 2.5)
  }

  function onContextMenu(e: MouseEvent) {
    e.preventDefault()
    // keep the menu inside the viewport when invoked near an edge
    const W = 200
    const H = 190
    ctxMenu = {
      x: Math.min(e.clientX, window.innerWidth - W - 8),
      y: Math.min(e.clientY, window.innerHeight - H - 8),
    }
  }

  let suppressClick = false

  function onPointerDown(e: PointerEvent) {
    if (ctxMenu) {
      ctxMenu = null
      suppressClick = true // dismissing the menu must not also close the lightbox
    }
    if (e.button !== 0) return
    panning = true
    moved = false
    lastX = e.clientX
    lastY = e.clientY
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
  }

  function onPointerMove(e: PointerEvent) {
    if (!panning || scale === 1) return
    const dx = e.clientX - lastX
    const dy = e.clientY - lastY
    if (dx || dy) moved = true
    lastX = e.clientX
    lastY = e.clientY
    tx += dx
    ty += dy
  }

  function onPointerUp() {
    panning = false
  }

  // A plain click (no pan, not zoomed) closes, matching the old behavior
  function onStageClick() {
    if (suppressClick) {
      suppressClick = false
      return
    }
    if (scale === 1 && !moved) onClose()
    moved = false
  }

  function branch() {
    onBranch?.()
    onClose()
  }

  function copy() {
    copyImage(src).then(() => {
      copied = true
      setTimeout(() => (copied = false), 1200)
    }).catch(() => {})
  }

  function menuCopy() {
    ctxMenu = null
    copyImage(src)
      .then(() => app.showToast('Image copied', undefined, 2000))
      .catch(() => app.showToast('Copy failed', undefined, 2000))
  }

  function submitContinue() {
    const text = continueText.trim()
    if (!text) return
    continueText = ''
    void app.continueFrom(node, src, text).catch(err => app.showError(err))
    onClose()
  }
</script>

<svelte:window
  onkeydown={e => {
    if (e.key === 'Escape') {
      if (isTyping(e)) (e.target as HTMLElement).blur()
      else if (ctxMenu) ctxMenu = null
      else onClose()
    } else if (isTyping(e)) {
      return
    } else if (isKey(e, 'b') && !e.metaKey && !e.ctrlKey && onBranch) {
      branch()
    } else if (ARROWS[e.key]) {
      e.preventDefault()
      app.navigateLightbox(ARROWS[e.key])
    }
  }}
/>

{#snippet action(title: string, icon: IconName, opts: { accent?: boolean; text?: string; onclick?: () => void; href?: string; download?: boolean; newTab?: boolean })}
  {@const cls = `flex items-center gap-1.5 rounded-[10px] border px-3 py-2 text-[13px] shadow-[0_6px_24px_rgba(0,0,0,.45)] backdrop-blur-md
    ${opts.accent
      ? 'border-accent-strong bg-accent-strong/90 font-medium text-white hover:bg-accent'
      : 'border-line bg-raised/80 text-ink hover:border-faint'}`}
  {#if opts.href}
    <a
      href={opts.href}
      download={opts.download}
      target={opts.newTab ? '_blank' : undefined}
      rel="noopener"
      {title}
      class={cls}
    >
      <Icon name={icon} size={14} />
      {#if opts.text}<span>{opts.text}</span>{/if}
    </a>
  {:else}
    <button onclick={opts.onclick} {title} class={cls}>
      <Icon name={icon} size={14} />
      {#if opts.text}<span>{opts.text}</span>{/if}
    </button>
  {/if}
{/snippet}

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div
  bind:this={stageEl}
  onclick={onStageClick}
  onwheel={onWheel}
  ondblclick={onDblClick}
  oncontextmenu={onContextMenu}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
  class="fixed inset-0 z-50 overflow-hidden bg-bg/97 {scale > 1 ? (panning ? 'cursor-grabbing' : 'cursor-grab') : ''}"
>
  <img
    {src}
    alt=""
    draggable={false}
    style="transform: translate({tx}px, {ty}px) scale({scale})"
    class="h-full w-full object-contain select-none {panning ? '' : 'transition-transform duration-75'}"
  />

  <div
    onclick={e => e.stopPropagation()}
    onpointerdown={e => e.stopPropagation()}
    ondblclick={e => e.stopPropagation()}
    class="absolute top-4 right-4 flex gap-2 opacity-90 transition-opacity hover:opacity-100"
  >
    {#if onBranch}
      {@render action('Branch from this image (B)', 'branch', { accent: true, text: 'Branch', onclick: branch })}
    {/if}
    {@render action('Show on canvas', 'locate', { onclick: () => app.locateNode(node.id) })}
    {@render action(copied ? 'Copied' : 'Copy image', copied ? 'check' : 'copy', { onclick: copy })}
    {@render action('Download', 'download', { href: src, download: true })}
    {@render action('Open original', 'external', { href: src, newTab: true })}
    {@render action('Close (Esc)', 'x', { onclick: onClose })}
  </div>

  {#if scale > 1}
    <div class="absolute bottom-5 left-5 rounded-lg border border-line bg-raised/80 px-2.5 py-1 text-[11.5px] text-dim backdrop-blur-md">
      {Math.round(scale * 100)}%
    </div>
  {/if}

  {#if ctxMenu}
    {#snippet menuItem(label: string, icon: IconName, opts: { onclick?: () => void; href?: string; download?: boolean; newTab?: boolean })}
      {@const cls = 'flex w-full items-center gap-2.5 px-3 py-2 text-left text-[12.5px] text-ink hover:bg-hover'}
      {#if opts.href}
        <a href={opts.href} download={opts.download} target={opts.newTab ? '_blank' : undefined} rel="noopener" class={cls} onclick={() => (ctxMenu = null)}>
          <Icon name={icon} size={13} class="text-dim" /> {label}
        </a>
      {:else}
        <button onclick={opts.onclick} class={cls}>
          <Icon name={icon} size={13} class="text-dim" /> {label}
        </button>
      {/if}
    {/snippet}
    <div
      onclick={e => e.stopPropagation()}
      onpointerdown={e => e.stopPropagation()}
      ondblclick={e => e.stopPropagation()}
      oncontextmenu={e => { e.preventDefault(); e.stopPropagation() }}
      style="left: {ctxMenu.x}px; top: {ctxMenu.y}px"
      class="absolute z-10 w-[200px] overflow-hidden rounded-xl border border-line bg-raised/95 py-1 shadow-[0_18px_60px_rgba(0,0,0,.55)] backdrop-blur-md"
    >
      {@render menuItem('Copy image', 'copy', { onclick: menuCopy })}
      {@render menuItem('Download', 'download', { href: src, download: true })}
      {@render menuItem('Open original', 'external', { href: src, newTab: true })}
      <div class="my-1 border-t border-line"></div>
      {@render menuItem('Show on canvas', 'locate', { onclick: () => app.locateNode(node.id) })}
      {#if onBranch}
        {@render menuItem('Branch from this image', 'branch', { onclick: branch })}
      {/if}
    </div>
  {/if}

  <!-- quick continue: branch from this exact image without leaving the lightbox -->
  <div
    onclick={e => e.stopPropagation()}
    onpointerdown={e => e.stopPropagation()}
    ondblclick={e => e.stopPropagation()}
    class="absolute inset-x-0 bottom-5 flex justify-center px-6"
  >
    <div class="flex w-full max-w-[540px] items-center gap-2 rounded-2xl border border-line bg-raised/90 p-1.5 pl-4 shadow-[0_12px_44px_rgba(0,0,0,.5)] backdrop-blur-md">
      <input
        bind:value={continueText}
        placeholder="Refine this image… (Enter to generate)"
        onkeydown={e => {
          if (e.key === 'Enter') submitContinue()
        }}
        class="min-w-0 flex-1 bg-transparent text-[13.5px] text-ink outline-none placeholder:text-faint"
      />
      <button
        onclick={submitContinue}
        title="Branch from this image with the new prompt"
        class="shrink-0 rounded-[10px] border border-accent-strong bg-accent-strong px-3.5 py-2 text-[12.5px] font-medium text-white hover:bg-accent"
      >
        Continue
      </button>
    </div>
  </div>
</div>
