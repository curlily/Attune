// App-wide state. Rust is the source of truth: this mirrors what commands
// return and what events report, and the screens render from it.

import { LogicalSize, getCurrentWindow } from "@tauri-apps/api/window";
import {
  api,
  errorText,
  events,
  type JoinLink,
  type JoinRequest,
  type Notice,
  type ProviderAvailability,
  type ProviderId,
  type RuntimeStatus,
  type Settings,
} from "./api";
import { saveTheme, savedTheme, systemMode, watchSystemMode, type Mode, type ThemeChoice } from "./theme";

export type SectionId = "queue" | "listeners";

export type Screen = "loading" | "firstRun" | "connect" | "lobby" | "jam";
export type Tone = "info" | "success" | "warning" | "error";

export interface Toast {
  id: number;
  text: string;
  tone: Tone;
}

const TOAST_MS = 5000;
const JOIN_REQUEST_MS = 60_000;
/** The window's width (page px), always: tauri.conf.json fixes it too. */
export const WINDOW_WIDTH = 380;
/** The window's full height; it can be made shorter, down to TINY_HEIGHT. */
export const FULL_HEIGHT = 640;
export const TINY_HEIGHT = 96;
/** Below this the session shows only the tiny view. */
const TINY_BELOW = 190;

/**
 * The window size that holds `width` x `height` page pixels. Windows' Text
 * size setting (Accessibility) makes WebView2 draw the whole page bigger
 * without growing the window, so one page pixel can be more than one
 * logical pixel; sized in logical pixels the page overflowed and wrapped.
 * The page's devicePixelRatio carries the display scale and the text
 * scale; the window's scale factor only the display's. Either way round:
 * checked with WebView2 forced to 1.0 and to 1.5 on a 1.25 display, the
 * page got its 380 px both times and the lobby fit.
 */
export async function windowSize(width: number, height: number): Promise<LogicalSize> {
  const display = await getCurrentWindow()
    .scaleFactor()
    .catch(() => window.devicePixelRatio);
  const text = Math.min(3, Math.max(0.5, window.devicePixelRatio / display));
  return new LogicalSize(Math.round(width * text), Math.round(height * text));
}

/**
 * The window's invisible resize borders, in logical pixels. On Windows a
 * frameless window with a shadow keeps them around its page, and tao sizes
 * the page right (it adds them) but applies min/max sizes to the window
 * *with* them. Limits of 380 page px let the whole window be 380 wide, so
 * the first drag of an edge snapped the page narrower by the borders. Sent
 * with the limits, they make min and max the page's size again.
 */
async function resizeBorders(): Promise<{ width: number; height: number }> {
  if (!navigator.userAgent.includes("Windows")) return { width: 0, height: 0 };
  const win = getCurrentWindow();
  const [outer, inner, scale] = await Promise.all([win.outerSize(), win.innerSize(), win.scaleFactor()]);
  return {
    width: Math.max(0, outer.width - inner.width) / scale,
    height: Math.max(0, outer.height - inner.height) / scale,
  };
}

