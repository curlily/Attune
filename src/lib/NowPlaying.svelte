<script lang="ts">
  import { fade } from "svelte/transition";
  import { api, type RuntimeStatus } from "./api";
  import { app } from "./app.svelte";
  import Icon from "./Icon.svelte";
  import { formatTime, ownBadge } from "./status";

  let { status }: { status: RuntimeStatus } = $props();

  let current = $derived(status.queue[0]);
  let badge = $derived(ownBadge(status.sync));
  let warning = $derived(status.warnings.find((w) => w.songsAhead === 0));

  // Cover art comes from this computer's music app while it plays the
  // session's song. Remembered per song, so it stays when stepping out (the
  // app may then play something else).
  const covers = new Map<string, string>();
  let cover = $state<string | null>(null);
  $effect(() => {
    const entryId = current?.entryId;
    const playable = current?.playableHere && status.sync === "synced";
    cover = entryId ? (covers.get(entryId) ?? null) : null;
    if (!entryId || !playable || cover) return;
    let cancelled = false;
    // Give the app a moment to load the new track first.
    const timer = setTimeout(async () => {
      const url = await api.coverUrl().catch(() => null);
      if (cancelled || !url) return;
      covers.set(entryId, url);
      cover = url;
    }, 700);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  });

  // Live position from the session's playback, redrawn every frame while
  // playing. Playback is controlled in the music app, so this is display only.
  let now = $state(Date.now());
  $effect(() => {
    if (!status.playback.isPlaying) return;
    let frame = requestAnimationFrame(function tick() {
      now = Date.now();
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  });

  let duration = $derived(current?.track.durationMs ?? 0);
  let position = $derived.by(() => {
    const { isPlaying, positionMs, positionCapturedAtMs } = status.playback;
    const live = isPlaying ? positionMs + Math.max(0, now - positionCapturedAtMs) : positionMs;
    return duration ? Math.min(live, duration) : live;
  });
</script>

<section class="now">
  {#if current}
    <div class="art">
      {#if cover}
        <img src={cover} alt="" transition:fade={{ duration: 300 }} />
      {:else}
        <Icon name="music" size={22} />
      {/if}
    </div>
    <div class="meta">
      {#key current.entryId}
        <p class="title" in:fade={{ duration: 220 }}>{current.track.title}</p>
        <p class="artist" in:fade={{ duration: 220, delay: 40 }}>{current.track.artists.join(", ")}</p>
      {/key}
    </div>
  {:else}
    <div class="art"><Icon name="music" size={22} /></div>
    <div class="meta">
      <p class="title">Nothing queued</p>
      <p class="artist">Add songs in {app.providerName} and they'll show up here.</p>
    </div>
  {/if}
</section>

{#if current}
  <div class="progress" aria-hidden="true">
    <div class="bar" style:width="{duration ? (position / duration) * 100 : 0}%"></div>
  </div>
  <div class="times">
    <span>{formatTime(position)}</span>
    {#if !status.playback.isPlaying}<span class="paused">Paused</span>{/if}
    <span>{duration ? formatTime(duration) : ""}</span>
  </div>
{/if}

<div class="badge {badge.tone}">
  {#if badge.busy}<span class="spinner"></span>{:else}<Icon name={badge.icon} size={14} />{/if}
  <span>{badge.text}</span>
  {#if status.sync === "waitingForNextSong"}
    <span class="hint">This song isn't on {app.providerName}.</span>
  {:else if status.sync === "steppedOut"}
    <span class="hint">Play anything; others won't hear it.</span>
  {/if}
</div>

{#if warning && warning.missingFor.length}
  <p class="warning"><Icon name="alert" size={13} /> {warning.missingFor.join(", ")} can't play this song</p>
{/if}

<style>
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
