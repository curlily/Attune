<script lang="ts">
  import { onMount } from "svelte";
  import { fade, fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { app } from "./lib/app.svelte";
  import { api, errorText } from "./lib/api";
  import Icon from "./lib/Icon.svelte";
  import Toasts from "./lib/Toasts.svelte";
  import FirstRun from "./screens/FirstRun.svelte";
  import Connect from "./screens/Connect.svelte";
  import Lobby from "./screens/Lobby.svelte";
  import Jam from "./screens/Jam.svelte";
  import Mini from "./screens/Mini.svelte";
  import SettingsScreen from "./screens/Settings.svelte";

  let failure = $state<string | null>(null);
  let windowElement: HTMLDivElement;

  // Slide in each time the window is shown from the tray. Replayed on the
  // same element, so nothing on the page (a half-typed code) is reset.
  $effect(() => {
    if (app.shown === 0 || !windowElement) return;
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    windowElement.animate(
      [
        { opacity: 0, transform: "translateY(10px) scale(0.985)" },
        { opacity: 1, transform: "none" },
      ],
      { duration: 320, easing: "cubic-bezier(0.2, 0.8, 0.2, 1)" },
    );
  });

  onMount(() => {
    app.init().catch((error) => (failure = errorText(error)));
  });

  let title = $derived(
    app.showSettings ? "Settings" : app.screen === "jam" ? "Jam" : "MyJam",
  );
</script>

<div class="window" bind:this={windowElement}>
{#if app.mini && app.screen === "jam"}
  <Mini />
{:else}
  <div class="shell">
    <!-- No window frame: the header is where the window is dragged. -->
    <header data-tauri-drag-region>
      <span class="mark" aria-hidden="true" data-tauri-drag-region></span>
      <h1 data-tauri-drag-region>{title}</h1>
      {#if app.screen === "jam" && app.code && !app.showSettings}
        <button
          class="code"
          title="Copy the join code"
          onclick={async () => {
            await navigator.clipboard.writeText(app.code!);
            app.toast(`Copied ${app.code}. Share it so others can join.`, "success");
          }}
        >
          {app.code}
          <Icon name="copy" size={13} />
        </button>
      {/if}
      <span class="spacer" data-tauri-drag-region></span>
      {#if app.screen === "jam" && !app.showSettings}
        <button class="icon-btn" title="Mini view" onclick={() => app.setMini(true)}>
          <Icon name="shrink" label="Mini view" />
        </button>
      {/if}
      {#if app.screen !== "loading" && app.screen !== "firstRun"}
        <button
          class="icon-btn"
          title="Settings"
          aria-pressed={app.showSettings}
          onclick={() => (app.showSettings = !app.showSettings)}
        >
          <Icon name="sliders" label="Settings" />
        </button>
      {/if}
      <button
        class="icon-btn"
        title={app.pinned ? "Unpin: hide when clicking elsewhere" : "Pin: keep open and on top"}
        aria-pressed={app.pinned}
        onclick={() => app.setPinned(!app.pinned)}
      >
        <Icon name="pin" label="Pin" />
      </button>
      <button class="icon-btn" title="Hide to the tray" onclick={() => api.hideWindow()}>
        <Icon name="x" label="Hide to the tray" />
      </button>
    </header>

    <main>
      {#if failure}
        <p class="failure">MyJam couldn't start: {failure}</p>
      {:else if app.showSettings}
        <div class="screen" in:fly={{ x: 24, duration: 220, easing: cubicOut }}>
          <SettingsScreen />
        </div>
      {:else}
        {#key app.screen}
          <div
            class="screen"
            in:fly={{ y: 10, duration: 260, delay: 60, easing: cubicOut }}
            out:fade={{ duration: 80 }}
          >
            {#if app.screen === "firstRun"}
              <FirstRun />
            {:else if app.screen === "connect"}
              <Connect />
            {:else if app.screen === "lobby"}
              <Lobby />
            {:else if app.screen === "jam"}
              <Jam />
            {/if}
          </div>
        {/key}
      {/if}
    </main>
  </div>
{/if}
</div>

<Toasts />

<style>
  .window {
    height: 100%;
  }

  .shell {
    display: grid;
    grid-template-rows: auto 1fr;
    height: 100%;
  }

  header {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 44px;
    padding: 0 6px 0 14px;
    border-bottom: 1px solid var(--border);
  }

  .mark {
    width: 18px;
    height: 18px;
    border-radius: 6px;
    background:
      radial-gradient(circle at 50% 50%, var(--bg) 0 2.5px, transparent 3px),
      conic-gradient(from 210deg, var(--accent-strong), var(--accent), #b493ff, var(--accent-strong));
  }

  h1 {
    margin: 0 4px 0 4px;
    font-size: 13.5px;
    font-weight: 600;
  }

  .code {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--text-2);
    font: 600 12px var(--font-mono);
    letter-spacing: 0.12em;
    transition:
      color var(--fast),
      border-color var(--fast);
  }
  .code:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }

  .spacer {
    flex: 1;
  }

  main {
    position: relative;
    min-height: 0;
  }

  .screen {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .failure {
    margin: 16px;
    color: var(--danger);
  }
</style>
