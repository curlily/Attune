<script lang="ts">
  import type { Snippet } from "svelte";
  import { slide } from "svelte/transition";
  import { app, type SectionId } from "./app.svelte";
  import { collapse } from "./collapse";
  import Icon from "./Icon.svelte";

  // A titled part of the session view, below a divider. It can be folded to
  // its title (remembered), and leaves altogether, animated, when the window
  // gets too short for it (`shown`). Its content scrolls on its own when it
  // doesn't fit, fading out at the bottom while there's more below.
  let {
    id,
    title,
    count,
    shown = true,
    grow = false,
    maxHeight,
    children,
  }: {
    id: SectionId;
    title: string;
    count?: number;
    shown?: boolean;
    /** Take the room that's left in the column. */
    grow?: boolean;
    maxHeight?: string;
    children: Snippet;
  } = $props();

  let collapsed = $derived(app.collapsed[id]);

  /** Fades the bottom edge while there's more content below it. */
  function fadeOverflow(node: HTMLElement) {
    const update = () => {
      const more = node.scrollTop + node.clientHeight < node.scrollHeight - 1;
      node.classList.toggle("more-below", more);
    };
    const resized = new ResizeObserver(update);
    resized.observe(node);
    const changed = new MutationObserver(update);
    changed.observe(node, { childList: true, subtree: true });
    node.addEventListener("scroll", update, { passive: true });
    update();
    return {
      destroy() {
        resized.disconnect();
        changed.disconnect();
        node.removeEventListener("scroll", update);
      },
    };
  }
</script>

{#if shown}
<section class:grow={grow && !collapsed} style:max-height={maxHeight} transition:collapse>
  <button class="head" aria-expanded={!collapsed} onclick={() => app.toggleSection(id)}>
    <span class="section-title">{title}{count !== undefined ? ` · ${count}` : ""}</span>
    <span class="chevron" class:collapsed><Icon name="chevron" size={14} /></span>
  </button>
  {#if !collapsed}
    <div class="content scroll" use:fadeOverflow transition:slide={{ duration: 180 }}>
      {@render children()}
    </div>
  {/if}
</section>
{/if}

<style>
  section {
    display: flex;
    flex-direction: column;
    flex: none;
    min-height: 0;
    /* The divider above it. */
    margin-top: 14px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }
  section.grow {
    flex: 1 1 0;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    margin: 0 0 4px;
    padding: 2px 0;
    border: 0;
    background: none;
    color: var(--text-3);
    text-align: left;
    transition: color var(--fast);
  }
  .head:hover {
    color: var(--text-2);
  }
  .head .section-title {
    flex: 1;
    margin: 0;
    color: inherit;
  }
  .chevron {
    display: grid;
    transition: transform var(--base) var(--ease-out);
  }
  .chevron.collapsed {
    transform: rotate(-90deg);
  }

  .content {
    min-height: 0;
    flex: 0 1 auto;
  }
  .content:global(.more-below) {
    mask-image: linear-gradient(to bottom, black calc(100% - 32px), transparent);
  }
</style>
