// App-wide state. Rust is the source of truth: this mirrors what commands
// return and what events report, and the screens render from it.

import { LogicalSize, getCurrentWindow } from "@tauri-apps/api/window";
import {
  api,
  errorText,
  events,
  type Notice,
  type ProviderAvailability,
  type ProviderId,
  type RuntimeStatus,
  type Settings,
} from "./api";

export type Screen = "loading" | "firstRun" | "connect" | "lobby" | "jam";
export type Tone = "info" | "success" | "warning" | "error";

export interface Toast {
  id: number;
  text: string;
  tone: Tone;
}

const TOAST_MS = 5000;
const MINI_HEIGHT = 96;

export const PROVIDER_NAMES: Record<string, string> = {
  Tidal: "TIDAL",
  Spotify: "Spotify",
  YoutubeMusic: "YouTube Music",
  tidal: "TIDAL",
  spotify: "Spotify",
  "youtube-music": "YouTube Music",
};

class App {
  loaded = $state(false);
  settings = $state<Settings | null>(null);
  providers = $state<ProviderAvailability[]>([]);
  connected = $state<ProviderId | null>(null);
  status = $state<RuntimeStatus | null>(null);
  code = $state<string | null>(null);
  rejoinCode = $state<string | null>(null);
  toasts = $state<Toast[]>([]);
  showSettings = $state(false);
  mini = $state(false);
  pinned = $state(false);
  /** Join code from a attune:// link, joined as soon as the lobby can. */
  linkCode = $state<string | null>(null);
  /** Bumped each time the window is shown, to replay the opening animation. */
  shown = $state(0);

  #nextToast = 1;
  /** Counts status events, so a slower command result can't undo a newer one. */
  #statusEvents = 0;
  #fullSize: LogicalSize | null = null;

  screen: Screen = $derived.by(() => {
    if (!this.loaded) return "loading";
    if (!this.settings?.displayName) return "firstRun";
    if (this.status && this.status.sync !== "ended") return "jam";
    if (!this.connected) return "connect";
    return "lobby";
  });

  get providerName(): string {
    return this.connected ? PROVIDER_NAMES[this.connected] : "your music app";
  }

  async init() {
    await Promise.all([
      events.onStatus((status) => this.#onStatus(status)),
      events.onNotice((notice) => this.#onNotice(notice)),
      events.onProviderChanged((id) => {
        this.connected = id;
      }),
      events.onJoinLink((code) => this.#onJoinLink(code)),
      events.onWindowShown(() => {
        this.shown++;
      }),
    ]);
    const [settings, overview, status, rejoin] = await Promise.all([
      api.getSettings(),
      api.getProviders(),
      api.getSessionStatus(),
      api.getRejoinOffer(),
    ]);
    this.settings = settings;
    this.providers = overview.providers;
    this.connected = overview.connected;
    this.status = status;
    this.rejoinCode = rejoin;
    if (status) this.code = await api.getJoinCode().catch(() => null);
    this.loaded = true;
    const link = await api.takeJoinLink().catch(() => null);
    if (link) this.#onJoinLink(link);
  }

  async setPinned(pinned: boolean) {
    this.pinned = pinned;
    await api.setPinned(pinned).catch((error) => this.toast(errorText(error), "error"));
  }

  #onJoinLink(code: string) {
    if (this.screen === "jam") {
      if (this.code !== code) this.toast(`Leave this jam first to join ${code}.`, "warning");
      return;
    }
    this.linkCode = code;
    if (this.screen !== "lobby") {
      this.toast(`Attune joins ${code} once your music app is connected.`, "info");
    }
  }

  async refreshProviders() {
    try {
      const overview = await api.getProviders();
      this.providers = overview.providers;
      this.connected = overview.connected;
    } catch (error) {
      this.toast(errorText(error), "error");
    }
  }

  async refreshRejoin() {
    this.rejoinCode = await api.getRejoinOffer().catch(() => null);
  }

  /** Run a create/join and adopt its result. Throws for the caller to show. */
  async enter(start: () => Promise<{ code: string; status: RuntimeStatus }>) {
    const before = this.#statusEvents;
    const info = await start();
    this.code = info.code;
    // Events can arrive before the command's reply; theirs is newer.
    if (this.#statusEvents === before) this.status = info.status;
    this.rejoinCode = null;
  }

  async leave() {
    try {
      await api.leaveSession();
    } catch (error) {
      this.toast(errorText(error), "error");
    }
    this.status = null;
    this.code = null;
    await this.setMini(false);
  }

  toast(text: string, tone: Tone = "info") {
    const id = this.#nextToast++;
    this.toasts = [...this.toasts, { id, text, tone }].slice(-3);
    setTimeout(() => this.dismissToast(id), TOAST_MS);
  }

  dismissToast(id: number) {
    this.toasts = this.toasts.filter((toast) => toast.id !== id);
  }

  /** Shrink the window to the mini view, or restore it. */
  async setMini(on: boolean) {
    if (on === this.mini) return;
    const window = getCurrentWindow();
    try {
      if (on) {
        const factor = await window.scaleFactor();
        this.#fullSize = (await window.innerSize()).toLogical(factor);
        this.mini = true;
        await window.setSize(new LogicalSize(this.#fullSize.width, MINI_HEIGHT));
      } else {
        this.mini = false;
        await window.setSize(this.#fullSize ?? new LogicalSize(380, 640));
      }
    } catch (error) {
      this.toast(errorText(error), "error");
    }
  }

  #onStatus(status: RuntimeStatus) {
    this.#statusEvents++;
    if (status.sync === "ended") {
      // The session is over for us; back to the lobby.
      this.status = null;
      this.code = null;
      void this.setMini(false);
      void this.refreshRejoin();
      return;
    }
    this.status = status;
  }

  #onNotice(notice: Notice) {
    const provider = this.providerName;
    switch (notice.kind) {
      case "queueEditUndone":
        this.toast("Someone changed the queue at the same time, so your change was undone.", "warning");
        break;
      case "songUnavailable":
        this.toast(`“${notice.title}” isn't on ${provider}. You'll rejoin at the next song.`, "warning");
        break;
      case "resynced":
        this.toast("Back in sync", "success");
        break;
      case "desynchronized":
        this.toast(`Stopped following: ${notice.message}`, "error");
        break;
      case "serverError":
        this.toast(notice.message, "error");
        break;
      case "ended":
        this.toast(notice.reason === "left" ? "You left the jam" : "The jam has ended", "info");
        break;
    }
  }
}

export const app = new App();
