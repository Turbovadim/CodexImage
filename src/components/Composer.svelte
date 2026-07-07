<script lang="ts">
  import { app, type PendingAttachment } from '../state.svelte.ts'
  import { isTyping } from '../hotkeys.ts'
  import Icon from './Icon.svelte'

  const ASPECTS = ['auto', '1:1', '16:9', '9:16', '3:2']
  const COUNTS = [1, 2, 3, 4]

  let aspect = $state('auto')
  let count = $state(1)
  let attachments = $state<PendingAttachment[]>([])
  let fileInput: HTMLInputElement | undefined = $state()
  let textarea: HTMLTextAreaElement | undefined = $state()
  let dragging = $state(false)

  function readAsBase64(file: File): Promise<string> {
    return new Promise(resolve => {
      const r = new FileReader()
      r.onload = () => resolve((r.result as string).split(',')[1])
      r.readAsDataURL(file)
    })
  }

  async function addFiles(files: Iterable<File>) {
    const added: PendingAttachment[] = []
    for (const file of files) {
      added.push({
        name: file.name || 'pasted.png',
        data: await readAsBase64(file),
        previewUrl: URL.createObjectURL(file),
      })
    }
    if (added.length) attachments.push(...added)
  }

  // keep the textarea sized to its content (also when a sample fills the draft)
  $effect(() => {
    void app.draft
    const el = textarea
    if (el) {
      el.style.height = 'auto'
      el.style.height = `${Math.min(el.scrollHeight, 160)}px`
    }
  })

  async function submit() {
    const trimmed = app.draft.trim()
    if (!trimmed) return
    app.draft = ''
    const toSend = attachments
    attachments = []
    try {
      await app.send(trimmed, { aspect, count }, toSend)
    } catch (err) {
      app.showError(err)
      app.draft = trimmed
      attachments = toSend
    }
  }
</script>

<svelte:window
  onkeydown={e => {
    if (app.editing) return
    if (e.key === '/' && !isTyping(e) && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault()
      textarea?.focus()
    } else if (e.key === 'Escape' && !app.lightbox) {
      if (app.target) app.target = null
      else if (document.activeElement === textarea) textarea?.blur()
    }
  }}
  ondragover={e => {
    if (e.dataTransfer?.types.includes('Files')) {
      e.preventDefault()
      dragging = true
    }
  }}
  ondragleave={e => {
    if (!e.relatedTarget) dragging = false
  }}
  ondrop={e => {
    e.preventDefault()
    dragging = false
    const files = [...(e.dataTransfer?.files ?? [])].filter(f => f.type.startsWith('image/'))
    if (files.length) void addFiles(files)
  }}
/>

{#if dragging}
  <div class="pointer-events-none fixed inset-0 z-40 flex items-center justify-center bg-bg/60">
    <div class="rounded-2xl border-2 border-dashed border-accent bg-raised/90 px-8 py-6 text-[14px] text-ink shadow-[0_18px_60px_rgba(0,0,0,.55)]">
      Drop images to attach as references
    </div>
  </div>
{/if}

{#snippet chip(label: string, onclick: () => void, active = false, title?: string)}
  <button
    {onclick}
    {title}
    class="rounded-full border px-2.5 py-1 text-[12px]
      {active
        ? 'border-accent bg-accent/10 text-accent'
        : 'border-line text-dim hover:border-faint hover:text-ink'}"
  >
    {label}
  </button>
{/snippet}

<div class="pointer-events-none absolute inset-x-0 bottom-0 z-20 flex justify-center px-6 pb-5">
  <div class="pointer-events-auto w-full max-w-[720px] rounded-2xl border border-line bg-raised/95 px-4 pt-3 pb-3.5 shadow-[0_18px_60px_rgba(0,0,0,.55)] backdrop-blur-md">
    {#if app.target}
      <div class="mb-2.5 flex items-center gap-2.5 rounded-xl border border-accent/30 bg-accent/8 px-2.5 py-1.5">
        {#if app.target.sourceImage || app.target.thumb}
          <img
            src={app.target.sourceImage || app.target.thumb}
            alt=""
            class="h-9 w-9 rounded-lg border border-line object-cover"
          />
        {/if}
        <div class="min-w-0 flex-1">
          <div class="text-[11px] font-medium text-accent">
            Branching from{app.target.sourceImage ? ' this image' : ''}
          </div>
          <div class="truncate text-[11.5px] text-dim">{app.target.prompt}</div>
        </div>
        <button
          onclick={() => (app.target = null)}
          title="Switch to starting a new root instead (Esc)"
          class="shrink-0 rounded-md p-1 text-faint hover:text-ink"
        >
          <Icon name="x" size={13} />
        </button>
      </div>
    {/if}

    <div class="mb-2.5 flex flex-wrap items-center gap-3">
      <div class="flex gap-1.5" title="Aspect ratio">
        {#each ASPECTS as a (a)}
          {@render chip(a === 'auto' ? 'Auto' : a, () => (aspect = a), aspect === a)}
        {/each}
      </div>
      <div class="flex gap-1.5" title="Number of parallel variations (each becomes its own node)">
        {#each COUNTS as n (n)}
          {@render chip(`×${n}`, () => (count = n), count === n)}
        {/each}
      </div>
      <button
        onclick={() => fileInput?.click()}
        title="Attach reference images"
        class="flex items-center rounded-full border border-line px-2.5 py-1 text-dim hover:border-faint hover:text-ink"
      >
        <Icon name="paperclip" size={13} />
      </button>
      <input
        bind:this={fileInput}
        type="file"
        accept="image/*"
        multiple
        hidden
        onchange={e => {
          const input = e.currentTarget
          if (input.files) void addFiles(input.files)
          input.value = ''
        }}
      />
    </div>

    {#if attachments.length > 0}
      <div class="mb-2.5 flex flex-wrap gap-2">
        {#each attachments as a, i (a.previewUrl)}
          <div class="relative">
            <img src={a.previewUrl} alt={a.name} class="h-12 w-12 rounded-lg border border-line object-cover" />
            <button
              onclick={() => (attachments = attachments.filter((_, j) => j !== i))}
              title="Remove"
              class="absolute -top-1.5 -right-1.5 flex size-[18px] items-center justify-center rounded-full bg-danger text-white"
            >
              <Icon name="x" size={10} />
            </button>
          </div>
        {/each}
      </div>
    {/if}

    <div class="flex items-end gap-2.5">
      <textarea
        bind:this={textarea}
        bind:value={app.draft}
        rows={1}
        placeholder={app.target
          ? 'Describe the change or continuation… (Enter to generate)'
          : 'Describe the image you want… (Enter to generate, Shift+Enter for a new line)'}
        onkeydown={e => {
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault()
            void submit()
          }
        }}
        onpaste={e => {
          const files = [...(e.clipboardData?.items ?? [])]
            .filter(i => i.type.startsWith('image/'))
            .map(i => i.getAsFile())
            .filter((f): f is File => f !== null)
          if (files.length) void addFiles(files)
        }}
        class="max-h-[160px] flex-1 resize-none rounded-xl border border-line bg-bg px-3.5 py-2.5
          text-[14px] text-ink outline-none placeholder:text-faint focus:border-accent"
      ></textarea>
      <button
        onclick={() => void submit()}
        class="rounded-[10px] border border-accent-strong bg-accent-strong px-4.5 py-2.5 text-[13.5px] font-medium text-white hover:border-accent hover:bg-accent"
      >
        {app.target ? 'Branch' : 'Generate'}
      </button>
    </div>
  </div>
</div>
