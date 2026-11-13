<script lang="ts">
  import { flip } from "svelte/animate";
  import { slide } from "svelte/transition";
  import { api, errorText, type QueueItem, type RuntimeStatus } from "./api";
  import { app } from "./app.svelte";
  import Icon from "./Icon.svelte";
  import Marquee from "./Marquee.svelte";
  import Section from "./Section.svelte";

  let { status, shown = true }: { status: RuntimeStatus; shown?: boolean } = $props();

  /** Row height in px; dragging maps pointer movement to positions with it. */
  const ROW = 42;
  /** Longer queues show this many and a count of the rest. */
  const SHOWN = 150;

  // The upcoming songs as shown. Follows the session, except while dragging
  // and until the server confirms a move or removal made here.
  let order = $state<QueueItem[]>([]);
  let holding = $state(false);
  $effect(() => {
    const upcoming = status.queue.slice(1);
    if (!holding && !drag) order = upcoming;
  });

  let warnings = $derived(new Map(status.warnings.map((w) => [w.entryId, w])));

  interface Drag {
    entryId: string;
    from: number;
    startY: number;
    dy: number;
  }
  let drag = $state<Drag | null>(null);
  /** The row under the pointer: its long title pans. */
  let hovered = $state<string | null>(null);

  let visibleItems = $derived(order.slice(0, SHOWN));

  function startDrag(event: PointerEvent, item: QueueItem) {
    if (event.button !== 0) return;
    const from = order.findIndex((entry) => entry.entryId === item.entryId);
    drag = { entryId: item.entryId, from, startY: event.clientY, dy: 0 };
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function moveDrag(event: PointerEvent) {
    if (!drag) return;
    drag.dy = event.clientY - drag.startY;
    const target = clamp(drag.from + Math.round(drag.dy / ROW));
    const current = order.findIndex((entry) => entry.entryId === drag!.entryId);
    if (target !== current) order = reorder(order, current, target);
  }

  async function endDrag() {
    if (!drag) return;
    const { entryId, from } = drag;
    const to = order.findIndex((entry) => entry.entryId === entryId);
    drag = null;
    if (to !== from) await commitMove(entryId, to);
  }

  function clamp(index: number): number {
    return Math.max(0, Math.min(Math.min(order.length, SHOWN) - 1, index));
  }

  function reorder(list: QueueItem[], from: number, to: number): QueueItem[] {
    const next = [...list];
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    return next;
  }

  /** `to` counts upcoming songs; the session counts the current song as 0. */
  async function commitMove(entryId: string, to: number) {
    holding = true;
    try {
      await api.queueMove(entryId, to + 1);
    } catch (error) {
      app.toast(errorText(error), "error");
    } finally {
      holding = false;
      order = status.queue.slice(1);
    }
  }

  async function playNow(item: QueueItem) {
    holding = true;
    order = order.filter((entry) => entry.entryId !== item.entryId);
    try {
      await api.queuePlayNow(item.entryId);
    } catch (error) {
      app.toast(errorText(error), "error");
    } finally {
      holding = false;
      order = status.queue.slice(1);
    }
  }

  async function remove(item: QueueItem) {
    holding = true;
    order = order.filter((entry) => entry.entryId !== item.entryId);
    try {
      await api.queueRemove(item.entryId);
    } catch (error) {
      app.toast(errorText(error), "error");
    } finally {
      holding = false;
      order = status.queue.slice(1);
    }
  }

  /** Keyboard reordering on the grip: arrow keys move by one. */
  function onGripKey(event: KeyboardEvent, item: QueueItem) {
    const from = order.findIndex((entry) => entry.entryId === item.entryId);
    const to = event.key === "ArrowUp" ? from - 1 : event.key === "ArrowDown" ? from + 1 : from;
    if (to === from || to < 0 || to >= order.length) return;
    event.preventDefault();
    order = reorder(order, from, to);
    void commitMove(item.entryId, to);
  }

  function offset(item: QueueItem): number {
    if (drag?.entryId !== item.entryId) return 0;
    const current = order.findIndex((entry) => entry.entryId === item.entryId);
    return drag.dy - (current - drag.from) * ROW;
  }
</script>

<Section {shown} id="queue" title="Up next" count={order.length} grow>
  {#if order.length === 0}
    <p class="empty muted">Queue songs in {app.providerName} and everyone gets them.</p>
  {/if}
  <ol style:--row="{ROW}px">
    {#each visibleItems as item (item.entryId)}
      {@const warning = warnings.get(item.entryId)}
      <li
        class:dragging={drag?.entryId === item.entryId}
        onpointerenter={() => (hovered = item.entryId)}
        onpointerleave={() => (hovered = null)}
        style:transform="translateY({offset(item)}px)"
        animate:flip={{ duration: drag?.entryId === item.entryId ? 0 : 180 }}
        out:slide={{ duration: 180 }}
      >
        <button
          class="grip"
          title="Drag to reorder"
          aria-label="Move {item.track.title}. Use the arrow keys to move it."
          onpointerdown={(event) => startDrag(event, item)}
          onpointermove={moveDrag}
          onpointerup={endDrag}
          onpointercancel={endDrag}
          onkeydown={(event) => onGripKey(event, item)}
        >
          <Icon name="grip" size={14} />
        </button>
        <button class="play" title="Play now" onclick={() => playNow(item)}>
        <span class="cover" aria-hidden="true">
          {#if item.track.coverUrl}
            <img src={item.track.coverUrl} alt="" loading="lazy" draggable="false" />
          {:else}
            <Icon name="music" size={14} />
          {/if}
          <span class="play-mark"><Icon name="play" size={14} /></span>
        </span>
        <span class="song">
          <Marquee class="title" active={hovered === item.entryId && !drag}>{item.track.title}</Marquee>
          <span class="artist">
            {#if !item.playableHere}
              <span class="missing">Not on {app.providerName} for you</span>
            {:else if warning}
              <span class="missing">{warning.missingFor.join(", ")} can't play this</span>
            {:else}
              {item.track.artists.join(", ")}
            {/if}
          </span>
        </span>
        {#if item.addedBy}
          <span class="added">added by {item.addedBy}</span>
        {/if}
        </button>
        {#if warning || !item.playableHere}
          <span class="flag" title="Not everyone can play this"><Icon name="alert" size={14} /></span>
        {/if}
        <button class="icon-btn remove" title="Remove from the queue" onclick={() => remove(item)}>
          <Icon name="trash" size={14} label="Remove {item.track.title}" />
        </button>
      </li>
    {/each}
  </ol>
  {#if order.length > SHOWN}
    <p class="more muted">and {order.length - SHOWN} more</p>
  {/if}
</Section>

<style>
  ol {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    height: var(--row);
    padding-right: 6px;
    border-radius: var(--radius);
    background: transparent;
    transition:
      background var(--fast),
      box-shadow var(--fast);
  }
  li:hover {
    background: color-mix(in oklab, var(--accent) 7%, transparent);
  }
  li.dragging {
    z-index: 2;
    background: var(--surface-3);
    box-shadow: 0 8px 22px rgb(0 0 0 / 0.4);
    transition: none;
  }

  .grip {
    display: grid;
    place-items: center;
    width: 20px;
    height: 30px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-3);
    cursor: grab;
    touch-action: none;
    opacity: 0;
    transition: opacity var(--fast);
  }
  li:hover .grip,
  li.dragging .grip,
  .grip:focus-visible {
    opacity: 1;
  }
  li.dragging .grip {
    cursor: grabbing;
  }

  .play {
    min-width: 0;
    flex: 1;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 100%;
    padding: 0;
    border: 0;
    background: none;
    text-align: left;
  }
  .play-mark {
    position: absolute;
    inset: 0;
    z-index: 1;
    display: grid;
    place-items: center;
    background: rgb(0 0 0 / 0.5);
    color: #fff;
    opacity: 0;
    transition: opacity var(--fast);
  }
  .play:hover .play-mark,
  .play:focus-visible .play-mark {
    opacity: 1;
  }
  .added {
    flex: none;
    max-width: 40%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-3);
    font-size: 11.5px;
    opacity: 0;
    transition: opacity var(--base);
  }
  li:hover .added {
    opacity: 1;
  }
  li.dragging .added {
    opacity: 0;
  }

  .cover {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    width: 32px;
    height: 32px;
    overflow: hidden;
    border-radius: var(--radius-sm);
    background: var(--surface-2);
    color: var(--text-3);
  }
  .cover img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .song {
    min-width: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
    padding-left: 4px;
  }
  .song span {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .song :global(.title) {
    font-weight: 600;
  }
  .artist {
    color: var(--text-2);
    font-size: 12px;
  }
  .missing {
    color: var(--warning);
  }

  .flag {
    color: var(--warning);
  }

  .remove {
    opacity: 0;
    transition:
      opacity var(--fast),
      background var(--fast),
      color var(--fast);
  }
  li:hover .remove,
  .remove:focus-visible {
    opacity: 1;
  }
  .remove:hover {
    color: var(--danger);
  }

  .empty,
  .more {
    margin: 4px 0 0;
    font-size: 12px;
  }
</style>
