<script lang="ts" module>
  // aspect ratios (w/h) of images measured this session, so re-renders reserve
  // the exact height before the file loads — no layout shift, ever
  const seenRatio = new Map<string, number>()
</script>

<script lang="ts">
  import { Handle, Position, useViewport } from '@xyflow/svelte'
  import { app } from '../state.svelte.ts'
  import { isTyping } from '../hotkeys.ts'
  import { thumbUrl, thumbFallback, fmtTokens } from '../media.ts'
  import { CARD_W, type GenNodeData } from './layout.ts'
  import Elapsed from './Elapsed.svelte'
  import Icon, { type IconName } from './Icon.svelte'

  let { data }: { data: GenNodeData } = $props()

  const bn = $derived(data.bn)
  const running = $derived(bn.status === 'running')
  const activity = $derived(app.activity[bn.id] ?? '')
  const isTarget = $derived(app.target?.nodeId === bn.id)
  const date = $derived(
    new Date(bn.createdAt).toLocaleDateString(undefined, { day: '2-digit', month: '2-digit', year: '2-digit' }),
  )
  const tokens = $derived((bn.usage?.input_tokens || 0) + (bn.usage?.output_tokens || 0))

  let copied = $state(false)
  let height = $state(0)
  let hovered = $state(false)

  // "Show more" appears only when the prompt actually overflows the clamp
  let promptEl = $state<HTMLDivElement>()
  let expanded = $state(false)
  let clamped = $state(false)
  $effect(() => {
    void bn.prompt
    if (expanded) return
    const el = promptEl
    if (el) clamped = el.scrollHeight > el.clientHeight + 1
  })

  // Thumbs are 720px; once zoomed past the point where a thumb can't fill the
  // card's device pixels, upgrade to the original. Sticky — never downgrade.
  const viewport = useViewport()
  const dpr = window.devicePixelRatio || 1
  const THUMB_W = 720
  const hiZoom = $derived(THUMB_W / ((bn.images.length > 1 ? CARD_W / 2 : CARD_W) * dpr))
  let hiRes = $state(false)
  $effect(() => {
    if (viewport.current.zoom >= hiZoom) hiRes = true
  })

  // Preload + decode originals off-screen; each img swaps to the original
  // only once it can paint instantly, so the upgrade never flickers.
  const requested = new Set<string>()
  let ready = $state<Record<string, true>>({})
  $effect(() => {
    if (!hiRes) return
    for (const s of bn.images) {
      if (requested.has(s)) continue
      requested.add(s)
      const img = new Image()
      img.src = s
      img.decode().then(() => { ready[s] = true }).catch(() => {})
    }
  })

  // report measured card height for the auto layout
  $effect(() => {
    if (height) data.onheight(bn.id, height)
  })

  // Reserve the image's box before it loads: exact ratio if we've measured it,
  // else the requested aspect. Only auto-aspect images can shift, once ever.
  function reservedRatio(src: string): string {
    const r = seenRatio.get(src)
    if (r) return `aspect-ratio: ${r}`
    if (bn.aspect !== 'auto') return `aspect-ratio: ${bn.aspect.replace(':', ' / ')}`
    return ''
  }

  function copyPrompt() {
    navigator.clipboard.writeText(bn.prompt).then(() => {
      copied = true
      setTimeout(() => (copied = false), 1200)
    }).catch(() => {})
  }

</script>

<svelte:window
  onkeydown={e => {
    if (!hovered || running || app.lightbox || app.editing) return
    if (e.metaKey || e.ctrlKey || e.altKey || isTyping(e)) return
    if (e.key.toLowerCase() === 'e') {
      e.preventDefault()
      app.editing = bn
    } else if (e.key.toLowerCase() === 'r') {
      e.preventDefault()
      app.regenerate(bn)
    } else if (e.key.toLowerCase() === 'b') {
      e.preventDefault()
      app.branch(bn)
    }
  }}
/>