export const PROVIDER_NAMES: Record<string, string> = {
  Tidal: "TIDAL",
  Spotify: "Spotify",
  YoutubeMusic: "YouTube Music",
  AppleMusic: "Apple Music",
  tidal: "TIDAL",
  spotify: "Spotify",
  "youtube-music": "YouTube Music",
  "apple-music": "Apple Music",
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
  /** "Change" in the lobby: choosing another music app while one is connected. */
  choosingProvider = $state(false);
  /** The window's height; the session view shows less as it shrinks. */
  height = $state(window.innerHeight);
  /** The lobby's content height, which the window fits. */
  lobbyHeight = $state(0);
  pinned = $state(false);
  /** "auto" takes the connected music app's colour. */
  theme = $state<ThemeChoice>(savedTheme());
  /** Light or dark, following the system (always dark on Linux). */
  mode = $state<Mode>(systemMode());
  /** Take songs someone in the session can't play out of the queue. */
  autoRemove = $state(readFlag("attune.autoRemove"));
  /** Session view sections folded to their title (remembered). */
  collapsed = $state<Record<SectionId, boolean>>({
    queue: readFlag("attune.section.queue.collapsed"),
    listeners: readFlag("attune.section.listeners.collapsed"),
  });
  /** Join code from a attune:// link, joined as soon as the lobby can. */
  linkCode = $state<string | null>(null);
  /** Discord invite to join, the same way as `linkCode`. */
  linkInvite = $state<string | null>(null);
  /** Friends asking through Discord to join, newest last. */
  joinRequests = $state<JoinRequest[]>([]);
  /** Bumped each time the window is shown, to replay the opening animation. */
  shown = $state(0);

  #nextToast = 1;
  /** Counts status events, so a slower command result can't undo a newer one. */
  #statusEvents = 0;
  /** Height to go back to from the tiny view. */
  #heightBeforeTiny = FULL_HEIGHT;
  /** Entries already asked to be removed, so each is asked for once. */
  #removing = new Set<string>();

  screen: Screen = $derived.by(() => {
    if (!this.loaded) return "loading";
    if (!this.settings?.displayName) return "firstRun";
    if (this.status && this.status.sync !== "ended") return "jam";
    if (!this.connected || this.choosingProvider) return "connect";
    return "lobby";
  });

  get providerName(): string {
    return this.connected ? PROVIDER_NAMES[this.connected] : "your music app";
  }

  /** The session in its smallest form: song, listeners' dots, a few buttons. */
  get tiny(): boolean {
    return this.screen === "jam" && this.height < TINY_BELOW;
  }

  async init() {
    this.height = window.innerHeight;
    // Put a window that came up the wrong width back to the right one.
    if (window.innerWidth !== WINDOW_WIDTH) void this.setHeight(window.innerHeight || FULL_HEIGHT);
    this.mode = systemMode();
    watchSystemMode((mode) => (this.mode = mode));
    window.addEventListener("resize", () => {
      this.height = window.innerHeight;
      if (window.devicePixelRatio === this.#ratio) this.#steadyHeight = this.height;
    });
    this.#refitOnTextSize();
    void this.#applyLimits();
    await Promise.all([
      events.onStatus((status) => this.#onStatus(status)),
      events.onNotice((notice) => this.#onNotice(notice)),
      events.onProviderChanged((id) => {
        this.connected = id;
      }),
      events.onJoinLink((link) => this.#onJoinLink(link)),
      events.onDiscordJoinRequest((request) => this.#onJoinRequest(request)),
      events.onProviderLost((name) => {
        this.connected = null;
        this.choosingProvider = false;
        this.showSettings = false;
        this.toast(`Lost the connection to ${name}. Connect again to keep listening.`, "warning");
        void this.refreshProviders();
        void this.refreshRejoin();
      }),
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

  toggleSection(id: SectionId) {
    this.collapsed[id] = !this.collapsed[id];
    writeFlag(`attune.section.${id}.collapsed`, this.collapsed[id]);
  }

  setAutoRemove(on: boolean) {
    this.autoRemove = on;
    writeFlag("attune.autoRemove", on);
    this.#removeUnplayable();
  }

  /**
   * With auto-remove on, take out every queued song someone can't play.
   * Others with it on may ask too; a song already gone is no error.
   */
  #removeUnplayable() {
    if (!this.autoRemove || !this.status) return;
    for (const warning of this.status.warnings) {
      if (!warning.missingFor.length || this.#removing.has(warning.entryId)) continue;
      this.#removing.add(warning.entryId);
      api.queueRemove(warning.entryId).catch(() => this.#removing.delete(warning.entryId));
    }
  }

  setTheme(theme: ThemeChoice) {
    this.theme = theme;
    saveTheme(theme);
  }

  async setPinned(pinned: boolean) {
    this.pinned = pinned;
    await api.setPinned(pinned).catch((error) => this.toast(errorText(error), "error"));
  }

  #onJoinLink(link: JoinLink) {
    const code = "code" in link ? link.code : null;
    const name = code ?? "your friend's session";
    if (this.screen === "jam") {
      if (this.code !== code) this.toast(`Leave this session first to join ${name}.`, "warning");
      return;
    }
    if ("invite" in link) this.linkInvite = link.invite;
    else this.linkCode = link.code;
    if (this.screen !== "lobby") {
      this.toast(`Attune joins ${name} once your music app is connected.`, "info");
    }
  }

  #onJoinRequest(request: JoinRequest) {
    this.joinRequests = [
      ...this.joinRequests.filter((other) => other.userId !== request.userId),
      request,
    ].slice(-3);
    // Discord lets a request lapse after a while; so does the card.
    setTimeout(() => this.#dropRequest(request.userId), JOIN_REQUEST_MS);
  }

  /** Accept or decline a friend's request to join. */
  async answerJoinRequest(userId: string, accept: boolean) {
    this.#dropRequest(userId);
    await api.answerJoinRequest(userId, accept).catch((error) => this.toast(errorText(error), "error"));
  }

  #dropRequest(userId: string) {
    this.joinRequests = this.joinRequests.filter((request) => request.userId !== userId);
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
    await this.setHeight(FULL_HEIGHT);
  }

  toast(text: string, tone: Tone = "info") {
    const id = this.#nextToast++;
    this.toasts = [...this.toasts, { id, text, tone }].slice(-3);
    setTimeout(() => this.dismissToast(id), TOAST_MS);
  }

  dismissToast(id: number) {
    this.toasts = this.toasts.filter((toast) => toast.id !== id);
  }

  /** Snap to the tiny view, or back to the height it had before. */
  async toggleTiny() {
    if (this.tiny) {
      await this.setHeight(this.#heightBeforeTiny);
    } else {
      this.#heightBeforeTiny = this.height;
      await this.setHeight(TINY_HEIGHT);
    }
  }

  /** devicePixelRatio, and the page's height while the window had it. */
  #ratio = window.devicePixelRatio;
  #steadyHeight = window.innerHeight;

  /**
   * The Text size setting changed while open (or the window moved to a
   * screen with another scale): refit the window so the page keeps its
   * height. Only a change of devicePixelRatio counts: refitting on every
   * resize event fought the user dragging the window's edge (the page's
   * width lags behind the window's mid-drag), so the window flickered and
   * jumped when let go.
   */
  #refitOnTextSize() {
    matchMedia(`(resolution: ${this.#ratio}dppx)`).addEventListener(
      "change",
      () => {
        this.#ratio = window.devicePixelRatio;
        void this.setHeight(this.#steadyHeight);
        this.#refitOnTextSize();
      },
      { once: true },
    );
  }

  /**
   * Resize to `height`, always at the fixed width. Never read the width
   * back from the page: early in start-up the web view can report a much
   * smaller one, and sending that back locked a friend's window at a sliver.
   */
  async setHeight(height: number) {
    height = Math.round(height);
    if (height === this.height && window.innerWidth === WINDOW_WIDTH) return;
    try {
      await this.#applyLimits();
      await getCurrentWindow().setSize(await windowSize(WINDOW_WIDTH, height));
    } catch (error) {
      this.toast(errorText(error), "error");
    }
  }

  /**
   * Fix the width and bound the height (in page px, see `windowSize`).
   * tauri.conf.json's limits leave out the resize borders, so these are
   * set at start-up too, not only when the window is resized from here.
   */
  async #applyLimits() {
    const win = getCurrentWindow();
    const borders = await resizeBorders();
    const limit = async (height: number) => {
      const size = await windowSize(WINDOW_WIDTH, height);
      return new LogicalSize(size.width + borders.width, size.height + borders.height);
    };
    await win.setMaxSize(await limit(FULL_HEIGHT));
    await win.setMinSize(await limit(TINY_HEIGHT));
  }

  #onStatus(status: RuntimeStatus) {
    this.#statusEvents++;
    if (status.sync === "ended") {
      // The session is over for us; back to the lobby.
      this.status = null;
      this.code = null;
      void this.setHeight(FULL_HEIGHT);
      void this.refreshRejoin();
      return;
    }
    this.status = status;
    this.#removeUnplayable();
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
        this.toast(notice.reason === "left" ? "You left the session" : "The session has ended", "info");
        break;
    }
  }
}

function readFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

function writeFlag(key: string, on: boolean) {
  try {
    localStorage.setItem(key, on ? "1" : "0");
  } catch {
    // Applies until Attune restarts.
  }
}

export const app = new App();
