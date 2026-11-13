<script lang="ts">
  import { onMount } from "svelte";
  import { slide } from "svelte/transition";
  import { api, errorText, type ProviderAvailability, type ProviderId } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Icon from "../lib/Icon.svelte";
  import ProviderLogo from "../lib/ProviderLogo.svelte";

  const POLL_MS = 3000;

  let connecting = $state<ProviderId | null>(null);
  let error = $state<{ id: ProviderId; text: string } | null>(null);

  // Keep the list fresh while this screen is open, e.g. after opening an app.
  onMount(() => {
    void app.refreshProviders();
    const timer = setInterval(() => {
      if (!connecting) void app.refreshProviders();
    }, POLL_MS);
    return () => clearInterval(timer);
  });

  // Browser providers whose sign-in refuses the window Attune connects to
  // (Google's does), so signing in is a step of its own. Apple's sign-in
  // works in the connected window (verified by the user).
  const SIGN_IN_FIRST = new Set<ProviderId>(["YoutubeMusic"]);

  const needsSignInStep = (provider: ProviderAvailability) =>
    SIGN_IN_FIRST.has(provider.id) && provider.inBrowser && provider.installed && !provider.ready;

  function describe(provider: ProviderAvailability): { state: string; note?: string; action?: string } {
    if (provider.id === app.connected) return { state: "Connected", action: `Keep ${provider.displayName}` };
    if (provider.unsupported) return { state: "Not supported", note: provider.unsupported };
    if (provider.ready) return { state: "Ready", action: "Connect" };
    if (provider.inBrowser) {
      if (!provider.installed)
        return { state: "No browser", note: "Needs Microsoft Edge, Google Chrome or Brave." };
      return { state: "Not open", action: `Open ${provider.displayName}` };
    }
    if (provider.running)
      return {
        state: "Running",
        note: `${provider.displayName} restarts once to connect. Your music pauses for a few seconds.`,
        action: "Restart and connect",
      };
    if (provider.installed) return { state: "Not open", action: `Open ${provider.displayName}` };
    return { state: "Not installed" };
  }

  async function signIn(id: ProviderId) {
    error = null;
    try {
      await api.openProviderSignIn(id);
    } catch (failure) {
      error = { id, text: errorText(failure) };
    }
  }

  /** Back to the lobby with the app that's still connected (from "Change"). */
  function back() {
    app.choosingProvider = false;
  }

  async function connect(id: ProviderId) {
    if (id === app.connected) return back();
    connecting = id;
    error = null;
    try {
      // Switching: only now let go of the current app.
      if (app.connected) {
        await api.disconnectProvider();
        app.connected = null;
      }
      await api.connectProvider(id);
      app.connected = id;
      app.choosingProvider = false;
    } catch (failure) {
      error = { id, text: errorText(failure) };
      void app.refreshProviders();
    } finally {
      connecting = null;
    }
  }

  // Apps that can be used first, then the rest.
  let sorted = $derived(
    [...app.providers].sort((a, b) => Number(b.installed) - Number(a.installed)),
  );
</script>

<div class="connect scroll">
  {#if app.connected}
    <button class="btn ghost back" onclick={back} disabled={connecting !== null}>
      <Icon name="back" size={14} /> Back
    </button>
  {/if}
  <p class="section-title">{app.connected ? "Switch music app" : "Connect your music app"}</p>
  <p class="muted intro">
    Attune follows along in your own app. You keep using it for play, pause, skip
    and the queue.
  </p>

  {#each sorted as provider (provider.id)}
    {@const info = describe(provider)}
    <div class="card" class:highlight={provider.ready || provider.id === app.connected} class:dim={!info.action}>
      <div class="row">
        <ProviderLogo provider={provider.id} size={20} />
        <span class="name">{provider.displayName}</span>
        <span class="spacer"></span>
        <span class="state" class:ok={provider.ready || provider.id === app.connected}>{info.state}</span>
      </div>
      {#if info.note}
        <p class="note">{info.note}</p>
      {/if}
      {#if info.action}
        <button
          class="btn wide"
          class:primary={provider.ready && provider.id !== app.connected}
          disabled={connecting !== null}
          aria-busy={connecting === provider.id}
          onclick={() => connect(provider.id)}
        >
          {#if connecting === provider.id}
            <span class="spinner"></span> Connecting…
          {:else}
            {info.action}
          {/if}
        </button>
      {/if}
      {#if needsSignInStep(provider)}
        <!-- Opens while the card is hovered or focused, so it stays tidy. -->
        <div class="extra">
          <div class="extra-inner">
            <p class="note">
              {provider.displayName} opens in a browser window of its own. To use your account,
              sign in once first, then close that window and connect.
            </p>
            <button class="btn ghost wide" disabled={connecting !== null} onclick={() => signIn(provider.id)}>
              Sign in first
            </button>
          </div>
        </div>
      {/if}
      {#if error?.id === provider.id}
        <p class="error-text" transition:slide={{ duration: 160 }}>{error.text}</p>
      {/if}
    </div>
  {/each}
</div>

<style>
  .connect {
    height: 100%;
    padding: 16px 14px;
  }

  .intro {
    margin: 0 0 14px;
  }

  .back {
    margin: -6px 0 8px -8px;
    height: 28px;
    padding: 0 8px;
    color: var(--text-2);
  }

  .card {
    padding: 12px;
    margin-bottom: 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    transition:
      border-color var(--base),
      opacity var(--base);
  }
  .card.highlight {
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .card.dim {
    opacity: 0.65;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .name {
    font-weight: 600;
  }
  .spacer {
    flex: 1;
  }
  .state {
    padding: 1px 8px;
    border-radius: 20px;
    background: var(--surface-2);
    color: var(--text-2);
    font-size: 11.5px;
  }
  .state.ok {
    background: var(--success-tint);
    color: var(--success);
  }

  .note {
    margin: 8px 0 0;
    color: var(--text-2);
    font-size: 12px;
  }

  /* Grid rows animate from 0 to the content's height. */
  .extra {
    display: grid;
    grid-template-rows: 0fr;
    opacity: 0;
    transition:
      grid-template-rows var(--base),
      opacity var(--base);
  }
  .extra-inner {
    min-height: 0;
    overflow: hidden;
  }
  .card:hover .extra,
  .card:focus-within .extra {
    grid-template-rows: 1fr;
    opacity: 1;
  }
  /* Hidden, its button can't be reached with Tab until the card opens. */
  .card:not(:hover):not(:focus-within) .extra {
    visibility: hidden;
    transition:
      grid-template-rows var(--base),
      opacity var(--base),
      visibility 0s var(--base);
  }

  .btn {
    margin-top: 10px;
  }
  .btn:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
