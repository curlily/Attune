<script lang="ts">
  // Settings > Overlay: the small notification drawn over games and other
  // apps when something happens in the session.
  import { slide } from "svelte/transition";
  import {
    api,
    errorText,
    type Corner,
    type OverlayMode,
    type OverlaySettings,
    type ShowWhen,
  } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";

  let error = $state<string | null>(null);
  let overlay = $derived(app.settings?.overlay);
  // The duration while the slider is dragged; saved when it's let go.
  let seconds = $state((app.settings?.overlay.flashMs ?? 3000) / 1000);

  async function change(patch: Partial<OverlaySettings>) {
    if (!overlay) return;
    error = null;
    try {
      app.settings = await api.updateSettings({ overlay: { ...overlay, ...patch } });
    } catch (failure) {
      error = errorText(failure);
    }
  }

  async function test() {
    error = null;
    try {
      await api.testOverlay();
    } catch (failure) {
      error = errorText(failure);
    }
  }

  const MODES: [OverlayMode, string][] = [
    ["flash", "Flash"],
    ["persistent", "Persistent"],
  ];
  const MODE_HINT: Record<OverlayMode, string> = {
    flash: "Shows for a moment when something happens",
    persistent: "Stays up in a session, showing the song between events",
  };

  const SHOW_WHEN: [ShowWhen, string][] = [
    ["fullScreen", "Full screen"],
    ["hidden", "Attune hidden"],
    ["always", "Always"],
  ];
  const SHOW_WHEN_HINT: Record<ShowWhen, string> = {
    fullScreen: "Only while a game or other app is full screen",
    hidden: "Also whenever Attune is in the tray or minimised",
    always: "Over any app, but never over Attune itself",
  };

  const CORNERS: [Corner, string][] = [
    ["topLeft", "Top left"],
    ["topRight", "Top right"],
    ["bottomLeft", "Bottom left"],
    ["bottomRight", "Bottom right"],
  ];

  /** Arrow keys move around the screen outline. */
  function cornerKey(event: KeyboardEvent, corner: Corner) {
    const top = corner.startsWith("top");
    const left = corner.endsWith("Left");
    const at = (t: boolean, l: boolean): Corner =>
      `${t ? "top" : "bottom"}${l ? "Left" : "Right"}` as Corner;
    const moves: Record<string, Corner> = {
      ArrowUp: at(true, left),
      ArrowDown: at(false, left),
      ArrowLeft: at(top, true),
      ArrowRight: at(top, false),
    };
    const to = moves[event.key];
    if (!to) return;
    event.preventDefault();
    void change({ corner: to });
    document.getElementById(`corner-${to}`)?.focus();
  }

  type GroupKey = "playback" | "people" | "sync" | "songs";
  const GROUPS: { key: GroupKey; label: string; hint: string; flashOnly?: boolean }[] = [
    { key: "playback", label: "Playback", hint: "Paused, skipped and queue changes by others" },
    { key: "people", label: "People", hint: "Joined, left and a new host" },
    { key: "sync", label: "Sync", hint: "Catching up, stepped out, out of sync" },
    // Persistent mode shows the song all along.
    { key: "songs", label: "Songs", hint: "The new song's name and cover", flashOnly: true },
  ];
</script>

