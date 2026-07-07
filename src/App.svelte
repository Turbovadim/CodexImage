<script lang="ts">
  import { onMount } from 'svelte'
  import { app } from './state.svelte.ts'
  import BoardSwitcher from './components/BoardSwitcher.svelte'
  import Canvas from './components/Canvas.svelte'
  import Composer from './components/Composer.svelte'
  import EditModal from './components/EditModal.svelte'
  import Lightbox from './components/Lightbox.svelte'

  const SAMPLES = [
    'A cozy cabin in a snowy forest at dusk, warm light in the windows',
    'Isometric illustration of a tiny home office, pastel palette',
    'Logo concept for a coffee brand called "Ember", minimal, flat',
    'Studio photo of a perfume bottle on black marble, dramatic lighting',
  ]

  const empty = $derived(!app.board || app.board.nodes.length === 0)

  onMount(() => {
    void app.init()
    const interval = setInterval(() => void app.refreshBoards(), 15000)
    return () => clearInterval(interval)
  })
</script>

<div class="flex h-screen">
  <main class="relative min-w-0 flex-1">
    <BoardSwitcher />
    {#if app.board && !empty}
      <Canvas />
    {/if}

    {#if empty}
      <div class="flex h-full flex-col items-center justify-center gap-4 px-6 pb-40 text-center text-dim">
        <div class="text-[42px] text-accent">❖</div>
        <h1 class="text-[26px] font-semibold text-ink">What should we create?</h1>
        <p class="max-w-lg">
          Prompt once — or ×4 for four takes side by side. Every generation becomes a node
          you can branch, continue, and regenerate on an infinite canvas.
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
          {#each [['/', 'prompt'], ['⌘K', 'boards'], ['F', 'fit view'], ['Esc', 'cancel']] as [key, label] (key)}
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
  {#if app.lightbox}
    {@const lb = app.lightbox}
    <Lightbox
      src={lb.src}
      onClose={() => (app.lightbox = null)}
      onBranch={() => app.branch(lb.node, lb.src)}
    />
  {/if}
  {#if app.editing}
    {#key app.editing.id}
      <EditModal node={app.editing} />
    {/key}
  {/if}
</div>
