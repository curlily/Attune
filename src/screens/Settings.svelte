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
  let serverError = $state<string | null>(null);
  // Server address, updates and bug reports: folded away until asked for.
  let advancedOpen = $state(false);
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

  // The music app Attune connected last, by name, for the switch below.
  let lastProviderName = $derived(
    app.providers.find((provider) => provider.id === app.settings?.lastProvider)?.displayName ?? null,
  );

  async function toggleStartMusicApp() {
    if (!app.settings) return;
    try {
      app.settings = await api.updateSettings({ startMusicApp: !app.settings.startMusicApp });
    } catch (failure) {
      error = errorText(failure);
    }
  }
  // Only the host's setting counts: someone else's is shown but can't be
  // changed while they're in that session.
  let controlLocked = $derived(app.screen === "jam" && !app.isHost && app.status?.hostClientId != null);
  let controlTitle = $derived(
    controlLocked
      ? `Only the host's setting counts in a session. ${app.hostName ?? "The host"} is hosting this one.`
      : undefined,
  );

  async function toggleControl() {
    if (!app.settings || controlLocked) return;
    try {
      app.settings = await api.updateSettings({ othersCanControl: !app.settings.othersCanControl });
    } catch (failure) {
      error = errorText(failure);
    }
  }

  async function toggleDiscord() {
    if (!app.settings) return;
    try {
      app.settings = await api.updateSettings({ discordPresence: !app.settings.discordPresence });
    } catch (failure) {
      error = errorText(failure);
    }
  }

  async function toggleListenAlong() {
    if (!app.settings) return;
    try {
      app.settings = await api.updateSettings({
        discordListenAlong: !app.settings.discordListenAlong,
      });
    } catch (failure) {
      error = errorText(failure);
    }
  }

  const MODE_LABEL = { flash: "Flash", persistent: "Persistent" };
  const CORNER_LABEL = {
    topLeft: "Top left",
    topRight: "Top right",
    bottomLeft: "Bottom left",
    bottomRight: "Bottom right",
  };
  // What the Overlay row says without opening it.
  let overlaySummary = $derived.by(() => {
    const overlay = app.settings?.overlay;
    if (!overlay?.enabled) return "Off";
    return `${MODE_LABEL[overlay.mode]} · ${CORNER_LABEL[overlay.corner]}`;
  });

  let saved = $state(false);
  let savedTimer: ReturnType<typeof setTimeout> | undefined;
  let serverSaved = $state(false);
  let serverSavedTimer: ReturnType<typeof setTimeout> | undefined;

  let changed = $derived(name.trim() !== (app.settings?.displayName ?? ""));
  let serverChanged = $derived(serverUrl.trim() !== (app.settings?.serverUrl ?? ""));
  let inJam = $derived(app.screen === "jam");

  async function save(event: SubmitEvent) {
    event.preventDefault();
    error = null;
    try {
      app.settings = await api.updateSettings({ displayName: name });
      name = app.settings.displayName ?? "";
      saved = true;
      clearTimeout(savedTimer);
      savedTimer = setTimeout(() => (saved = false), 2000);
    } catch (failure) {
      error = errorText(failure);
    }
  }

  async function saveServer(event: SubmitEvent) {
    event.preventDefault();
    serverError = null;
    try {
      app.settings = await api.updateSettings({ serverUrl });
      serverUrl = app.settings.serverUrl;
      serverSaved = true;
      clearTimeout(serverSavedTimer);
      serverSavedTimer = setTimeout(() => (serverSaved = false), 2000);
    } catch (failure) {
      serverError = errorText(failure);
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

  /** After "Save log": where it went, or what went wrong. */
  let logResult = $state<string | null>(null);
  async function saveLog() {
    logResult = null;
    try {
      const path = await api.saveLog();
      logResult = `Saved as ${path.split(/[\\/]/).pop()}`;
    } catch (failure) {
      logResult = errorText(failure);
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

  {#if autostart === true}
    <button
      class="switch-row"
      role="switch"
      aria-checked={app.settings?.startMusicApp === true}
      onclick={toggleStartMusicApp}
      transition:slide={{ duration: 160 }}
    >
      <span class="switch-text">
        <span class="switch-label">Start your music app too</span>
        <span class="switch-hint">
          {#if lastProviderName}
            Opens {lastProviderName} and connects to it
          {:else}
            Opens the music app you connected last and connects to it
          {/if}
        </span>
      </span>
      <span class="switch" class:on={app.settings?.startMusicApp === true}><span></span></span>
    </button>
  {/if}

  <button
    class="switch-row"
    role="switch"
    aria-checked={app.autoRemove}
    onclick={() => app.setAutoRemove(!app.autoRemove)}
  >
    <span class="switch-label">Remove songs not everyone has</span>
    <span class="switch" class:on={app.autoRemove}><span></span></span>
  </button>

  <button
    class="switch-row"
    role="switch"
    aria-checked={app.settings?.othersCanControl === true}
    disabled={controlLocked}
    title={controlTitle}
    onclick={toggleControl}
  >
    <span class="switch-text">
      <span class="switch-label">Allow others to change what's playing</span>
      <span class="switch-hint">
        {#if controlLocked}
          {app.hostName ?? "The host"} hosts this session, so their setting counts
        {:else}
          Playback and the queue, when you host a session
        {/if}
      </span>
    </span>
    <span class="switch" class:on={app.settings?.othersCanControl === true}><span></span></span>
  </button>

  <button
    class="switch-row"
    role="switch"
    aria-checked={app.settings?.discordPresence === true}
    onclick={toggleDiscord}
  >
    <span class="switch-text">
      <span class="switch-label">Show on Discord</span>
      <span class="switch-hint">"Listening to" the song you're playing, on your profile</span>
    </span>
    <span class="switch" class:on={app.settings?.discordPresence === true}><span></span></span>
  </button>

  {#if app.settings?.discordPresence}
    <button
      class="switch-row"
      role="switch"
      aria-checked={app.settings.discordListenAlong === true}
      onclick={toggleListenAlong}
    >
      <span class="switch-text">
        <span class="switch-label">Listen along button</span>
        <span class="switch-hint">In a session, lets anyone who sees your status join with its code</span>
      </span>
      <span class="switch" class:on={app.settings.discordListenAlong === true}><span></span></span>
    </button>
  {/if}

  <button class="nav-row" onclick={() => (app.settingsPage = "overlay")}>
    <span class="switch-text">
      <span class="switch-label">Overlay</span>
      <span class="switch-hint">Notifications over games</span>
    </span>
    <span class="nav-value">{overlaySummary}</span>
    <span class="nav-arrow"><Icon name="chevron" size={14} /></span>
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

  <button class="advanced-head" aria-expanded={advancedOpen} onclick={() => (advancedOpen = !advancedOpen)}>
    <span class="section-title">Advanced settings</span>
    <span class="chevron" class:collapsed={!advancedOpen}><Icon name="chevron" size={14} /></span>
  </button>

  {#if advancedOpen}
    <div class="advanced" transition:slide={{ duration: 180 }}>
      <form onsubmit={saveServer}>
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
        {#if serverError}
          <p class="error-text" transition:slide={{ duration: 160 }}>{serverError}</p>
        {/if}
        <button
          class="btn wide"
          class:primary={serverChanged}
          type="submit"
          disabled={!serverChanged && !serverSaved}
        >
          {#if serverSaved}<Icon name="check" size={14} /> Saved{:else}Save{/if}
        </button>
      </form>

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

      <p class="section-title">Bug reports</p>
      <div class="app-row">
        <button class="btn small" onclick={saveLog}>Save log</button>
      </div>
      {#if logResult}
        <p class="hint" transition:slide={{ duration: 160 }}>{logResult}</p>
      {/if}
    </div>
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

  .nav-row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    margin-top: 12px;
    padding: 0;
    border: 0;
    background: none;
    text-align: left;
  }
  .nav-row > .switch-text {
    flex: 1;
    min-width: 0;
  }
  .nav-value {
    color: var(--text-2);
    font-size: 12px;
    white-space: nowrap;
  }
  .nav-arrow {
    color: var(--text-3);
    transform: rotate(-90deg);
  }
  .nav-row:hover .nav-arrow {
    color: var(--text);
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
    flex: none;
    white-space: nowrap;
  }

  .advanced-head {
    display: flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    padding: 2px 0;
    border: 0;
    background: none;
    color: var(--text-3);
    text-align: left;
    transition: color var(--fast);
  }
  .advanced-head:hover {
    color: var(--text-2);
  }
  .advanced-head .section-title {
    flex: 1;
    margin: 0;
    color: inherit;
  }
  .chevron {
    display: grid;
    transition: transform var(--base) var(--ease-out);
  }
  .chevron.collapsed {
    transform: rotate(-90deg);
  }
  .advanced .section-title {
    margin-top: 20px;
  }
  .small {
    height: 28px;
    padding: 0 10px;
    font-size: 12px;
  }
</style>