<div class="settings scroll">
  <button class="btn ghost back" onclick={() => (app.settingsPage = "main")}>
    <Icon name="back" size={14} /> Settings
  </button>

  {#if overlay}
    <button
      class="switch-row"
      role="switch"
      aria-checked={overlay.enabled}
      onclick={() => change({ enabled: !overlay.enabled })}
    >
      <span class="switch-text">
        <span class="switch-label">Show overlay</span>
        <span class="switch-hint">A small notification over games and other apps</span>
      </span>
      <span class="switch" class:on={overlay.enabled}><span></span></span>
    </button>

    {#if error}<p class="error-text" transition:slide={{ duration: 160 }}>{error}</p>{/if}

    {#if overlay.enabled}
      <div transition:slide={{ duration: 180 }}>
        <div class="divider"></div>

        <p class="section-title">Mode</p>
        <div class="segmented" role="radiogroup" aria-label="Mode">
          {#each MODES as [value, label]}
            <button role="radio" aria-checked={overlay.mode === value} onclick={() => change({ mode: value })}>
              {label}
            </button>
          {/each}
        </div>
        <p class="hint">{MODE_HINT[overlay.mode]}</p>

        {#if overlay.mode === "persistent"}
          <button
            class="switch-row dim-row"
            role="switch"
            aria-checked={overlay.dimIdle}
            onclick={() => change({ dimIdle: !overlay.dimIdle })}
            transition:slide={{ duration: 160 }}
          >
            <span class="switch-text">
              <span class="switch-label">Fade the song</span>
              <span class="switch-hint">Until something happens</span>
            </span>
            <span class="switch" class:on={overlay.dimIdle}><span></span></span>
          </button>
        {/if}

        <p class="section-title gap">Show when</p>
        <div class="segmented" role="radiogroup" aria-label="Show when">
          {#each SHOW_WHEN as [value, label]}
            <button role="radio" aria-checked={overlay.showWhen === value} onclick={() => change({ showWhen: value })}>
              {label}
            </button>
          {/each}
        </div>
        <p class="hint">{SHOW_WHEN_HINT[overlay.showWhen]}</p>

        <div class="row-pair">
          <div>
            <p class="section-title">Position</p>
            <div class="screen-outline" role="radiogroup" aria-label="Position">
              {#each CORNERS as [corner, label]}
                <button
                  id="corner-{corner}"
                  class="corner {corner}"
                  role="radio"
                  aria-checked={overlay.corner === corner}
                  aria-label={label}
                  title={label}
                  tabindex={overlay.corner === corner ? 0 : -1}
                  onclick={() => change({ corner })}
                  onkeydown={(event) => cornerKey(event, corner)}
                >
                  <span></span>
                </button>
              {/each}
            </div>
          </div>

          <div class="duration">
            <p class="section-title">
              <label for="duration">Duration</label>
              <span class="value">{seconds.toFixed(1)} s</span>
            </p>
            <input
              id="duration"
              type="range"
              min="1"
              max="5"
              step="0.5"
              bind:value={seconds}
              onchange={() => change({ flashMs: Math.round(seconds * 1000) })}
            />
          </div>
        </div>

        <div class="divider"></div>

        <p class="section-title">What to show</p>
        {#each GROUPS.filter((group) => !group.flashOnly || overlay.mode === "flash") as group (group.key)}
          <button
            class="switch-row"
            role="switch"
            aria-checked={overlay[group.key]}
            onclick={() => change({ [group.key]: !overlay[group.key] })}
            transition:slide={{ duration: 160 }}
          >
            <span class="switch-text">
              <span class="switch-label">{group.label}</span>
              <span class="switch-hint">{group.hint}</span>
            </span>
            <span class="switch" class:on={overlay[group.key]}><span></span></span>
          </button>
        {/each}

        <button class="btn wide test" onclick={test}>Show a test notification</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .settings {
    height: 100%;
    padding: 8px 14px 14px;
  }
  .back {
    margin: 0 0 8px -8px;
    height: 28px;
    padding: 0 8px;
    color: var(--text-2);
  }
  .hint {
    margin: 5px 0 0;
    color: var(--text-3);
    font-size: 11.5px;
  }

  .row-pair {
    display: flex;
    gap: 18px;
    margin-top: 16px;
  }
  .duration {
    flex: 1;
    min-width: 0;
  }
  .duration .section-title {
    display: flex;
    justify-content: space-between;
  }
  .value {
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  input[type="range"] {
    width: 100%;
    margin: 14px 0 0;
    accent-color: var(--accent-strong);
  }

  /* A little screen with a pill in each corner: the chosen one is filled. */
  .screen-outline {
    position: relative;
    width: 92px;
    height: 58px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
  }
  .corner {
    position: absolute;
    display: grid;
    place-items: center;
    width: 26px;
    height: 22px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
  }
  .corner > span {
    width: 14px;
    height: 6px;
    border: 1px solid var(--text-3);
    border-radius: 3px;
    transition:
      background var(--fast),
      border-color var(--fast);
  }
  .corner:hover > span {
    border-color: var(--text-2);
  }
  .corner[aria-checked="true"] > span {
    border-color: var(--accent);
    background: var(--accent);
  }
  .topLeft {
    top: 2px;
    left: 2px;
  }
  .topRight {
    top: 2px;
    right: 2px;
  }
  .bottomLeft {
    bottom: 2px;
    left: 2px;
  }
  .bottomRight {
    bottom: 2px;
    right: 2px;
  }

  .test {
    margin-top: 18px;
  }
  .dim-row {
    margin-top: 14px;
  }
  .gap {
    margin-top: 16px;
  }
</style>
