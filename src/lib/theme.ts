// Each music app's signature colour, for its logo and the "match" theme,
// in a shade for each mode. `accent` is used for text and marks on the
// background, `strong` fills buttons, `onAccent` is text on those fills.

export type Mode = "dark" | "light";

export interface ProviderColors {
  accent: string;
  strong: string;
  onAccent: string;
}

export const PROVIDER_COLORS: Record<string, Record<Mode, ProviderColors>> = {
  tidal: {
    dark: { accent: "#ffffff", strong: "#ededed", onAccent: "#000000" },
    // TIDAL's white is invisible on light: its black.
    light: { accent: "#000000", strong: "#111111", onAccent: "#ffffff" },
  },
  spotify: {
    dark: { accent: "#1ed760", strong: "#1db954", onAccent: "#04130a" },
    light: { accent: "#13843d", strong: "#1db954", onAccent: "#04130a" },
  },
  youtubemusic: {
    dark: { accent: "#ff5266", strong: "#ff0033", onAccent: "#ffffff" },
    light: { accent: "#d4002b", strong: "#ff0033", onAccent: "#ffffff" },
  },
  applemusic: {
    dark: { accent: "#ff4e6b", strong: "#fa2d48", onAccent: "#ffffff" },
    light: { accent: "#d60f2d", strong: "#fa2d48", onAccent: "#ffffff" },
  },
};

/** Accepts the Rust ID ("YoutubeMusic") or the participant label ("youtube-music"). */
export function providerColors(
  provider: string | null | undefined,
  mode: Mode,
): ProviderColors | undefined {
  return provider ? PROVIDER_COLORS[provider.toLowerCase().replaceAll("-", "")]?.[mode] : undefined;
}

/**
 * Light or dark, following the system on Windows (WebView2 reports its
 * setting). Linux always gets dark: whether its desktop is light can't be
 * told reliably from the web view.
 */
export function systemMode(): Mode {
  if (/Linux/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent)) return "dark";
  return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/** Calls `onChange` when the system switches between light and dark. */
export function watchSystemMode(onChange: (mode: Mode) => void): () => void {
  const query = matchMedia("(prefers-color-scheme: light)");
  const listener = () => onChange(systemMode());
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}

export type ThemeChoice = "attune" | "auto";

const THEME_KEY = "attune.theme";

export function savedTheme(): ThemeChoice {
  try {
    return localStorage.getItem(THEME_KEY) === "auto" ? "auto" : "attune";
  } catch {
    return "attune";
  }
}

export function saveTheme(theme: ThemeChoice) {
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    // Applies until Attune restarts.
  }
}

/**
 * Point the accent tokens at the connected app's colour, or back at Attune's
 * own when there's no app or the theme isn't "auto".
 */
export function applyTheme(theme: ThemeChoice, provider: string | null, mode: Mode) {
  document.documentElement.dataset.mode = mode;
  const root = document.documentElement.style;
  const colors = theme === "auto" ? providerColors(provider, mode) : undefined;
  if (!colors) {
    for (const token of ["--accent", "--accent-strong", "--accent-tint", "--on-accent"]) {
      root.removeProperty(token);
    }
    return;
  }
  root.setProperty("--accent", colors.accent);
  root.setProperty("--accent-strong", colors.strong);
  root.setProperty("--accent-tint", `color-mix(in srgb, ${colors.accent} 14%, transparent)`);
  root.setProperty("--on-accent", colors.onAccent);
}
