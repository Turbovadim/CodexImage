<script lang="ts">
  let { since }: { since: number } = $props()

  let now = $state(Date.now())

  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000)
    return () => clearInterval(t)
  })
</script>

{Math.max(0, Math.round((now - since) / 1000))}s
