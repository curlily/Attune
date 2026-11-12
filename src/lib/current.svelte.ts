// The session's current song as the views show it: its cover and live
// position.
//
// Songs carry the art of the provider that queued them. When that's missing
// (a song from an older Attune), ask this computer's music app while it
// plays the song. Remembered per song, so it stays when stepping out (the
// app may then play something else).

import { api, type RuntimeStatus } from "./api";

const fetched = new Map<string, string>();

/** Tracks the current song's cover; call during component setup. */
export function currentCover(status: () => RuntimeStatus | null) {
  let local = $state<string | null>(null);

  $effect(() => {
    const current = status()?.queue[0];
    const entryId = current?.entryId;
    local = entryId ? (fetched.get(entryId) ?? null) : null;
    if (!entryId || current.track.coverUrl || local) return;
    if (!current.playableHere || status()?.sync !== "synced") return;
    let cancelled = false;
    // Give the app a moment to load the new track first.
    const timer = setTimeout(async () => {
      const url = await api.coverUrl().catch(() => null);
      if (cancelled || !url) return;
      fetched.set(entryId, url);
      local = url;
    }, 700);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  });

  return {
    get url(): string | null {
      return status()?.queue[0]?.track.coverUrl ?? local;
    },
  };
}

/**
 * Live position of the session's playback, redrawn every frame while
 * playing. Playback is controlled in the music app, so this is display only.
 */
export function livePosition(status: () => RuntimeStatus | null) {
  let now = $state(Date.now());
  $effect(() => {
    if (!status()?.playback.isPlaying) return;
    let frame = requestAnimationFrame(function tick() {
      now = Date.now();
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  });

  const duration = $derived(status()?.queue[0]?.track.durationMs ?? 0);
  const position = $derived.by(() => {
    const playback = status()?.playback;
    if (!playback) return 0;
    const { isPlaying, positionMs, positionCapturedAtMs } = playback;
    const live = isPlaying ? positionMs + Math.max(0, now - positionCapturedAtMs) : positionMs;
    return duration ? Math.min(live, duration) : live;
  });

  return {
    get duration() {
      return duration;
    },
    get position() {
      return position;
    },
    /** 0 to 1, or 0 when the length is unknown. */
    get fraction() {
      return duration ? position / duration : 0;
    },
  };
}
