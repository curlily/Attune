// Fold an element away (or open it) by its height, fading as it goes.
//
// Unlike Svelte's `slide`, this animates `max-height`, which also limits
// flex items that grow into the room left over (where `height` is ignored).

import { cubicOut } from "svelte/easing";
import type { TransitionConfig } from "svelte/transition";

export function collapse(node: HTMLElement, { duration = 220 } = {}): TransitionConfig {
  const style = getComputedStyle(node);
  const height = node.offsetHeight;
  const sizes = {
    "margin-top": parseFloat(style.marginTop),
    "padding-top": parseFloat(style.paddingTop),
    "padding-bottom": parseFloat(style.paddingBottom),
  };
  return {
    duration,
    easing: cubicOut,
    css: (t) =>
      `overflow: hidden; opacity: ${t}; max-height: ${t * height}px; min-height: 0;` +
      Object.entries(sizes)
        .map(([property, value]) => `${property}: ${t * value}px;`)
        .join(""),
  };
}
