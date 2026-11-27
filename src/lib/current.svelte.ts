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

/** Redrawn at least this often while playing (the time shown), at most this often. */
const SLOWEST_TICK_MS = 1_000;
const FASTEST_TICK_MS = 50;

/**
 * Live position of the session's playback. Playback is controlled in the
 * music app, so this is display only.
 *
 * Redrawn only when something visible changes: the time shown (once a
 * second) or the bar (once it has moved a device pixel; `bar` tells how
 * wide that is). It used to redraw every frame, which on a 144–240 Hz
 * screen kept the window's renderer and GPU busy for a bar moving a
 * couple of pixels a second.
 */
export function livePosition(status: () => RuntimeStatus | null) {
  let now = $state(Date.now());
  // Width of the widest bar shown, in device pixels.
  let barPixels = $state(0);

  const duration = $derived(status()?.queue[0]?.track.durationMs ?? 0);
  const liveAt = (at: number) => {
    const playback = status()?.playback;
    if (!playback) return 0;
    const { isPlaying, positionMs, positionCapturedAtMs } = playback;
    const live = isPlaying ? positionMs + Math.max(0, at - positionCapturedAtMs) : positionMs;
    return duration ? Math.min(live, duration) : live;
  };
  const position = $derived(liveAt(now));

  $effect(() => {
    if (!status()?.playback.isPlaying) return;
    const perPixel = duration && barPixels ? duration / barPixels : SLOWEST_TICK_MS;
    const nextIn = () => {
      const toNextSecond = SLOWEST_TICK_MS - (liveAt(Date.now()) % SLOWEST_TICK_MS);
      return Math.max(FASTEST_TICK_MS, Math.min(toNextSecond, perPixel));
    };
    let timer = setTimeout(function tick() {
      now = Date.now();
      timer = setTimeout(tick, nextIn());
    }, nextIn());
    now = Date.now();
    return () => clearTimeout(timer);
  });

  return {
    /** Action for the bar's track: its width sets how often the bar moves. */
    bar(node: HTMLElement) {
      const measure = () => {
        barPixels = Math.max(barPixels, Math.round(node.clientWidth * (window.devicePixelRatio || 1)));
      };
      const observer = new ResizeObserver(() => {
        barPixels = 0;
        measure();
      });
      observer.observe(node);
      measure();
      return { destroy: () => observer.disconnect() };
    },
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
