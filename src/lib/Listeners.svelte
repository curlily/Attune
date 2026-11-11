<script lang="ts">
  import { flip } from "svelte/animate";
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import type { RuntimeStatus } from "./api";
  import Icon from "./Icon.svelte";
  import ProviderLogo from "./ProviderLogo.svelte";
  import { participantBadge } from "./status";

  let { status }: { status: RuntimeStatus } = $props();
</script>

<section>
  <p class="section-title">Listening · {status.participants.length}</p>
  <ul>
    {#each status.participants as person (person.clientId)}
      {@const badge = participantBadge(person)}
      <li
        animate:flip={{ duration: 220 }}
        in:fly={{ x: -12, duration: 260, easing: cubicOut }}
        out:fly={{ x: 12, duration: 180 }}
      >
        <span class="avatar" aria-hidden="true">{person.displayName.slice(0, 1).toUpperCase()}</span>
        <span class="name">{person.displayName}</span>
        {#if person.clientId === status.clientId}<span class="you">you</span>{/if}
        <span class="provider" title={person.provider}><ProviderLogo provider={person.provider} size={13} /></span>
        <span class="spacer"></span>
        <span class="state {badge.tone}">
          <Icon name={badge.icon} size={13} />
          {badge.text}
        </span>
      </li>
    {/each}
  </ul>
</section>

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
    background: var(--surface-3);
    color: var(--text-2);
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
  .provider {
    color: var(--text-3);
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
