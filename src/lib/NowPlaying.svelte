<script lang="ts">
  import { fade } from "svelte/transition";
  import type { RuntimeStatus } from "./api";
  import { app } from "./app.svelte";
  import { currentCover, livePosition } from "./current.svelte";
  import Icon from "./Icon.svelte";
  import Marquee from "./Marquee.svelte";
  import { api, errorText } from "./api";
  import { formatTime, ownBadge } from "./status";

  let { status }: { status: RuntimeStatus } = $props();

  let current = $derived(status.queue[0]);
  let badge = $derived(ownBadge(status.sync));
  let warning = $derived(status.warnings.find((w) => w.songsAhead === 0));
  const cover = currentCover(() => status);
  const playback = livePosition(() => status);

  let skipping = $state(false);
  async function skip() {
    if (!current) return;
    skipping = true;
    try {
      await api.queueRemove(current.entryId);
    } catch (error) {
      app.toast(errorText(error), "error");
    } finally {
      skipping = false;
    }
  }
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
        <p class="title" in:fade={{ duration: 220 }}><Marquee>{current.track.title}</Marquee></p>
        <p class="artist" in:fade={{ duration: 220, delay: 40 }}>
          <Marquee>{current.track.artists.join(", ")}</Marquee>
        </p>
      {/key}
    {:else}
      <p class="title">Nothing queued</p>
    {/if}
  </div>
</section>

{#if current}
  <div class="timeline" class:paused={!status.playback.isPlaying} title={status.playback.isPlaying ? undefined : "Paused"}>
    <span class="time">{formatTime(playback.position)}</span>
    <div class="progress" aria-hidden="true">
      <div class="bar" style:width="{playback.fraction * 100}%"></div>
    </div>
    <span class="time">{playback.duration ? formatTime(playback.duration) : "–:––"}</span>
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
  <p class="warning">
    <Icon name="alert" size={13} />
    <span class="who">{warning.missingFor.join(", ")} can't play this song</span>
    {#if !app.autoRemove}
      <button class="skip" onclick={skip} aria-busy={skipping} disabled={skipping}>
        <Icon name="skip" size={12} /> Skip
      </button>
    {/if}
  </p>
{/if}

<style>
  /* Laid out in the session view's column: never squeezed by the lists. */
  .now,
  .timeline,
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
    /* Lifted off the page by a shadow in the accent. */
    box-shadow: 0 6px 18px -4px color-mix(in oklab, var(--accent) 35%, transparent);
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

  .timeline {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 12px;
    color: var(--text-3);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .time {
    flex: none;
    min-width: 28px;
  }
  .time:last-child {
    text-align: right;
  }
  .progress {
    flex: 1;
    height: 4px;
    overflow: hidden;
    border-radius: 2px;
    background: var(--surface-3);
  }
  .bar {
    height: 100%;
    border-radius: 2px;
    background: var(--accent);
    transition: background var(--base);
  }
  .timeline.paused .bar {
    background: var(--text-3);
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
  .who {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .skip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    height: 22px;
    padding: 0 8px;
    border: 1px solid color-mix(in srgb, var(--warning) 35%, transparent);
    border-radius: var(--radius-sm);
    background: var(--warning-tint);
    color: var(--warning);
    font-size: 11.5px;
    font-weight: 600;
    transition: background var(--fast);
  }
  .skip:hover {
    background: color-mix(in srgb, var(--warning) 22%, transparent);
  }
  .skip:disabled {
    opacity: 0.6;
  }
</style>
