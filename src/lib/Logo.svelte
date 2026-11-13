<script lang="ts" module>
  // The Attune mark: a cat's head and ears over three bars, which read both
  // as a sound wave and as the cat's face (two eyes and a low nose). Drawn
  // as vectors so it stays sharp at any size; the same face at every size.
  //
  // This is the master for src-tauri/icons. Render it with a transparent
  // background: 1024 px with the tile for `npx tauri icon <png>`, then 16 px
  // natively into icon.ico (crisper than a downscale), the tile with swapped
  // colours as icon-light.png, and the bare face for tray.png. The app icon
  // keeps the tile (it must show on any background: taskbar, Alt+Tab,
  // Start, Explorer); the tray, like the system's own tray icons, uses the
  // bare face, recoloured for a light taskbar.

  const HEAD =
    "M279 882C279 560 330 433 450 433C560 433 680 545 760 638Q966 564 1172 638C1252 545 1372 433 1482 433C1602 433 1653 560 1653 882";
  const FACE = {
    stroke: 170,
    bars: "M650 990V1250M966 1210V1300M1282 990V1250",
    bottom: 1300,
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

  // The face's extent, strokes included.
  let box = $derived.by(() => {
    const half = FACE.stroke / 2;
    const x = 279 - half;
    const y = 433 - half;
    return { x, y, width: 1653 + half - x, height: FACE.bottom + half - y };
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
    stroke-width={FACE.stroke}
    stroke-linecap="round"
    stroke-linejoin="round"
  >
    <path d={HEAD} />
    <path d={FACE.bars} />
  </g>
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
