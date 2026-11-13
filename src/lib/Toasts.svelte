<script lang="ts">
  import { flip } from "svelte/animate";
  import { fly, fade } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { app } from "./app.svelte";
  import Icon, { type IconName } from "./Icon.svelte";

  const ICON: Record<string, IconName> = {
    info: "music",
    success: "check",
    warning: "alert",
    error: "alert",
  };
</script>

<!-- The tiny view is too small for messages; its dots show the state. -->
<div class="toasts" aria-live="polite">
  {#each app.tiny ? [] : app.toasts as toast (toast.id)}
    <div
      class="toast {toast.tone}"
      animate:flip={{ duration: 200 }}
      in:fly={{ y: 16, duration: 240, easing: cubicOut }}
      out:fade={{ duration: 140 }}
    >
      <Icon name={ICON[toast.tone]} size={15} />
      <span>{toast.text}</span>
      {#if toast.action}
        <button
          class="btn action"
          onclick={() => {
            toast.action?.run();
            app.dismissToast(toast.id);
          }}>{toast.action.label}</button
        >
      {/if}
      <button class="icon-btn" title="Dismiss" onclick={() => app.dismissToast(toast.id)}>
        <Icon name="x" size={13} label="Dismiss" />
      </button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    left: 10px;
    right: 10px;
    bottom: 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    pointer-events: none;
    z-index: 10;
  }

  .toast {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 6px 8px 11px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--surface-3);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.35);
    pointer-events: auto;
  }
  .toast span {
    flex: 1;
  }
  .toast .action {
    flex: none;
    height: 26px;
    padding: 0 9px;
    font-size: 12px;
  }
  .toast .icon-btn {
    width: 22px;
    height: 22px;
  }
  .success > :global(svg) {
    color: var(--success);
  }
  .warning > :global(svg) {
    color: var(--warning);
  }
  .error > :global(svg) {
    color: var(--danger);
  }
  .info > :global(svg) {
    color: var(--accent);
  }
</style>
