<script lang="ts">
  import type { Snippet } from "svelte";

  // One line of text that slowly pans to show its end when it doesn't fit,
  // pauses, and pans back. Text that fits stays still. `active` lets a list
  // pan only the row under the pointer.
  let {
    active = true,
    class: className = "",
    children,
  }: { active?: boolean; class?: string; children: Snippet } = $props();

  /** Pan speed in px per second. */
  const SPEED = 28;

  let box = $state<HTMLElement>();
  let inner = $state<HTMLElement>();
  let overflow = $state(0);

  $effect(() => {
    if (!box || !inner) return;
    const measure = () => {
      overflow = Math.max(0, Math.ceil(inner!.scrollWidth - box!.clientWidth));
    };
    const observer = new ResizeObserver(measure);
    observer.observe(box);
    observer.observe(inner);
    measure();
    return () => observer.disconnect();
  });

  // Pan time plus two pauses at either end (the keyframes hold 20% each).
  let duration = $derived(((overflow + 14) / SPEED / 0.6) * 2);
</script>

<span class="marquee {className}" class:fades={overflow > 0} bind:this={box}>
  <span
    class="inner"
    class:moving={active && overflow > 0}
    style:--shift="-{overflow + 14}px"
    style:--duration="{duration}s"
    bind:this={inner}
  >
    {@render children()}
  </span>
</span>

<style>
  .marquee {
    display: block;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
  }
  .marquee.fades {
    mask-image: linear-gradient(to right, black calc(100% - 14px), transparent);
  }
  .inner {
    display: inline-block;
  }
  .inner.moving {
    animation: pan var(--duration) ease-in-out infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .inner.moving {
      animation: none;
    }
  }
  @keyframes pan {
    0%,
    20% {
      transform: translateX(0);
    }
    50%,
    70% {
      transform: translateX(var(--shift));
    }
    100% {
      transform: translateX(0);
    }
  }
</style>
