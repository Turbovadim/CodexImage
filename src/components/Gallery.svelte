<script lang="ts">
  import { app } from '../state.svelte.ts'
  import { thumbUrl, thumbFallback } from '../media.ts'
  import type { BoardNode } from '../types.ts'
  import Icon from './Icon.svelte'

  interface TreeRow {
    node: BoardNode
    depth: number
    /** Vertical rails belonging to ancestors above the node's direct parent. */
    guides: boolean[]
    isLast: boolean
    childCount: number
    /** Index within a parallel root run; descendants do not have one. */
    rootTake?: number
  }

  interface BranchTree {
    roots: BoardNode[]
    rows: TreeRow[]
    imageCount: number
  }

  const nodes = $derived(app.board?.nodes ?? [])
  const imageCount = $derived(nodes.reduce((total, node) => total + node.images.length, 0))

  /**
   * Turn the board graph into a deterministic depth-first forest. Roots are
   * newest-first (the active explorations stay near the top), while each set
   * of branches reads chronologically. Orphans and accidental cycles are
   * promoted to roots rather than disappearing from the gallery.
   */
  const trees = $derived.by((): BranchTree[] => {
    const ordered = [...nodes].sort((a, b) => a.createdAt - b.createdAt || a.id.localeCompare(b.id))
    const byId = new Map(ordered.map(node => [node.id, node]))
    const children = new Map<string, BoardNode[]>()

    for (const node of ordered) {
      if (!node.parentId || !byId.has(node.parentId)) continue
      const siblings = children.get(node.parentId) ?? []
      siblings.push(node)
      children.set(node.parentId, siblings)
    }

    const visited = new Set<string>()
    const buildTree = (roots: BoardNode[]): BranchTree => {
      const rows: TreeRow[] = []
      let treeImages = 0

      const visit = (
        node: BoardNode,
        depth: number,
        guides: boolean[],
        isLast: boolean,
        rootTake?: number,
      ) => {
        if (visited.has(node.id)) return
        visited.add(node.id)
        const branchNodes = (children.get(node.id) ?? []).filter(child => !visited.has(child.id))
        rows.push({ node, depth, guides, isLast, childCount: branchNodes.length, rootTake })
        treeImages += node.images.length

        branchNodes.forEach((child, index) => {
          const childIsLast = index === branchNodes.length - 1
          const childGuides = depth === 0 ? [] : [...guides, !isLast]
          visit(child, depth + 1, childGuides, childIsLast)
        })
      }

      roots.forEach((root, index) => visit(root, 0, [], true, index))
      return { roots, rows, imageCount: treeImages }
    }

    const roots = ordered
      .filter(node => !node.parentId || !byId.has(node.parentId))

    const sameRequest = (a: BoardNode, b: BoardNode) =>
      a.prompt === b.prompt
      && a.aspect === b.aspect
      && a.sourceImages.join('\0') === b.sourceImages.join('\0')
      && a.attachments.join('\0') === b.attachments.join('\0')

    // Parallel root takes have no shared parent, so the generation run is their
    // common identity. Pre-runStartedAt boards used consecutive millisecond
    // timestamps for siblings; retain that persisted convention for migration.
    const rootRuns: BoardNode[][] = []
    for (const root of roots) {
      const run = rootRuns[rootRuns.length - 1]
      const previous = run?.[run.length - 1]
      const sameRun = previous && sameRequest(previous, root) && (
        (previous.runStartedAt !== undefined && previous.runStartedAt === root.runStartedAt)
        || (previous.runStartedAt === undefined && root.runStartedAt === undefined
          && run.length < 4 && root.createdAt === previous.createdAt + 1)
      )
      if (sameRun) run.push(root)
      else rootRuns.push([root])
    }

    const result = rootRuns
      .sort((a, b) => b[0].createdAt - a[0].createdAt)
      .map(buildTree)

    // Defensive fallback for malformed cyclic data: every generation remains reachable.
    for (const node of [...ordered].reverse()) {
      if (!visited.has(node.id)) result.push(buildTree([node]))
    }
    return result
  })

  function close() {
    app.gallery = false
  }

  function formatCreatedAt(timestamp: number): string {
    return new Date(timestamp).toLocaleString(undefined, {
      day: 'numeric',
      month: 'short',
      hour: '2-digit',
      minute: '2-digit',
    })
  }

  function statusLabel(node: BoardNode): string {
    if (node.status === 'running') return 'Generating'
    if (node.status === 'error') return 'Failed'
    if (node.status === 'stopped') return 'Stopped'
    return node.images.length === 1 ? '1 image' : `${node.images.length} images`
  }
