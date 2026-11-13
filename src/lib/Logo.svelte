<script lang="ts">
  // The app icon drawn as vectors, so it stays sharp at header size: a dark
  // rounded square in a white outline, with a head and ears over five sound
  // bars. This is also the master for src-tauri/icons: render it at 1024 px
  // with a transparent background and run `npx tauri icon <png>`.
  let { size = 18 }: { size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 1932 1932" aria-hidden="true" data-tauri-drag-region>
  <rect width="1932" height="1932" rx="520" fill="#fff" />
  <rect x="84" y="84" width="1764" height="1764" rx="453" fill="#1a1b23" />
  <g transform="translate(0 34)" fill="none" stroke="#fff" stroke-width="124" stroke-linecap="round" stroke-linejoin="round">
    <path
      d="M279 882C279 560 330 433 450 433C560 433 680 545 760 638Q966 564 1172 638C1252 545 1372 433 1482 433C1602 433 1653 560 1653 882"
    />
    <path d="M523 1083V1227M745 979V1317M966 887V1420M1188 979V1317M1408 1083V1227" />
  </g>
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
