<script lang="ts">
  import { api, errorText } from "../lib/api";
  import { app } from "../lib/app.svelte";

  let name = $state("");
  let error = $state<string | null>(null);
  let saving = $state(false);

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!name.trim()) {
      error = "Enter a name first.";
      return;
    }
    saving = true;
    try {
      app.settings = await api.updateSettings({ displayName: name });
    } catch (failure) {
      error = errorText(failure);
    } finally {
      saving = false;
    }
  }
</script>

<form class="first-run" onsubmit={save}>
  <div class="hero" aria-hidden="true">
    <span></span><span></span><span></span>
  </div>
  <h2>Listen together</h2>
  <p class="muted">
    Attune keeps your music app in step with your friends'. Everyone controls
    playback from their own app.
  </p>

  <label class="label" for="name">What should others see you as?</label>
  <!-- svelte-ignore a11y_autofocus -->
  <input
    id="name"
    class="field"
    bind:value={name}
    oninput={() => (error = null)}
    maxlength="32"
    placeholder="Your name"
    autocomplete="off"
    autofocus
  />
  {#if error}<p class="error-text">{error}</p>{/if}

  <button class="btn primary wide" type="submit" aria-busy={saving}>Continue</button>
</form>

<style>
  .first-run {
    display: flex;
    flex-direction: column;
    padding: 28px 20px 20px;
  }

  h2 {
    margin: 18px 0 6px;
    font-size: 19px;
    font-weight: 600;
  }

  p.muted {
    margin: 0 0 24px;
  }

  .btn {
    margin-top: 16px;
  }

  /* Three bars bouncing like an equaliser. */
  .hero {
    display: flex;
    align-items: flex-end;
    gap: 5px;
    height: 34px;
  }
  .hero span {
    width: 8px;
    border-radius: 3px;
    background: var(--accent);
    animation: bar 1.1s var(--ease-out) infinite alternate;
  }
  .hero span:nth-child(1) {
    height: 60%;
  }
  .hero span:nth-child(2) {
    height: 100%;
    animation-delay: -0.4s;
    background: #9c8cff;
  }
  .hero span:nth-child(3) {
    height: 75%;
    animation-delay: -0.8s;
    background: var(--accent-strong);
  }
  @keyframes bar {
    to {
      transform: scaleY(0.35);
    }
  }
  .hero span {
    transform-origin: bottom;
  }
</style>
