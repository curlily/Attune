<script lang="ts">
  import { api, errorText } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";
  import Listeners from "../lib/Listeners.svelte";
  import NowPlaying from "../lib/NowPlaying.svelte";
  import Queue from "../lib/Queue.svelte";

  let status = $derived(app.status!);
  // Stepped out or broken: the main action is getting back in.
  let canResync = $derived(status.sync === "steppedOut" || status.sync === "error");
  // A shorter window shows less: listeners go first, then the queue, then
  // these buttons; below that App shows the tiny view.
  let showListeners = $derived(app.height >= 520);
  let showQueue = $derived(app.height >= 400);
  let showFooter = $derived(app.height >= 290);
  let busy = $state(false);
  let confirmLeave = $state(false);
  let confirmTimer: ReturnType<typeof setTimeout> | undefined;

  async function toggleFollowing() {
    busy = true;
    try {
      await (canResync ? api.resync() : api.stepOut());
    } catch (error) {
      app.toast(errorText(error), "error");
    } finally {
      busy = false;
    }
  }

  function leave() {
    if (!confirmLeave) {
      confirmLeave = true;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirmLeave = false), 3000);
      return;
    }
    clearTimeout(confirmTimer);
    void app.leave();
  }
</script>

{#if status}
  <div class="jam">
    <div class="body">
      <NowPlaying {status} />
      {#if showQueue}
        <div class="divider"></div>
        <Queue {status} />
      {/if}
      {#if showListeners}
        <div class="divider"></div>
        <Listeners {status} />
      {/if}
    </div>
    {#if showFooter}
    <footer>
      <button
        class="btn"
        class:primary={canResync}
        onclick={toggleFollowing}
        aria-busy={busy}
        disabled={status.sync === "reconnecting" || status.sync === "resyncing"}
      >
        <Icon name={canResync ? "refresh" : "eject"} size={14} />
        {canResync ? "Resync" : "Step out"}
      </button>
      <button class="btn danger" class:confirm={confirmLeave} onclick={leave}>
        <Icon name="logout" size={14} />
        {confirmLeave ? "Leave the session?" : "Leave"}
      </button>
    </footer>
    {/if}
  </div>
{/if}

<style>
  .jam {
    display: grid;
    grid-template-rows: 1fr auto;
    height: 100%;
  }

  .body {
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 14px 14px 12px;
  }

  .divider {
    flex: none;
    height: 1px;
    margin: 14px 0 12px;
    background: var(--border);
  }

  footer {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
    padding: 10px 14px 12px;
    border-top: 1px solid var(--border);
  }
  footer .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .confirm {
    background: var(--danger-tint);
    border-color: color-mix(in srgb, var(--danger) 40%, transparent);
  }
</style>
