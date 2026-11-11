<script lang="ts">
  import { fade } from "svelte/transition";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";
  import { ownBadge, participantBadge } from "../lib/status";

  let status = $derived(app.status);
  let current = $derived(status?.queue[0]);
  let badge = $derived(status ? ownBadge(status.sync) : null);
</script>

{#if status}
  <div class="mini" in:fade={{ duration: 200 }}>
    <div class="song">
      <span class="title">{current?.track.title ?? "Nothing queued"}</span>
      <span class="artist">{current?.track.artists.join(", ") ?? ""}</span>
      <span class="dots">
        {#each status.participants as person (person.clientId)}
          {@const state = participantBadge(person)}
          <span class="dot {state.tone}" title="{person.displayName}: {state.text}"></span>
        {/each}
        {#if badge && status.sync !== "synced"}
          <span class="own {badge.tone}">{badge.text}</span>
        {/if}
      </span>
    </div>
    <button class="icon-btn" title="Full view" onclick={() => app.setMini(false)}>
      <Icon name="expand" label="Full view" />
    </button>
  </div>
{/if}

<style>
  .mini {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 100%;
    padding: 0 10px 0 14px;
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
  .title {
    font-weight: 600;
  }
  .artist {
    color: var(--text-2);
    font-size: 12px;
  }

  .dots {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-top: 5px;
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
