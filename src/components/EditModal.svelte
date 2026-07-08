<script lang="ts">
  import { app } from '../state.svelte.ts'
  import { thumbUrl, thumbFallback } from '../media.ts'
  import type { BoardNode } from '../types.ts'
  import Icon from './Icon.svelte'

  let { node }: { node: BoardNode } = $props()

  const ASPECTS = ['auto', '1:1', '16:9', '9:16', '3:2']

  // svelte-ignore state_referenced_locally — seeded once by design; App keys this modal by node id
  let draft = $state(node.prompt)
  // svelte-ignore state_referenced_locally — seeded once by design
  let aspect = $state(node.aspect)
  const lastImage = $derived(node.images[node.images.length - 1])

  function close() {
    app.editing = null
  }

  function save() {
    // read `node` BEFORE close(): the prop is a live getter into app.editing,
    // which close() nulls out
    const next = draft.trim()
    if (next) app.edit(node, next, aspect)
    close()
  }

  function focusEnd(el: HTMLTextAreaElement) {
    el.focus()
    el.setSelectionRange(el.value.length, el.value.length)
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div onclick={close} class="fixed inset-0 z-50 flex items-center justify-center bg-bg/80 px-6 backdrop-blur-sm">
  <div
    onclick={e => e.stopPropagation()}
    class="w-full max-w-[560px] rounded-2xl border border-line bg-raised p-4 shadow-[0_24px_80px_rgba(0,0,0,.6)]"
  >
    <div class="mb-3 flex items-center gap-2.5">
      {#if lastImage}
        <img
          src={thumbUrl(lastImage)}
          onerror={thumbFallback(lastImage)}
          alt=""
          class="h-10 w-10 rounded-lg border border-line object-cover"
        />
      {/if}
      <div class="min-w-0 flex-1">
        <div class="text-[13.5px] font-medium text-ink">Edit prompt</div>
        <div class="text-[11.5px] text-faint">Regenerates this node in a fresh session</div>
      </div>
      <button onclick={close} title="Close (Esc)" class="rounded-md p-1 text-faint hover:text-ink">
        <Icon name="x" size={14} />
      </button>
    </div>

    <textarea
      use:focusEnd
      bind:value={draft}
      rows={7}
      onkeydown={e => {
        e.stopPropagation()
        if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); save() }
        if (e.key === 'Escape') close()
      }}
      class="w-full resize-none rounded-xl border border-line bg-bg px-3.5 py-2.5 text-[13.5px] leading-relaxed
        text-ink outline-none placeholder:text-dim focus:border-accent"
    ></textarea>

    <div class="mt-2.5 flex gap-1.5" title="Aspect ratio">
      {#each ASPECTS as a (a)}
        <button
          onclick={() => (aspect = a)}
          class="rounded-full border px-2.5 py-1 text-[12px]
            {aspect === a
              ? 'border-accent bg-accent/10 text-accent'
              : 'border-line text-dim hover:border-faint hover:text-ink'}"
        >
          {a === 'auto' ? 'Auto' : a}
        </button>
      {/each}
    </div>

    <div class="mt-3 flex items-center gap-2">
      <span class="text-[11px] text-faint">Enter to save · Shift+Enter for a new line</span>
      <div class="ml-auto flex gap-2">
        <button
          onclick={close}
          class="rounded-lg border border-line px-3 py-1.5 text-[12.5px] text-dim hover:border-faint hover:text-ink"
        >
          Cancel
        </button>
        <button
          onclick={save}
          class="flex items-center gap-1.5 rounded-lg border border-accent-strong bg-accent-strong px-3 py-1.5
            text-[12.5px] font-medium text-white hover:bg-accent"
        >
          <Icon name="refresh" size={12} /> Save & regenerate
        </button>
      </div>
    </div>
  </div>
</div>
