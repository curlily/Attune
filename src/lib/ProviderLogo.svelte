<script lang="ts">
  import tidal from "../assets/tidal.svg";
  import spotify from "../assets/spotify.svg";

  // Accepts the Rust ID ("Tidal") or the participant label ("tidal").
  let { provider, size = 16 }: { provider?: string | null; size?: number } = $props();

  const LOGOS: Record<string, string> = { tidal, spotify };
  let url = $derived(provider ? LOGOS[provider.toLowerCase()] : undefined);
</script>

{#if url}
  <!-- Used as a mask so the logo takes the text colour. -->
  <span
    class="logo"
    style:width="{size}px"
    style:height="{size}px"
    style:mask-image={`url("${url}")`}
    style:-webkit-mask-image={`url("${url}")`}
    aria-hidden="true"
  ></span>
{/if}

<style>
  .logo {
    flex: none;
    display: inline-block;
    background: currentColor;
    mask-size: contain;
    mask-repeat: no-repeat;
    mask-position: center;
  }
</style>
