<script lang="ts">
  import { slide } from "svelte/transition";
  import { api, errorText } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";
  import ProviderLogo from "../lib/ProviderLogo.svelte";
  import { updates } from "../lib/updates.svelte";

  let name = $state(app.settings?.displayName ?? "");
  let serverUrl = $state(app.settings?.serverUrl ?? "");
  let error = $state<string | null>(null);
  // The address is kept out of sight (screen sharing) until asked for.
  let revealServer = $state(false);
  let serverInput = $state<HTMLInputElement>();
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

  /** After "Check for updates": a found update shows its own offer. */
  let updateResult = $state<string | null>(null);
  async function checkUpdates() {
    updateResult = null;
    try {
      const found = await updates.check();
      updateResult = found ? null : "You have the latest version.";
    } catch (failure) {
      updateResult = errorText(failure);
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
      <p class="hint">Others see a new name the next time you join a session.</p>
    {/if}

    <label class="label" for="server">Server address</label>
    {#if revealServer}
      <input
        id="server"
        class="field mono"
        bind:this={serverInput}
        bind:value={serverUrl}
        autocomplete="off"
        spellcheck="false"
      />
    {:else}
      <button
        id="server"
        type="button"
        class="field reveal"
        onclick={async () => {
          revealServer = true;
          await Promise.resolve();
          serverInput?.focus();
        }}
      >
        <span class="dots" aria-hidden="true">••••••••••••••••</span>
        <span>Click to reveal</span>
      </button>
    {/if}
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
    <span class="switch-label">Start with Windows</span>
    <span class="switch" class:on={autostart === true}><span></span></span>
  </button>

  <button
    class="switch-row"
    role="switch"
    aria-checked={app.autoRemove}
    onclick={() => app.setAutoRemove(!app.autoRemove)}
  >
    <span class="switch-label">Remove songs not everyone has</span>
    <span class="switch" class:on={app.autoRemove}><span></span></span>
  </button>

  <div class="divider"></div>

  <p class="section-title">Theme</p>
  <div class="segmented" role="radiogroup" aria-label="Theme">
    <button role="radio" aria-checked={app.theme === "attune"} onclick={() => app.setTheme("attune")}>
      Attune
    </button>
    <button role="radio" aria-checked={app.theme === "auto"} onclick={() => app.setTheme("auto")}>
      {#if app.connected}<ProviderLogo provider={app.connected} size={13} />{/if}
      Match {app.connected ? app.providerName : "music app"}
    </button>
  </div>

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

  <div class="divider"></div>

  <p class="section-title">Updates</p>
  <div class="app-row">
    <span>Attune {updates.version ?? ""}</span>
    <span class="spacer"></span>
    <button
      class="btn small"
      aria-busy={updates.checking}
      disabled={updates.checking || updates.installing}
      onclick={checkUpdates}
    >
      {updates.checking ? "Checking…" : "Check for updates"}
    </button>
  </div>
  {#if updateResult}
    <p class="hint" transition:slide={{ duration: 160 }}>{updateResult}</p>
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

  .reveal {
    display: flex;
    align-items: center;
    justify-content: space-between;
    color: var(--text-2);
    font-size: 12px;
    cursor: pointer;
  }
  .reveal:hover {
    color: var(--text);
  }
  .reveal .dots {
    color: var(--text-3);
    letter-spacing: 0.1em;
  }

  .switch-row + .switch-row {
    margin-top: 12px;
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
  .switch-row > .switch-label {
    flex: 1;
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

  .segmented {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--surface-1);
  }
  .segmented button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-width: 0;
    height: 28px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-2);
    font-size: 12px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transition:
      background var(--fast),
      color var(--fast);
  }
  .segmented button:hover {
    color: var(--text);
  }
  .segmented button[aria-checked="true"] {
    background: var(--accent-tint);
    color: var(--accent);
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
