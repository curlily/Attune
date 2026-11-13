<script lang="ts">
  import { flip } from "svelte/animate";
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import type { RuntimeStatus } from "./api";
  import Icon from "./Icon.svelte";
  import { PROVIDER_NAMES, app } from "./app.svelte";
  import ProviderLogo from "./ProviderLogo.svelte";
  import Section from "./Section.svelte";
  import { participantBadge } from "./status";
  import { providerColors } from "./theme";

  let { status, shown = true }: { status: RuntimeStatus; shown?: boolean } = $props();
</script>

<!-- Below the queue, scrolling once it would take more than about a third. -->
<Section {shown} id="listeners" title="Listening" count={status.participants.length} maxHeight="36%">
  <ul>
    {#each status.participants as person (person.clientId)}
      {@const badge = participantBadge(person)}
      {@const colors = providerColors(person.provider, app.mode)}
      <li
        animate:flip={{ duration: 220 }}
        in:fly={{ x: -12, duration: 260, easing: cubicOut }}
        out:fly={{ x: 12, duration: 180 }}
      >
        <span
          class="avatar"
          title={person.provider ? (PROVIDER_NAMES[person.provider] ?? person.provider) : undefined}
          style:--brand={colors?.accent}
        >
          {#if colors}
            <ProviderLogo provider={person.provider} size={12} />
          {:else}
            <span aria-hidden="true">{person.displayName.slice(0, 1).toUpperCase()}</span>
          {/if}
        </span>
        <span class="name">{person.displayName}</span>
        {#if person.clientId === status.clientId}<span class="you">you</span>{/if}
        <span class="spacer"></span>
        <span class="state {badge.tone}">
          <Icon name={badge.icon} size={13} />
          {badge.text}
        </span>
      </li>
    {/each}
  </ul>
</Section>

<style>
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
  }

  .avatar {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    flex: none;
    background: color-mix(in srgb, var(--brand, var(--text-2)) 16%, var(--surface-1));
    color: var(--brand, var(--text-2));
    font-size: 11px;
    font-weight: 600;
  }
  .name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .you {
    padding: 0 6px;
    border-radius: 10px;
    background: var(--surface-2);
    color: var(--text-3);
    font-size: 11px;
  }
  .spacer {
    flex: 1;
  }

  .state {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: none;
    font-size: 11.5px;
  }
  .success {
    color: var(--success);
  }
  .warning {
    color: var(--warning);
  }
  .danger {
    color: var(--danger);
  }
  .muted {
    color: var(--text-3);
  }
  .accent {
    color: var(--accent);
  }
</style>
