<script lang="ts" module>
  // 24×24 outline icons, drawn for Attune. Each entry is a list of SVG path
  // strings stroked with the current text colour.
  const ICONS = {
    // Eight teeth with rounded corners, and the hub.
    cog: [
      "M9.59 5.42Q10.42 5.08 10.51 4.18L10.59 3.39Q10.68 2.49 11.58 2.49L12.42 2.49Q13.32 2.49 13.41 3.39L13.49 4.18Q13.58 5.08 14.41 5.42L14.94 5.64Q15.78 5.99 16.47 5.42L17.09 4.91Q17.79 4.34 18.42 4.98L19.02 5.58Q19.66 6.21 19.09 6.91L18.58 7.53Q18.01 8.22 18.36 9.06L18.58 9.59Q18.92 10.42 19.82 10.51L20.61 10.59Q21.51 10.68 21.51 11.58L21.51 12.42Q21.51 13.32 20.61 13.41L19.82 13.49Q18.92 13.58 18.58 14.41L18.36 14.94Q18.01 15.78 18.58 16.47L19.09 17.09Q19.66 17.79 19.02 18.42L18.42 19.02Q17.79 19.66 17.09 19.09L16.47 18.58Q15.78 18.01 14.94 18.36L14.41 18.58Q13.58 18.92 13.49 19.82L13.41 20.61Q13.32 21.51 12.42 21.51L11.58 21.51Q10.68 21.51 10.59 20.61L10.51 19.82Q10.42 18.92 9.59 18.58L9.06 18.36Q8.22 18.01 7.53 18.58L6.91 19.09Q6.21 19.66 5.58 19.02L4.98 18.42Q4.34 17.79 4.91 17.09L5.42 16.47Q5.99 15.78 5.64 14.94L5.42 14.41Q5.08 13.58 4.18 13.49L3.39 13.41Q2.49 13.32 2.49 12.42L2.49 11.58Q2.49 10.68 3.39 10.59L4.18 10.51Q5.08 10.42 5.42 9.59L5.64 9.06Q5.99 8.22 5.42 7.53L4.91 6.91Q4.34 6.21 4.98 5.58L5.58 4.98Q6.21 4.34 6.91 4.91L7.53 5.42Q8.22 5.99 9.06 5.64Z",
      "M12 9a3 3 0 1 0 0 6a3 3 0 0 0 0-6",
    ],
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
    minimize: ["M6 12h12"],
    play: ["M8 5.5v13l10.5-6.5z"],
    skip: ["M6 5.5v13l9-6.5z", "M18 5v14"],
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
