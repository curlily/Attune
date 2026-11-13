<script lang="ts">
  // The app icon drawn as vectors, so it stays sharp at any size: a dark
  // rounded square, with a head and ears over sound bars, optionally in a
  // white outline (the app icon has one; the header doesn't need it).
  //
  // Two levels of detail, as in src-tauri/icons: five bars, and below 24 px
  // three bars with heavier strokes (icon.ico's 16 px image and tray.png),
  // since five bars run together that small. This is the master for those
  // files: render at the size needed with a transparent background; for the
  // large icons run `npx tauri icon <1024 px png>`.
  let { size = 18, border = true }: { size?: number; border?: boolean } = $props();

  const HEAD =
    "M279 882C279 560 330 433 450 433C560 433 680 545 760 638Q966 564 1172 638C1252 545 1372 433 1482 433C1602 433 1653 560 1653 882";
  const DETAIL = {
    full: {
      stroke: 124,
      ring: 84,
      bars: "M523 1083V1227M745 979V1317M966 887V1420M1188 979V1317M1408 1083V1227",
    },
    small: { stroke: 170, ring: 110, bars: "M650 1040V1290M966 920V1400M1282 1040V1290" },
  };

  let detail = $derived(size < 24 ? DETAIL.small : DETAIL.full);
  let ring = $derived(border ? detail.ring : 0);
</script>

<svg width={size} height={size} viewBox="0 0 1932 1932" aria-hidden="true" data-tauri-drag-region>
  {#if border}
    <rect width="1932" height="1932" rx="520" fill="#fff" />
  {/if}
  <rect
    x={ring}
    y={ring}
    width={1932 - 2 * ring}
    height={1932 - 2 * ring}
    rx={Math.round(520 - ring * 0.8)}
    fill="#1a1b23"
  />
  <g
    transform="translate(0 34)"
    fill="none"
    stroke="#fff"
    stroke-width={detail.stroke}
    stroke-linecap="round"
    stroke-linejoin="round"
  >
    <path d={HEAD} />
    <path d={detail.bars} />
  </g>
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
