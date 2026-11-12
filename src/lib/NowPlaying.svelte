<script lang="ts">
  import { fade } from "svelte/transition";
  import type { RuntimeStatus } from "./api";
  import { app } from "./app.svelte";
  import { currentCover, livePosition } from "./current.svelte";
  import Icon from "./Icon.svelte";
  import { formatTime, ownBadge } from "./status";

  let { status }: { status: RuntimeStatus } = $props();

  let current = $derived(status.queue[0]);
  let badge = $derived(ownBadge(status.sync));
  let warning = $derived(status.warnings.find((w) => w.songsAhead === 0));
  const cover = currentCover(() => status);
  const playback = livePosition(() => status);
</script>

<section class="now">
  <div class="art">
    {#if cover.url}
      {#key cover.url}
        <img src={cover.url} alt="" transition:fade={{ duration: 300 }} />
      {/key}
    {:else}
      <Icon name="music" size={22} />
    {/if}
  </div>
  <div class="meta">
    {#if current}
      {#key current.entryId}
        <p class="title" in:fade={{ duration: 220 }}>{current.track.title}</p>
        <p class="artist" in:fade={{ duration: 220, delay: 40 }}>{current.track.artists.join(", ")}</p>
      {/key}
    {:else}
      <p class="title">Nothing queued</p>
    {/if}
  </div>
</section>

{#if current}
  <div class="progress" aria-hidden="true">
    <div class="bar" style:width="{playback.fraction * 100}%"></div>
  </div>
  <div class="times">
    <span>{formatTime(playback.position)}</span>
    {#if !status.playback.isPlaying}<span class="paused">Paused</span>{/if}
    <span>{playback.duration ? formatTime(playback.duration) : ""}</span>
  </div>
{/if}

<!-- Being in sync shows next to "you" in the listeners; this space is for
     anything else. -->
{#if status.sync !== "synced"}
  <div class="badge {badge.tone}">
    {#if badge.busy}<span class="spinner"></span>{:else}<Icon name={badge.icon} size={14} />{/if}
    <span>{badge.text}</span>
    {#if status.sync === "waitingForNextSong"}
      <span class="hint">This song isn't on {app.providerName}.</span>
    {/if}
  </div>
{/if}

{#if warning && warning.missingFor.length}
  <p class="warning"><Icon name="alert" size={13} /> {warning.missingFor.join(", ")} can't play this song</p>
{/if}

<style>
  /* Laid out in the session view's column: never squeezed by the lists. */
  .now,
  .progress,
  .times,
  .badge,
  p.warning {
    flex: none;
  }

  .now {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .art {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    width: 56px;
    height: 56px;
    overflow: hidden;
    border-radius: var(--radius);
    background: var(--surface-2);
    color: var(--text-3);
  }
  .art img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .meta {
    min-width: 0;
    flex: 1;
    display: grid;
  }
  .meta p {
    grid-area: 1 / 1;
    margin: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .meta .artist {
    grid-area: 2 / 1;
    color: var(--text-2);
  }
  .title {
    font-size: 14.5px;
    font-weight: 600;
  }

  .progress {
    height: 3px;
    margin-top: 12px;
    overflow: hidden;
    border-radius: 2px;
    background: var(--surface-3);
  }
  .bar {
    height: 100%;
    border-radius: 2px;
    background: var(--accent);
  }
  .times {
    display: flex;
    justify-content: space-between;
    margin-top: 4px;
    color: var(--text-3);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .paused {
    color: var(--text-2);
  }

  .badge {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
    padding: 6px 10px;
    border-radius: var(--radius);
    font-size: 12px;
    font-weight: 600;
  }
  .badge .hint {
    flex-basis: 100%;
    font-weight: 400;
    color: var(--text-2);
  }
  .badge .spinner {
    width: 12px;
    height: 12px;
  }
  .success {
    background: var(--success-tint);
    color: var(--success);
  }
  .warning {
    color: var(--warning);
  }
  .badge.warning {
    background: var(--warning-tint);
  }
  .danger {
    background: var(--danger-tint);
    color: var(--danger);
  }
  .muted {
    background: var(--surface-2);
    color: var(--text-2);
  }
  .accent {
    background: var(--accent-tint);
    color: var(--accent);
  }

  p.warning {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 8px 0 0;
    font-size: 12px;
  }
</style>
