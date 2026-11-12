// Typed access to the Rust side: commands (invoke) and events (listen).
// The types mirror the Rust structs' JSON (camelCase).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ProviderId = "Tidal" | "Spotify" | "YoutubeMusic";

export interface ProviderAvailability {
  id: ProviderId;
  displayName: string;
  installed: boolean;
  running: boolean;
  /** Running with Attune's debug port: connecting won't restart it. */
  ready: boolean;
  unsupported: string | null;
  /** A website Attune opens in a browser window of its own. */
  inBrowser: boolean;
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
  /** Album art from the provider that queued the song. */
  coverUrl?: string;
}

export interface QueueItem {
  entryId: string;
  track: Track;
  /** Display name of who queued it. */
  addedBy?: string | null;
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

// Server refusals people run into, in plain words. Commands reject with the
// full technical cause chain, which names the error code.
const FRIENDLY: [string, string][] = [
  ["(InvalidCredentials)", "There's no session with that code. Check the letters and try again."],
  ["(RateLimited)", "Too many wrong codes. Try again in a minute."],
  ["(SessionFull)", "That session is full."],
  ["(ServerBusy)", "The server is busy right now. Try again later."],
  ["(UnsupportedVersion)", "This version of Attune is too old or too new for the server."],
];

/** A message for the user from a rejected command. */
export function errorText(error: unknown): string {
  const text =
    typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.";
  return FRIENDLY.find(([code]) => text.includes(code))?.[1] ?? text;
}

export const api = {
  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (update: SettingsUpdate) => invoke<Settings>("update_settings", { update }),

  getProviders: () => invoke<ProviderOverview>("get_providers"),
  connectProvider: (id: ProviderId) => invoke<void>("connect_provider", { id }),
  /** Open a browser-based service in an ordinary window, for signing in. */
  openProviderSignIn: (id: ProviderId) => invoke<void>("open_provider_sign_in", { id }),
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
  /** Play a queued song now, in place of the current one. */
  queuePlayNow: (entryId: string) => invoke<void>("queue_play_now", { entryId }),
  getSessionStatus: () => invoke<RuntimeStatus | null>("get_session_status"),
  getJoinCode: () => invoke<string>("get_join_code"),

  setPinned: (pinned: boolean) => invoke<void>("set_window_pinned", { pinned }),
  hideWindow: () => invoke<void>("hide_window"),
  /** A join code from a attune:// link not handled yet. */
  takeJoinLink: () => invoke<string | null>("take_join_link"),
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
};

export const events = {
  onStatus: (handler: (status: RuntimeStatus) => void): Promise<UnlistenFn> =>
    listen<RuntimeStatus>("session-status-changed", (event) => handler(event.payload)),
  onNotice: (handler: (notice: Notice) => void): Promise<UnlistenFn> =>
    listen<Notice>("session-notice", (event) => handler(event.payload)),
  onProviderChanged: (handler: (id: ProviderId | null) => void): Promise<UnlistenFn> =>
    listen<ProviderId | null>("provider-changed", (event) => handler(event.payload)),
  onJoinLink: (handler: (code: string) => void): Promise<UnlistenFn> =>
    listen<string>("join-link", (event) => handler(event.payload)),
  onWindowShown: (handler: () => void): Promise<UnlistenFn> =>
    listen("window-shown", () => handler()),
};
