<script lang="ts">
  // The overlay window's page: one small pill, drawn over other apps and
  // games. The window is click-through and never focused; Rust places it in
  // the chosen corner and shows or hides it.
  //
  // Two layers: a flash (something just happened) over the rest (persistent
  // mode only: an ongoing state, or what's playing). A new flash replaces
  // the one before at once; when its time is up the rest shows again.
  //
  // Every change animates, and none waits for an animation: the pill is one
  // shell that stays while its contents swap. The shell morphs to the new
  // width and fades to the new tone; the old contents roll out while the
  // new roll in, at the same time. With Windows' animation effects off,
  // all of it becomes plain fades.
  import { onMount } from "svelte";
  import { cubicOut } from "svelte/easing";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import Icon, { type IconName } from "../lib/Icon.svelte";
  import type { ProviderId } from "../lib/api";
  import { applyTheme, savedTheme, systemMode, watchSystemMode, type Mode } from "../lib/theme";

  type Corner = "topLeft" | "topRight" | "bottomLeft" | "bottomRight";
  interface Pill {
    icon: IconName;
    text: string;
    tone: "info" | "success" | "warning" | "danger";
    /** Album art in place of the icon (a new song). */
    cover?: string;
  }
  interface Flash extends Pill {
    durationMs: number;
    corner: Corner;
  }
  interface Rest {
    rest: Pill | null;
    /** Shown faded: the song, with "dim" on. */
    dim: boolean;
    corner: Corner;
  }

  let flash = $state<Flash | null>(null);
  let rest = $state<Rest>({ rest: null, dim: false, corner: "bottomRight" });
  // Bumped per flash, so each one replaces the last at once (and restarts
  // its time) even when the text is the same.
  let flashes = $state(0);
  let timer: ReturnType<typeof setTimeout> | undefined;
  // A cover that didn't load: the icon shows instead.
  let brokenCover = $state<string | null>(null);
  // Windows' "Animation effects" off.
  let reduced = $state(false);
  // The contents' natural width, which the shell morphs to.
  let contentWidth = $state(0);
  // Looks like the main window: Attune's colours, or with the Match theme
  // the music app's. The choice lives in localStorage, which this page
  // shares with the main window (and hears changes to).
  let theme = $state(savedTheme());
  let provider = $state<ProviderId | null>(null);
  let mode = $state<Mode>(systemMode());
  $effect(() => applyTheme(theme, provider, mode));

  onMount(() => {
    const unwatch = watchSystemMode((next) => (mode = next));
    const onStorage = () => (theme = savedTheme());
    window.addEventListener("storage", onStorage);
    const motion = matchMedia("(prefers-reduced-motion: reduce)");
    const onMotion = () => (reduced = motion.matches);
    onMotion();
    motion.addEventListener("change", onMotion);
    const stops = [
      listen<Flash>("overlay-flash", (event) => {
        flash = event.payload;
        flashes += 1;
        clearTimeout(timer);
        timer = setTimeout(() => (flash = null), event.payload.durationMs);
      }),
      listen<Rest>("overlay-rest", (event) => (rest = event.payload)),
      listen<ProviderId | null>("provider-changed", (event) => (provider = event.payload)),
    ];
    Promise.all(stops)
      .then(() => invoke<ProviderId | null>("overlay_ready"))
      .then((connected) => (provider = connected));
    return () => {
      unwatch();
      window.removeEventListener("storage", onStorage);
      motion.removeEventListener("change", onMotion);
      stops.forEach((stop) => stop.then((unlisten) => unlisten()));
      clearTimeout(timer);
    };
  });

  let pill = $derived<Pill | null>(flash ?? rest.rest);
  let dim = $derived(!flash && rest.dim);
  // A new key per flash and per change of rest: the contents swap.
  let key = $derived(flash ? `flash-${flashes}` : `rest-${rest.rest?.icon}-${rest.rest?.text}`);
  let corner = $derived(flash?.corner ?? rest.corner);
  let right = $derived(corner.endsWith("Right"));
  let bottom = $derived(corner.startsWith("bottom"));

  /** The shell comes from, and leaves toward, the screen edge it sits on. */
  function edge(_node: Element, { toward }: { toward: number }) {
    return {
      duration: reduced ? 120 : 180,
      easing: cubicOut,
      css: (t: number, u: number) =>
        reduced ? `opacity: ${t}` : `opacity: ${t}; transform: translateY(${u * toward}px) scale(${0.96 + 0.04 * t})`,
    };
  }

  /** Contents roll: the new in from below, the old out above, together.
   *  The old leaves quicker, so the two barely overlap. */
  function roll(_node: Element, { by, quick = false }: { by: number; quick?: boolean }) {
    return {
      duration: reduced ? 120 : quick ? 130 : 220,
      easing: cubicOut,
      css: (t: number, u: number) =>
        reduced ? `opacity: ${t}` : `opacity: ${t}; transform: translateY(${u * by}px)`,
    };
  }
