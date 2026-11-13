// Typed access to the Rust side: commands (invoke) and events (listen).
// The types mirror the Rust structs' JSON (camelCase).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ProviderId = "Tidal" | "Spotify" | "YoutubeMusic" | "AppleMusic";

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
  skippedUpdateVersion: string | null;
  /** Show the session's song on your Discord profile. */
  discordPresence: boolean;
  /** Let others change what's playing (counts while you host). */
  othersCanControl: boolean;
}

/** A session to join: by its code, or by an invite passed on by Discord. */
export type JoinLink = { code: string } | { invite: string };

/** A friend asking, through Discord, to join your session. */
export interface JoinRequest {
  userId: string;
  name: string;
  avatarUrl: string | null;
}

export interface UpdateInfo {
  version: string;
  currentVersion: string;
  /** Release notes, if the release has any. */
  notes: string | null;
}

export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}

export interface SettingsUpdate {
  displayName?: string;
  serverUrl?: string;
  listeningOffsetMs?: number;
  discordPresence?: boolean;
  othersCanControl?: boolean;
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
  /** Who hosts the session; null on servers that predate hosts. */
  hostClientId: string | null;
  /** Whether people other than the host may change what's playing. */
  othersCanControl: boolean;
  /** What happens when the current song ends. */
  repeat: RepeatMode;
}

export type RepeatMode = "off" | "one";

export interface SessionInfo {
  code: string;
  status: RuntimeStatus;
}

export type Notice =
  | { kind: "queueEditUndone" }
  | { kind: "notAllowed"; host: string }
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
  /** Join through a Discord invite (after Join in Discord). */
  joinInvite: (secret: string) => invoke<SessionInfo>("join_invite", { secret }),
  answerJoinRequest: (userId: string, accept: boolean) =>
    invoke<void>("answer_join_request", { userId, accept }),
  getRejoinOffer: () => invoke<string | null>("get_rejoin_offer"),
  rejoinSession: () => invoke<SessionInfo>("rejoin_session"),
  dismissRejoin: () => invoke<void>("dismiss_rejoin"),
  leaveSession: () => invoke<void>("leave_session"),
  stepOut: () => invoke<void>("step_out"),
  resync: () => invoke<void>("resync"),
  queueMove: (entryId: string, toIndex: number) => invoke<void>("queue_move", { entryId, toIndex }),
  queueRemove: (entryId: string) => invoke<void>("queue_remove", { entryId }),
  setRepeat: (mode: RepeatMode) => invoke<void>("set_repeat", { mode }),
  /** Play a queued song now, in place of the current one. */
  queuePlayNow: (entryId: string) => invoke<void>("queue_play_now", { entryId }),
  getSessionStatus: () => invoke<RuntimeStatus | null>("get_session_status"),
  getJoinCode: () => invoke<string>("get_join_code"),

  setPinned: (pinned: boolean) => invoke<void>("set_window_pinned", { pinned }),
  hideWindow: () => invoke<void>("hide_window"),
  /** A join (attune:// link or Discord) not handled yet. */
  takeJoinLink: () => invoke<JoinLink | null>("take_join_link"),
  getAutostart: () => invoke<boolean>("get_autostart"),

  /** A newer release, if there is one. `automatic`: the quiet check on launch. */
  checkForUpdate: (automatic: boolean) => invoke<UpdateInfo | null>("check_for_update", { automatic }),
  /** Download and install what the last check found, then restart. */
  installUpdate: () => invoke<void>("install_update"),
  skipUpdate: (version: string) => invoke<void>("skip_update", { version }),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
};

export const events = {
  onStatus: (handler: (status: RuntimeStatus) => void): Promise<UnlistenFn> =>
    listen<RuntimeStatus>("session-status-changed", (event) => handler(event.payload)),
  onNotice: (handler: (notice: Notice) => void): Promise<UnlistenFn> =>
    listen<Notice>("session-notice", (event) => handler(event.payload)),
  onProviderChanged: (handler: (id: ProviderId | null) => void): Promise<UnlistenFn> =>
    listen<ProviderId | null>("provider-changed", (event) => handler(event.payload)),
  /** The music app went away on its own (closed, or reopened normally). */
  onProviderLost: (handler: (name: string) => void): Promise<UnlistenFn> =>
    listen<string>("provider-lost", (event) => handler(event.payload)),
  onJoinLink: (handler: (link: JoinLink) => void): Promise<UnlistenFn> =>
    listen<JoinLink>("join-link", (event) => handler(event.payload)),
  onDiscordJoinRequest: (handler: (request: JoinRequest) => void): Promise<UnlistenFn> =>
    listen<JoinRequest>("discord-join-request", (event) => handler(event.payload)),
  onUpdateProgress: (handler: (progress: UpdateProgress) => void): Promise<UnlistenFn> =>
    listen<UpdateProgress>("update-progress", (event) => handler(event.payload)),
  onWindowShown: (handler: () => void): Promise<UnlistenFn> =>
    listen("window-shown", () => handler()),
};