</script>

<svelte:window
  onkeydown={e => {
    if (app.lightbox) return
    if (e.key === 'Escape') close()
  }}
/>

<div class="fixed inset-0 z-40 flex flex-col bg-bg">
  <header class="shrink-0 border-b border-line/80 px-5 py-3.5 sm:px-6">
    <div class="mx-auto flex max-w-[1680px] items-center gap-3">
      <div class="flex h-7 w-7 items-center justify-center rounded-lg bg-accent/10 text-accent">
        <Icon name="branch" size={14} />
      </div>
      <div class="min-w-0">
        <h2 class="text-[14px] leading-5 font-medium text-ink">Branch gallery</h2>
        <p class="truncate text-[11.5px] leading-4 text-dim">
          {imageCount} {imageCount === 1 ? 'image' : 'images'} · {nodes.length}
          {nodes.length === 1 ? 'generation' : 'generations'} · {trees.length}
          {trees.length === 1 ? 'exploration' : 'explorations'}
        </p>
      </div>
      <button
        onclick={close}
        title="Close (Esc or G)"
        aria-label="Close branch gallery"
        class="ml-auto flex h-8 w-8 items-center justify-center rounded-lg border border-line bg-raised text-dim
          hover:border-faint hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
      >
        <Icon name="x" size={14} />
      </button>
    </div>
  </header>

  {#if imageCount === 0}
    <div class="flex flex-1 flex-col items-center justify-center gap-1.5 px-6 text-center">
      <div class="text-[13px] font-medium text-dim">No images on this board yet</div>
      <div class="text-[12px] text-faint">Generated images will stay grouped with their branches here.</div>
    </div>
  {:else}
    <div class="min-h-0 flex-1 overflow-y-auto px-5 pb-8 sm:px-6">
      <div class="mx-auto max-w-[1680px]">
        {#each trees as tree (tree.roots.map(root => root.id).join(':'))}
          <section aria-label={`Exploration beginning with ${tree.roots[0].prompt}`} class="branch-tree py-5">
            <div class="mb-2.5">
              <div class="flex items-center gap-2 text-[10.5px] text-faint">
                <span class="h-px w-4 bg-line"></span>
                <span>Root exploration</span>
                {#if tree.roots.length > 1}
                  <span>·</span>
                  <span>{tree.roots.length} parallel takes</span>
                {/if}
                <span>·</span>
                <span>{tree.rows.length} {tree.rows.length === 1 ? 'generation' : 'generations'}</span>
                <span>·</span>
                <span>{tree.imageCount} {tree.imageCount === 1 ? 'image' : 'images'}</span>
              </div>
              {#if tree.roots.length > 1}
                <div class="mt-2 line-clamp-3 max-w-[680px] text-[12.5px] leading-[1.45] text-ink/85">
                  {tree.roots[0].prompt}
                </div>
              {/if}
            </div>

            <div>
              {#each tree.rows as row (row.node.id)}
                {@const node = row.node}
                <article class="generation-row border-t border-line/65 py-3.5 first:border-t-0">
                  <div class="tree-path min-w-0 pr-4">
                    {#each row.guides as continues}
                      <span class:continues class="tree-guide" aria-hidden="true"></span>
                    {/each}
                    {#if row.depth > 0}
                      <span class:last={row.isLast} class="tree-turn" aria-hidden="true"></span>
                    {/if}
                    <span class:has-children={row.childCount > 0} class="tree-node" aria-hidden="true">
                      <span class="flex h-5 w-5 items-center justify-center rounded-md border border-line bg-bg text-faint">
                        <Icon name={row.depth === 0 ? 'grid' : 'branch'} size={10} />
                      </span>
                    </span>

                    <div class="min-w-0 flex-1 pl-2.5">
                      <div class="line-clamp-2 text-[12.5px] leading-[1.45] text-ink/90" title={node.prompt}>
                        {row.depth === 0 && tree.roots.length > 1
                          ? `Take ${(row.rootTake ?? 0) + 1} of ${tree.roots.length}`
                          : node.prompt}
                      </div>

                      <div class="mt-1.5 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[10.5px] text-faint">
                        <span class="flex items-center gap-1.5">
                          <span
                            class="h-1.5 w-1.5 rounded-full
                              {node.status === 'running'
                                ? 'bg-accent'
                                : node.status === 'error'
                                  ? 'bg-danger'
                                  : 'bg-faint'}"
                          ></span>
                          {statusLabel(node)}
                        </span>
                        {#if row.childCount > 0}
                          <span>· {row.childCount} {row.childCount === 1 ? 'branch' : 'branches'}</span>
                        {/if}
                        <span>· {formatCreatedAt(node.createdAt)}</span>
                      </div>

                      {#if node.sourceImages.length > 0}
                        <div class="mt-2 flex items-center gap-1.5 text-[10.5px] text-faint">
                          <span>From</span>
                          <div class="flex -space-x-1">
                            {#each node.sourceImages.slice(0, 3) as source (source)}
                              <img
                                src={thumbUrl(source)}
                                onerror={thumbFallback(source)}
                                alt=""
                                loading="lazy"
                                decoding="async"
                                class="h-5 w-5 rounded border border-bg bg-raised object-cover"
                              />
                            {/each}
                          </div>
                          {#if node.sourceImages.length > 3}<span>+{node.sourceImages.length - 3}</span>{/if}
                        </div>
                      {/if}

                      <button
                        onclick={() => app.locateNode(node.id)}
                        title="Show generation on canvas"
                        class="mt-2 flex items-center gap-1.5 rounded-md px-1.5 py-1 text-[10.5px] text-dim hover:bg-hover
                          hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent"
                      >
                        <Icon name="locate" size={11} />
                        Show on canvas
                      </button>
                    </div>
                  </div>

                  {#if node.images.length > 0}
                    <div class="image-strip">
                      {#each node.images as src, index (`${node.id}:${src}`)}
                        <button
                          onclick={() => app.openImage(src, node)}
                          title={`Open output ${index + 1}: ${node.prompt}`}
                          aria-label={`Open output ${index + 1} from ${node.prompt}`}
                          class="group/image relative min-w-0 cursor-zoom-in overflow-hidden rounded-lg bg-raised
                            outline outline-1 -outline-offset-1 outline-line hover:outline-faint
                            focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
                        >
                          <img
                            src={thumbUrl(src)}
                            onerror={thumbFallback(src)}
                            alt=""
                            loading="lazy"
                            decoding="async"
                            class="aspect-square w-full object-cover"
                          />
                          {#if node.images.length > 1}
                            <span
                              class="absolute right-1.5 bottom-1.5 rounded bg-black/65 px-1.5 py-0.5 text-[9.5px] tabular-nums text-white/80"
                            >
                              {index + 1}/{node.images.length}
                            </span>
                          {/if}
                        </button>
                      {/each}
                    </div>
                  {:else}
                    <div class="flex min-h-24 items-center justify-center rounded-lg border border-dashed border-line/70 bg-raised/35 px-4 text-[11px] text-faint">
                      {node.status === 'running' ? 'Waiting for first output…' : statusLabel(node)}
                    </div>
                  {/if}
                </article>
              {/each}
            </div>
          </section>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .branch-tree + .branch-tree {
    border-top: 1px solid color-mix(in srgb, var(--color-line) 82%, transparent);
  }

  .generation-row {
    display: grid;
    grid-template-columns: minmax(240px, 320px) minmax(0, 1fr);
    gap: 20px;
  }

  .image-strip {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(132px, 184px));
    gap: 8px;
    align-content: start;
  }

  .tree-path {
    display: flex;
    align-items: stretch;
  }

  .tree-guide,
  .tree-turn {
    position: relative;
    width: 16px;
    flex: 0 0 16px;
    min-height: 20px;
  }

  .tree-guide.continues::before {
    position: absolute;
    top: -15px;
    bottom: -15px;
    left: 7px;
    width: 1px;
    background: var(--color-line);
    content: '';
  }

  .tree-turn::before {
    position: absolute;
    top: -15px;
    bottom: -15px;
    left: 7px;
    width: 1px;
    background: var(--color-line);
    content: '';
  }

  .tree-turn.last::before {
    bottom: calc(100% - 10px);
  }

  .tree-turn::after {
    position: absolute;
    top: 10px;
    right: 0;
    left: 7px;
    height: 1px;
    background: var(--color-line);
    content: '';
  }

  .tree-node {
    position: relative;
    width: 20px;
    flex: 0 0 20px;
    min-height: 20px;
  }

  .tree-node.has-children::after {
    position: absolute;
    top: 20px;
    bottom: -15px;
    left: 9px;
    width: 1px;
    background: var(--color-line);
    content: '';
  }

  @media (max-width: 760px) {
    .generation-row {
      grid-template-columns: minmax(0, 1fr);
      gap: 10px;
    }

    .image-strip {
      padding-left: 30px;
      grid-template-columns: repeat(auto-fit, minmax(112px, 184px));
    }
  }
</style>
