<script lang="ts" module>
  // 24×24 outline icons, drawn for Attune. Each entry is a list of SVG path
  // strings stroked with the current text colour.
  const ICONS = {
    sliders: ["M4 7h10", "M18 7h2", "M4 17h4", "M12 17h8", "M16 5v4", "M10 15v4"],
    copy: ["M9 9h10v10H9z", "M5 15V5h10"],
    check: ["M5 12.5l4.5 4.5L19 7.5"],
    x: ["M6 6l12 12", "M18 6L6 18"],
    grip: ["M9 6h.01", "M15 6h.01", "M9 12h.01", "M15 12h.01", "M9 18h.01", "M15 18h.01"],
    back: ["M15 5l-7 7 7 7"],
    shrink: ["M5 15h4v4", "M19 9h-4V5", "M9 15l-5 5", "M15 9l5-5"],
    expand: ["M15 4h5v5", "M9 20H4v-5", "M20 4l-6 6", "M4 20l6-6"],
    music: ["M9 18V6l10-2v12", "M9 18a2.5 2.5 0 1 1-5 0a2.5 2.5 0 0 1 5 0", "M19 16a2.5 2.5 0 1 1-5 0a2.5 2.5 0 0 1 5 0"],
    alert: ["M12 4l9 16H3z", "M12 10v4", "M12 17h.01"],
    clock: ["M12 3a9 9 0 1 0 0 18a9 9 0 0 0 0-18", "M12 7v5l3 2"],
    eject: ["M12 5l7 9H5z", "M5 19h14"],
    refresh: ["M20 11a8 8 0 0 0-14.6-4.4L4 8", "M4 4v4h4", "M4 13a8 8 0 0 0 14.6 4.4L20 16", "M20 20v-4h-4"],
    cloudOff: ["M4 4l16 16", "M8.5 8.6A5 5 0 0 0 7 18h10", "M11 6.2A5 5 0 0 1 17.6 10 3.5 3.5 0 0 1 20 15.6"],
    logout: ["M14 4h5v16h-5", "M10 8l-4 4 4 4", "M6 12h9"],
    trash: ["M5 7h14", "M10 4h4", "M7 7l1 13h8l1-13"],
    pin: ["M9 4h6l-1 6 3 3H7l3-3z", "M12 13v7"],
    chevron: ["M7 10l5 5 5-5"],
  } satisfies Record<string, string[]>;

  export type IconName = keyof typeof ICONS;
</script>

<script lang="ts">
  let { name, size = 16, label }: { name: IconName; size?: number; label?: string } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  role={label ? "img" : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : "true"}
>
  {#each ICONS[name] as d}
    <path {d} />
  {/each}
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
