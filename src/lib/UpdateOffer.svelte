<script lang="ts">
  import { fly, slide } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { app } from "./app.svelte";
  import { updates } from "./updates.svelte";
  import Icon from "./Icon.svelte";

  let showNotes = $state(false);
  let info = $derived(updates.available);
  let inSession = $derived(app.screen === "jam");
  let percent = $derived(
    updates.progress === null ? null : Math.round(updates.progress * 100),
  );
</script>

<!-- Offered, never forced: nothing downloads until "Update and restart". -->
{#if info && updates.offered && !app.tiny}
  <div
    class="offer"
    role="dialog"
    aria-label="Update available"
    in:fly={{ y: -12, duration: 240, easing: cubicOut }}
    out:fly={{ y: -12, duration: 160 }}
  >
    <div class="head">
      <Icon name="refresh" size={15} />
      <div class="text">
        <strong>Attune {info.version} is available</strong>
        <span class="sub">You have {info.currentVersion}.</span>
      </div>
      {#if !updates.installing}
        <button class="icon-btn" title="Later" onclick={() => updates.later()}>
          <Icon name="x" size={13} label="Later" />
        </button>
      {/if}
    </div>

    {#if info.notes}
      <button class="notes-toggle" onclick={() => (showNotes = !showNotes)} aria-expanded={showNotes}>
        What's new
        <span class="chevron" class:open={showNotes}><Icon name="chevron" size={12} /></span>
      </button>
      {#if showNotes}
        <p class="notes scroll" transition:slide={{ duration: 160 }}>{info.notes}</p>
      {/if}
    {/if}

    {#if inSession && !updates.installing}
      <p class="sub">Attune restarts to update. You can rejoin this session afterwards.</p>
    {/if}

    {#if updates.installing}
      <div
        class="bar"
        role="progressbar"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow={percent ?? undefined}
      >
        <span class:unknown={percent === null} style:width={percent === null ? null : `${percent}%`}></span>
      </div>
      <p class="sub">
        {percent === null || percent < 100 ? `Downloading${percent === null ? "…" : ` ${percent}%`}` : "Installing…"}
      </p>
    {:else}
      {#if updates.error}
        <p class="error-text" transition:slide={{ duration: 160 }}>{updates.error}</p>
      {/if}
      <div class="actions">
        <button class="btn primary" onclick={() => updates.install()}>
          {updates.error ? "Try again" : "Update and restart"}
        </button>
        <button class="btn ghost" onclick={() => updates.later()}>Later</button>
        <span class="spacer"></span>
        <button class="btn ghost skip" onclick={() => updates.skip()}>Skip this version</button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .offer {
    position: fixed;
    top: 52px;
    left: 10px;
    right: 10px;
    z-index: 9;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 8px 10px 12px;
    border: 1px solid color-mix(in oklab, var(--accent) 35%, var(--border-strong));
    border-radius: var(--radius);
    background: var(--surface-3);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.35);
  }

  .head {
    display: flex;
    align-items: flex-start;
    gap: 9px;
  }
  .head > :global(svg) {
    flex: none;
    margin-top: 2px;
    color: var(--accent);
  }
  .text {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .head .icon-btn {
    width: 22px;
    height: 22px;
  }

  .sub {
    margin: 0;
    color: var(--text-2);
    font-size: 11.5px;
  }

  .notes-toggle {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    align-self: flex-start;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text-2);
    font-size: 12px;
    font-weight: 600;
  }
  .notes-toggle:hover {
    color: var(--text);
  }
  .chevron {
    display: inline-flex;
    transition: transform var(--fast);
  }
  .chevron.open {
    transform: rotate(180deg);
  }
  .notes {
    max-height: 120px;
    margin: 0;
    color: var(--text-2);
    font-size: 12px;
    white-space: pre-wrap;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .actions .btn {
    height: 28px;
    padding: 0 10px;
    font-size: 12px;
  }
  .actions .skip {
    color: var(--text-3);
  }
  .spacer {
    flex: 1;
  }

  .bar {
    height: 4px;
    margin-right: 4px;
    border-radius: 2px;
    background: var(--surface-1);
    overflow: hidden;
  }
  .bar > span {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: var(--accent);
    transition: width var(--fast);
  }
  .bar > span.unknown {
    width: 30%;
    animation: sweep 1.2s ease-in-out infinite;
  }
  @keyframes sweep {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(340%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .bar > span.unknown {
      animation: none;
      width: 100%;
      opacity: 0.5;
    }
  }
</style>
