<script lang="ts" module>
  // The left fade is animated through this property, so it has to be
  // registered as a length (an unregistered one would jump, not ease).
  try {
    CSS.registerProperty({ name: "--fade-left", syntax: "<length>", inherits: false, initialValue: "0px" });
  } catch {
    // Registered already (hot reload), or not supported: the fade jumps.
  }
</script>

<script lang="ts">
  import type { Snippet } from "svelte";

  // One line of text that slowly pans to show its end when it doesn't fit,
  // pauses, and pans back. Text that fits stays still. `active` lets a list
  // pan only the row under the pointer. While the start of the text is
  // panned out of view, the left edge fades like the right one.
  let {
    active = true,
    class: className = "",
    children,
  }: { active?: boolean; class?: string; children: Snippet } = $props();

  /** Pan speed in px per second. */
  const SPEED = 18;
  /** Seconds the text rests at its start before panning, and at its end. */
  const REST_START = 3.5;
  const REST_END = 2;
  /** Width of the faded edges, and room left after the text's end. */
  const FADE = 14;
  /** Seconds the left fade takes to come in or go. */
  const FADE_TIME = 0.4;

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

  $effect(() => {
    if (!box || !inner || !active || overflow === 0) return;
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const shift = overflow + FADE;
    const pan = shift / SPEED;
    const total = REST_START + pan + REST_END + pan;
    const at = (seconds: number) => seconds / total;
    const ease = "ease-in-out";
    const timing = { duration: total * 1000, iterations: Infinity };

    const moving = inner.animate(
      [
        { offset: 0, transform: "translateX(0)" },
        { offset: at(REST_START), transform: "translateX(0)", easing: ease },
        { offset: at(REST_START + pan), transform: `translateX(-${shift}px)` },
        { offset: at(REST_START + pan + REST_END), transform: `translateX(-${shift}px)`, easing: ease },
        { offset: 1, transform: "translateX(0)" },
      ],
      timing,
    );
    // The fade comes in as the text starts to leave, and goes as it's back.
    const fadeTime = Math.min(FADE_TIME, pan / 2);
    const fading = box.animate(
      [
        { offset: 0, "--fade-left": "0px" },
        { offset: at(REST_START), "--fade-left": "0px" },
        { offset: at(REST_START + fadeTime), "--fade-left": `${FADE}px` },
        { offset: at(total - fadeTime), "--fade-left": `${FADE}px` },
        { offset: 1, "--fade-left": "0px" },
      ],
      timing,
    );
    return () => {
      moving.cancel();
      fading.cancel();
    };
  });
</script>

<span class="marquee {className}" class:fades={overflow > 0} bind:this={box}>
  <span class="inner" bind:this={inner}>
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
    mask-image: linear-gradient(
      to right,
      transparent,
      black var(--fade-left),
      black calc(100% - 14px),
      transparent
    );
  }
  .inner {
    display: inline-block;
  }
</style>
