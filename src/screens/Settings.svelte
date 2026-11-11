<script lang="ts">
  import { slide } from "svelte/transition";
  import { api, errorText } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";
  import ProviderLogo from "../lib/ProviderLogo.svelte";

  let name = $state(app.settings?.displayName ?? "");
  let serverUrl = $state(app.settings?.serverUrl ?? "");
  let error = $state<string | null>(null);
  let saved = $state(false);
  let savedTimer: ReturnType<typeof setTimeout> | undefined;

  let changed = $derived(
    name.trim() !== (app.settings?.displayName ?? "") ||
      serverUrl.trim() !== (app.settings?.serverUrl ?? ""),
  );
  let inJam = $derived(app.screen === "jam");

  async function save(event: SubmitEvent) {
    event.preventDefault();
    error = null;
    try {
      app.settings = await api.updateSettings({ displayName: name, serverUrl });
      name = app.settings.displayName ?? "";
      serverUrl = app.settings.serverUrl;
      saved = true;
      clearTimeout(savedTimer);
      savedTimer = setTimeout(() => (saved = false), 2000);
    } catch (failure) {
      error = errorText(failure);
    }
  }

  async function disconnect() {
    try {
      await api.disconnectProvider();
      app.connected = null;
      app.status = null;
      app.code = null;
      app.showSettings = false;
    } catch (failure) {
      error = errorText(failure);
    }
  }
</script>

<div class="settings scroll">
  <button class="btn ghost back" onclick={() => (app.showSettings = false)}>
    <Icon name="back" size={14} /> Back
  </button>

  <form onsubmit={save}>
    <label class="label" for="name">Your name</label>
    <input id="name" class="field" bind:value={name} maxlength="32" autocomplete="off" />
    {#if inJam}
      <p class="hint">Others see a new name the next time you join a jam.</p>
    {/if}

    <label class="label" for="server">Server address</label>
    <input id="server" class="field mono" bind:value={serverUrl} autocomplete="off" spellcheck="false" />
    <p class="hint">Change this only if you run your own MyJam server.</p>

    {#if error}<p class="error-text" transition:slide={{ duration: 160 }}>{error}</p>{/if}

    <button class="btn wide" class:primary={changed} type="submit" disabled={!changed && !saved}>
      {#if saved}<Icon name="check" size={14} /> Saved{:else}Save{/if}
    </button>
  </form>

  <div class="divider"></div>

  <p class="section-title">Music app</p>
  <div class="app-row">
    {#if app.connected}
      <ProviderLogo provider={app.connected} size={16} />
      <span>{app.providerName}</span>
      <span class="spacer"></span>
      <button class="btn small" onclick={disconnect}>
        {inJam ? "Leave and disconnect" : "Disconnect"}
      </button>
    {:else}
      <span class="muted">Not connected</span>
    {/if}
  </div>
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

  form .label {
    margin-top: 12px;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .hint {
    margin: 5px 0 0;
    color: var(--text-3);
    font-size: 11.5px;
  }
  form .btn {
    margin-top: 16px;
  }
  form .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .divider {
    height: 1px;
    margin: 18px 0 14px;
    background: var(--border);
  }

  .app-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .spacer {
    flex: 1;
  }
  .small {
    height: 28px;
    padding: 0 10px;
    font-size: 12px;
  }
</style>
