<script lang="ts">
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { app } from "./app.svelte";
  import Icon from "./Icon.svelte";
</script>

<!-- Friends asking through Discord's "Ask to Join". Accepting sends them a
     Discord invite, which opens Attune for them and joins this session. -->
{#each app.tiny ? [] : app.joinRequests as request (request.userId)}
  <div
    class="request"
    role="dialog"
    aria-label="{request.name} wants to join"
    in:fly={{ y: -12, duration: 240, easing: cubicOut }}
    out:fly={{ y: -12, duration: 160 }}
  >
    {#if request.avatarUrl}
      <img class="avatar" src={request.avatarUrl} alt="" />
    {:else}
      <span class="avatar initial" aria-hidden="true">{request.name.slice(0, 1).toUpperCase()}</span>
    {/if}
    <div class="text">
      <strong>{request.name}</strong>
      <span>wants to join from Discord</span>
    </div>
    <button class="btn primary" onclick={() => app.answerJoinRequest(request.userId, true)}>
      Accept
    </button>
    <button class="icon-btn" title="Decline" onclick={() => app.answerJoinRequest(request.userId, false)}>
      <Icon name="x" size={13} label="Decline" />
    </button>
  </div>
{/each}

<style>
  .request {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 6px 8px 10px;
    border: 1px solid color-mix(in oklab, var(--accent) 35%, var(--border-strong));
    border-radius: var(--radius);
    background: var(--surface-3);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.35);
  }
  .avatar {
    flex: none;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    object-fit: cover;
  }
  .initial {
    display: grid;
    place-items: center;
    background: var(--accent-tint);
    color: var(--accent);
    font-weight: 700;
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .text strong {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .text span {
    color: var(--text-2);
    font-size: 11.5px;
  }
  .btn {
    height: 28px;
    padding: 0 10px;
    font-size: 12px;
  }
  .icon-btn {
    width: 24px;
    height: 24px;
  }
</style>
