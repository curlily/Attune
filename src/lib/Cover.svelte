<script module lang="ts">
  import { SvelteSet } from "svelte/reactivity";

  // Art that couldn't be loaded (gone, blocked, offline): not asked for
  // again while Attune runs.
  const failed = new SvelteSet<string>();
</script>

<script lang="ts">
  import { fade } from "svelte/transition";
  import Icon from "./Icon.svelte";

  let {
    url,
    iconSize = 14,
    lazy = false,
    animate = false,
  }: { url: string | null | undefined; iconSize?: number; lazy?: boolean; animate?: boolean } = $props();

  let shown = $derived(url && !failed.has(url) ? url : null);
</script>

<!-- The note stays underneath: it shows while the image loads and when
     there is none. The parent sizes the box and positions the image. -->
<Icon name="music" size={iconSize} />
{#if shown}
  {#key shown}
    <img
      src={shown}
      alt=""
      loading={lazy ? "lazy" : "eager"}
      draggable="false"
      onerror={() => failed.add(shown)}
      transition:fade={{ duration: animate ? 300 : 0 }}
    />
  {/key}
{/if}