</script>

{#snippet face(p: Pill)}
  {#if p.cover && p.cover !== brokenCover}
    <img class="cover" src={p.cover} alt="" onerror={() => (brokenCover = p.cover ?? null)} />
  {:else}
    <span class="glyph"><Icon name={p.icon} size={14} /></span>
  {/if}
  <span class="text">{p.text}</span>
{/snippet}

<div class="stage" class:right class:bottom>
  {#if pill}
    <!-- Sizes the shell: the contents as they'd lay out, unseen. -->
    <div class="contents measure" aria-hidden="true" bind:offsetWidth={contentWidth}>
      {@render face(pill)}
    </div>

    <div
      class="pill {pill.tone}"
      class:dim
      class:reduced
      style:width="{contentWidth + 22}px"
      in:edge={{ toward: bottom ? 6 : -6 }}
      out:edge={{ toward: bottom ? 6 : -6 }}
    >
      {#key key}
        <div class="contents" in:roll={{ by: 7 }} out:roll={{ by: -10, quick: true }}>
          {@render face(pill)}
        </div>
      {/key}
    </div>
  {/if}
</div>

<style>
  :global(html),
  :global(body.overlay) {
    background: transparent;
  }

  .stage {
    position: relative;
    display: flex;
    height: 100%;
    padding: 4px;
    align-items: flex-start;
    justify-content: flex-start;
  }
  .stage.right {
    justify-content: flex-end;
  }
  .stage.bottom {
    align-items: flex-end;
  }

  /* The shell: holds its place while the contents change, and morphs to
     their width (growing away from the screen edge it's pinned to). */
  .pill {
    position: relative;
    flex: none;
    max-width: 100%;
    height: 30px;
    overflow: hidden;
    border: 1px solid var(--border-strong);
    border-radius: 15px;
    background: color-mix(in oklab, var(--surface-2) 94%, transparent);
    color: var(--text);
    font-size: 12.5px;
    font-weight: 600;
    transition:
      width 260ms var(--ease-out),
      border-color 260ms,
      opacity var(--base);
  }
  .pill.reduced {
    transition:
      border-color 120ms,
      opacity 120ms;
  }
  /* The tone tints the outline too, and fades between tones. */
  .pill.success {
    border-color: color-mix(in oklab, var(--success) 40%, var(--border-strong));
  }
  .pill.warning {
    border-color: color-mix(in oklab, var(--warning) 40%, var(--border-strong));
  }
  .pill.danger {
    border-color: color-mix(in oklab, var(--danger) 45%, var(--border-strong));
  }
  /* The song, while nothing else is up: there, but out of the way. */
  .pill.dim {
    opacity: 0.6;
    font-weight: 500;
  }

  /* Contents sit on top of each other while one rolls out and the next in. */
  .contents {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 10px;
    display: flex;
    align-items: center;
    gap: 7px;
    max-width: 326px;
    white-space: nowrap;
  }
  /* Pinned to the same edge as the shell: the text holds still and the far
     edge does the moving. */
  .right .pill .contents {
    left: auto;
    right: 12px;
  }
  .measure {
    visibility: hidden;
    pointer-events: none;
    top: 0;
    left: 0;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .glyph {
    display: grid;
    color: var(--accent);
    animation: pop 300ms var(--ease-spring);
  }
  .success .glyph {
    color: var(--success);
  }
  .warning .glyph {
    color: var(--warning);
  }
  .danger .glyph {
    color: var(--danger);
  }
  .cover {
    flex: none;
    width: 20px;
    height: 20px;
    margin-left: -5px;
    border-radius: 50%;
    object-fit: cover;
    animation: reveal 320ms var(--ease-out);
  }

  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.5);
    }
  }
  @keyframes reveal {
    from {
      opacity: 0;
      transform: scale(0.6);
    }
  }
  .reduced .glyph,
  .reduced .cover {
    animation: none;
  }
</style>
