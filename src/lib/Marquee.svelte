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

  // The left edge fades while the text's start is out of view.
  let panned = $state(false);
  let fadeTime = $state(FADE_TIME);

  // Each pan is an animation of its own, started by a timer: while the
  // text rests nothing animates, so nothing is drawn. (One endless
  // animation with the rests in it, plus the fade animated as a custom
  // property, redrew the text every frame for as long as it was shown.)
  $effect(() => {
    if (!box || !inner || !active || overflow === 0) return;
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const el = inner;
    const shift = overflow + FADE;
    const pan = (shift / SPEED) * 1000;
    fadeTime = Math.min(FADE_TIME, pan / 2000);
    const timing = { duration: pan, easing: "ease-in-out", fill: "forwards" as const };
    const left = `translateX(-${shift}px)`;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let animations: Animation[] = [];
    let stopped = false;

    const after = (ms: number, next: () => void) => {
      timer = setTimeout(() => !stopped && next(), ms);
    };
    const panOut = () => {
      panned = true;
      const out = el.animate([{ transform: "translateX(0)" }, { transform: left }], timing);
      animations = [out];
      out.onfinish = () => !stopped && after(REST_END * 1000, panBack);
    };
    const panBack = () => {
      const back = el.animate([{ transform: left }, { transform: "translateX(0)" }], timing);
      animations.push(back);
      // The fade goes as the text arrives back.
      after(pan - fadeTime * 1000, () => (panned = false));
      back.onfinish = () => {
        if (stopped) return;
        animations.forEach((animation) => animation.cancel());
        animations = [];
        rest();
      };
    };
    const rest = () => after(REST_START * 1000, panOut);

    rest();
    return () => {
      stopped = true;
      clearTimeout(timer);
      animations.forEach((animation) => animation.cancel());
      panned = false;
    };
  });
</script>

<span
  class="marquee {className}"
  class:fades={overflow > 0}
  class:panned
  style:--fade-time="{fadeTime}s"
  bind:this={box}
>
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
    transition: --fade-left var(--fade-time) ease;
  }
  .marquee.panned {
    --fade-left: 14px;
  }
  .inner {
    display: inline-block;
  }
</style>
