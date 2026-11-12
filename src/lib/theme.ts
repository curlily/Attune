// Each music app's signature colour, for its logo and the "match" theme.
// `accent` is the lighter shade used for text and marks on the dark
// background, `strong` fills buttons, `onAccent` is text on those fills.

export interface ProviderColors {
  accent: string;
  strong: string;
  onAccent: string;
}

export const PROVIDER_COLORS: Record<string, ProviderColors> = {
  tidal: { accent: "#ffffff", strong: "#ededed", onAccent: "#000000" },
  spotify: { accent: "#1ed760", strong: "#1db954", onAccent: "#04130a" },
  youtubemusic: { accent: "#ff5266", strong: "#ff0033", onAccent: "#ffffff" },
};

/** Accepts the Rust ID ("YoutubeMusic") or the participant label ("youtube-music"). */
export function providerColors(provider?: string | null): ProviderColors | undefined {
  return provider ? PROVIDER_COLORS[provider.toLowerCase().replaceAll("-", "")] : undefined;
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
export function applyTheme(theme: ThemeChoice, provider: string | null) {
  const root = document.documentElement.style;
  const colors = theme === "auto" ? providerColors(provider) : undefined;
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
