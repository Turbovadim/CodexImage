<script lang="ts">
  import { app } from '../state.svelte.ts'
  import { thumbUrl, thumbFallback } from '../media.ts'
  import type { BoardNode } from '../types.ts'
  import Icon from './Icon.svelte'

  interface Entry {
    src: string
    node: BoardNode
  }

  // newest nodes first; a node's own images keep generation order
  const entries = $derived.by((): Entry[] => {
    const nodes = [...(app.board?.nodes ?? [])].sort((a, b) => b.createdAt - a.createdAt)
    return nodes.flatMap(node => node.images.map(src => ({ src, node })))
  })

  function close() {
    app.gallery = false
  }
</script>

<svelte:window
  onkeydown={e => {
    if (app.lightbox) return
    if (e.key === 'Escape') close()
  }}
/>

<div class="fixed inset-0 z-40 flex flex-col bg-bg/97 backdrop-blur-sm">
  <div class="flex items-center gap-3 px-6 py-4">
    <Icon name="grid" size={16} class="text-accent" />
    <h2 class="text-[15px] font-medium text-ink">
      All images
      <span class="text-faint">· {entries.length}</span>
    </h2>
    <button
      onclick={close}
      title="Close (Esc or G)"
      class="ml-auto rounded-lg border border-line bg-raised/80 p-2 text-dim hover:border-faint hover:text-ink"
    >
      <Icon name="x" size={14} />
    </button>
  </div>

  {#if entries.length === 0}
    <div class="flex flex-1 items-center justify-center text-[13px] text-faint">No images on this board yet</div>
  {:else}
    <div class="min-h-0 flex-1 overflow-y-auto px-6 pb-6">
      <div class="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-3">
        {#each entries as { src, node } (src)}
          <div class="group relative overflow-hidden rounded-xl border border-line bg-raised">
            <button
              onclick={() => app.openImage(src, node)}
              title={node.prompt}
              class="block w-full cursor-zoom-in"
            >
              <img
                src={thumbUrl(src)}
                onerror={thumbFallback(src)}
                alt=""
                loading="lazy"
                decoding="async"
                class="aspect-square w-full object-cover"
              />
            </button>
            <div
              class="pointer-events-none absolute inset-x-0 bottom-0 flex items-end gap-2 bg-gradient-to-t from-black/75 to-transparent
                p-2.5 pt-8 opacity-0 transition-opacity group-hover:opacity-100"
            >
              <div class="line-clamp-2 min-w-0 flex-1 text-[11px] leading-snug text-white/90">{node.prompt}</div>
              <button
                onclick={() => app.locateNode(node.id)}
                title="Show on canvas"
                class="pointer-events-auto shrink-0 rounded-lg border border-white/20 bg-black/50 p-1.5 text-white/90 hover:border-white/50 hover:text-white"
              >
                <Icon name="locate" size={13} />
              </button>
            </div>
          </div>
        {/each}
      </div>
    </div>
  {/if}
</div>
