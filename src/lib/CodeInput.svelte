<script lang="ts">
  // Four letter boxes backed by one input, so typing, deleting and pasting
  // all behave like a normal text field.
  const ALLOWED = /[ABCDEFGHJKLMNPQRSTUVWXYZ]/;
  const LENGTH = 4;

  let {
    value = $bindable(""),
    disabled = false,
    oncomplete,
  }: { value?: string; disabled?: boolean; oncomplete?: (code: string) => void } = $props();

  let focused = $state(false);
  let input: HTMLInputElement;

  function clean(raw: string): string {
    return [...raw.toUpperCase()].filter((char) => ALLOWED.test(char)).join("").slice(0, LENGTH);
  }

  function onInput() {
    value = clean(input.value);
    input.value = value;
    if (value.length === LENGTH) oncomplete?.(value);
  }

  export function focus() {
    input?.focus();
  }
</script>

<label class="code" class:focused class:disabled>
  <span class="sr-only">Join code</span>
  <input
    bind:this={input}
    {value}
    {disabled}
    oninput={onInput}
    onfocus={() => (focused = true)}
    onblur={() => (focused = false)}
    maxlength="8"
    autocomplete="off"
    autocapitalize="characters"
    spellcheck="false"
  />
  {#each Array(LENGTH) as _, index}
    <span
      class="box"
      class:filled={index < value.length}
      class:active={focused && index === Math.min(value.length, LENGTH - 1)}
    >
      {value[index] ?? ""}
    </span>
  {/each}
</label>

<style>
  .code {
    position: relative;
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
    cursor: text;
  }
  .code.disabled {
    opacity: 0.6;
  }

  input {
    position: absolute;
    inset: 0;
    opacity: 0;
    width: 100%;
    cursor: text;
  }

  .box {
    display: grid;
    place-items: center;
    height: 46px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--surface-1);
    font: 600 20px var(--font-mono);
    transition:
      border-color var(--fast),
      background var(--fast),
      transform var(--base) var(--ease-spring);
  }
  .box.filled {
    background: var(--surface-2);
    transform: translateY(-1px);
  }
  .box.active {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-tint);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
</style>
