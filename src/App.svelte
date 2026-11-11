<script lang="ts">
  import { onMount } from "svelte";
  import { fade, fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { app } from "./lib/app.svelte";
  import { errorText } from "./lib/api";
  import Icon from "./lib/Icon.svelte";
  import Toasts from "./lib/Toasts.svelte";
  import FirstRun from "./screens/FirstRun.svelte";
  import Connect from "./screens/Connect.svelte";
  import Lobby from "./screens/Lobby.svelte";
  import Jam from "./screens/Jam.svelte";
  import Mini from "./screens/Mini.svelte";
  import SettingsScreen from "./screens/Settings.svelte";

  let failure = $state<string | null>(null);

  onMount(() => {
    app.init().catch((error) => (failure = errorText(error)));
  });

  let title = $derived(
    app.showSettings ? "Settings" : app.screen === "jam" ? "Jam" : "MyJam",
  );
</script>

{#if app.mini && app.screen === "jam"}
  <Mini />
{:else}
  <div class="shell">
    <header>
      <span class="mark" aria-hidden="true"></span>
      <h1>{title}</h1>
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
      <span class="spacer"></span>
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

<Toasts />

<style>
  .shell {
    display: grid;
    grid-template-rows: auto 1fr;
    height: 100%;
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 44px;
    padding: 0 8px 0 14px;
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
    margin: 0;
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
