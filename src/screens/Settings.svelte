<script lang="ts">
  import { slide } from "svelte/transition";
  import { api, errorText } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";
  import ProviderLogo from "../lib/ProviderLogo.svelte";

  let name = $state(app.settings?.displayName ?? "");
  let serverUrl = $state(app.settings?.serverUrl ?? "");
  let error = $state<string | null>(null);
  let autostart = $state<boolean | null>(null);
  api.getAutostart().then((enabled) => (autostart = enabled), () => (autostart = null));

  async function toggleAutostart() {
    if (autostart === null) return;
    try {
      autostart = await api.setAutostart(!autostart);
    } catch (failure) {
      error = errorText(failure);
    }
  }
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
    <p class="hint">Change this only if you run your own Attune server.</p>

    {#if error}<p class="error-text" transition:slide={{ duration: 160 }}>{error}</p>{/if}

    <button class="btn wide" class:primary={changed} type="submit" disabled={!changed && !saved}>
      {#if saved}<Icon name="check" size={14} /> Saved{:else}Save{/if}
    </button>
  </form>

  <div class="divider"></div>

  <button
    class="switch-row"
    role="switch"
    aria-checked={autostart === true}
    disabled={autostart === null}
    onclick={toggleAutostart}
  >
    <span>
      <span class="switch-label">Start with Windows</span>
      <span class="hint">Attune waits in the tray until you need it.</span>
    </span>
    <span class="switch" class:on={autostart === true}><span></span></span>
  </button>

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

  .switch-row {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 0;
    border: 0;
    background: none;
    text-align: left;
  }
  .switch-row > span:first-child {
    flex: 1;
    display: flex;
    flex-direction: column;
  }
  .switch-row:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .switch-label {
    font-weight: 600;
  }
  .switch {
    position: relative;
    flex: none;
    width: 34px;
    height: 20px;
    border-radius: 10px;
    background: var(--surface-3);
    transition: background var(--base);
  }
  .switch > span {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--text-2);
    transition:
      transform var(--base) var(--ease-spring),
      background var(--base);
  }
  .switch.on {
    background: var(--accent-strong);
  }
  .switch.on > span {
    transform: translateX(14px);
    background: var(--on-accent);
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
