<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { fade, fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { Effect, LogicalSize, getCurrentWindow } from "@tauri-apps/api/window";
  import { FULL_HEIGHT, TINY_HEIGHT, app } from "./lib/app.svelte";
  import { api, errorText } from "./lib/api";
  import Icon from "./lib/Icon.svelte";
  import Logo from "./lib/Logo.svelte";
  import Toasts from "./lib/Toasts.svelte";
  import FirstRun from "./screens/FirstRun.svelte";
  import Connect from "./screens/Connect.svelte";
  import Lobby from "./screens/Lobby.svelte";
  import Jam from "./screens/Jam.svelte";
  import Mini from "./screens/Mini.svelte";
  import SettingsScreen from "./screens/Settings.svelte";
  import { applyTheme } from "./lib/theme";

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

  $effect(() => applyTheme(app.theme, app.connected, app.mode));

  // Acrylic: Windows blurs what's behind the (transparent) window, and the
  // page's backgrounds turn see-through (app.css, [data-acrylic]). It's
  // Windows' blur-behind, not its "acrylic" effect: on Windows 11 that one
  // is a system backdrop, which Windows only draws on windows with a frame,
  // so on Attune's frameless window it left plain, unblurred transparency.
  // The tint and grain that make it acrylic come from the page instead.
  $effect(() => {
    const on = app.acrylic;
    document.documentElement.toggleAttribute("data-acrylic", on);
    const window = getCurrentWindow();
    (on ? window.setEffects({ effects: [Effect.Blur] }) : window.clearEffects()).catch(() => {});
  });

  // Only the session view can be resized (by the user); elsewhere the
  // window can't be (it starts that way, see tauri.conf.json). The lobby is
  // short, so the window fits it instead of showing empty space; other
  // screens, and a session starting, get the full height.
  const HEADER_HEIGHT = 44;
  $effect(() => {
    const inSession = app.screen === "jam";
    const window = getCurrentWindow();
    window.setMinSize(new LogicalSize(380, TINY_HEIGHT)).catch(() => {});
    window.setResizable(inSession).catch(() => {});
    if (inSession) return;
    const fit = app.screen === "lobby" && !app.showSettings && app.lobbyHeight > 0;
    void app.setHeight(fit ? Math.min(FULL_HEIGHT, HEADER_HEIGHT + app.lobbyHeight) : FULL_HEIGHT);
  });
  let wasInSession = false;
  $effect(() => {
    const inSession = app.screen === "jam";
    if (inSession && !wasInSession && untrack(() => app.height) < FULL_HEIGHT) {
      void app.setHeight(FULL_HEIGHT);
    }
    wasInSession = inSession;
  });

  /** The window has no frame: this strip along the bottom resizes it. */
  function startResize(event: PointerEvent) {
    if (event.button !== 0 || app.screen !== "jam") return;
    event.preventDefault();
    void getCurrentWindow().startResizeDragging("South").catch(() => {});
  }

  onMount(() => {
    app.init().catch((error) => (failure = errorText(error)));
  });

  let title = $derived(
    app.showSettings ? "Settings" : app.screen === "jam" ? "Session" : "Attune",
  );

  // Screens rise into place; coming back from Settings (which slides in
  // from the right) slides back in from the left instead.
  let backFromSettings = false;
  $effect.pre(() => {
    if (app.showSettings) backFromSettings = true;
  });
  function screenIntro() {
    const back = backFromSettings;
    backFromSettings = false;
    return back
      ? { x: -24, duration: 220, easing: cubicOut }
      : { y: 10, duration: 260, delay: 60, easing: cubicOut };
  }
</script>

<div class="window" bind:this={windowElement}>
{#if app.tiny}
  <Mini />
{:else}
  <div class="shell" in:fade={{ duration: 200 }}>
    <!-- No window frame: the header is where the window is dragged. -->
    <header data-tauri-drag-region>
      <Logo background={false} />
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
        <button class="icon-btn" title="Tiny view" onclick={() => app.toggleTiny()}>
          <Icon name="shrink" label="Tiny view" />
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
        title={app.pinned ? "Unpin: stop keeping on top" : "Pin: keep on top of other windows"}
        aria-pressed={app.pinned}
        onclick={() => app.setPinned(!app.pinned)}
      >
        <Icon name="pin" label="Pin" />
      </button>
      <button class="icon-btn" title="Minimize to the tray" onclick={() => api.hideWindow()}>
        <Icon name="minimize" label="Minimize to the tray" />
      </button>
    </header>

    <main>
      {#if failure}
        <p class="failure">Attune couldn't start: {failure}</p>
      {:else if app.showSettings}
        <div class="screen" in:fly={{ x: 24, duration: 220, easing: cubicOut }}>
          <SettingsScreen />
        </div>
      {:else}
        {#key app.screen}
          <div
            class="screen"
            in:fly|global={screenIntro()}
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
{#if app.screen === "jam"}
  <div class="resize" onpointerdown={startResize} aria-hidden="true"></div>
{/if}
</div>

<Toasts />

<style>
  /* A soft wash of the accent from the top, behind the header and the
     current song, gone by the middle of the window. */
  .window {
    position: relative;
    height: 100%;
    background: radial-gradient(130% 55% at 50% -8%, var(--glow), transparent 72%);
    /* Its own stacking context, so the grain below can sit behind the
       content (z-index -1) and never over the text. */
    isolation: isolate;
  }
  /* Acrylic's fine grain over the blur, as Windows draws it. */
  :global(:root[data-acrylic]) .window::after {
    content: "";
    position: absolute;
    inset: 0;
    z-index: -1;
    pointer-events: none;
    opacity: 0.035;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='160' height='160'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.9' numOctaves='2' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
  }

  .resize {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 5;
    height: 5px;
    cursor: ns-resize;
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
    border: 1px solid color-mix(in oklab, var(--accent) 30%, transparent);
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
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
