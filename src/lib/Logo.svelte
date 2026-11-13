<script lang="ts" module>
  // The Attune mark: a cat's head and ears over sound bars, drawn as
  // vectors so it stays sharp at any size.
  //
  // Two levels of detail, as in src-tauri/icons: five bars, and below 24 px
  // three bars with heavier strokes (icon.ico's 16 px image and tray.png),
  // since five bars run together that small. The three read as a sound wave
  // and as a face: two eyes and a low nose.
  //
  // This is the master for those files. Render it at the size needed with a
  // transparent background; for the large icons render 1024 px and run
  // `npx tauri icon <png>`. The app icon keeps the tile (it must show on
  // any background: taskbar, Alt+Tab, Start, Explorer); the tray, like the
  // system's own tray icons, uses the bare face (tray.png: white, small
  // detail, 1 px margin at 32 px), recoloured for a light taskbar.

  const HEAD =
    "M279 882C279 560 330 433 450 433C560 433 680 545 760 638Q966 564 1172 638C1252 545 1372 433 1482 433C1602 433 1653 560 1653 882";
  const DETAIL = {
    full: {
      stroke: 124,
      bars: "M523 1083V1227M745 979V1317M966 887V1420M1188 979V1317M1408 1083V1227",
      bottom: 1420,
    },
    small: {
      stroke: 170,
      bars: "M650 990V1250M966 1210V1300M1282 990V1250",
      bottom: 1300,
    },
  };
  /** The tile's side; the face is drawn on the same grid. */
  const TILE = 1932;
  /** Margin around the face on the tile. */
  const MARGIN = 0.1;
</script>

<script lang="ts">
  let {
    size = 18,
    background = true,
  }: {
    size?: number;
    /** The dark rounded tile (the app icon); without it, just the face. */
    background?: boolean;
  } = $props();

  let detail = $derived(size < 24 ? DETAIL.small : DETAIL.full);

  // The face's extent, strokes included.
  let box = $derived.by(() => {
    const half = detail.stroke / 2;
    const x = 279 - half;
    const y = 433 - half;
    return { x, y, width: 1653 + half - x, height: detail.bottom + half - y };
  });

  // On the tile: scaled to fill it, less the margin, and centred.
  let fit = $derived.by(() => {
    const scale = (TILE * (1 - 2 * MARGIN)) / Math.max(box.width, box.height);
    const x = (TILE - box.width * scale) / 2 - box.x * scale;
    const y = (TILE - box.height * scale) / 2 - box.y * scale;
    return `translate(${x} ${y}) scale(${scale})`;
  });

  // Without the tile: a square view just around the face.
  let viewBox = $derived.by(() => {
    if (background) return `0 0 ${TILE} ${TILE}`;
    const side = Math.max(box.width, box.height);
    const x = box.x - (side - box.width) / 2;
    const y = box.y - (side - box.height) / 2;
    return `${x} ${y} ${side} ${side}`;
  });
</script>

<svg width={size} height={size} {viewBox} aria-hidden="true" data-tauri-drag-region>
  {#if background}
    <rect width={TILE} height={TILE} rx="520" fill="#1a1b23" />
  {/if}
  <g
    transform={background ? fit : undefined}
    fill="none"
    stroke={background ? "#fff" : "currentColor"}
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
