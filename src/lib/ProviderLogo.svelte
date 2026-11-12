<script lang="ts">
  import tidal from "../assets/tidal.svg";
  import spotify from "../assets/spotify.svg";
  import youtubeMusic from "../assets/youtube-music.svg";

  // Accepts the Rust ID ("Tidal", "YoutubeMusic") or the participant label
  // ("tidal", "youtube-music").
  let { provider, size = 16 }: { provider?: string | null; size?: number } = $props();

  const LOGOS: Record<string, string> = { tidal, spotify, youtubemusic: youtubeMusic };
  let url = $derived(provider ? LOGOS[provider.toLowerCase().replaceAll("-", "")] : undefined);
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
