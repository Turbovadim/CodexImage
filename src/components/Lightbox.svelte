<script lang="ts">
  import { app } from '../state.svelte.ts'
  import Icon, { type IconName } from './Icon.svelte'

  const ARROWS: Record<string, 'up' | 'down' | 'left' | 'right'> = {
    ArrowUp: 'up',
    ArrowDown: 'down',
    ArrowLeft: 'left',
    ArrowRight: 'right',
  }

  let { src, onClose, onBranch }: {
    src: string
    onClose: () => void
    onBranch?: () => void
  } = $props()

  function branch() {
    onBranch?.()
    onClose()
  }
</script>

<svelte:window
  onkeydown={e => {
    if (e.key === 'Escape') onClose()
    else if (e.key.toLowerCase() === 'b' && !e.metaKey && !e.ctrlKey && onBranch) branch()
    else if (ARROWS[e.key]) {
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
<div onclick={onClose} class="fixed inset-0 z-50 bg-bg/97">
  <img {src} alt="" class="h-full w-full object-contain" />
  <div
    onclick={e => e.stopPropagation()}
    class="absolute top-4 right-4 flex gap-2 opacity-90 transition-opacity hover:opacity-100"
  >
    {#if onBranch}
      {@render action('Branch from this image (B)', 'branch', { accent: true, text: 'Branch', onclick: branch })}
    {/if}
    {@render action('Download', 'download', { href: src, download: true })}
    {@render action('Open original', 'external', { href: src, newTab: true })}
    {@render action('Close (Esc)', 'x', { onclick: onClose })}
  </div>
</div>
