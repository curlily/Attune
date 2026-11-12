<script lang="ts">
  import { slide } from "svelte/transition";
  import { api, errorText } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import CodeInput from "../lib/CodeInput.svelte";
  import Icon from "../lib/Icon.svelte";
  import ProviderLogo from "../lib/ProviderLogo.svelte";

  type Busy = "join" | "create" | "rejoin" | null;

  let code = $state("");
  let busy = $state<Busy>(null);
  let error = $state<string | null>(null);

  async function run(kind: Exclude<Busy, null>, start: Parameters<typeof app.enter>[0]) {
    if (busy) return;
    busy = kind;
    error = null;
    try {
      await app.enter(start);
    } catch (failure) {
      error = errorText(failure);
      if (kind === "rejoin") void app.refreshRejoin();
    } finally {
      busy = null;
    }
  }

  // A attune:// link: fill in its code and join.
  $effect(() => {
    const linked = app.linkCode;
    if (!linked || busy) return;
    app.linkCode = null;
    code = linked;
    join();
  });

  function join() {
    if (code.length !== 4) {
      error = "A join code has four letters.";
      return;
    }
    void run("join", () => api.joinSession(code));
  }

  async function change() {
    try {
      await api.disconnectProvider();
      app.connected = null;
    } catch (failure) {
      error = errorText(failure);
    }
  }
</script>

<div class="lobby scroll">
<div class="content" bind:offsetHeight={app.lobbyHeight}>
  <div class="connected">
    <ProviderLogo provider={app.connected} size={16} />
    <span>Connected to {app.providerName}</span>
    <span class="spacer"></span>
    <button class="btn ghost small" onclick={change} disabled={busy !== null}>Change</button>
  </div>

  {#if app.rejoinCode}
    <div class="rejoin" transition:slide={{ duration: 200 }}>
      <div>
        <p class="title">Rejoin {app.rejoinCode}?</p>
        <p class="muted">You were in this session when Attune closed.</p>
      </div>
      <div class="rejoin-actions">
        <button
          class="btn primary"
          aria-busy={busy === "rejoin"}
          onclick={() => run("rejoin", api.rejoinSession)}
        >
          {#if busy === "rejoin"}<span class="spinner"></span>{/if} Rejoin
        </button>
        <button
          class="icon-btn"
          title="Don't rejoin"
          onclick={async () => {
            await api.dismissRejoin();
            app.rejoinCode = null;
          }}
        >
          <Icon name="x" label="Don't rejoin" />
        </button>
      </div>
    </div>
  {/if}

  <section>
    <p class="section-title">Join a session</p>
    <CodeInput
      bind:value={code}
      disabled={busy !== null}
      oncomplete={() => {
        error = null;
      }}
    />
    <button class="btn wide join" class:primary={code.length === 4} onclick={join} aria-busy={busy === "join"}>
      {#if busy === "join"}
        <span class="spinner"></span> Joining and finding songs in {app.providerName}…
      {:else}
        Join
      {/if}
    </button>
  </section>

  <div class="or"><span>or</span></div>

  <button class="btn wide" onclick={() => run("create", api.createSession)} aria-busy={busy === "create"}>
    {#if busy === "create"}
      <span class="spinner"></span> Starting…
    {:else}
      Start a new session with your current queue
    {/if}
  </button>

  {#if error}
    <p class="error-text" transition:slide={{ duration: 160 }}>{error}</p>
  {/if}
</div>
</div>

<style>
  /* The window fits the content (App.svelte); it scrolls only when that
     would be taller than the full window. */
  .lobby {
    height: 100%;
  }
  .content {
    padding: 14px 14px 18px;
  }

  .connected {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 36px;
    padding: 0 4px 0 11px;
    margin-bottom: 16px;
    border-radius: var(--radius);
    background: var(--surface-1);
    color: var(--text-2);
  }
  .spacer {
    flex: 1;
  }
  .small {
    height: 26px;
    padding: 0 9px;
    font-size: 12px;
  }

  .rejoin {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px;
    margin-bottom: 18px;
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
    border-radius: var(--radius-lg);
    background: var(--accent-tint);
  }
  .rejoin > div:first-child {
    flex: 1;
  }
  .rejoin p {
    margin: 0;
  }
  .rejoin .title {
    font-weight: 600;
  }
  .rejoin .muted {
    font-size: 12px;
  }
  .rejoin-actions {
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .join {
    margin-top: 10px;
  }

  .or {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 18px 0;
    color: var(--text-3);
    font-size: 12px;
  }
  .or::before,
  .or::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--border);
  }
</style>
