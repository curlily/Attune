<script lang="ts">
  import { fade } from "svelte/transition";
  import { app } from "../lib/app.svelte";
  import { currentCover, livePosition } from "../lib/current.svelte";
  import Icon from "../lib/Icon.svelte";
  import Marquee from "../lib/Marquee.svelte";
  import { ownBadge, participantBadge, showsOwnBadge } from "../lib/status";

  let status = $derived(app.status);
  let current = $derived(status?.queue[0]);
  let badge = $derived(status ? ownBadge(status.sync, status.behindMs) : null);
  const cover = currentCover(() => status);
  const playback = livePosition(() => status);
</script>

{#if status}
  <div class="mini" in:fade={{ duration: 200 }} data-tauri-drag-region>
    <div class="art" data-tauri-drag-region>
      {#if cover.url}
        {#key cover.url}
          <img src={cover.url} alt="" draggable="false" transition:fade={{ duration: 300 }} />
        {/key}
      {:else}
        <Icon name="music" size={20} />
      {/if}
    </div>
    <div class="song" data-tauri-drag-region>
      <Marquee class="title">{current?.track.title ?? "Nothing queued"}</Marquee>
      <Marquee class="artist">{current?.track.artists.join(", ") ?? ""}</Marquee>
      <span class="separator"></span>
      <span class="dots" data-tauri-drag-region>
        {#each status.participants as person (person.clientId)}
          {@const state = participantBadge(person)}
          <span class="dot {state.tone}" title="{person.displayName}: {state.text}"></span>
        {/each}
        {#if badge && showsOwnBadge(status)}
          <span class="own {badge.tone}">{badge.text}</span>
        {/if}
      </span>
    </div>
    <div class="actions">
      <button
        class="icon-btn"
        title={app.pinned ? "Unpin: stop keeping on top" : "Pin: keep on top of other windows"}
        aria-pressed={app.pinned}
        onclick={() => app.setPinned(!app.pinned)}
      >
        <Icon name="pin" label="Pin" />
      </button>
      <button class="icon-btn" title="Full view" onclick={() => app.toggleTiny()}>
        <Icon name="expand" label="Full view" />
      </button>
    </div>
    {#if current}
      <div class="progress" aria-hidden="true">
        <div class="bar" style:transform="scaleX({playback.fraction})"></div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .mini {
    position: relative;
    display: flex;
    align-items: center;
    gap: 12px;
    height: 100%;
    padding: 0 8px 0 14px;
  }

  .art {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    width: 60px;
    height: 60px;
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

  .actions {
    display: flex;
    flex-direction: column;
    gap: 2px;
    align-self: center;
  }

  .progress {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 3px;
    background: var(--surface-2);
  }
  /* Scaled rather than sized: smooth at fractions of a pixel. */
  .bar {
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    will-change: transform;
  }

  .song {
    min-width: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
  }
  .song > span {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .song :global(.title) {
    font-weight: 600;
  }
  .song :global(.artist) {
    color: var(--text-2);
    font-size: 12px;
  }

  .separator {
    height: 1px;
    margin: 6px 0;
    background: var(--border-strong);
  }
  .dots {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-3);
    transition: background var(--base);
  }
  .dot.success {
    background: var(--success);
  }
  .dot.warning {
    background: var(--warning);
  }
  .dot.danger {
    background: var(--danger);
  }
  .dot.accent {
    background: var(--accent);
  }
  .own {
    margin-left: 4px;
    font-size: 11.5px;
  }
  .own.warning {
    color: var(--warning);
  }
  .own.danger {
    color: var(--danger);
  }
  .own.muted {
    color: var(--text-3);
  }
  .own.accent {
    color: var(--accent);
  }
</style>
