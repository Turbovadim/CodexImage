<script lang="ts">
  import { untrack } from 'svelte'
  import { app } from '../state.svelte.ts'
  import { thumbUrl, thumbFallback, fmtTokens } from '../media.ts'
  import Icon from './Icon.svelte'

  let open = $state(false)
  let query = $state('')
  let rootEl: HTMLDivElement | undefined = $state()
  let searchEl: HTMLInputElement | undefined = $state()
  let renamingId = $state<string | null>(null)
  let renameDraft = $state('')

  function startRename(id: string, title: string) {
    renamingId = id
    renameDraft = title
  }

  function commitRename() {
    const id = renamingId
    renamingId = null
    if (id && renameDraft.trim()) void app.renameBoard(id, renameDraft)
  }

  const active = $derived(app.boards.find(b => b.id === app.board?.id))
  const anyGenerating = $derived(app.boards.some(b => b.generating))
  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase()
    return q ? app.boards.filter(b => b.title.toLowerCase().includes(q)) : app.boards
  })

  function timeAgo(ts: number): string {
    const s = Math.floor((Date.now() - ts) / 1000)
    if (s < 60) return 'just now'
    if (s < 3600) return `${Math.floor(s / 60)}m ago`
    if (s < 86400) return `${Math.floor(s / 3600)}h ago`
    return new Date(ts).toLocaleDateString()
  }

  $effect(() => {
    if (!open) return
    untrack(() => {
      query = ''
      searchEl?.focus()
    })
    const onDown = (e: MouseEvent) => {
      if (!rootEl?.contains(e.target as Node)) open = false
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') open = false
    }
    document.addEventListener('mousedown', onDown)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onDown)
      document.removeEventListener('keydown', onKey)
    }
  })
</script>

<svelte:window
  onkeydown={e => {
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k') {
      e.preventDefault()
      open = !open
    }
  }}
/>

<div bind:this={rootEl} class="absolute top-4 left-4 z-30">
  <button
    onclick={() => (open = !open)}
    title="Switch board (⌘K)"
    class="flex items-center gap-2.5 rounded-xl border px-3.5 py-2 shadow-[0_8px_30px_rgba(0,0,0,.35)] backdrop-blur-md
      {open ? 'border-faint bg-hover' : 'border-line bg-raised/95 hover:border-faint'}"
  >
    <span class="text-[15px] text-accent">❖</span>
    <span class="max-w-[220px] truncate text-[13.5px] font-medium text-ink">
      {active?.title ?? 'CodexImage'}
    </span>
    {#if anyGenerating}
      <span class="size-[13px] shrink-0 animate-spin rounded-full border-2 border-line border-t-accent"></span>
    {/if}
    <Icon name="chevronDown" size={13} class="text-faint transition-transform {open ? 'rotate-180' : ''}" />
  </button>

  {#if open}
    <div class="mt-2 w-[320px] overflow-hidden rounded-2xl border border-line bg-raised/95 shadow-[0_18px_60px_rgba(0,0,0,.55)] backdrop-blur-md">
      <div class="p-2.5 pb-1.5">
        <input
          bind:this={searchEl}
          bind:value={query}
          placeholder="Search boards…"
          class="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] text-ink outline-none placeholder:text-faint focus:border-accent"
        />
      </div>

      <div class="max-h-[52vh] overflow-y-auto px-1.5 py-1">
        {#if filtered.length === 0}
          <div class="px-3 py-4 text-center text-[12.5px] text-faint">No boards found</div>
        {/if}
        {#each filtered as b (b.id)}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <div
            onclick={() => { void app.openBoard(b.id); open = false }}
            class="group flex cursor-pointer items-center gap-2.5 rounded-[10px] px-2.5 py-2
              {b.id === app.board?.id ? 'bg-hover text-ink' : 'text-dim hover:bg-hover hover:text-ink'}"
          >
            {#if b.lastImage}
              <img
                src={thumbUrl(b.lastImage)}
                onerror={thumbFallback(b.lastImage)}
                alt=""
                loading="lazy"
                class="h-[30px] w-[30px] shrink-0 rounded-md border border-line object-cover"
              />
            {:else}
              <div class="flex h-[30px] w-[30px] shrink-0 items-center justify-center rounded-md border border-line bg-bg text-[13px] text-faint">❖</div>
            {/if}
            <div class="min-w-0 flex-1">
              {#if renamingId === b.id}
                <!-- svelte-ignore a11y_autofocus — the input appears on explicit rename intent -->
                <input
                  bind:value={renameDraft}
                  autofocus
                  onclick={e => e.stopPropagation()}
                  onblur={commitRename}
                  onkeydown={e => {
                    e.stopPropagation()
                    if (e.key === 'Enter') commitRename()
                    if (e.key === 'Escape') renamingId = null
                  }}
                  class="w-full rounded-md border border-accent bg-bg px-1.5 py-0.5 text-[13px] text-ink outline-none"
                />
              {:else}
                <div class="truncate text-[13px]" ondblclick={e => { e.stopPropagation(); startRename(b.id, b.title) }}>
                  {b.title}
                </div>
              {/if}
              <div class="text-[11px] text-faint">
                {b.imageCount} image{b.imageCount === 1 ? '' : 's'}{b.totalTokens ? ` · ${fmtTokens(b.totalTokens)} tok` : ''} · {timeAgo(b.updatedAt)}
              </div>
            </div>
            {#if b.generating}
              <div class="size-[13px] shrink-0 animate-spin rounded-full border-2 border-line border-t-accent"></div>
            {/if}
            <button
              onclick={e => { e.stopPropagation(); startRename(b.id, b.title) }}
              title="Rename board"
              class="shrink-0 rounded-md p-1 text-faint opacity-0 group-hover:opacity-100 hover:bg-hover hover:text-ink"
            >
              <Icon name="pencil" size={13} />
            </button>
            <button
              onclick={e => { e.stopPropagation(); void app.deleteBoard(b.id) }}
              title="Delete board"
              class="shrink-0 rounded-md p-1 text-faint opacity-0 group-hover:opacity-100 hover:bg-danger/10 hover:text-danger"
            >
              <Icon name="trash" size={13} />
            </button>
          </div>
        {/each}
      </div>

      <button
        onclick={() => { void app.newBoard(); open = false }}
        class="flex w-full items-center gap-2 border-t border-line px-4 py-2.5 text-[13px] font-medium text-accent hover:bg-hover"
      >
        <Icon name="plus" size={13} /> New board
      </button>
    </div>
  {/if}
</div>
