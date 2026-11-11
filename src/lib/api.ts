// Typed access to the Rust side: commands (invoke) and events (listen).
// The types mirror the Rust structs' JSON (camelCase).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ProviderId = "Tidal" | "Spotify";

export interface ProviderAvailability {
  id: ProviderId;
  displayName: string;
  installed: boolean;
  running: boolean;
  /** Running with MyJam's debug port: connecting won't restart it. */
  ready: boolean;
  unsupported: string | null;
}

export interface ProviderOverview {
  providers: ProviderAvailability[];
  connected: ProviderId | null;
}

export interface Settings {
  displayName: string | null;
  serverUrl: string;
  lastProvider: ProviderId | null;
  listeningOffsetMs: number;
  clientId: string;
}

export interface SettingsUpdate {
  displayName?: string;
  serverUrl?: string;
  listeningOffsetMs?: number;
}

export type SyncState =
  | "resyncing"
  | "synced"
  | "waitingForNextSong"
  | "steppedOut"
  | "error"
  | "reconnecting"
  | "ended";

export type ParticipantStatus =
  | "synced"
  | "resyncing"
  | "waitingForNextSong"
  | "detached"
  | "error";

export interface Participant {
  clientId: string;
  displayName: string;
  provider?: string;
  status: ParticipantStatus;
  connected: boolean;
  unavailableEntryIds: string[];
}

export interface Track {
  title: string;
  artists: string[];
  isrc?: string;
  album?: string;
  durationMs?: number;
}

export interface QueueItem {
  entryId: string;
  track: Track;
  playableHere: boolean;
}

export interface Playback {
  isPlaying: boolean;
  positionMs: number;
  /** On this computer's clock. */
  positionCapturedAtMs: number;
}

export interface UnavailableWarning {
  entryId: string;
  title: string;
  artists: string[];
  songsAhead: number;
  missingFor: string[];
}

export interface RuntimeStatus {
  sessionId: string;
  clientId: string;
  sync: SyncState;
  sequence: number;
  queue: QueueItem[];
  playback: Playback;
  participants: Participant[];
  warnings: UnavailableWarning[];
}

export interface SessionInfo {
  code: string;
  status: RuntimeStatus;
}

export type Notice =
  | { kind: "queueEditUndone" }
  | { kind: "songUnavailable"; title: string }
  | { kind: "resynced" }
  | { kind: "desynchronized"; message: string }
  | { kind: "serverError"; message: string }
  | { kind: "ended"; reason: string };

interface PlayState {
  track: { cover_url: string | null } | null;
}

/** Commands reject with a plain message string. */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Something went wrong.";
}

export const api = {
  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (update: SettingsUpdate) => invoke<Settings>("update_settings", { update }),

  getProviders: () => invoke<ProviderOverview>("get_providers"),
  connectProvider: (id: ProviderId) => invoke<void>("connect_provider", { id }),
  disconnectProvider: () => invoke<void>("disconnect_provider"),
  /** Cover art of what the connected app is playing, if it has one. */
  coverUrl: async () => (await invoke<PlayState>("get_playstate")).track?.cover_url ?? null,

  createSession: () => invoke<SessionInfo>("create_session"),
  joinSession: (code: string) => invoke<SessionInfo>("join_session", { code }),
  getRejoinOffer: () => invoke<string | null>("get_rejoin_offer"),
  rejoinSession: () => invoke<SessionInfo>("rejoin_session"),
  dismissRejoin: () => invoke<void>("dismiss_rejoin"),
  leaveSession: () => invoke<void>("leave_session"),
  stepOut: () => invoke<void>("step_out"),
  resync: () => invoke<void>("resync"),
  queueMove: (entryId: string, toIndex: number) => invoke<void>("queue_move", { entryId, toIndex }),
  queueRemove: (entryId: string) => invoke<void>("queue_remove", { entryId }),
  getSessionStatus: () => invoke<RuntimeStatus | null>("get_session_status"),
  getJoinCode: () => invoke<string>("get_join_code"),
};

export const events = {
  onStatus: (handler: (status: RuntimeStatus) => void): Promise<UnlistenFn> =>
    listen<RuntimeStatus>("session-status-changed", (event) => handler(event.payload)),
  onNotice: (handler: (notice: Notice) => void): Promise<UnlistenFn> =>
    listen<Notice>("session-notice", (event) => handler(event.payload)),
  onProviderChanged: (handler: (id: ProviderId | null) => void): Promise<UnlistenFn> =>
    listen<ProviderId | null>("provider-changed", (event) => handler(event.payload)),
};
