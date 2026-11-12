<script lang="ts">
  import { api, errorText } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";
  import Listeners from "../lib/Listeners.svelte";
  import NowPlaying from "../lib/NowPlaying.svelte";
  import Queue from "../lib/Queue.svelte";
  import { collapse } from "../lib/collapse";

  let status = $derived(app.status!);
  // Stepped out or broken: the main action is getting back in.
  let canResync = $derived(status.sync === "steppedOut" || status.sync === "error");
  // A shorter window shows less: the listeners go first, then the queue,
  // then these buttons; below that App shows the tiny view. Each goes when
  // the room it needs runs out, counting folded sections as just a title.
  const HEADER = 44;
  const BODY_PADDING = 26;
  const FOOTER = 55;
  /** Divider, gap and title of a section. */
  const SECTION = 52;
  /** The part of SECTION above the divider. */
  const SECTION_GAP = 14;
  const QUEUE_ROW = 42;
  const LISTENER_ROW = 32;
  const EMPTY_QUEUE = 22;

  let topHeight = $state(0);
  let room = $derived(app.height - HEADER - BODY_PADDING - topHeight);
  let showFooter = $derived(room >= FOOTER);
  let left = $derived(room - FOOTER);
  let upcoming = $derived(status.queue.length - 1);

  /** Room the queue needs to show `rows` songs. */
  function queueNeeds(rows: number): number {
    if (app.collapsed.queue) return SECTION;
    return SECTION + (upcoming === 0 ? EMPTY_QUEUE : Math.min(upcoming, rows) * QUEUE_ROW);
  }
  let listenersNeed = $derived.by(() => {
    if (app.collapsed.listeners) return SECTION;
    // They stop growing at 36% of the column, then scroll. The cap leaves
    // out the gap above the divider.
    const column = app.height - HEADER - BODY_PADDING - FOOTER;
    const box = SECTION - SECTION_GAP + status.participants.length * LISTENER_ROW;
    return SECTION_GAP + Math.min(box, column * 0.36);
  });
  // The queue stays down to one song; the listeners leave while it still
  // has three.
  let showQueue = $derived(showFooter && left >= queueNeeds(1));
  let showListeners = $derived(showQueue && left >= queueNeeds(3) + listenersNeed);
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
      <div class="top" bind:clientHeight={topHeight}>
        <NowPlaying {status} />
      </div>
      <Queue {status} shown={showQueue} />
      <Listeners {status} shown={showListeners} />
    </div>
    {#if showFooter}
    <footer transition:collapse>
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
    /* One column exactly as wide as the window. An `auto` column grows to
       the longest unbroken line, such as a long title held on one line to
       pan, and pushes everything past the right edge. */
    grid-template-columns: minmax(0, 1fr);
    height: 100%;
  }

  .body {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    padding: 14px 14px 12px;
  }

  .top {
    flex: none;
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
