// Updates: found on launch, offered, and installed only when asked.

import { getVersion } from "@tauri-apps/api/app";
import { api, errorText, events, type UpdateInfo } from "./api";

/** Wait this long after launch, so the check doesn't slow starting up. */
const LAUNCH_CHECK_DELAY_MS = 4000;

class Updates {
  /** This copy's version, once known. */
  version = $state<string | null>(null);
  /** A newer release found by the last check. */
  available = $state<UpdateInfo | null>(null);
  /** "Later": hidden until the next launch or the next check. */
  postponed = $state(false);
  checking = $state(false);
  installing = $state(false);
  /** Download progress from 0 to 1; null while unknown. */
  progress = $state<number | null>(null);
  error = $state<string | null>(null);

  /** Whether to show the offer now. */
  get offered(): boolean {
    return this.available !== null && !this.postponed;
  }

  async init() {
    this.version = await getVersion().catch(() => null);
    await events.onUpdateProgress(({ downloaded, total }) => {
      this.progress = total ? Math.min(1, downloaded / total) : null;
    });
    setTimeout(() => void this.#launchCheck(), LAUNCH_CHECK_DELAY_MS);
  }

  /** Quiet: a failed check on launch (offline, say) isn't worth a message. */
  async #launchCheck() {
    try {
      this.available = await api.checkForUpdate(true);
    } catch {
      // Checked again on the next launch, or from Settings.
    }
  }

  /** The check from Settings. Resolves to whether an update was found. */
  async check(): Promise<boolean> {
    this.checking = true;
    this.error = null;
    try {
      this.available = await api.checkForUpdate(false);
      this.postponed = false;
      return this.available !== null;
    } finally {
      this.checking = false;
    }
  }

  async install() {
    if (this.installing) return;
    this.installing = true;
    this.progress = 0;
    this.error = null;
    try {
      // Attune closes and restarts from here on success.
      await api.installUpdate();
    } catch (failure) {
      this.error = errorText(failure);
      this.installing = false;
      this.progress = null;
    }
  }

  later() {
    this.postponed = true;
    this.error = null;
  }

  async skip() {
    const version = this.available?.version;
    if (!version) return;
    await api.skipUpdate(version).catch(() => {});
    this.available = null;
    this.error = null;
  }
}

export const updates = new Updates();
