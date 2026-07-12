<script lang="ts">
  import { onMount } from 'svelte'
  import { app } from './state.svelte.ts'
  import { isKey, isTyping } from './hotkeys.ts'
  import BoardSwitcher from './components/BoardSwitcher.svelte'
  import Canvas from './components/Canvas.svelte'
  import Composer from './components/Composer.svelte'
  import EditModal from './components/EditModal.svelte'
  import Gallery from './components/Gallery.svelte'
  import Icon from './components/Icon.svelte'
  import Lightbox from './components/Lightbox.svelte'

  const SAMPLES = [
    'A cozy cabin in a snowy forest at dusk, warm light in the windows',
    'Isometric illustration of a tiny home office, pastel palette',
    'Logo concept for a coffee brand called "Ember", minimal, flat',
    'Studio photo of a perfume bottle on black marble, dramatic lighting',
  ]

  const empty = $derived(!app.board || app.board.nodes.length === 0)
  const hasImages = $derived((app.board?.nodes ?? []).some(n => n.images.length > 0))

  onMount(() => {
    void app.init()
    const interval = setInterval(() => void app.refreshBoards(), 15000)
    return () => clearInterval(interval)
  })
</script>

<svelte:window
  onkeydown={e => {
    if (e.metaKey || e.ctrlKey || e.altKey || isTyping(e)) return
    if (isKey(e, 'g') && !app.lightbox && !app.editing && hasImages) {
      e.preventDefault()
      app.gallery = !app.gallery
    }
  }}
/>

<div class="flex h-screen">
  <main class="relative min-w-0 flex-1">
    <BoardSwitcher />

    {#if hasImages}
      <button
        onclick={() => (app.gallery = true)}
        title="All images on this board (G)"
        class="absolute top-4 right-4 z-30 flex items-center gap-2 rounded-xl border border-line bg-raised/95 px-3 py-2
          text-[12.5px] text-dim shadow-[0_8px_30px_rgba(0,0,0,.35)] backdrop-blur-md hover:border-faint hover:text-ink"
      >
        <Icon name="grid" size={14} /> Gallery
      </button>
    {/if}

    {#if app.board && !empty}
      <Canvas />
    {/if}

    {#if empty}
      <div class="flex h-full flex-col items-center justify-center gap-4 px-6 pb-40 text-center text-dim">
        <div class="text-[42px] text-accent">❖</div>
        <h1 class="text-[26px] font-semibold text-ink">What should we create?</h1>
        <p class="max-w-lg">
          Ask for one image or a complete ordered series — Codex decides the set. Use ×4 for
          parallel takes, then branch, continue, and regenerate on an infinite canvas.
        </p>
        <div class="flex max-w-[560px] flex-wrap justify-center gap-2">
          {#each SAMPLES as s (s)}
            <button
              onclick={() => (app.draft = s)}
              class="rounded-full border border-line px-3 py-1.5 text-[12.5px] text-dim hover:border-faint hover:text-ink"
            >
              {s}
            </button>
          {/each}
        </div>
        <div class="mt-3 flex items-center gap-3 text-[11.5px] text-faint">
          {#each [['/', 'prompt'], ['⌘K', 'boards'], ['G', 'gallery'], ['F', 'fit view'], ['Esc', 'cancel']] as [key, label] (key)}
            <span class="flex items-center gap-1.5">
              <kbd class="rounded-md border border-line bg-raised px-1.5 py-0.5 font-sans">{key}</kbd>
              {label}
            </span>
          {/each}
        </div>
      </div>
    {/if}

    <Composer />
  </main>

  {#if app.gallery}
    <Gallery />
  {/if}
  {#if app.lightbox}
    {@const lb = app.lightbox}
    <Lightbox
      src={lb.src}
      node={lb.node}
      onClose={() => (app.lightbox = null)}
      onBranch={() => app.branch(lb.node, lb.src)}
    />
  {/if}
  {#if app.editing}
    {#key app.editing.id}
      <EditModal node={app.editing} />
    {/key}
  {/if}

  {#if app.toast}
    {@const toast = app.toast}
    <div
      class="fixed top-5 left-1/2 z-[60] flex -translate-x-1/2 items-center gap-3 rounded-xl border
        bg-raised/95 py-2 pr-2 pl-4 text-[13px] shadow-[0_18px_60px_rgba(0,0,0,.55)] backdrop-blur-md
        {toast.error ? 'border-danger/50 text-danger' : 'border-line text-ink'}"
    >
      <span>{toast.text}</span>
      {#if toast.action}
        {@const action = toast.action}
        <button
          onclick={() => action.fn()}
          class="rounded-lg border border-accent-strong bg-accent-strong/15 px-2.5 py-1 text-[12.5px] font-medium text-accent hover:bg-accent-strong/25"
        >
          {action.label}
        </button>
      {/if}
      <button onclick={() => app.dismissToast()} title="Dismiss" class="rounded-md p-1 text-faint hover:text-ink">
        <Icon name="x" size={12} />
      </button>
    </div>
  {/if}
</div>