{#snippet toolButton(title: string, icon: IconName, onclick: () => void, opts: { danger?: boolean; text?: string } = {})}
  <button
    {title}
    onclick={e => { e.stopPropagation(); onclick() }}
    class="nodrag flex h-7 min-w-7 items-center justify-center gap-1 rounded-lg border border-line bg-raised px-1.5
      text-[12px] text-dim shadow-md transition-colors
      {opts.danger ? 'hover:border-danger/50 hover:text-danger' : 'hover:border-faint hover:text-ink'}"
  >
    <Icon name={icon} size={13} />
    {#if opts.text}<span>{opts.text}</span>{/if}
  </button>
{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions — pointer handlers only track hover for the E hotkey -->
<div
  class="group relative"
  style="width: {CARD_W}px"
  bind:clientHeight={height}
  onpointerenter={() => (hovered = true)}
  onpointerleave={() => (hovered = false)}
>
  <Handle type="target" position={Position.Top} class="!pointer-events-none !top-0 !opacity-0" />
  <Handle type="source" position={Position.Bottom} class="!pointer-events-none !bottom-0 !opacity-0" />

  <!-- hover toolbar -->
  <div class="absolute -top-9 right-1 z-10 flex gap-1 opacity-0 transition-opacity group-hover:opacity-100">
    {#if running}
      {@render toolButton('Stop generation', 'stop', () => app.stop(bn), { danger: true, text: 'Stop' })}
    {:else}
      {@render toolButton('Branch from this node (B)', 'branch', () => app.branch(bn), { text: 'Branch' })}
      {@render toolButton('Edit prompt & regenerate (E)', 'pencil', () => (app.editing = bn))}
      {@render toolButton('Regenerate (new session, same prompt) (R)', 'refresh', () => app.regenerate(bn))}
    {/if}
    {@render toolButton('Copy prompt', copied ? 'check' : 'copy', copyPrompt)}
    {#if bn.images.length > 0}
      <a
        href={bn.images[bn.images.length - 1]}
        download
        onclick={e => e.stopPropagation()}
        title="Download image"
        class="nodrag flex h-7 min-w-7 items-center justify-center rounded-lg border border-line bg-raised px-1.5 text-dim shadow-md hover:border-faint hover:text-ink"
      >
        <Icon name="download" size={13} />
      </a>
    {/if}
    {@render toolButton('Delete node and its branches', 'trash', () => app.remove(bn), { danger: true })}
  </div>

  <!-- content-visibility lets the browser skip layout/paint for offscreen cards
       without unmounting them (no remount flicker); intrinsic-size falls back to
       the layout estimate until the browser remembers the real size -->
  <div
    class="overflow-hidden rounded-[20px] border bg-raised shadow-[0_10px_36px_rgba(0,0,0,.4)] transition-[border-color,box-shadow]
      [contain-intrinsic-size:auto_340px_auto_460px] [content-visibility:auto]
      {isTarget ? 'border-accent shadow-[0_0_0_3px_rgba(124,140,255,.18),0_10px_36px_rgba(0,0,0,.4)]' : 'border-line'}"
  >
    <!-- prompt -->
    <div class="px-4 pt-3.5 pb-3">
      <div
        bind:this={promptEl}
        class="nodrag cursor-text text-[12.5px] leading-relaxed break-words whitespace-pre-wrap text-ink/90 select-text
          {expanded ? 'nowheel max-h-[320px] overflow-y-auto' : 'line-clamp-6'}"
      >
        {bn.prompt}
      </div>
      {#if clamped || expanded}
        <button
          onclick={e => { e.stopPropagation(); expanded = !expanded }}
          class="nodrag mt-1 text-[11px] text-accent/80 hover:text-accent"
        >
          {expanded ? 'Show less' : 'Show more'}
        </button>
      {/if}
      {#if bn.attachments.length > 0}
        <div class="mt-2 flex gap-1.5">
          {#each bn.attachments as src (src)}
            <img
              src={thumbUrl(src)}
              onerror={thumbFallback(src)}
              alt=""
              loading="lazy"
              decoding="async"
              class="h-9 w-9 rounded-md border border-line object-cover"
            />
          {/each}
        </div>
      {/if}
      <div class="mt-2.5 flex items-center gap-2 text-[10.5px] text-faint">
        <span class="flex items-center gap-1"><span class="text-accent">❖</span> Me</span>
        {#if bn.aspect !== 'auto'}
          <span class="rounded-full border border-line px-1.5 py-px">{bn.aspect}</span>
        {/if}
        <span class="ml-auto">{date}</span>
      </div>
    </div>

    <div class="border-t border-dashed border-line/80"></div>

    <!-- result -->
    {#if bn.images.length > 0}
      <div class={bn.images.length > 1 ? 'grid grid-cols-2 gap-px bg-line/50' : ''}>
        {#each bn.images as src (src)}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
          <!-- lazy is safe here because the box height is reserved via aspect-ratio -->
          <img
            src={ready[src] ? src : thumbUrl(src)}
            style={reservedRatio(src)}
            onerror={ready[src] ? undefined : thumbFallback(src)}
            onload={e => {
              const el = e.currentTarget as HTMLImageElement
              if (el.naturalWidth && el.naturalHeight) seenRatio.set(src, el.naturalWidth / el.naturalHeight)
            }}
            alt=""
            loading="lazy"
            decoding="async"
            onclick={() => app.openImage(src, bn)}
            class="nodrag block w-full cursor-zoom-in"
            draggable={false}
          />
        {/each}
      </div>
    {/if}

    {#if running}
      <div class="relative">
        {#if bn.images.length === 0}
          <div
            class="aspect-square w-full animate-shimmer
              bg-[linear-gradient(100deg,var(--color-raised)_40%,var(--color-hover)_50%,var(--color-raised)_60%)] bg-[length:200%_100%]"
          ></div>
        {/if}
        <div class="flex items-center gap-2 px-4 py-2.5 text-[11.5px] text-dim {bn.images.length === 0 ? 'absolute inset-x-0 bottom-0' : ''}">
          <div class="size-3.5 shrink-0 animate-spin rounded-full border-2 border-line border-t-accent"></div>
          <span class="shrink-0">Generating · <Elapsed since={bn.createdAt} /></span>
          {#if activity}<span class="min-w-0 truncate text-faint">{activity}</span>{/if}
        </div>
      </div>
    {/if}

    {#if bn.status === 'error'}
      <div class="flex flex-col items-center gap-2.5 px-5 py-8 text-center">
        <Icon name="alert" size={22} class="text-danger" />
        <div class="line-clamp-4 max-w-full text-[12px] break-words text-danger/90" title={bn.error}>
          {bn.error || 'Generation failed'}
        </div>
        <button
          onclick={e => { e.stopPropagation(); app.regenerate(bn) }}
          class="nodrag flex items-center gap-1.5 rounded-lg border border-line px-3 py-1.5 text-[12px] text-dim hover:border-faint hover:text-ink"
        >
          <Icon name="refresh" size={12} /> Retry
        </button>
      </div>
    {/if}

    {#if bn.status === 'stopped'}
      <div class="flex items-center justify-between px-4 py-3.5 text-[12px] text-faint">
        <span>Stopped.</span>
        <button
          onclick={e => { e.stopPropagation(); app.regenerate(bn) }}
          class="nodrag flex items-center gap-1.5 rounded-lg border border-line px-2.5 py-1 text-[12px] text-dim hover:border-faint hover:text-ink"
        >
          <Icon name="refresh" size={12} /> Retry
        </button>
      </div>
    {/if}

    {#if !running && bn.status === 'done'}
      <div class="flex items-center gap-2 px-4 py-2.5 text-[10.5px] text-faint">
        <span class="min-w-0 flex-1 truncate" title={bn.text}>{bn.text || 'Done'}</span>
        <span class="shrink-0">
          codex{bn.finishedAt ? ` · ${Math.round((bn.finishedAt - bn.createdAt) / 1000)}s` : ''}{tokens ? ` · ${fmtTokens(tokens)} tok` : ''}
        </span>
      </div>
    {/if}
  </div>
</div>
