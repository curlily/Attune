// How sync states are shown: always an icon and text, never colour alone.

import type { Participant, SyncState } from "./api";
import type { IconName } from "./Icon.svelte";

export type BadgeTone = "success" | "warning" | "danger" | "muted" | "accent";

export interface Badge {
  text: string;
  icon: IconName;
  tone: BadgeTone;
  busy?: boolean;
}

export function ownBadge(sync: SyncState): Badge {
  switch (sync) {
    case "synced":
      return { text: "In sync", icon: "check", tone: "success" };
    case "resyncing":
      return { text: "Catching up", icon: "refresh", tone: "accent", busy: true };
    case "waitingForNextSong":
      return { text: "Waiting for the next song", icon: "clock", tone: "warning" };
    case "steppedOut":
      return { text: "Stepped out", icon: "eject", tone: "muted" };
    case "error":
      return { text: "Out of sync", icon: "alert", tone: "danger" };
    case "reconnecting":
      return { text: "Reconnecting", icon: "cloudOff", tone: "warning", busy: true };
    case "ended":
      return { text: "Ended", icon: "x", tone: "muted" };
  }
}

export function participantBadge(participant: Participant): Badge {
  if (!participant.connected) return { text: "Reconnecting", icon: "cloudOff", tone: "warning" };
  switch (participant.status) {
    case "synced":
      return { text: "In sync", icon: "check", tone: "success" };
    case "resyncing":
      return { text: "Catching up", icon: "refresh", tone: "accent" };
    case "waitingForNextSong":
      return { text: "Waiting for next song", icon: "clock", tone: "warning" };
    case "detached":
      return { text: "Stepped out", icon: "eject", tone: "muted" };
    case "error":
      return { text: "Out of sync", icon: "alert", tone: "danger" };
  }
}

export function formatTime(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}
